use crate::application::kamino_shortlist::{
    KaminoShortlistRefreshRequest, KaminoShortlistRuntime, PreparedExecutionContext,
    ShortlistCandidate, ShortlistEntry, ShortlistState, enforce_candidate_history_limit,
    select_shortlist,
};
use crate::application::kamino_tx::{
    KaminoBuildRequest, KaminoBuiltAttempt, KaminoReserveMeta, KaminoResolvedAccounts,
    build_create_ata_idempotent_ix, build_kamino_attempt_tx, decode_kamino_reserve,
    discriminator, find_kamino_liquidate_ix, get_or_fetch_kamino_reserve_meta,
    get_ata, get_ata_with_program, ix_refresh_obligation, ix_refresh_reserve,
    kamino_destination_ata_setup_enabled, optional_pubkey,
    resolve_kamino_accounts_from_tx_info,
};
use crate::application::solend_hunter::execute_solend_opportunity;
use crate::config::hunter::{
    HunterRuntimeConfig, HunterTxFetchConfig, read_kamino_signal_source_config,
};
use crate::config::wallet::WalletToken;
use crate::domain::protocol::{KAMINO_PROGRAM_ID, SOLEND_PROGRAM_ID};
use crate::ports::jito::JitoPort;
use crate::ports::logger::{LiquidationLogger, ObservationEvent};
use crate::ports::rpc::{
    ProgramAccount, RpcClient, SignatureStatusInfo, StreamingRpcClient,
};
use crate::utils::log_stderr;
use borsh::BorshDeserialize;
use dashmap::mapref::entry::Entry as DashEntry;
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::sysvar;
use std::collections::HashMap;
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const FARMS_PROGRAM: &str = "FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr";

// Solend
const SOLEND_LIQUIDATE_FILTER: &str = "LiquidateWithoutReceivingCtokens";

// Kamino
pub const DEFAULT_KAMINO_REPLAY_SIGNATURE: &str =
    "3V11m9fyEiUqbrihZPF1QJdXW9g6tr4mHS9VtCS2BNSunUeQWvRTgXf48uoC7gXgij8bKp7hSERZ1CZvNhSYgCLA";
const DEFAULT_JITO_TIP_ACCOUNTS: [&str; 8] = [
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
];

/// Tip is refreshed every 60s.
const TIP_REFRESH_SECS: u64 = 60;

pub(crate) fn hunter_dry_run_enabled() -> bool {
    std::env::var("HUNTER_DRY_RUN")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(crate) fn jito_send_max_attempts() -> usize {
    std::env::var("JITO_SEND_MAX_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 4))
        .unwrap_or(2)
}

fn hunter_verbose_log(enabled: bool, protocol: &str, message: impl AsRef<str>) {
    if enabled {
        log_stderr(format!("[hunter-{protocol}] {}", message.as_ref()));
    }
}

pub(crate) fn format_signature_status(status: Option<&SignatureStatusInfo>) -> String {
    match status {
        Some(status) => format!(
            "status(slot={:?},confirmation={:?},has_error={})",
            status.slot, status.confirmation_status, status.has_error
        ),
        None => "status(absent)".to_string(),
    }
}

pub(crate) fn is_expired_blockhash_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains("expired blockhash")
}

pub(crate) fn is_retryable_jito_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("rate limited")
        || lower.contains("network congested")
        || lower.contains("too many requests")
        || lower.contains("temporarily unavailable")
        || lower.contains("timeout")
        || is_expired_blockhash_error(&lower)
}

pub(crate) fn retry_tip_lamports(base_tip_lamports: u64, attempt: usize) -> u64 {
    if attempt <= 1 {
        return base_tip_lamports;
    }
    let multiplier = 1.0 + 0.25 * (attempt.saturating_sub(1) as f64);
    ((base_tip_lamports as f64) * multiplier).round() as u64
}

pub(crate) fn retry_backoff_ms(attempt: usize) -> u64 {
    match attempt {
        0 | 1 => 0,
        2 => 40,
        3 => 100,
        _ => 200,
    }
}

fn kamino_logs_look_like_liquidation(logs: &[String]) -> bool {
    logs.iter().any(|log| {
        let lower = log.to_ascii_lowercase();
        lower.contains("liquidate") || lower.contains("[truncated]")
    })
}

fn kamino_logs_indicate_healthy_obligation(logs: &[String]) -> bool {
    logs.iter().any(|log| {
        let lower = log.to_ascii_lowercase();
        lower.contains("obligation is healthy") || lower.contains("cannot be liquidated")
    })
}

fn summarize_candidate_logs(logs: &[String]) -> String {
    logs.iter()
        .filter(|log| {
            let lower = log.to_ascii_lowercase();
            lower.contains("liquidate")
                || lower.contains("flashborrow")
                || lower.contains("[truncated]")
        })
        .take(4)
        .map(|log| log.replace('\n', " "))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HunterTraceEvent {
    pub(crate) timestamp: String,
    pub(crate) protocol: &'static str,
    pub(crate) stage: &'static str,
    pub(crate) signature: String,
    pub(crate) obligation: Option<String>,
    pub(crate) repay_mint: Option<String>,
    pub(crate) repay_symbol: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) ws_received_at_ms: Option<u64>,
    pub(crate) elapsed_ms: Option<u64>,
    pub(crate) bundle_id: Option<String>,
    pub(crate) shortlist_hit: Option<bool>,
    pub(crate) shortlist_state: Option<String>,
    pub(crate) shortlist_age_ms: Option<u64>,
    pub(crate) prepared_context_used: Option<bool>,
    pub(crate) candidate_score: Option<f64>,
    pub(crate) refresh_reason: Option<String>,
}

impl HunterTraceEvent {
    pub(crate) fn new(
        protocol: &'static str,
        stage: &'static str,
        signature: impl Into<String>,
    ) -> Self {
        Self {
            timestamp: crate::utils::utc_now(),
            protocol,
            stage,
            signature: signature.into(),
            obligation: None,
            repay_mint: None,
            repay_symbol: None,
            reason: None,
            detail: None,
            ws_received_at_ms: None,
            elapsed_ms: None,
            bundle_id: None,
            shortlist_hit: None,
            shortlist_state: None,
            shortlist_age_ms: None,
            prepared_context_used: None,
            candidate_score: None,
            refresh_reason: None,
        }
    }

    pub(crate) fn with_obligation(mut self, obligation: impl Into<String>) -> Self {
        self.obligation = Some(obligation.into());
        self
    }

    pub(crate) fn with_optional_repay_mint(mut self, repay_mint: Option<String>) -> Self {
        self.repay_mint = repay_mint;
        self
    }

    pub(crate) fn with_repay_mint(mut self, repay_mint: impl Into<String>) -> Self {
        self.repay_mint = Some(repay_mint.into());
        self
    }

    pub(crate) fn with_repay_symbol(mut self, repay_symbol: impl Into<String>) -> Self {
        self.repay_symbol = Some(repay_symbol.into());
        self
    }

    pub(crate) fn with_optional_reason(mut self, reason: Option<String>) -> Self {
        self.reason = reason;
        self
    }

    pub(crate) fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub(crate) fn with_optional_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    pub(crate) fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(crate) fn with_timing(mut self, ws_received_at_ms: u64, elapsed_ms: u64) -> Self {
        self.ws_received_at_ms = Some(ws_received_at_ms);
        self.elapsed_ms = Some(elapsed_ms);
        self
    }

    pub(crate) fn with_optional_bundle_id(mut self, bundle_id: Option<String>) -> Self {
        self.bundle_id = bundle_id;
        self
    }

    pub(crate) fn with_shortlist_context(
        mut self,
        shortlist_hit: Option<bool>,
        shortlist_state: Option<String>,
        shortlist_age_ms: Option<u64>,
        prepared_context_used: Option<bool>,
        candidate_score: Option<f64>,
        refresh_reason: Option<String>,
    ) -> Self {
        self.shortlist_hit = shortlist_hit;
        self.shortlist_state = shortlist_state;
        self.shortlist_age_ms = shortlist_age_ms;
        self.prepared_context_used = prepared_context_used;
        self.candidate_score = candidate_score;
        self.refresh_reason = refresh_reason;
        self
    }
}

#[derive(Clone)]
pub(crate) struct HunterTraceLogger {
    writer: Option<Arc<std::sync::Mutex<std::fs::File>>>,
}

impl HunterTraceLogger {
    pub(crate) fn from_env() -> Self {
        let path =
            std::env::var("HUNTER_LOG_FILE").unwrap_or_else(|_| "hunter_trace.jsonl".to_string());

        if path.eq_ignore_ascii_case("off") || path.eq_ignore_ascii_case("disabled") {
            return Self { writer: None };
        }

        let writer = (|| -> std::io::Result<std::fs::File> {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        })()
        .map(|file| Arc::new(std::sync::Mutex::new(file)))
        .ok();

        Self { writer }
    }

    pub(crate) fn log(&self, event: HunterTraceEvent) {
        let Some(writer) = &self.writer else {
            return;
        };
        let Ok(line) = serde_json::to_string(&event) else {
            return;
        };
        if let Ok(mut file) = writer.lock() {
            let _ = writeln!(file, "{}", line);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum HunterSignalSource {
    PrimaryRpc,
    SecondaryRpc,
    PriceFeed,
}

impl HunterSignalSource {
    fn as_str(self) -> &'static str {
        match self {
            HunterSignalSource::PrimaryRpc => "primary_rpc",
            HunterSignalSource::SecondaryRpc => "secondary_rpc",
            HunterSignalSource::PriceFeed => "price_feed",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum HunterSignalKind {
    KaminoLogLiquidation,
    PriceFeedPredictedLiquidable,
}

impl HunterSignalKind {
    fn as_str(&self) -> &'static str {
        match self {
            HunterSignalKind::KaminoLogLiquidation => "kamino_log_liquidation",
            HunterSignalKind::PriceFeedPredictedLiquidable => "price_feed_predicted_liquidable",
        }
    }
}

#[derive(Debug, Clone)]
struct HunterSignalEvent {
    source: HunterSignalSource,
    protocol: &'static str,
    signal_kind: HunterSignalKind,
    received_at_ms: u64,
    signature: Option<String>,
    obligation_pubkey: String,
    repay_mint: Option<String>,
    detail: Option<String>,
    tx_info: Option<crate::ports::rpc::TransactionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
struct SignalFingerprint {
    protocol: &'static str,
    obligation: String,
}

#[derive(Debug, Clone, Serialize)]
struct DetectionStats {
    first_ts_ms: u64,
    count: u32,
    won_lock: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SignalLockSummary {
    protocol: &'static str,
    obligation: String,
    repay_mint: Option<String>,
    winner_source: String,
    fire_outcome: String,
    detections: HashMap<String, DetectionStats>,
}

#[derive(Debug, Clone)]
enum FireOutcome {
    BundleSent,
    DryRun,
    BundleFailed,
    OpportunityError,
    HeldExpired,
    Skipped,
}

impl FireOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            FireOutcome::BundleSent => "bundle_sent",
            FireOutcome::DryRun => "dry_run",
            FireOutcome::BundleFailed => "bundle_failed",
            FireOutcome::OpportunityError => "opportunity_error",
            FireOutcome::HeldExpired => "held_expired",
            FireOutcome::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum KaminoExecutionOutcome {
    BundleSent,
    DryRun,
    BundleFailed,
    Skipped,
}

#[derive(Debug, Clone)]
enum LockState {
    Held {
        winner_source: HunterSignalSource,
        acquired_at_ms: u64,
    },
    Firing {
        winner_source: HunterSignalSource,
        acquired_at_ms: u64,
    },
    Fired {
        winner_source: HunterSignalSource,
        acquired_at_ms: u64,
        outcome: FireOutcome,
    },
}

#[derive(Debug, Clone)]
struct LockRecord {
    state: LockState,
    repay_mint: Option<String>,
    detections: HashMap<HunterSignalSource, DetectionStats>,
}

impl LockRecord {
    fn new_held(
        source: HunterSignalSource,
        acquired_at_ms: u64,
        repay_mint: Option<String>,
    ) -> Self {
        Self {
            state: LockState::Held {
                winner_source: source,
                acquired_at_ms,
            },
            repay_mint,
            detections: HashMap::new(),
        }
    }

    #[cfg(test)]
    fn winner_source(&self) -> HunterSignalSource {
        match &self.state {
            LockState::Held { winner_source, .. }
            | LockState::Firing { winner_source, .. }
            | LockState::Fired { winner_source, .. } => *winner_source,
        }
    }

    fn acquired_at_ms(&self) -> u64 {
        match &self.state {
            LockState::Held { acquired_at_ms, .. }
            | LockState::Firing { acquired_at_ms, .. }
            | LockState::Fired { acquired_at_ms, .. } => *acquired_at_ms,
        }
    }

    fn is_expired(&self, now_ms: u64, lock_ms: u64) -> bool {
        now_ms.saturating_sub(self.acquired_at_ms()) >= lock_ms
    }

    fn record_detection(
        &mut self,
        source: HunterSignalSource,
        received_at_ms: u64,
        won_lock: bool,
    ) {
        let entry = self.detections.entry(source).or_insert(DetectionStats {
            first_ts_ms: received_at_ms,
            count: 0,
            won_lock,
        });
        entry.count = entry.count.saturating_add(1);
        entry.first_ts_ms = entry.first_ts_ms.min(received_at_ms);
        entry.won_lock |= won_lock;
    }

    fn transition_to_firing(&mut self, source: HunterSignalSource, _now_ms: u64) -> bool {
        match &self.state {
            LockState::Held {
                winner_source,
                acquired_at_ms,
            } if *winner_source == source => {
                self.state = LockState::Firing {
                    winner_source: *winner_source,
                    acquired_at_ms: *acquired_at_ms,
                };
                true
            }
            _ => false,
        }
    }

    fn transition_to_fired(
        &mut self,
        source: HunterSignalSource,
        _now_ms: u64,
        outcome: FireOutcome,
    ) -> bool {
        match &self.state {
            LockState::Firing {
                winner_source,
                acquired_at_ms,
            } if *winner_source == source => {
                self.state = LockState::Fired {
                    winner_source: *winner_source,
                    acquired_at_ms: *acquired_at_ms,
                    outcome,
                };
                true
            }
            _ => false,
        }
    }

    fn into_summary(self, fingerprint: SignalFingerprint) -> SignalLockSummary {
        let winner_source = match &self.state {
            LockState::Held { winner_source, .. }
            | LockState::Firing { winner_source, .. }
            | LockState::Fired { winner_source, .. } => winner_source.as_str().to_string(),
        };
        let fire_outcome = match self.state {
            LockState::Held { .. } => FireOutcome::HeldExpired,
            LockState::Firing { .. } => FireOutcome::HeldExpired,
            LockState::Fired { outcome, .. } => outcome,
        };

        SignalLockSummary {
            protocol: fingerprint.protocol,
            obligation: fingerprint.obligation,
            repay_mint: self.repay_mint,
            winner_source,
            fire_outcome: fire_outcome.as_str().to_string(),
            detections: self
                .detections
                .into_iter()
                .map(|(source, stats)| (source.as_str().to_string(), stats))
                .collect(),
        }
    }
}

#[derive(Clone)]
struct SignalMetricsLogger {
    summary_tx: mpsc::Sender<SignalLockSummary>,
}

impl SignalMetricsLogger {
    fn from_env() -> Self {
        let (summary_tx, mut summary_rx) = mpsc::channel::<SignalLockSummary>(512);
        let path = std::env::var("HUNTER_SIGNAL_METRICS_FILE")
            .unwrap_or_else(|_| "hunter_signal_metrics.jsonl".to_string());

        tokio::spawn(async move {
            if path.eq_ignore_ascii_case("off") || path.eq_ignore_ascii_case("disabled") {
                while summary_rx.recv().await.is_some() {}
                return;
            }

            let writer = (|| -> std::io::Result<std::fs::File> {
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent)?;
                    }
                }
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
            })();

            let Ok(mut file) = writer else {
                return;
            };

            while let Some(summary) = summary_rx.recv().await {
                if let Ok(line) = serde_json::to_string(&summary) {
                    let _ = writeln!(file, "{}", line);
                }
            }
        });

        Self { summary_tx }
    }

    fn try_log_summary(&self, summary: SignalLockSummary) {
        let _ = self.summary_tx.try_send(summary);
    }
}

/// Token available in the hunter wallet (loaded from wallet.toml at startup).
#[derive(Debug, Clone)]
pub(crate) struct WalletTokenRuntime {
    pub(crate) symbol: String,
    pub(crate) mint: String,
    pub(crate) max_repay_native: u64,
    pub(crate) source_ata: Pubkey,
}

fn jito_tip_accounts() -> Vec<Pubkey> {
    let configured = std::env::var("JITO_TIP_ACCOUNTS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|value| Pubkey::from_str(value.trim()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !configured.is_empty() {
        return configured;
    }

    DEFAULT_JITO_TIP_ACCOUNTS
        .iter()
        .filter_map(|value| Pubkey::from_str(value).ok())
        .collect()
}

pub(crate) fn select_jito_tip_account(seed: &str) -> anyhow::Result<Pubkey> {
    let accounts = jito_tip_accounts();
    if accounts.is_empty() {
        anyhow::bail!("no valid Jito tip account configured");
    }

    let digest = Sha256::digest(seed.as_bytes());
    let idx = (digest[0] as usize) % accounts.len();
    Ok(accounts[idx])
}

fn build_wallet_token_index(
    liquidator: &Pubkey,
    wallet_tokens: &[WalletToken],
) -> anyhow::Result<HashMap<String, WalletTokenRuntime>> {
    let mut index = HashMap::new();
    for token in wallet_tokens {
        let mint_pk = Pubkey::from_str(&token.mint)?;
        index.insert(
            token.mint.clone(),
            WalletTokenRuntime {
                symbol: token.symbol.clone(),
                mint: token.mint.clone(),
                max_repay_native: token.max_repay_native,
                source_ata: get_ata(liquidator, &mint_pk),
            },
        );
    }
    Ok(index)
}

pub struct HunterService<
    R: RpcClient,
    JI: JitoPort,
    L: LiquidationLogger + Clone,
> {
    hunter_rpc: R,
    signal_secondary_rpc: Option<R>,
    jito: JI,
    logger: L,
    keypair: Arc<Keypair>,
    max_repay_usd: f64,
    trace_logger: HunterTraceLogger,
}

impl<
        R: RpcClient,
        JI: JitoPort,
        L: LiquidationLogger + Clone + 'static,
    > HunterService<R, JI, L>
{
    pub fn new(
        hunter_rpc: R,
        signal_secondary_rpc: Option<R>,
        jito: JI,
        logger: L,
        keypair: Arc<Keypair>,
        max_repay_usd: f64,
    ) -> Self {
        Self {
            hunter_rpc,
            signal_secondary_rpc,
            jito,
            logger,
            keypair,
            max_repay_usd,
            trace_logger: HunterTraceLogger::from_env(),
        }
    }

    // ── Kamino autonomous hunter ─────────────────────────────────────────────
    //
    // Flow:
    //   primary RPC WS notification (LiquidateObligationAndRedeemReserveCollateralV2)
    //   → getTransaction (single attempt, 500ms timeout)
    //   → extract obligation PDA + reserve addresses from competitor's tx accounts
    //   → build optimistic tx (RefreshReserve x2 + RefreshObligation + Liquidate)
    //     using pre-cached blockhash and tip
    //   → sendBundle (Jito)
    //
    // The observer is NOT involved in this cycle. It logs independently.
    /// Runs one Kamino hunter loop with runtime config reloaded from env at loop start.
    /// The outer restart loop in `spawn_hunter` is therefore also the reload boundary.
    pub async fn run_kamino(&self, wallet_tokens: Vec<WalletToken>) -> anyhow::Result<()>
    where
        R: StreamingRpcClient + RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let runtime = HunterRuntimeConfig::from_env("KAMINO");
        log_stderr("[hunter-kamino] runtime config reloaded from env at loop start".to_string());
        let wallet_index = Arc::new(build_wallet_token_index(
            &self.keypair.pubkey(),
            &wallet_tokens,
        )?);
        let reserve_cache: Arc<tokio::sync::RwLock<HashMap<String, KaminoReserveMeta>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let shortlist_runtime = Arc::new(tokio::sync::RwLock::new(KaminoShortlistRuntime::new()));
        let (shortlist_refresh_tx, mut shortlist_refresh_rx) =
            mpsc::channel::<KaminoShortlistRefreshRequest>(64);

        log_stderr(format!(
            "[hunter-kamino] Starting autonomous hunter. Wallet: {} | max_repay: ${:.0} | signal_commitment={:?} | tx_fetch={:?} | shortlist_enabled={} | shortlist_max={} | shortlist_refresh_secs={} | tokens: {}",
            self.keypair.pubkey(),
            self.max_repay_usd,
            runtime.signal_commitment,
            runtime.tx_fetch,
            runtime.shortlist_enabled,
            runtime.shortlist_max_obligations,
            runtime.shortlist_refresh_secs,
            wallet_tokens.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>().join(", ")
        ));

        // ── Hot cache: blockhash ─────────────────────────────────────────────
        let initial_blockhash = self
            .hunter_rpc
            .get_latest_blockhash()
            .await
            .unwrap_or_default();
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(initial_blockhash));
        let blockhash_refresh_secs = std::env::var("BLOCKHASH_REFRESH_SECS")
            .unwrap_or_else(|_| "3".to_string())
            .parse::<u64>()
            .unwrap_or(3);

        {
            let rpc = self.hunter_rpc.clone();
            let bh = cached_blockhash.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(blockhash_refresh_secs))
                        .await;
                    match rpc.get_latest_blockhash().await {
                        Ok(hash) => {
                            *bh.write().await = hash;
                        }
                        Err(e) => {
                            log_stderr(format!("[hunter-kamino] blockhash refresh failed: {}", e))
                        }
                    }
                }
            });
        }

        // ── Hot cache: Jito tip ──────────────────────────────────────────────
        let initial_tip = self.jito.get_tip_recommendation().await.unwrap_or(100_000);
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(initial_tip));
        {
            let jito = self.jito.clone();
            let tip = cached_tip.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(TIP_REFRESH_SECS)).await;
                    if let Ok(t) = jito.get_tip_recommendation().await {
                        tip.store(t, Ordering::Relaxed);
                    }
                }
            });
        }

        let recent_non_whitelist: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let signal_locks: Arc<DashMap<SignalFingerprint, LockRecord>> = Arc::new(DashMap::new());
        let signal_metrics = SignalMetricsLogger::from_env();
        let (signal_tx, mut signal_rx) = mpsc::channel::<HunterSignalEvent>(512);
        let hunter_wallet = self.keypair.pubkey().to_string();

        {
            let locks = signal_locks.clone();
            let metrics = signal_metrics.clone();
            let lock_ms = runtime.signal_lock_ms;
            tokio::spawn(async move {
                let sweep_every = std::cmp::max(250, lock_ms / 2);
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(sweep_every)).await;
                    let now = now_ms();
                    let expired = collect_expired_signal_fingerprints(&locks, now, lock_ms);
                    remove_expired_signal_fingerprints(&locks, &metrics, expired, now, lock_ms);
                }
            });
        }

        let source_config = read_kamino_signal_source_config(self.signal_secondary_rpc.is_some());
        let primary_rpc_enabled = source_config.primary_rpc_enabled;
        let secondary_rpc_enabled = source_config.secondary_rpc_enabled;
        let price_feed_enabled = source_config.price_feed_enabled;

        if primary_rpc_enabled {
            spawn_kamino_log_signal_source(
                HunterSignalSource::PrimaryRpc,
                self.hunter_rpc.clone(),
                runtime,
                signal_tx.clone(),
                self.trace_logger.clone(),
                self.logger.clone(),
                hunter_wallet.clone(),
            );
        }

        if secondary_rpc_enabled {
            if let Some(secondary_rpc) = self.signal_secondary_rpc.clone() {
                spawn_kamino_log_signal_source(
                    HunterSignalSource::SecondaryRpc,
                    secondary_rpc,
                    runtime,
                    signal_tx.clone(),
                    self.trace_logger.clone(),
                    self.logger.clone(),
                    hunter_wallet.clone(),
                );
            } else {
                log_stderr(
                    "[hunter-kamino] secondary RPC signal source enabled but no HUNTER_SIGNAL_SECONDARY_* endpoint configured.",
                );
            }
        }

        if price_feed_enabled {
            spawn_price_feed_signal_source(
                self.hunter_rpc.clone(),
                wallet_tokens.clone(),
                signal_tx.clone(),
                self.trace_logger.clone(),
            );
        }

        if runtime.shortlist_enabled {
            let shortlist_runtime_for_refresh = shortlist_runtime.clone();
            let reserve_cache = reserve_cache.clone();
            let trace_logger = self.trace_logger.clone();
            let refresh_runtime = runtime;
            let refresh_rpc = self.hunter_rpc.clone();
            tokio::spawn(async move {
                while let Some(request) = shortlist_refresh_rx.recv().await {
                    match refresh_kamino_shortlist(
                        &refresh_rpc,
                        &shortlist_runtime_for_refresh,
                        &reserve_cache,
                        refresh_runtime,
                        &request.reason,
                    )
                    .await
                    {
                        Ok(active_count) => {
                            trace_logger.log(
                                HunterTraceEvent::new(
                                    "kamino",
                                    "shortlist_refresh",
                                    request.prioritize_obligation.unwrap_or_else(|| {
                                        format!("shortlist:{}", request.reason)
                                    }),
                                )
                                .with_detail(format!(
                                    "reason={} active={}",
                                    request.reason, active_count
                                ))
                                .with_timing(now_ms(), 0)
                                .with_shortlist_context(
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    Some(request.reason),
                                ),
                            );
                        }
                        Err(error) => {
                            trace_logger.log(
                                HunterTraceEvent::new(
                                    "kamino",
                                    "error",
                                    format!("shortlist:{}", request.reason),
                                )
                                .with_reason("shortlist_refresh_failed")
                                .with_detail(error.to_string())
                                .with_timing(now_ms(), 0)
                                .with_shortlist_context(
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    Some(request.reason),
                                ),
                            );
                        }
                    }
                }
            });

            let shortlist_runtime = shortlist_runtime.clone();
            let shortlist_refresh_tx = shortlist_refresh_tx.clone();
            let refresh_secs = runtime.shortlist_refresh_secs;
            let debounce_ms = runtime.shortlist_refresh_debounce_ms;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(refresh_secs)).await;
                    request_shortlist_refresh(
                        &shortlist_refresh_tx,
                        &shortlist_runtime,
                        debounce_ms,
                        "safety_interval",
                        None,
                    )
                    .await;
                }
            });
        }

        while let Some(signal) = signal_rx.recv().await {
            if runtime.shortlist_enabled {
                if let (Some(repay_mint), Some(tx_info)) =
                    (signal.repay_mint.as_ref(), signal.tx_info.as_ref())
                {
                    if let Some(wallet_token) = wallet_index.get(repay_mint) {
                        if let Ok(resolved) = resolve_kamino_accounts_from_tx_info(
                            tx_info,
                            Some(&signal.obligation_pubkey),
                            Some(repay_mint),
                        ) {
                            let mut shortlist_state = shortlist_runtime.write().await;
                            let candidate = shortlist_state
                                .candidates
                                .entry(signal.obligation_pubkey.clone())
                                .or_insert_with(|| {
                                    ShortlistCandidate::new(
                                        prepared_context_from_resolved_accounts(
                                            &resolved,
                                            wallet_token.symbol.clone(),
                                            "observed_liquidation".to_string(),
                                        ),
                                        signal.received_at_ms,
                                    )
                                });
                            candidate.context = prepared_context_from_resolved_accounts(
                                &resolved,
                                wallet_token.symbol.clone(),
                                "observed_liquidation".to_string(),
                            );
                            candidate.record_observation(signal.received_at_ms);
                            enforce_candidate_history_limit(
                                &mut shortlist_state.candidates,
                                runtime.shortlist_candidate_history_limit,
                            );
                            drop(shortlist_state);

                            request_shortlist_refresh(
                                &shortlist_refresh_tx,
                                &shortlist_runtime,
                                runtime.shortlist_refresh_debounce_ms,
                                "candidate_observed",
                                Some(signal.obligation_pubkey.clone()),
                            )
                            .await;
                        }
                    }
                }
            }

            let shortlist_entry = if runtime.shortlist_enabled {
                let state = shortlist_runtime.read().await;
                shortlist_entry_for_signal(&state, &signal)
            } else {
                None
            };
            let (
                shortlist_hit,
                shortlist_state_value,
                shortlist_age_ms,
                shortlist_score,
                shortlist_refresh_reason,
            ) = shortlist_trace_fields(shortlist_entry.as_ref());

            let fingerprint = SignalFingerprint {
                protocol: signal.protocol,
                obligation: signal.obligation_pubkey.clone(),
            };
            let won_lock = try_accept_signal(
                &signal_locks,
                &signal_metrics,
                fingerprint.clone(),
                &signal,
                runtime.signal_lock_ms,
            );

            self.trace_logger.log(
                HunterTraceEvent::new(
                    "kamino",
                    if won_lock {
                        "signal_accepted"
                    } else {
                        "signal_rejected_duplicate"
                    },
                    signal.signature.clone().unwrap_or_else(|| {
                        format!("{}:{}", signal.source.as_str(), signal.obligation_pubkey)
                    }),
                )
                .with_obligation(signal.obligation_pubkey.clone())
                .with_optional_repay_mint(signal.repay_mint.clone())
                .with_optional_reason((!won_lock).then(|| "lock_held".to_string()))
                .with_detail(format!(
                    "source={} kind={}",
                    signal.source.as_str(),
                    signal.signal_kind.as_str()
                ))
                .with_timing(signal.received_at_ms, 0)
                .with_shortlist_context(
                    shortlist_hit,
                    shortlist_state_value.clone(),
                    shortlist_age_ms,
                    Some(false),
                    shortlist_score,
                    shortlist_refresh_reason.clone(),
                ),
            );

            if !won_lock {
                continue;
            }

            if runtime.shortlist_enabled && shortlist_entry.is_some() {
                {
                    let mut state = shortlist_runtime.write().await;
                    if let Some(candidate) = state.candidates.get_mut(&signal.obligation_pubkey) {
                        candidate.cooldown(
                            now_ms().saturating_add(runtime.shortlist_cooling_down_ms),
                        );
                    }
                }
                request_shortlist_refresh(
                    &shortlist_refresh_tx,
                    &shortlist_runtime,
                    runtime.shortlist_refresh_debounce_ms,
                    "shortlisted_liquidation",
                    Some(signal.obligation_pubkey.clone()),
                )
                .await;
            }

            let keypair = self.keypair.clone();
            let jito = self.jito.clone();
            let bh = cached_blockhash.clone();
            let tip = cached_tip.clone();
            let non_whitelist = recent_non_whitelist.clone();
            let max_repay = self.max_repay_usd;
            let wallet_idx = wallet_index.clone();
            let reserve_cache = reserve_cache.clone();
            let trace_logger = self.trace_logger.clone();
            let runtime_cfg = runtime;
            let airtable_logger = self.logger.clone();
            let hunter_wallet = hunter_wallet.clone();
            let signal_locks = signal_locks.clone();
            let shortlist_context = shortlist_entry.map(|entry| entry.context);
            let sig_for_error = signal.signature.clone().unwrap_or_else(|| {
                format!("{}:{}", signal.source.as_str(), signal.obligation_pubkey)
            });
            let rpc = match signal.source {
                HunterSignalSource::PrimaryRpc => self.hunter_rpc.clone(),
                HunterSignalSource::SecondaryRpc => self
                    .signal_secondary_rpc
                    .clone()
                    .unwrap_or_else(|| self.hunter_rpc.clone()),
                HunterSignalSource::PriceFeed => self.hunter_rpc.clone(),
            };

            tokio::spawn(async move {
                mark_lock_firing(&signal_locks, &fingerprint, signal.source, now_ms());
                let result = execute_kamino_opportunity(
                    sig_for_error.clone(),
                    signal.received_at_ms,
                    rpc,
                    jito,
                    keypair,
                    wallet_idx,
                    reserve_cache,
                    bh,
                    tip,
                    non_whitelist,
                    max_repay,
                    runtime_cfg,
                    trace_logger.clone(),
                    airtable_logger.clone(),
                    signal.source,
                    signal.tx_info,
                    Some(signal.obligation_pubkey.clone()),
                    signal.repay_mint.clone(),
                    shortlist_context,
                )
                .await;

                let outcome = match &result {
                    Ok(KaminoExecutionOutcome::BundleSent) => FireOutcome::BundleSent,
                    Ok(KaminoExecutionOutcome::DryRun) => FireOutcome::DryRun,
                    Ok(KaminoExecutionOutcome::BundleFailed) => FireOutcome::BundleFailed,
                    Ok(KaminoExecutionOutcome::Skipped) => FireOutcome::Skipped,
                    Err(_) => FireOutcome::OpportunityError,
                };
                mark_lock_fired(
                    &signal_locks,
                    &fingerprint,
                    signal.source,
                    now_ms(),
                    outcome,
                );

                if let Err(e) = result {
                    trace_logger.log(
                        HunterTraceEvent::new("kamino", "error", sig_for_error.clone())
                            .with_obligation(signal.obligation_pubkey.clone())
                            .with_optional_repay_mint(signal.repay_mint.clone())
                            .with_reason("opportunity_error")
                            .with_detail(format!("source={} {}", signal.source.as_str(), e))
                            .with_timing(
                                signal.received_at_ms,
                                elapsed_ms_since(signal.received_at_ms),
                            ),
                    );
                    let _ = log_hunter_observation(
                        &airtable_logger,
                        "Kamino",
                        "HUNTER_BUNDLE_FAILED",
                        &sig_for_error,
                        Some(signal.obligation_pubkey.clone()),
                        Some(hunter_wallet),
                        None,
                        Some(format!("source={} {}", signal.source.as_str(), e)),
                        Some(elapsed_ms_since(signal.received_at_ms)),
                    )
                    .await;
                    log_stderr(format!(
                        "[hunter-kamino] opportunity error (source={}): {}",
                        signal.source.as_str(),
                        e
                    ));
                }
            });
        }

        Ok(())
    }

    pub async fn replay_kamino(
        &self,
        wallet_tokens: Vec<WalletToken>,
        signature: String,
    ) -> anyhow::Result<()>
    where
        R: RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let wallet_index = Arc::new(build_wallet_token_index(
            &self.keypair.pubkey(),
            &wallet_tokens,
        )?);
        let reserve_cache: Arc<tokio::sync::RwLock<HashMap<String, KaminoReserveMeta>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(
            self.hunter_rpc
                .get_latest_blockhash()
                .await
                .unwrap_or_default(),
        ));
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(
            self.jito.get_tip_recommendation().await.unwrap_or(100_000),
        ));
        let runtime = HunterRuntimeConfig::from_env("KAMINO");
        let non_whitelist: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        log_stderr(format!("[hunter-kamino] REPLAY | signature={}", signature));
        self.trace_logger.log(
            HunterTraceEvent::new("kamino", "replay_start", signature.clone())
                .with_detail("manual replay")
                .with_timing(now_ms(), 0),
        );

        execute_kamino_opportunity(
            signature,
            now_ms(),
            self.hunter_rpc.clone(),
            self.jito.clone(),
            self.keypair.clone(),
            wallet_index,
            reserve_cache,
            cached_blockhash,
            cached_tip,
            non_whitelist,
            self.max_repay_usd,
            runtime,
            self.trace_logger.clone(),
            self.logger.clone(),
            HunterSignalSource::PrimaryRpc,
            None,
            None,
            None,
            None,
        )
        .await
        .map(|_| ())
    }

    // ── Solend autonomous hunter ─────────────────────────────────────────────
    //
    // Flow (identical spirit to Kamino):
    //   QuikNode WS notification (LiquidateWithoutReceivingCtokens)
    //   → getTransaction (single attempt, 500ms timeout)
    //   → copy competitor's refresh instructions verbatim
    //   → copy competitor's liquidate instruction, replacing user accounts
    //     with our own, and setting our own liquidity_amount
    //   → build tx with pre-cached blockhash + tip → sendBundle (Jito)
    //
    // No getAccountInfo, no obligation decode, no is_liquidatable() check.
    // Optimistic: include RefreshObligation in tx, let Solend decide on-chain.
    /// Runs one Solend hunter loop with runtime config reloaded from env at loop start.
    /// The outer restart loop in `spawn_hunter` is therefore also the reload boundary.
    pub async fn run_solend(&self, wallet_tokens: Vec<WalletToken>) -> anyhow::Result<()>
    where
        R: StreamingRpcClient + RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let runtime = HunterRuntimeConfig::from_env("SOLEND");
        log_stderr("[hunter-solend] runtime config reloaded from env at loop start".to_string());
        let wallet_index = Arc::new(build_wallet_token_index(
            &self.keypair.pubkey(),
            &wallet_tokens,
        )?);

        log_stderr(format!(
            "[hunter-solend] Starting autonomous hunter. Wallet: {} | signal_commitment={:?} | tx_fetch={:?} | tokens: {}",
            self.keypair.pubkey(), runtime.signal_commitment, runtime.tx_fetch,
            wallet_tokens.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>().join(", ")
        ));

        // ── Hot cache: blockhash ─────────────────────────────────────────────
        let initial_blockhash = self
            .hunter_rpc
            .get_latest_blockhash()
            .await
            .unwrap_or_default();
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(initial_blockhash));
        let blockhash_refresh_secs = std::env::var("BLOCKHASH_REFRESH_SECS")
            .unwrap_or_else(|_| "12".to_string())
            .parse::<u64>()
            .unwrap_or(12);

        {
            let rpc = self.hunter_rpc.clone();
            let bh = cached_blockhash.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(blockhash_refresh_secs))
                        .await;
                    match rpc.get_latest_blockhash().await {
                        Ok(hash) => {
                            *bh.write().await = hash;
                        }
                        Err(e) => {
                            log_stderr(format!("[hunter-solend] blockhash refresh failed: {}", e))
                        }
                    }
                }
            });
        }

        // ── Hot cache: Jito tip ──────────────────────────────────────────────
        let initial_tip = self.jito.get_tip_recommendation().await.unwrap_or(100_000);
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(initial_tip));
        {
            let jito = self.jito.clone();
            let tip = cached_tip.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(TIP_REFRESH_SECS)).await;
                    if let Ok(t) = jito.get_tip_recommendation().await {
                        tip.store(t, Ordering::Relaxed);
                    }
                }
            });
        }

        // ── Obligation dedup ─────────────────────────────────────────────────
        let recent_obligations: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        // ── Main WS loop ─────────────────────────────────────────────────────
        loop {
            let mut rx = match self
                .hunter_rpc
                .subscribe_to_logs(SOLEND_PROGRAM_ID, runtime.signal_commitment)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    log_stderr(format!(
                        "[hunter-solend] WS subscribe failed: {}. Retrying in 2s...",
                        e
                    ));
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            log_stderr("[hunter-solend] WS subscription task started.");

            loop {
                let entry = match tokio::time::timeout(
                    tokio::time::Duration::from_secs(runtime.ws_idle_timeout_secs),
                    rx.recv(),
                )
                .await
                {
                    Ok(Some(e)) => e,
                    Ok(None) => {
                        log_stderr("[hunter-solend] WS stream ended. Reconnecting...");
                        break;
                    }
                    Err(_) => {
                        log_stderr(format!(
                            "[hunter-solend] WS idle timeout: no messages received for {}s. Reconnecting...",
                            runtime.ws_idle_timeout_secs
                        ));
                        break;
                    }
                };

                if !entry
                    .logs
                    .iter()
                    .any(|l| l.contains(SOLEND_LIQUIDATE_FILTER))
                {
                    continue;
                }

                let sig = entry.signature.clone();
                let rpc = self.hunter_rpc.clone();
                let keypair = self.keypair.clone();
                let jito = self.jito.clone();
                let wallet_idx = wallet_index.clone();
                let bh = cached_blockhash.clone();
                let tip = cached_tip.clone();
                let dedup = recent_obligations.clone();
                let trace_logger = self.trace_logger.clone();
                let tx_fetch_cfg = runtime.tx_fetch;
                let airtable_logger = self.logger.clone();
                let hunter_wallet = self.keypair.pubkey().to_string();
                let err_sig = sig.clone();
                let err_trace_logger = trace_logger.clone();

                hunter_verbose_log(
                    runtime.verbose,
                    "solend",
                    format!("candidate | sig={}", sig),
                );

                trace_logger.log(
                    HunterTraceEvent::new("solend", "ws_received", sig.clone())
                        .with_timing(entry.received_at_ms, 0),
                );
                let _ = log_hunter_observation(
                    &airtable_logger,
                    "Solend",
                    "HUNTER_WS_RECEIVED",
                    &sig,
                    None,
                    Some(hunter_wallet.clone()),
                    None,
                    None,
                    Some(0),
                )
                .await;

                tokio::spawn(async move {
                    if let Err(e) = execute_solend_opportunity(
                        sig,
                        entry.received_at_ms,
                        rpc,
                        jito,
                        keypair,
                        wallet_idx,
                        bh,
                        tip,
                        dedup,
                        tx_fetch_cfg,
                        trace_logger,
                        airtable_logger.clone(),
                    )
                    .await
                    {
                        err_trace_logger.log(
                            HunterTraceEvent::new("solend", "error", err_sig.clone())
                                .with_reason("opportunity_error")
                                .with_detail(e.to_string())
                                .with_timing(
                                    entry.received_at_ms,
                                    elapsed_ms_since(entry.received_at_ms),
                                ),
                        );
                        let _ = log_hunter_observation(
                            &airtable_logger,
                            "Solend",
                            "HUNTER_BUNDLE_FAILED",
                            &err_sig,
                            None,
                            Some(hunter_wallet),
                            None,
                            Some(e.to_string()),
                            Some(elapsed_ms_since(entry.received_at_ms)),
                        )
                        .await;
                        log_stderr(format!("[hunter-solend] opportunity error: {}", e));
                    }
                });
            }
        }
    }

    pub async fn replay_solend(
        &self,
        wallet_tokens: Vec<WalletToken>,
        signature: String,
    ) -> anyhow::Result<()>
    where
        R: RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let wallet_index = Arc::new(build_wallet_token_index(
            &self.keypair.pubkey(),
            &wallet_tokens,
        )?);
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(
            self.hunter_rpc
                .get_latest_blockhash()
                .await
                .unwrap_or_default(),
        ));
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(
            self.jito.get_tip_recommendation().await.unwrap_or(100_000),
        ));
        let tx_fetch = HunterTxFetchConfig::from_env("SOLEND");
        let dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        log_stderr(format!("[hunter-solend] REPLAY | signature={}", signature));
        self.trace_logger.log(
            HunterTraceEvent::new("solend", "replay_start", signature.clone())
                .with_detail("manual replay")
                .with_timing(now_ms(), 0),
        );

        execute_solend_opportunity(
            signature,
            now_ms(),
            self.hunter_rpc.clone(),
            self.jito.clone(),
            self.keypair.clone(),
            wallet_index,
            cached_blockhash,
            cached_tip,
            dedup,
            tx_fetch,
            self.trace_logger.clone(),
            self.logger.clone(),
        )
        .await
    }
}

fn try_accept_signal(
    locks: &DashMap<SignalFingerprint, LockRecord>,
    metrics: &SignalMetricsLogger,
    fingerprint: SignalFingerprint,
    signal: &HunterSignalEvent,
    lock_ms: u64,
) -> bool {
    let now = signal.received_at_ms;
    match locks.entry(fingerprint.clone()) {
        DashEntry::Vacant(v) => {
            let mut record = LockRecord::new_held(signal.source, now, signal.repay_mint.clone());
            record.record_detection(signal.source, now, true);
            v.insert(record);
            true
        }
        DashEntry::Occupied(mut o) => {
            if o.get().is_expired(now, lock_ms) {
                let expired = o.insert(LockRecord::new_held(
                    signal.source,
                    now,
                    signal.repay_mint.clone(),
                ));
                metrics.try_log_summary(expired.into_summary(fingerprint));
                o.get_mut().record_detection(signal.source, now, true);
                true
            } else {
                o.get_mut().record_detection(signal.source, now, false);
                if o.get().repay_mint.is_none() && signal.repay_mint.is_some() {
                    o.get_mut().repay_mint = signal.repay_mint.clone();
                }
                false
            }
        }
    }
}

fn mark_lock_firing(
    locks: &DashMap<SignalFingerprint, LockRecord>,
    fingerprint: &SignalFingerprint,
    source: HunterSignalSource,
    now_ms: u64,
) {
    if let Some(mut record) = locks.get_mut(fingerprint) {
        let _ = record.transition_to_firing(source, now_ms);
    }
}

fn mark_lock_fired(
    locks: &DashMap<SignalFingerprint, LockRecord>,
    fingerprint: &SignalFingerprint,
    source: HunterSignalSource,
    now_ms: u64,
    outcome: FireOutcome,
) {
    if let Some(mut record) = locks.get_mut(fingerprint) {
        let _ = record.transition_to_fired(source, now_ms, outcome);
    }
}

fn collect_expired_signal_fingerprints(
    locks: &DashMap<SignalFingerprint, LockRecord>,
    now_ms: u64,
    lock_ms: u64,
) -> Vec<SignalFingerprint> {
    let mut expired = Vec::new();
    for entry in locks.iter() {
        if entry.value().is_expired(now_ms, lock_ms) {
            expired.push(entry.key().clone());
        }
    }
    expired
}

fn remove_expired_signal_fingerprints(
    locks: &DashMap<SignalFingerprint, LockRecord>,
    metrics: &SignalMetricsLogger,
    fingerprints: Vec<SignalFingerprint>,
    now_ms: u64,
    lock_ms: u64,
) {
    for fingerprint in fingerprints {
        if let DashEntry::Occupied(o) = locks.entry(fingerprint.clone()) {
            if o.get().is_expired(now_ms, lock_ms) {
                let (fp, record) = o.remove_entry();
                metrics.try_log_summary(record.into_summary(fp));
            }
        }
    }
}

fn shortlist_trace_fields(entry: Option<&ShortlistEntry>) -> (Option<bool>, Option<String>, Option<u64>, Option<f64>, Option<String>) {
    match entry {
        Some(entry) => (
            Some(true),
            Some(entry.state.as_str().to_string()),
            Some(entry.shortlist_age_ms),
            Some(entry.distance_to_liq),
            Some(entry.refresh_reason.clone()),
        ),
        None => (Some(false), None, None, None, None),
    }
}

fn prepared_context_from_resolved_accounts(
    resolved: &KaminoResolvedAccounts,
    repay_symbol: String,
    inclusion_reason: String,
) -> PreparedExecutionContext {
    PreparedExecutionContext {
        obligation_pubkey: resolved.obligation_pubkey.clone(),
        repay_mint: resolved.repay_mint.clone(),
        repay_symbol,
        wallet_eligible: true,
        repay_reserve: resolved.repay_reserve.clone(),
        withdraw_reserve: resolved.withdraw_reserve.clone(),
        withdraw_mint: resolved.withdraw_liquidity_mint.clone(),
        active_reserve_pubkeys: vec![],
        inclusion_reason,
    }
}

fn active_kamino_reserve_pubkeys(
    obligation: &crate::domain::kamino::Obligation,
) -> Vec<String> {
    let mut reserve_pubkeys = Vec::new();

    for deposit in obligation.deposits.iter() {
        if deposit.deposited_amount > 0 || deposit.market_value_sf > 0 {
            let reserve = Pubkey::new_from_array(deposit.deposit_reserve).to_string();
            if !reserve_pubkeys.contains(&reserve) {
                reserve_pubkeys.push(reserve);
            }
        }
    }

    for borrow in obligation.borrows.iter() {
        if borrow.borrowed_amount_sf > 0 || borrow.market_value_sf > 0 {
            let reserve = Pubkey::new_from_array(borrow.borrow_reserve).to_string();
            if !reserve_pubkeys.contains(&reserve) {
                reserve_pubkeys.push(reserve);
            }
        }
    }

    reserve_pubkeys
}

async fn refresh_kamino_shortlist<R: RpcClient>(
    rpc: &R,
    runtime: &tokio::sync::RwLock<KaminoShortlistRuntime>,
    reserve_cache: &tokio::sync::RwLock<HashMap<String, KaminoReserveMeta>>,
    config: HunterRuntimeConfig,
    reason: &str,
) -> anyhow::Result<usize> {
    let candidate_snapshot = {
        let state = runtime.read().await;
        state
            .candidates
            .iter()
            .map(|(obligation, candidate)| (obligation.clone(), candidate.clone()))
            .collect::<Vec<_>>()
    };

    if candidate_snapshot.is_empty() {
        let mut state = runtime.write().await;
        state.active.clear();
        state.last_refresh_completed_at_ms = Some(now_ms());
        return Ok(0);
    }

    let refreshed_at_ms = now_ms();
    let mut refreshed_candidates = HashMap::new();

    for (obligation, mut candidate) in candidate_snapshot {
        candidate.clear_cooldown_if_expired(refreshed_at_ms);
        match rpc.get_account_info(&obligation).await {
            Ok(data) => {
                if let Some(obligation_account) = decode_kamino_obligation(&data) {
                    if obligation_account.has_debt == 0
                        || obligation_account.borrowed_assets_market_value_sf == 0
                    {
                        candidate.distance_to_liq = None;
                    } else {
                        candidate.context.active_reserve_pubkeys =
                            active_kamino_reserve_pubkeys(&obligation_account);
                        candidate.update_refresh(
                            obligation_account.dist_to_liq(),
                            refreshed_at_ms,
                            reason,
                        );
                        let repay_reserve_pk =
                            Pubkey::from_str(&candidate.context.repay_reserve)?;
                        let withdraw_reserve_pk =
                            Pubkey::from_str(&candidate.context.withdraw_reserve)?;
                        let _ = get_or_fetch_kamino_reserve_meta(
                            rpc,
                            reserve_cache,
                            &repay_reserve_pk,
                        )
                        .await?;
                        let _ = get_or_fetch_kamino_reserve_meta(
                            rpc,
                            reserve_cache,
                            &withdraw_reserve_pk,
                        )
                        .await?;
                    }
                } else {
                    candidate.distance_to_liq = None;
                }
            }
            Err(_) => {
                candidate.distance_to_liq = None;
            }
        }
        refreshed_candidates.insert(obligation, candidate);
    }

    let active = select_shortlist(
        &refreshed_candidates,
        config.shortlist_max_obligations,
        refreshed_at_ms,
    );

    let mut state = runtime.write().await;
    state.candidates = refreshed_candidates;
    state.active = active;
    state.last_refresh_completed_at_ms = Some(refreshed_at_ms);
    Ok(state.active.len())
}

async fn request_shortlist_refresh(
    tx: &mpsc::Sender<KaminoShortlistRefreshRequest>,
    runtime: &tokio::sync::RwLock<KaminoShortlistRuntime>,
    debounce_ms: u64,
    reason: &str,
    prioritize_obligation: Option<String>,
) {
    let now = now_ms();
    {
        let state = runtime.read().await;
        if state
            .last_refresh_requested_at_ms
            .is_some_and(|last| now.saturating_sub(last) < debounce_ms)
        {
            return;
        }
    }
    {
        let mut state = runtime.write().await;
        state.last_refresh_requested_at_ms = Some(now);
    }
    let _ = tx
        .send(KaminoShortlistRefreshRequest {
            reason: reason.to_string(),
            prioritize_obligation,
        })
        .await;
}

fn shortlist_entry_for_signal(
    runtime: &KaminoShortlistRuntime,
    signal: &HunterSignalEvent,
) -> Option<ShortlistEntry> {
    runtime.shortlist_entry(&signal.obligation_pubkey)
}

async fn resolve_kamino_signal_event<R: RpcClient>(
    rpc: &R,
    source: HunterSignalSource,
    signature: String,
    received_at_ms: u64,
    logs: Vec<String>,
    runtime: HunterRuntimeConfig,
) -> anyhow::Result<HunterSignalEvent> {
    let tx_info = rpc
        .get_transaction_with_retries(
            &signature,
            runtime.tx_fetch.attempts,
            runtime.tx_fetch.retry_delay_ms,
        )
        .await?;
    let liquidate_ix_idx = find_kamino_liquidate_ix(&tx_info)
        .ok_or_else(|| anyhow::anyhow!("no KLEND liquidate instruction found"))?;
    let ix_accs = &tx_info.instruction_accounts[liquidate_ix_idx];
    if ix_accs.len() < 6 {
        anyhow::bail!(
            "liquidate instruction has too few accounts ({})",
            ix_accs.len()
        );
    }

    let obligation_pubkey = tx_info
        .account_keys
        .get(ix_accs[1])
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing obligation account"))?;
    let repay_mint = tx_info.account_keys.get(ix_accs[5]).cloned();

    Ok(HunterSignalEvent {
        source,
        protocol: "kamino",
        signal_kind: HunterSignalKind::KaminoLogLiquidation,
        received_at_ms,
        signature: Some(signature),
        obligation_pubkey,
        repay_mint,
        detail: Some(summarize_candidate_logs(&logs)),
        tx_info: Some(tx_info),
    })
}

fn spawn_kamino_log_signal_source<R, L>(
    source: HunterSignalSource,
    rpc: R,
    runtime: HunterRuntimeConfig,
    signal_tx: mpsc::Sender<HunterSignalEvent>,
    trace_logger: HunterTraceLogger,
    logger: L,
    hunter_wallet: String,
) where
    R: StreamingRpcClient + RpcClient + Clone + Send + Sync + 'static,
    L: LiquidationLogger + Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        loop {
            let mut rx = match rpc
                .subscribe_to_logs(KAMINO_PROGRAM_ID, runtime.signal_commitment)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    log_stderr(format!(
                        "[hunter-kamino] {} subscribe failed: {}. Retrying in 2s...",
                        source.as_str(),
                        e
                    ));
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            log_stderr(format!(
                "[hunter-kamino] {} signal subscription task started.",
                source.as_str()
            ));

            loop {
                let entry = match tokio::time::timeout(
                    tokio::time::Duration::from_secs(runtime.ws_idle_timeout_secs),
                    rx.recv(),
                )
                .await
                {
                    Ok(Some(e)) => e,
                    Ok(None) => break,
                    Err(_) => break,
                };

                if !kamino_logs_look_like_liquidation(&entry.logs) {
                    continue;
                }

                let detail = summarize_candidate_logs(&entry.logs);
                if kamino_logs_indicate_healthy_obligation(&entry.logs) {
                    hunter_verbose_log(
                        runtime.verbose,
                        "kamino",
                        format!(
                            "skip healthy obligation | source={} sig={} logs={}",
                            source.as_str(),
                            entry.signature,
                            detail
                        ),
                    );
                    let mut event =
                        HunterTraceEvent::new("kamino", "skip", entry.signature.clone())
                            .with_optional_repay_mint(extract_log_field(
                                &entry.logs,
                                "repay_reserve:",
                            ))
                            .with_reason("source_obligation_healthy")
                            .with_detail(format!("source={} {}", source.as_str(), detail))
                            .with_timing(entry.received_at_ms, 0);
                    if let Some(obligation) = extract_obligation_pda_from_logs(&entry.logs) {
                        event = event.with_obligation(obligation);
                    }
                    trace_logger.log(event);
                    continue;
                }

                hunter_verbose_log(
                    runtime.verbose,
                    "kamino",
                    format!(
                        "candidate | source={} sig={} logs={}",
                        source.as_str(),
                        entry.signature,
                        detail
                    ),
                );

                let mut event = HunterTraceEvent::new("kamino", "ws_received", entry.signature.clone())
                    .with_optional_repay_mint(extract_log_field(
                        &entry.logs,
                        "repay_reserve:",
                    ))
                    .with_detail(format!("source={} {}", source.as_str(), detail))
                    .with_timing(entry.received_at_ms, 0);
                if let Some(obligation) = extract_obligation_pda_from_logs(&entry.logs) {
                    event = event.with_obligation(obligation);
                }
                trace_logger.log(event);
                let _ = log_hunter_observation(
                    &logger,
                    "Kamino",
                    "HUNTER_WS_RECEIVED",
                    &entry.signature,
                    None,
                    Some(hunter_wallet.clone()),
                    None,
                    Some(format!("source={} {}", source.as_str(), detail)),
                    Some(0),
                )
                .await;

                match resolve_kamino_signal_event(
                    &rpc,
                    source,
                    entry.signature.clone(),
                    entry.received_at_ms,
                    entry.logs.clone(),
                    runtime,
                )
                .await
                {
                    Ok(signal) => {
                        if signal_tx.send(signal).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        trace_logger.log(
                            HunterTraceEvent::new("kamino", "error", entry.signature.clone())
                                .with_reason("signal_resolution_failed")
                                .with_detail(format!("source={} {}", source.as_str(), e))
                                .with_timing(
                                    entry.received_at_ms,
                                    elapsed_ms_since(entry.received_at_ms),
                                ),
                        );
                    }
                }
            }
        }
    });
}

#[derive(Clone)]
struct HermesShortlistEntry {
    obligation_pubkey: String,
    repay_mint: String,
    tracked_feed_ids: Vec<String>,
    distance_to_liq: f64,
}

#[derive(Debug, Clone)]
struct HermesReserveInfo {
    mint: String,
    pyth_feed_id: Option<String>,
}

fn decode_kamino_obligation(data: &[u8]) -> Option<crate::domain::kamino::Obligation> {
    if data.len() < 8 {
        return None;
    }
    let mut cursor = &data[8..];
    crate::domain::kamino::Obligation::deserialize(&mut cursor).ok()
}

fn hermes_feed_id_from_pubkey(pk: Pubkey) -> String {
    format!("0x{}", hex_encode_lower(&pk.to_bytes()))
}

fn hex_encode_lower(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(LUT[(b >> 4) as usize] as char);
        out.push(LUT[(b & 0x0f) as usize] as char);
    }
    out
}

fn build_hermes_shortlist(
    wallet_tokens: &[WalletToken],
    program_accounts: Vec<ProgramAccount>,
) -> Vec<HermesShortlistEntry> {
    let mut reserve_infos: HashMap<[u8; 32], HermesReserveInfo> = HashMap::new();
    let mut obligations = Vec::new();
    for account in &program_accounts {
        if let Ok(reserve) = decode_kamino_reserve(&account.data) {
            let mint = Pubkey::new_from_array(reserve.liquidity.mint_pubkey).to_string();
            let pyth_feed_id = optional_pubkey(reserve.config.token_info.pyth_configuration.price)
                .map(hermes_feed_id_from_pubkey);
            reserve_infos.insert(
                reserve.liquidity.mint_pubkey,
                HermesReserveInfo { mint, pyth_feed_id },
            );
        }
        if let Some(obligation) = decode_kamino_obligation(&account.data) {
            obligations.push((account.pubkey.clone(), obligation));
        }
    }

    build_hermes_shortlist_from_decoded(wallet_tokens, obligations, &reserve_infos)
}

fn build_hermes_shortlist_from_decoded(
    wallet_tokens: &[WalletToken],
    obligations: Vec<(String, crate::domain::kamino::Obligation)>,
    reserve_infos: &HashMap<[u8; 32], HermesReserveInfo>,
) -> Vec<HermesShortlistEntry> {
    let whitelist: HashMap<String, &WalletToken> =
        wallet_tokens.iter().map(|t| (t.mint.clone(), t)).collect();
    let mut shortlist = Vec::new();
    for (account_pubkey, obligation) in obligations {
        if obligation.has_debt == 0 || obligation.borrowed_assets_market_value_sf == 0 {
            continue;
        }
        let mut repay_mint = None;
        let mut tracked_feed_ids = Vec::new();
        for borrow in obligation.borrows.iter() {
            if borrow.borrowed_amount_sf == 0 && borrow.market_value_sf == 0 {
                continue;
            }
            if let Some(reserve) = reserve_infos.get(&borrow.borrow_reserve) {
                if whitelist.contains_key(&reserve.mint) && repay_mint.is_none() {
                    repay_mint = Some(reserve.mint.clone());
                }
                if let Some(feed_id) = &reserve.pyth_feed_id {
                    tracked_feed_ids.push(feed_id.clone());
                }
            }
        }

        if let Some(repay_mint) = repay_mint {
            shortlist.push(HermesShortlistEntry {
                obligation_pubkey: account_pubkey,
                repay_mint,
                tracked_feed_ids,
                distance_to_liq: obligation.dist_to_liq(),
            });
        }
    }

    shortlist.sort_by(|a, b| {
        a.distance_to_liq
            .partial_cmp(&b.distance_to_liq)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    shortlist
}

fn parse_hermes_changed_feed_ids(payload: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return ids;
    };
    if let Some(parsed) = value["parsed"].as_array() {
        for item in parsed {
            if let Some(id) = item["id"].as_str() {
                ids.push(format!("0x{}", id.trim_start_matches("0x")));
            }
        }
    }
    ids
}

fn build_hermes_signals_from_changed_feeds(
    current: &[HermesShortlistEntry],
    changed: &[String],
    trigger_buffer_bps: f64,
    received_at_ms: u64,
) -> Vec<HunterSignalEvent> {
    current
        .iter()
        .filter(|entry| {
            entry.tracked_feed_ids.iter().any(|id| changed.contains(id))
                && entry.distance_to_liq <= trigger_buffer_bps
        })
        .map(|entry| HunterSignalEvent {
            source: HunterSignalSource::PriceFeed,
            protocol: "kamino",
            signal_kind: HunterSignalKind::PriceFeedPredictedLiquidable,
            received_at_ms,
            signature: None,
            obligation_pubkey: entry.obligation_pubkey.clone(),
            repay_mint: Some(entry.repay_mint.clone()),
            detail: Some(format!(
                "hermes_feed_update distance_to_liq={:.8} chunk_received_at_ms={}",
                entry.distance_to_liq, received_at_ms
            )),
            tx_info: None,
        })
        .collect()
}

fn spawn_price_feed_signal_source<R>(
    rpc: R,
    wallet_tokens: Vec<WalletToken>,
    signal_tx: mpsc::Sender<HunterSignalEvent>,
    trace_logger: HunterTraceLogger,
) where
    R: RpcClient + Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let hermes_url = std::env::var("SIGNAL_FEED_WS_URL")
            .or_else(|_| std::env::var("HERMES_WS_URL"))
            .unwrap_or_else(|_| "https://hermes.pyth.network".to_string())
            .trim_end_matches('/')
            .to_string();
        let refresh_secs = std::env::var("HERMES_REFRESH_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10);
        let shortlist_size = std::env::var("HERMES_SHORTLIST_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(200);
        let trigger_buffer_bps = std::env::var("HERMES_TRIGGER_BUFFER_BPS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(25) as f64
            / 10_000.0;

        let shortlist = Arc::new(tokio::sync::RwLock::new(Vec::<HermesShortlistEntry>::new()));
        {
            let rpc = rpc.clone();
            let wallet_tokens = wallet_tokens.clone();
            let shortlist = shortlist.clone();
            tokio::spawn(async move {
                loop {
                    match rpc.get_program_accounts(KAMINO_PROGRAM_ID).await {
                        Ok(accounts) => {
                            let mut entries = build_hermes_shortlist(&wallet_tokens, accounts);
                            entries.truncate(shortlist_size);
                            *shortlist.write().await = entries;
                        }
                        Err(e) => {
                            log_stderr(format!(
                                "[hunter-kamino] hermes shortlist refresh failed: {}",
                                e
                            ));
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(refresh_secs)).await;
                }
            });
        }

        loop {
            let current = shortlist.read().await.clone();
            if current.is_empty() {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }

            let mut feed_ids = current
                .iter()
                .flat_map(|entry| entry.tracked_feed_ids.iter().cloned())
                .collect::<Vec<_>>();
            feed_ids.sort();
            feed_ids.dedup();

            let mut url = format!("{}/v2/updates/price/stream", hermes_url);
            if !feed_ids.is_empty() {
                let query = feed_ids
                    .iter()
                    .map(|id| format!("ids[]={}", id))
                    .collect::<Vec<_>>()
                    .join("&");
                url.push('?');
                url.push_str(&query);
            }

            let client = reqwest::Client::new();
            match client.get(&url).send().await {
                Ok(resp) => {
                    let mut stream = resp.bytes_stream();
                    let mut buffer = String::new();
                    while let Some(item) = stream.next().await {
                        let chunk_received_at_ms = now_ms();
                        let Ok(chunk) = item else {
                            break;
                        };
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(idx) = buffer.find("\n\n") {
                            let raw_event = buffer[..idx].to_string();
                            buffer = buffer[idx + 2..].to_string();
                            for line in raw_event.lines() {
                                if let Some(payload) = line.strip_prefix("data:") {
                                    let changed = parse_hermes_changed_feed_ids(payload.trim());
                                    if changed.is_empty() {
                                        continue;
                                    }
                                    for signal in build_hermes_signals_from_changed_feeds(
                                        &current,
                                        &changed,
                                        trigger_buffer_bps,
                                        chunk_received_at_ms,
                                    ) {
                                        trace_logger.log(
                                            HunterTraceEvent::new(
                                                "kamino",
                                                "signal_received",
                                                format!("hermes:{}", signal.obligation_pubkey),
                                            )
                                            .with_obligation(signal.obligation_pubkey.clone())
                                            .with_optional_repay_mint(signal.repay_mint.clone())
                                            .with_optional_detail(signal.detail.clone())
                                            .with_timing(chunk_received_at_ms, 0),
                                        );
                                        if signal_tx.send(signal).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    log_stderr(format!("[hunter-kamino] hermes stream error: {}", e));
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    });
}

// ── Kamino opportunity execution (free function for tokio::spawn) ────────────
//
// Runs in its own task. Uses pre-cached blockhash and tip.
// Optimistic: does NOT call getAccountInfo — includes RefreshObligation in the tx
// and lets Kamino decide on-chain. If the position is already healthy, the tx
// fails cheaply (5000 lamports priority fee lost). If liquidatable, we win.
//
// Reserve addresses are extracted directly from the competitor's transaction,
// which means this works for ANY token pair without a hardcoded lookup table.
async fn execute_kamino_opportunity<R, JI>(
    sig: String,
    ws_received_at_ms: u64,
    rpc: R,
    jito: JI,
    keypair: Arc<Keypair>,
    wallet_index: Arc<HashMap<String, WalletTokenRuntime>>,
    reserve_cache: Arc<tokio::sync::RwLock<HashMap<String, KaminoReserveMeta>>>,
    cached_blockhash: Arc<tokio::sync::RwLock<solana_sdk::hash::Hash>>,
    cached_tip: Arc<std::sync::atomic::AtomicU64>,
    non_whitelist: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    max_repay_usd: f64,
    runtime: HunterRuntimeConfig,
    trace_logger: HunterTraceLogger,
    logger: impl LiquidationLogger,
    source: HunterSignalSource,
    preloaded_tx_info: Option<crate::ports::rpc::TransactionInfo>,
    known_obligation: Option<String>,
    known_repay_mint: Option<String>,
    prepared_context: Option<PreparedExecutionContext>,
) -> anyhow::Result<KaminoExecutionOutcome>
where
    R: RpcClient,
    JI: JitoPort,
{
    let started_at = Instant::now();

    // ── 1. getTransaction — bounded retry window ─────────────────────────────
    let tx_fetch_started_at = Instant::now();
    let tx_info = match preloaded_tx_info {
        Some(tx_info) => tx_info,
        None => match tokio::time::timeout(
            tokio::time::Duration::from_millis(runtime.tx_fetch.timeout_ms),
            rpc.get_transaction_with_retries(
                &sig,
                runtime.tx_fetch.attempts,
                runtime.tx_fetch.retry_delay_ms,
            ),
        )
        .await
        {
            Ok(Ok(tx_info)) => tx_info,
            Ok(Err(e)) => {
                let status = rpc.get_signature_status(&sig).await.ok().flatten();
                anyhow::bail!(
                    "getTransaction failed after {}ms: {} | {}",
                    tx_fetch_started_at.elapsed().as_millis(),
                    e,
                    format_signature_status(status.as_ref())
                );
            }
            Err(_) => {
                let status = rpc.get_signature_status(&sig).await.ok().flatten();
                anyhow::bail!(
                    "getTransaction timeout after {}ms | {}",
                    tx_fetch_started_at.elapsed().as_millis(),
                    format_signature_status(status.as_ref())
                );
            }
        },
    };
    let tx_fetch_ms = tx_fetch_started_at.elapsed().as_millis();

    // ── 2. Find the liquidate instruction ────────────────────────────────────
    let liquidate_ix_idx = find_kamino_liquidate_ix(&tx_info);

    let ix_idx =
        liquidate_ix_idx.ok_or_else(|| anyhow::anyhow!("no KLEND liquidate instruction found"))?;

    let ix_accs = &tx_info.instruction_accounts[ix_idx];
    if ix_accs.len() < 13 {
        anyhow::bail!(
            "liquidate instruction has too few accounts ({})",
            ix_accs.len()
        );
    }

    // ── 3. Resolve account pubkeys from the competitor's instruction ─────────
    // Account layout of LiquidateObligationAndRedeemReserveCollateralV2:
    //   0  liquidator (competitor's, we replace with ours)
    //   1  obligation PDA
    //   2  lending_market
    //   3  lending_market_authority
    //   4  repay_reserve
    //   5  repay_liquidity_mint
    //   6  repay_liquidity_supply
    //   7  withdraw_reserve
    //   8  withdraw_liquidity_mint
    //   9  withdraw_collateral_mint
    //   10 withdraw_collateral_supply
    //   11 withdraw_liquidity_supply
    //   12 withdraw_liquidity_fee_receiver
    macro_rules! resolve {
        ($i:expr) => {
            tx_info.account_keys
                .get(ix_accs[$i])
                .map(|s| s.as_str())
                .ok_or_else(|| anyhow::anyhow!(
                    "missing account at position {} (ix_accounts_len={} account_keys_len={} referenced_index={})",
                    $i,
                    ix_accs.len(),
                    tx_info.account_keys.len(),
                    ix_accs[$i]
                ))?
        }
    }

    let resolve_started_at = Instant::now();
    let obligation_owned = match known_obligation {
        Some(value) => value,
        None => resolve!(1).to_string(),
    };
    let obligation_str = obligation_owned.as_str();
    let market_str = resolve!(2);
    let market_auth_str = resolve!(3);
    let repay_reserve_str = resolve!(4);
    let repay_mint_owned = match known_repay_mint {
        Some(value) => value,
        None => resolve!(5).to_string(),
    };
    let repay_mint_str = repay_mint_owned.as_str();
    let repay_supply_str = resolve!(6);
    let wdr_reserve_str = resolve!(7);
    let wdr_liq_mint_str = resolve!(8);
    let wdr_col_mint_str = resolve!(9);
    let wdr_col_sup_str = resolve!(10);
    let wdr_liq_sup_str = resolve!(11);
    let wdr_fee_str = resolve!(12);
    let resolve_ms = resolve_started_at.elapsed().as_millis();
    let prepared_context_used = prepared_context
        .as_ref()
        .is_some_and(|context| context.obligation_pubkey == obligation_str);
    let shortlist_hit = Some(prepared_context.is_some());
    let shortlist_state = prepared_context
        .as_ref()
        .map(|_| ShortlistState::Armed.as_str().to_string());
    let shortlist_age_ms = None;
    let refresh_reason = prepared_context
        .as_ref()
        .map(|context| context.inclusion_reason.clone());

    // ── 4. Check we hold the repay token ────────────────────────────────────
    let Some(repay_token) = wallet_index.get(repay_mint_str) else {
        let non_whitelist_key = format!("{obligation_str}:{repay_mint_str}");
        let should_log = {
            let mut map = non_whitelist.lock().unwrap();
            map.retain(|_, t| t.elapsed().as_millis() < runtime.non_whitelist_cooldown_ms);
            if map.contains_key(&non_whitelist_key) {
                false
            } else {
                map.insert(non_whitelist_key, std::time::Instant::now());
                true
            }
        };
        trace_logger.log(
            HunterTraceEvent::new("kamino", "skip", sig.clone())
                .with_obligation(obligation_str.to_string())
                .with_repay_mint(repay_mint_str.to_string())
                .with_reason("token_not_whitelisted")
                .with_detail("token not whitelisted")
                .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
                .with_shortlist_context(
                    shortlist_hit,
                    shortlist_state.clone(),
                    shortlist_age_ms,
                    Some(prepared_context_used),
                    None,
                    refresh_reason.clone(),
                ),
        );
        if should_log {
            log_stderr(format!(
                "[hunter-kamino] skip: token not whitelisted | obligation={} repay_mint={}",
                obligation_str.chars().take(8).collect::<String>(),
                repay_mint_str
            ));
        } else {
            hunter_verbose_log(
                runtime.verbose,
                "kamino",
                format!(
                    "skip suppressed by cooldown | obligation={} repay_mint={}",
                    obligation_str.chars().take(8).collect::<String>(),
                    repay_mint_str
                ),
            );
        }
        return Ok(KaminoExecutionOutcome::Skipped);
    };
    if repay_token.max_repay_native == 0 {
        trace_logger.log(
            HunterTraceEvent::new("kamino", "skip", sig.clone())
                .with_obligation(obligation_str.to_string())
                .with_repay_mint(repay_mint_str.to_string())
                .with_repay_symbol(repay_token.symbol.clone())
                .with_reason("wallet_token_zero_cap")
                .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
                .with_shortlist_context(
                    shortlist_hit,
                    shortlist_state.clone(),
                    shortlist_age_ms,
                    Some(prepared_context_used),
                    None,
                    refresh_reason.clone(),
                ),
        );
        return Ok(KaminoExecutionOutcome::Skipped);
    }

    // Cap repay at max_repay_usd (approximate: we cap native amount, not USD)
    // The actual USD cap is enforced by wallet.toml max_repay_native.
    let _ = max_repay_usd; // available for future price-based capping

    // ── 6. Parse pubkeys ─────────────────────────────────────────────────────
    let obligation_pk = Pubkey::from_str(obligation_str)?;
    let market_pk = Pubkey::from_str(market_str)?;
    let market_auth_pk = Pubkey::from_str(market_auth_str)?;
    let repay_reserve_pk = Pubkey::from_str(repay_reserve_str)?;
    let repay_mint_pk = Pubkey::from_str(repay_mint_str)?;
    let repay_supply_pk = Pubkey::from_str(repay_supply_str)?;
    let wdr_reserve_pk = Pubkey::from_str(wdr_reserve_str)?;
    let wdr_liq_mint_pk = Pubkey::from_str(wdr_liq_mint_str)?;
    let wdr_col_mint_pk = Pubkey::from_str(wdr_col_mint_str)?;
    let wdr_col_sup_pk = Pubkey::from_str(wdr_col_sup_str)?;
    let wdr_liq_sup_pk = Pubkey::from_str(wdr_liq_sup_str)?;
    let wdr_fee_pk = Pubkey::from_str(wdr_fee_str)?;

    let klend_pk =
        Pubkey::from_str(KAMINO_PROGRAM_ID).expect("static constant KAMINO_PROGRAM_ID");
    let farms_pk = Pubkey::from_str(FARMS_PROGRAM).expect("static constant FARMS_PROGRAM");
    let tip_account = select_jito_tip_account(&sig)?;

    let liquidator = keypair.pubkey();
    let active_reserve_pubkeys = prepared_context
        .as_ref()
        .map(|context| context.active_reserve_pubkeys.clone())
        .filter(|pubkeys| !pubkeys.is_empty())
        .unwrap_or_else(|| {
            vec![
                repay_reserve_pk.to_string(),
                wdr_reserve_pk.to_string(),
            ]
        });
    let active_reserve_pks = active_reserve_pubkeys
        .iter()
        .map(|value| Pubkey::from_str(value))
        .collect::<Result<Vec<_>, _>>()?;
    let full_refresh_context = !active_reserve_pubkeys.is_empty()
        && active_reserve_pubkeys.len() > 2;
    let reserve_meta_started_at = Instant::now();
    let mut reserve_refresh_order = Vec::with_capacity(active_reserve_pks.len());
    for reserve_pk in &active_reserve_pks {
        let reserve_meta = get_or_fetch_kamino_reserve_meta(&rpc, &reserve_cache, reserve_pk).await?;
        reserve_refresh_order.push((*reserve_pk, reserve_meta));
    }
    let reserve_meta_ms = reserve_meta_started_at.elapsed().as_millis();
    let repay_reserve_meta = reserve_refresh_order
        .iter()
        .find(|(reserve_pk, _)| *reserve_pk == repay_reserve_pk)
        .map(|(_, meta)| meta.clone())
        .ok_or_else(|| anyhow::anyhow!("missing repay reserve metadata"))?;
    let withdraw_reserve_meta = reserve_refresh_order
        .iter()
        .find(|(reserve_pk, _)| *reserve_pk == wdr_reserve_pk)
        .map(|(_, meta)| meta.clone())
        .ok_or_else(|| anyhow::anyhow!("missing withdraw reserve metadata"))?;

    // ── 7. Build instructions ────────────────────────────────────────────────
    // Compute budget: 350k CU is sufficient for refresh x2 + refresh_obligation + liquidate.
    // Optimistic: we include the refresh instructions so on-chain state is fresh.
    // If ObligationHealthy, tx fails and we lose only the priority fee.
    let compute_unit_limit = std::env::var("KAMINO_COMPUTE_UNIT_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(400_000);
    let compute_unit_price = std::env::var("KAMINO_CU_PRICE_MICROLAMPORTS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5_000);

    let mut instruction_prefix: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
    ];
    for (reserve_pk, reserve_meta) in &reserve_refresh_order {
        instruction_prefix.push(ix_refresh_reserve(&klend_pk, reserve_pk, reserve_meta));
    }
    let refresh_obligation_reserve_refs = active_reserve_pks.iter().collect::<Vec<_>>();
    instruction_prefix.push(ix_refresh_obligation(
        &klend_pk,
        &market_pk,
        &obligation_pk,
        &refresh_obligation_reserve_refs,
    ));
    let user_src =
        get_ata_with_program(&liquidator, &repay_mint_pk, &repay_reserve_meta.token_program);
    let user_dst_col = get_ata_with_program(
        &liquidator,
        &wdr_col_mint_pk,
        &withdraw_reserve_meta.token_program,
    );
    let user_dst_liq = get_ata_with_program(
        &liquidator,
        &wdr_liq_mint_pk,
        &withdraw_reserve_meta.token_program,
    );
    let ata_setup_instructions = if kamino_destination_ata_setup_enabled() {
        vec![
            build_create_ata_idempotent_ix(
                &liquidator,
                &liquidator,
                &wdr_col_mint_pk,
                &withdraw_reserve_meta.token_program,
            ),
            build_create_ata_idempotent_ix(
                &liquidator,
                &liquidator,
                &wdr_liq_mint_pk,
                &withdraw_reserve_meta.token_program,
            ),
        ]
    } else {
        Vec::new()
    };

    // Liquidate instruction
    let disc = discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2");
    let liquidity_amount = repay_token.max_repay_native;

    let mut liquidation_data = disc.to_vec();
    liquidation_data.extend_from_slice(&liquidity_amount.to_le_bytes());
    liquidation_data.extend_from_slice(&0u64.to_le_bytes()); // minAcceptableReceivedLiquidityAmount
    liquidation_data.extend_from_slice(&0u64.to_le_bytes()); // maxAllowedLtvOverridePercent

    let mut liquidation_accounts = vec![
        AccountMeta::new_readonly(liquidator, true),
        AccountMeta::new(obligation_pk, false),
        AccountMeta::new_readonly(market_pk, false),
        AccountMeta::new_readonly(market_auth_pk, false),
        AccountMeta::new(repay_reserve_pk, false),
        AccountMeta::new_readonly(repay_mint_pk, false),
        AccountMeta::new(repay_supply_pk, false),
        AccountMeta::new(wdr_reserve_pk, false),
        AccountMeta::new_readonly(wdr_liq_mint_pk, false),
        AccountMeta::new(wdr_col_mint_pk, false),
        AccountMeta::new(wdr_col_sup_pk, false),
        AccountMeta::new(wdr_liq_sup_pk, false),
        AccountMeta::new(wdr_fee_pk, false),
        AccountMeta::new(user_src, false),
        AccountMeta::new(user_dst_col, false),
        AccountMeta::new(user_dst_liq, false),
        AccountMeta::new_readonly(withdraw_reserve_meta.token_program, false),
        AccountMeta::new_readonly(repay_reserve_meta.token_program, false),
        AccountMeta::new_readonly(withdraw_reserve_meta.token_program, false),
        AccountMeta::new_readonly(sysvar::instructions::id(), false),
    ];
    if ix_accs.len() >= 25 {
        for &account_idx in &ix_accs[20..24] {
            let pk = Pubkey::from_str(
                tx_info
                    .account_keys
                    .get(account_idx)
                    .ok_or_else(|| anyhow::anyhow!("missing farm account"))?,
            )?;
            liquidation_accounts.push(AccountMeta::new(pk, false));
        }
        let farms_program_idx = *ix_accs
            .get(24)
            .ok_or_else(|| anyhow::anyhow!("missing farms program"))?;
        let farms_program_pk = Pubkey::from_str(
            tx_info
                .account_keys
                .get(farms_program_idx)
                .ok_or_else(|| anyhow::anyhow!("missing farms program key"))?,
        )?;
        liquidation_accounts.push(AccountMeta::new_readonly(farms_program_pk, false));
    } else {
        liquidation_accounts.push(AccountMeta::new(klend_pk, false));
        liquidation_accounts.push(AccountMeta::new(klend_pk, false));
        liquidation_accounts.push(AccountMeta::new(klend_pk, false));
        liquidation_accounts.push(AccountMeta::new(klend_pk, false));
        liquidation_accounts.push(AccountMeta::new_readonly(farms_pk, false));
    }
    let liquidation_ix = Instruction {
        program_id: klend_pk,
        accounts: liquidation_accounts,
        data: liquidation_data,
    };

    let base_tip_lamports = cached_tip.load(Ordering::Relaxed);
    let max_send_attempts = jito_send_max_attempts();
    let ata_setup_instruction_count = ata_setup_instructions.len();
    let max_tx_size_bytes = 1232usize;
    let initial_timing_detail = format_stage_timings(
        tx_fetch_ms,
        resolve_ms,
        reserve_meta_ms,
        0,
        None,
        started_at.elapsed().as_millis(),
    );

    // ── 9. Send bundle ───────────────────────────────────────────────────────
    trace_logger.log(
        HunterTraceEvent::new("kamino", "firing", sig.clone())
            .with_obligation(obligation_str.to_string())
            .with_repay_mint(repay_mint_str.to_string())
            .with_repay_symbol(repay_token.symbol.clone())
            .with_detail(format!(
                "source={} tip={} tip_account={} cu_price={} max_send_attempts={} active_reserve_count={} full_refresh_context={} ata_setup_instruction_count={} {}",
                source.as_str(),
                base_tip_lamports,
                tip_account,
                compute_unit_price,
                max_send_attempts,
                active_reserve_pks.len(),
                full_refresh_context,
                ata_setup_instruction_count,
                initial_timing_detail
            ))
            .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
            .with_shortlist_context(
                shortlist_hit,
                shortlist_state.clone(),
                shortlist_age_ms,
                Some(prepared_context_used),
                None,
                refresh_reason.clone(),
            ),
    );
    let _ = log_hunter_observation(
        &logger,
        "Kamino",
        "HUNTER_FIRING",
        &sig,
        Some(obligation_str.to_string()),
        Some(liquidator.to_string()),
        Some(repay_token),
        Some(format!(
            "source={} tip={} tip_account={} cu_price={} max_send_attempts={} active_reserve_count={} full_refresh_context={} ata_setup_instruction_count={} {}",
            source.as_str(),
            base_tip_lamports,
            tip_account,
            compute_unit_price,
            max_send_attempts,
            active_reserve_pks.len(),
            full_refresh_context,
            ata_setup_instruction_count,
            initial_timing_detail
        )),
        Some(elapsed_ms_since(ws_received_at_ms)),
    )
    .await;
    log_stderr(format!(
        "[hunter-kamino] FIRING | source={} obligation={} repay={} tip={} cu_price={} max_attempts={} reserves={} full_refresh={} ata_setup={}",
        source.as_str(),
        &obligation_str[..8],
        repay_token.symbol,
        base_tip_lamports,
        compute_unit_price,
        max_send_attempts,
        active_reserve_pks.len(),
        full_refresh_context,
        ata_setup_instruction_count
    ));

    if hunter_dry_run_enabled() {
        let dry_run_blockhash = *cached_blockhash.read().await;
        let build_started_at = Instant::now();
        let KaminoBuiltAttempt {
            tx_size_bytes: tx_bytes,
            ata_setup_dropped_for_size,
            ..
        } = build_kamino_attempt_tx(KaminoBuildRequest {
            liquidator,
            keypair: keypair.clone(),
            blockhash: dry_run_blockhash,
            tip_account,
            tip_lamports: base_tip_lamports,
            instruction_prefix: instruction_prefix.clone(),
            ata_setup_instructions: ata_setup_instructions.clone(),
            liquidation_ix: liquidation_ix.clone(),
            max_tx_size_bytes,
            full_refresh_context,
        })?;
        let build_ms = build_started_at.elapsed().as_millis();
        trace_logger.log(
            HunterTraceEvent::new("kamino", "dry_run", sig.clone())
                .with_obligation(obligation_str.to_string())
                .with_repay_mint(repay_mint_str.to_string())
                .with_repay_symbol(repay_token.symbol.clone())
                .with_reason("dry_run_enabled")
                .with_detail(format!(
                    "source={} tx_size_bytes={} tip={} cu_price={} attempt=1/{} active_reserve_count={} full_refresh_context={} ata_setup_instruction_count={} ata_setup_dropped_for_size={} {}",
                    source.as_str(),
                    tx_bytes,
                    base_tip_lamports,
                    compute_unit_price,
                    max_send_attempts,
                    active_reserve_pks.len(),
                    full_refresh_context,
                    ata_setup_instruction_count,
                    ata_setup_dropped_for_size,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        reserve_meta_ms,
                        build_ms,
                        None,
                        started_at.elapsed().as_millis(),
                    )
                ))
                .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
                .with_shortlist_context(
                    shortlist_hit,
                    shortlist_state.clone(),
                    shortlist_age_ms,
                    Some(prepared_context_used),
                    None,
                    refresh_reason.clone(),
                ),
        );
        log_stderr(format!(
            "[hunter-kamino] DRY RUN | obligation={} repay={} tx_size={} reserves={} full_refresh={} ata_setup={} ata_dropped={}",
            &obligation_str[..8],
            repay_token.symbol,
            tx_bytes,
            active_reserve_pks.len(),
            full_refresh_context,
            ata_setup_instruction_count,
            ata_setup_dropped_for_size
        ));
        return Ok(KaminoExecutionOutcome::DryRun);
    }

    for attempt in 1..=max_send_attempts {
        let tip_lamports = retry_tip_lamports(base_tip_lamports, attempt);

        let blockhash = if attempt == 1 {
            *cached_blockhash.read().await
        } else {
            match rpc.get_latest_blockhash().await {
                Ok(latest_blockhash) => {
                    *cached_blockhash.write().await = latest_blockhash;
                    latest_blockhash
                }
                Err(_) => *cached_blockhash.read().await,
            }
        };

        let build_started_at = Instant::now();
        let KaminoBuiltAttempt {
            tx,
            tx_size_bytes: tx_bytes,
            ata_setup_dropped_for_size,
            ..
        } = build_kamino_attempt_tx(KaminoBuildRequest {
            liquidator,
            keypair: keypair.clone(),
            blockhash,
            tip_account,
            tip_lamports,
            instruction_prefix: instruction_prefix.clone(),
            ata_setup_instructions: ata_setup_instructions.clone(),
            liquidation_ix: liquidation_ix.clone(),
            max_tx_size_bytes,
            full_refresh_context,
        })?;
        let build_ms = build_started_at.elapsed().as_millis();

        let send_started_at = Instant::now();
        match jito.send_bundle(vec![tx]).await {
            Ok(bundle_id) => {
                let send_bundle_ms = send_started_at.elapsed().as_millis();
                let bundle_detail = format!(
                    "attempt={}/{} tip={} tx_size_bytes={} active_reserve_count={} full_refresh_context={} ata_setup_instruction_count={} ata_setup_dropped_for_size={} {}",
                    attempt,
                    max_send_attempts,
                    tip_lamports,
                    tx_bytes,
                    active_reserve_pks.len(),
                    full_refresh_context,
                    ata_setup_instruction_count,
                    ata_setup_dropped_for_size,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        reserve_meta_ms,
                        build_ms,
                        Some(send_bundle_ms),
                        started_at.elapsed().as_millis(),
                    )
                );
                trace_logger.log(
                    HunterTraceEvent::new("kamino", "bundle_sent", sig.clone())
                        .with_obligation(obligation_str.to_string())
                        .with_repay_mint(repay_mint_str.to_string())
                        .with_repay_symbol(repay_token.symbol.clone())
                        .with_detail(format!("source={} {}", source.as_str(), bundle_detail.clone()))
                        .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
                        .with_optional_bundle_id(Some(bundle_id.clone()))
                        .with_shortlist_context(
                            shortlist_hit,
                            shortlist_state.clone(),
                            shortlist_age_ms,
                            Some(prepared_context_used),
                            None,
                            refresh_reason.clone(),
                        ),
                );
                let _ = log_hunter_observation(
                    &logger,
                    "Kamino",
                    "HUNTER_BUNDLE_SENT",
                    &sig,
                    Some(obligation_str.to_string()),
                    Some(liquidator.to_string()),
                    Some(repay_token),
                    Some(format!("source={} {}", source.as_str(), bundle_detail)),
                    Some(elapsed_ms_since(ws_received_at_ms)),
                )
                .await;
                log_stderr(format!(
                    "[hunter-kamino] BUNDLE SENT | source={} obligation={} bundle={} attempt={}/{} tx_size={} reserves={} full_refresh={} ata_setup={} ata_dropped={}",
                    source.as_str(),
                    &obligation_str[..8],
                    &bundle_id[..12.min(bundle_id.len())],
                    attempt,
                    max_send_attempts,
                    tx_bytes,
                    active_reserve_pks.len(),
                    full_refresh_context,
                    ata_setup_instruction_count,
                    ata_setup_dropped_for_size
                ));
                return Ok(KaminoExecutionOutcome::BundleSent);
            }
            Err(error) => {
                let send_bundle_ms = send_started_at.elapsed().as_millis();
                let error_message = error.to_string();
                let bundle_detail = format!(
                    "attempt={}/{} tip={} tx_size_bytes={} active_reserve_count={} full_refresh_context={} ata_setup_instruction_count={} ata_setup_dropped_for_size={} {} | {}",
                    attempt,
                    max_send_attempts,
                    tip_lamports,
                    tx_bytes,
                    active_reserve_pks.len(),
                    full_refresh_context,
                    ata_setup_instruction_count,
                    ata_setup_dropped_for_size,
                    error_message,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        reserve_meta_ms,
                        build_ms,
                        Some(send_bundle_ms),
                        started_at.elapsed().as_millis(),
                    )
                );

                if attempt < max_send_attempts && is_retryable_jito_error(&error_message) {
                    trace_logger.log(
                        HunterTraceEvent::new("kamino", "bundle_retry", sig.clone())
                            .with_obligation(obligation_str.to_string())
                            .with_repay_mint(repay_mint_str.to_string())
                            .with_repay_symbol(repay_token.symbol.clone())
                            .with_reason(if is_expired_blockhash_error(&error_message) {
                                "expired_blockhash_retry"
                            } else {
                                "retryable_bundle_send_error"
                            })
                            .with_detail(format!("source={} {}", source.as_str(), bundle_detail))
                            .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
                            .with_shortlist_context(
                                shortlist_hit,
                                shortlist_state.clone(),
                                shortlist_age_ms,
                                Some(prepared_context_used),
                                None,
                                refresh_reason.clone(),
                            ),
                    );
                    let backoff_ms = retry_backoff_ms(attempt);
                    if backoff_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                    continue;
                }

                trace_logger.log(
                    HunterTraceEvent::new("kamino", "error", sig.clone())
                        .with_obligation(obligation_str.to_string())
                        .with_repay_mint(repay_mint_str.to_string())
                        .with_repay_symbol(repay_token.symbol.clone())
                        .with_reason("bundle_send_failed")
                        .with_detail(format!("source={} {}", source.as_str(), bundle_detail.clone()))
                        .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
                        .with_shortlist_context(
                            shortlist_hit,
                            shortlist_state.clone(),
                            shortlist_age_ms,
                            Some(prepared_context_used),
                            None,
                            refresh_reason.clone(),
                        ),
                );
                let _ = log_hunter_observation(
                    &logger,
                    "Kamino",
                    "HUNTER_BUNDLE_FAILED",
                    &sig,
                    Some(obligation_str.to_string()),
                    Some(liquidator.to_string()),
                    Some(repay_token),
                    Some(format!("source={} {}", source.as_str(), bundle_detail)),
                    Some(elapsed_ms_since(ws_received_at_ms)),
                )
                .await;
                log_stderr(format!(
                    "[hunter-kamino] bundle send failed (source={}, attempt={}/{}): {}",
                    source.as_str(),
                    attempt,
                    max_send_attempts,
                    error_message
                ));
                return Ok(KaminoExecutionOutcome::BundleFailed);
            }
        }
    }

    Ok(KaminoExecutionOutcome::BundleFailed)
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn elapsed_ms_since(ws_received_at_ms: u64) -> u64 {
    now_ms().saturating_sub(ws_received_at_ms)
}

pub(crate) fn format_stage_timings(
    tx_fetch_ms: u128,
    resolve_ms: u128,
    prep_ms: u128,
    build_ms: u128,
    send_bundle_ms: Option<u128>,
    total_ms: u128,
) -> String {
    match send_bundle_ms {
        Some(send_bundle_ms) => format!(
            "timings_ms={{get_tx:{tx_fetch_ms},resolve:{resolve_ms},prep:{prep_ms},build:{build_ms},send_bundle:{send_bundle_ms},total:{total_ms}}}"
        ),
        None => format!(
            "timings_ms={{get_tx:{tx_fetch_ms},resolve:{resolve_ms},prep:{prep_ms},build:{build_ms},total:{total_ms}}}"
        ),
    }
}

pub(crate) async fn log_hunter_observation<L: LiquidationLogger>(
    logger: &L,
    protocol: &str,
    status: &str,
    signature: &str,
    obligation: Option<String>,
    liquidator: Option<String>,
    repay_token: Option<&WalletTokenRuntime>,
    detail: Option<String>,
    delay_ms: Option<u64>,
) -> anyhow::Result<()> {
    let event = ObservationEvent {
        timestamp: crate::utils::utc_now(),
        signature: signature.to_string(),
        protocol: protocol.to_string(),
        market: detail.unwrap_or_else(|| "N/A".to_string()),
        liquidated_user: obligation.unwrap_or_else(|| "N/A".to_string()),
        liquidator: liquidator.unwrap_or_else(|| "N/A".to_string()),
        repay_mint: repay_token
            .map(|t| t.mint.clone())
            .unwrap_or_else(|| "N/A".to_string()),
        withdraw_mint: "N/A".to_string(),
        repay_symbol: repay_token
            .map(|t| t.symbol.clone())
            .unwrap_or_else(|| "N/A".to_string()),
        withdraw_symbol: "N/A".to_string(),
        repay_amount: 0.0,
        withdraw_amount: 0.0,
        repaid_usd: 0.0,
        withdrawn_usd: 0.0,
        profit_usd: 0.0,
        delay_ms: delay_ms.unwrap_or(0),
        competing_bots: 0,
        status: status.to_string(),
    };

    logger.log_observation(&event).await
}

// ── Solend log helpers ────────────────────────────────────────────────────────

fn extract_obligation_pda_from_logs(logs: &[String]) -> Option<String> {
    for line in logs {
        let content = line.strip_prefix("Program log: ").unwrap_or(line);
        if let Some(rest) = content.strip_prefix("obligation_info:") {
            let pda = rest.trim().split_whitespace().next()?.to_string();
            if !pda.is_empty() {
                return Some(pda);
            }
        }
    }
    None
}

fn extract_log_field(logs: &[String], key: &str) -> Option<String> {
    for line in logs {
        let content = line.strip_prefix("Program log: ").unwrap_or(line);
        if let Some(rest) = content.strip_prefix(key) {
            let val = rest.trim().split_whitespace().next()?.to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::rpc::TransactionInfo;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::{mpsc, Barrier};

    #[test]
    fn finds_kamino_liquidate_instruction_by_discriminator() {
        let mut liquidate_data = kamino_liquidate_discriminators()[0].to_vec();
        liquidate_data.extend_from_slice(&[0; 24]);

        let tx = TransactionInfo {
            account_keys: vec![KAMINO_PROGRAM_ID.to_string(), "Other111".to_string()],
            instruction_accounts: vec![vec![0, 1, 2], vec![0, 1, 2, 3, 4, 5]],
            instruction_programs: vec![0, 0],
            instruction_data: vec![vec![1, 2, 3], liquidate_data],
            block_time: None,
            pre_token_balances: vec![],
            post_token_balances: vec![],
        };

        assert_eq!(find_kamino_liquidate_ix(&tx), Some(1));
    }

    #[test]
    fn falls_back_to_large_klend_instruction_when_discriminator_is_missing() {
        let tx = TransactionInfo {
            account_keys: vec![KAMINO_PROGRAM_ID.to_string(), "Other111".to_string()],
            instruction_accounts: vec![vec![0, 1, 2], (0..13).collect()],
            instruction_programs: vec![0, 0],
            instruction_data: vec![vec![1, 2, 3], vec![9, 9, 9]],
            block_time: None,
            pre_token_balances: vec![],
            post_token_balances: vec![],
        };

        assert_eq!(find_kamino_liquidate_ix(&tx), Some(1));
    }

    #[test]
    fn retryable_jito_errors_cover_rate_limit_and_expired_blockhash() {
        assert!(is_retryable_jito_error(
            "Jito error: Network congested. Endpoint is globally rate limited."
        ));
        assert!(is_retryable_jito_error(
            "bundle contains an expired blockhash"
        ));
        assert!(!is_retryable_jito_error("custom program error: 0x1"));
    }

    #[test]
    fn hunter_trace_logger_writes_jsonl() {
        let unique = format!("jawas_hunter_test_{}.jsonl", now_ms());
        let path = std::env::temp_dir().join(unique);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let logger = HunterTraceLogger {
            writer: Some(Arc::new(std::sync::Mutex::new(file))),
        };

        logger.log(
            HunterTraceEvent::new("kamino", "skip", "sig")
                .with_obligation("obl")
                .with_repay_mint("mint")
                .with_repay_symbol("USDC")
                .with_reason("dedup")
                .with_timing(1, 2),
        );

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"stage\":\"skip\""));
        assert!(content.contains("\"reason\":\"dedup\""));

        let _ = std::fs::remove_file(path);
    }

    fn test_metrics_logger() -> (SignalMetricsLogger, mpsc::Receiver<SignalLockSummary>) {
        let (tx, rx) = mpsc::channel(32);
        (SignalMetricsLogger { summary_tx: tx }, rx)
    }

    fn test_metrics_logger_with_capacity(
        capacity: usize,
    ) -> (SignalMetricsLogger, mpsc::Receiver<SignalLockSummary>) {
        let (tx, rx) = mpsc::channel(capacity);
        (SignalMetricsLogger { summary_tx: tx }, rx)
    }

    fn test_fingerprint() -> SignalFingerprint {
        SignalFingerprint {
            protocol: "kamino",
            obligation: "Obligation1111111111111111111111111111111111".to_string(),
        }
    }

    fn test_signal(source: HunterSignalSource, received_at_ms: u64) -> HunterSignalEvent {
        HunterSignalEvent {
            source,
            protocol: "kamino",
            signal_kind: HunterSignalKind::KaminoLogLiquidation,
            received_at_ms,
            signature: Some(format!("sig-{}-{}", source.as_str(), received_at_ms)),
            obligation_pubkey: test_fingerprint().obligation,
            repay_mint: Some("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".to_string()),
            detail: None,
            tx_info: None,
        }
    }

    fn test_wallet_token(mint: &str) -> WalletToken {
        WalletToken {
            symbol: "TEST".to_string(),
            mint: mint.to_string(),
            decimals: 6,
            max_repay_native: 1_000_000,
        }
    }

    fn test_obligation_with_borrow(
        borrow_reserve: [u8; 32],
        borrowed_amount_sf: u128,
        market_value_sf: u128,
        unhealthy_borrow_value_sf: u128,
        borrow_factor_adjusted_debt_value_sf: u128,
        deposited_value_sf: u128,
    ) -> crate::domain::kamino::Obligation {
        let mut obligation: crate::domain::kamino::Obligation = unsafe { std::mem::zeroed() };
        obligation.has_debt = 1;
        obligation.borrowed_assets_market_value_sf = market_value_sf;
        obligation.unhealthy_borrow_value_sf = unhealthy_borrow_value_sf;
        obligation.borrow_factor_adjusted_debt_value_sf = borrow_factor_adjusted_debt_value_sf;
        obligation.deposited_value_sf = deposited_value_sf;
        obligation.borrows[0].borrow_reserve = borrow_reserve;
        obligation.borrows[0].borrowed_amount_sf = borrowed_amount_sf;
        obligation.borrows[0].market_value_sf = market_value_sf;
        obligation
    }

    #[test]
    fn signal_lock_first_source_wins_from_free_to_held() {
        let locks = DashMap::new();
        let (metrics, _rx) = test_metrics_logger();
        let fingerprint = test_fingerprint();
        let signal = test_signal(HunterSignalSource::PrimaryRpc, 100);

        let won = try_accept_signal(&locks, &metrics, fingerprint.clone(), &signal, 1_500);

        assert!(won);
        let record = locks.get(&fingerprint).unwrap();
        match &record.state {
            LockState::Held {
                winner_source,
                acquired_at_ms,
            } => {
                assert_eq!(*winner_source, HunterSignalSource::PrimaryRpc);
                assert_eq!(*acquired_at_ms, 100);
            }
            other => panic!("unexpected state: {other:?}"),
        }
        let stats = record
            .detections
            .get(&HunterSignalSource::PrimaryRpc)
            .unwrap();
        assert_eq!(stats.first_ts_ms, 100);
        assert_eq!(stats.count, 1);
        assert!(stats.won_lock);
    }

    #[test]
    fn signal_lock_records_losing_detection_while_held() {
        let locks = DashMap::new();
        let (metrics, _rx) = test_metrics_logger();
        let fingerprint = test_fingerprint();

        assert!(try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::PrimaryRpc, 100),
            1_500,
        ));

        let won = try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::SecondaryRpc, 101),
            1_500,
        );

        assert!(!won);
        let record = locks.get(&fingerprint).unwrap();
        let quicknode = record
            .detections
            .get(&HunterSignalSource::PrimaryRpc)
            .unwrap();
        let secondary = record
            .detections
            .get(&HunterSignalSource::SecondaryRpc)
            .unwrap();
        assert_eq!(quicknode.count, 1);
        assert_eq!(secondary.count, 1);
        assert!(!secondary.won_lock);
    }

    #[test]
    fn only_winner_can_transition_from_held_to_firing() {
        let locks = DashMap::new();
        let (metrics, _rx) = test_metrics_logger();
        let fingerprint = test_fingerprint();

        assert!(try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::PrimaryRpc, 100),
            1_500,
        ));

        mark_lock_firing(&locks, &fingerprint, HunterSignalSource::SecondaryRpc, 101);
        assert!(matches!(
            locks.get(&fingerprint).unwrap().state,
            LockState::Held { .. }
        ));

        mark_lock_firing(&locks, &fingerprint, HunterSignalSource::PrimaryRpc, 102);
        let record = locks.get(&fingerprint).unwrap();
        match &record.state {
            LockState::Firing {
                winner_source,
                acquired_at_ms,
            } => {
                assert_eq!(*winner_source, HunterSignalSource::PrimaryRpc);
                assert_eq!(*acquired_at_ms, 100);
            }
            other => panic!("unexpected state: {other:?}"),
        }
    }

    #[test]
    fn only_winner_can_transition_from_firing_to_fired() {
        let locks = DashMap::new();
        let (metrics, _rx) = test_metrics_logger();
        let fingerprint = test_fingerprint();

        assert!(try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::PrimaryRpc, 100),
            1_500,
        ));
        mark_lock_firing(&locks, &fingerprint, HunterSignalSource::PrimaryRpc, 102);

        mark_lock_fired(
            &locks,
            &fingerprint,
            HunterSignalSource::SecondaryRpc,
            103,
            FireOutcome::BundleSent,
        );
        assert!(matches!(
            locks.get(&fingerprint).unwrap().state,
            LockState::Firing { .. }
        ));

        mark_lock_fired(
            &locks,
            &fingerprint,
            HunterSignalSource::PrimaryRpc,
            104,
            FireOutcome::BundleSent,
        );
        let record = locks.get(&fingerprint).unwrap();
        match &record.state {
            LockState::Fired {
                winner_source,
                acquired_at_ms,
                outcome,
            } => {
                assert_eq!(*winner_source, HunterSignalSource::PrimaryRpc);
                assert_eq!(*acquired_at_ms, 100);
                assert!(matches!(outcome, FireOutcome::BundleSent));
            }
            other => panic!("unexpected state: {other:?}"),
        }
    }

    #[test]
    fn expired_lock_can_be_reacquired_and_emits_summary_on_replacement() {
        let locks = DashMap::new();
        let (metrics, mut rx) = test_metrics_logger();
        let fingerprint = test_fingerprint();

        assert!(try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::PrimaryRpc, 100),
            10,
        ));

        let won = try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::SecondaryRpc, 111),
            10,
        );

        assert!(won);
        let summary = rx
            .try_recv()
            .expect("expired lock summary should be emitted");
        assert_eq!(summary.winner_source, "primary_rpc");
        assert_eq!(summary.fire_outcome, "held_expired");
        assert_eq!(
            locks.get(&fingerprint).unwrap().winner_source(),
            HunterSignalSource::SecondaryRpc
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn atomic_cas_selects_exactly_one_winner_per_iteration() {
        for iteration in 0..1_000u64 {
            let locks = Arc::new(DashMap::new());
            let (metrics, _rx) = test_metrics_logger();
            let metrics = Arc::new(metrics);
            let fingerprint = test_fingerprint();
            let barrier = Arc::new(Barrier::new(3));

            let mut tasks = Vec::new();
            for source in [
                HunterSignalSource::PrimaryRpc,
                HunterSignalSource::SecondaryRpc,
                HunterSignalSource::PriceFeed,
            ] {
                let locks = locks.clone();
                let metrics = metrics.clone();
                let barrier = barrier.clone();
                let fingerprint = fingerprint.clone();
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    try_accept_signal(
                        &locks,
                        &metrics,
                        fingerprint,
                        &test_signal(source, iteration + 1),
                        1_500,
                    )
                }));
            }

            let mut winners = 0u32;
            for task in tasks {
                if task.await.unwrap() {
                    winners += 1;
                }
            }
            assert_eq!(winners, 1, "iteration {iteration}");
        }
    }

    #[test]
    fn cleanup_bug_repro_removes_fresh_lock_after_stale_expiration_scan() {
        let locks = DashMap::new();
        let (metrics, mut rx) = test_metrics_logger();
        let fingerprint = test_fingerprint();

        assert!(try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::PrimaryRpc, 100),
            10,
        ));

        let stale_expired = collect_expired_signal_fingerprints(&locks, 111, 10);
        assert_eq!(stale_expired.len(), 1);

        assert!(try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::SecondaryRpc, 111),
            10,
        ));

        remove_expired_signal_fingerprints(&locks, &metrics, stale_expired, 111, 10);

        assert!(
            locks.contains_key(&fingerprint),
            "cleanup removed a fresh lock inserted after the stale expiration scan"
        );

        let mut summaries = Vec::new();
        while let Ok(summary) = rx.try_recv() {
            summaries.push(summary);
        }
        assert_eq!(summaries.len(), 1, "expected exactly one summary emission");
    }

    #[test]
    fn hermes_shortlist_filters_non_whitelisted_repay_assets() {
        let whitelisted_mint = Pubkey::new_unique();
        let non_whitelisted_mint = Pubkey::new_unique();
        let feed = "0xfeed".to_string();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            whitelisted_mint.to_bytes(),
            HermesReserveInfo {
                mint: whitelisted_mint.to_string(),
                pyth_feed_id: Some(feed.clone()),
            },
        );
        reserve_infos.insert(
            non_whitelisted_mint.to_bytes(),
            HermesReserveInfo {
                mint: non_whitelisted_mint.to_string(),
                pyth_feed_id: Some("0xnope".to_string()),
            },
        );

        let obligations = vec![
            (
                "allowed".to_string(),
                test_obligation_with_borrow(whitelisted_mint.to_bytes(), 1, 10, 20, 15, 100),
            ),
            (
                "blocked".to_string(),
                test_obligation_with_borrow(non_whitelisted_mint.to_bytes(), 1, 10, 20, 15, 100),
            ),
        ];

        let shortlist = build_hermes_shortlist_from_decoded(
            &[test_wallet_token(&whitelisted_mint.to_string())],
            obligations,
            &reserve_infos,
        );

        assert_eq!(shortlist.len(), 1);
        assert_eq!(shortlist[0].obligation_pubkey, "allowed");
        assert_eq!(shortlist[0].repay_mint, whitelisted_mint.to_string());
        assert_eq!(shortlist[0].tracked_feed_ids, vec![feed]);
    }

    #[test]
    fn hermes_predictive_trigger_emits_only_for_matching_feed_and_buffer() {
        let shortlist = vec![
            HermesShortlistEntry {
                obligation_pubkey: "inside".to_string(),
                repay_mint: "mint1".to_string(),
                tracked_feed_ids: vec!["0xfeed-a".to_string()],
                distance_to_liq: 0.0010,
            },
            HermesShortlistEntry {
                obligation_pubkey: "outside".to_string(),
                repay_mint: "mint2".to_string(),
                tracked_feed_ids: vec!["0xfeed-b".to_string()],
                distance_to_liq: 0.0050,
            },
        ];

        let signals = build_hermes_signals_from_changed_feeds(
            &shortlist,
            &["0xfeed-a".to_string(), "0xother".to_string()],
            0.0025,
            1234,
        );

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].obligation_pubkey, "inside");
        assert_eq!(signals[0].received_at_ms, 1234);
        assert!(matches!(
            signals[0].signal_kind,
            HunterSignalKind::PriceFeedPredictedLiquidable
        ));
    }

    #[test]
    #[ignore = "Hermes phase 2 validation gate is not implemented yet"]
    fn hermes_validation_gate_requires_backtest_thresholds() {
        panic!("stub: implement Hermes validation gate when phase 2 starts");
    }

    #[test]
    #[ignore = "Hermes historical backtest harness is deferred to phase 2"]
    fn hermes_backtest_methodology_replays_historical_liquidations() {
        panic!("stub: implement historical liquidation backtest harness in phase 2");
    }

    #[test]
    fn winning_source_remains_stable_after_duplicate_detections() {
        let locks = DashMap::new();
        let (metrics, _rx) = test_metrics_logger();
        let fingerprint = test_fingerprint();

        assert!(try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::PrimaryRpc, 100),
            1_500,
        ));
        mark_lock_firing(&locks, &fingerprint, HunterSignalSource::PrimaryRpc, 101);
        assert!(!try_accept_signal(
            &locks,
            &metrics,
            fingerprint.clone(),
            &test_signal(HunterSignalSource::SecondaryRpc, 102),
            1_500,
        ));
        mark_lock_fired(
            &locks,
            &fingerprint,
            HunterSignalSource::PrimaryRpc,
            103,
            FireOutcome::BundleSent,
        );

        let record = locks.get(&fingerprint).unwrap();
        assert_eq!(record.winner_source(), HunterSignalSource::PrimaryRpc);
        match &record.state {
            LockState::Fired { outcome, .. } => assert!(matches!(outcome, FireOutcome::BundleSent)),
            other => panic!("unexpected state: {other:?}"),
        }
    }

    #[test]
    fn metrics_saturation_adds_less_than_ten_ms_median_on_signal_to_firing_path() {
        fn percentile(mut values: Vec<u128>, numerator: usize, denominator: usize) -> u128 {
            values.sort_unstable();
            let idx = ((values.len() - 1) * numerator) / denominator;
            values[idx]
        }

        fn sample_duration_ns(metrics: &SignalMetricsLogger, iteration: u64) -> u128 {
            let locks = DashMap::new();
            let fingerprint = test_fingerprint();
            let signal = test_signal(HunterSignalSource::PrimaryRpc, iteration);

            let started = Instant::now();
            let won = try_accept_signal(&locks, metrics, fingerprint.clone(), &signal, 1_500);
            assert!(won);
            mark_lock_firing(
                &locks,
                &fingerprint,
                HunterSignalSource::PrimaryRpc,
                iteration + 1,
            );
            started.elapsed().as_nanos()
        }

        let iterations = 10_000u64;

        let (empty_metrics, mut empty_rx) =
            test_metrics_logger_with_capacity(iterations as usize + 8);
        let mut empty = Vec::with_capacity(iterations as usize);
        for i in 0..iterations {
            empty.push(sample_duration_ns(&empty_metrics, i * 10));
        }
        while empty_rx.try_recv().is_ok() {}

        let (full_metrics, _full_rx) = test_metrics_logger_with_capacity(1);
        full_metrics.try_log_summary(SignalLockSummary {
            protocol: "kamino",
            obligation: "prefill".to_string(),
            repay_mint: None,
            winner_source: "primary_rpc".to_string(),
            fire_outcome: "held_expired".to_string(),
            detections: HashMap::new(),
        });
        let mut full = Vec::with_capacity(iterations as usize);
        for i in 0..iterations {
            full.push(sample_duration_ns(&full_metrics, i * 10 + 1));
        }

        let empty_median_ns = percentile(empty.clone(), 50, 100);
        let empty_p95_ns = percentile(empty, 95, 100);
        let full_median_ns = percentile(full.clone(), 50, 100);
        let full_p95_ns = percentile(full, 95, 100);
        let delta_median_ns = full_median_ns as i128 - empty_median_ns as i128;
        let delta_p95_ns = full_p95_ns as i128 - empty_p95_ns as i128;
        let metrics_report = format!(
            "signal_to_firing metrics: median empty={}ns full={}ns delta={}ns | p95 empty={}ns full={}ns delta={}ns\n",
            empty_median_ns,
            full_median_ns,
            delta_median_ns,
            empty_p95_ns,
            full_p95_ns,
            delta_p95_ns
        );
        let report_path = std::env::temp_dir().join("jawas_metrics_saturation.txt");
        let _ = std::fs::write(&report_path, metrics_report.as_bytes());

        assert!(
            delta_median_ns.abs() < 10_000_000,
            "metrics full-channel median delta too high: {} ns (empty={} ns full={} ns)",
            delta_median_ns,
            empty_median_ns,
            full_median_ns
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn contention_race_regression_100_rounds() {
        const ROUNDS: usize = 100;
        const WINDOWS: usize = 1_000;
        const LOCK_MS: u64 = 10;

        for round in 0..ROUNDS {
            let locks = Arc::new(DashMap::new());
            let (metrics, mut rx) = test_metrics_logger_with_capacity(WINDOWS + 8);
            let metrics = Arc::new(metrics);
            let fingerprint = test_fingerprint();
            let barrier = Arc::new(Barrier::new(3));
            let winners_by_window = Arc::new(
                (0..WINDOWS)
                    .map(|_| AtomicUsize::new(0))
                    .collect::<Vec<_>>(),
            );

            let mut tasks = Vec::new();
            for source in [
                HunterSignalSource::PrimaryRpc,
                HunterSignalSource::SecondaryRpc,
                HunterSignalSource::PriceFeed,
            ] {
                let locks = locks.clone();
                let metrics = metrics.clone();
                let fingerprint = fingerprint.clone();
                let barrier = barrier.clone();
                let winners_by_window = winners_by_window.clone();
                tasks.push(tokio::spawn(async move {
                    for window in 0..WINDOWS {
                        barrier.wait().await;
                        let ts = (window as u64) * (LOCK_MS + 1) + 1;
                        let won = try_accept_signal(
                            &locks,
                            &metrics,
                            fingerprint.clone(),
                            &test_signal(source, ts),
                            LOCK_MS,
                        );
                        if won {
                            winners_by_window[window].fetch_add(1, AtomicOrdering::SeqCst);
                        }
                    }
                }));
            }

            for task in tasks {
                task.await.unwrap();
            }

            let final_now = (WINDOWS as u64) * (LOCK_MS + 1) + LOCK_MS + 1;
            let expired = collect_expired_signal_fingerprints(&locks, final_now, LOCK_MS);
            remove_expired_signal_fingerprints(&locks, &metrics, expired, final_now, LOCK_MS);

            let mut summaries = 0usize;
            while rx.try_recv().is_ok() {
                summaries += 1;
            }

            for (window, winners) in winners_by_window.iter().enumerate() {
                assert_eq!(
                    winners.load(AtomicOrdering::SeqCst),
                    1,
                    "round {round} window {window} should have exactly one winner"
                );
            }
            assert_eq!(summaries, WINDOWS, "round {round} summary count mismatch");
        }
    }
}
