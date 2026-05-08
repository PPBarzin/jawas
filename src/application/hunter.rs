use crate::application::kamino_shortlist::{
    PreparedExecutionContext, ShortlistCandidate, ShortlistEntry, ShortlistState,
    enforce_candidate_history_limit, select_shortlist,
};
use crate::ports::config::ConfigPort;
use crate::ports::jito::JitoPort;
use crate::ports::jupiter::JupiterPort;
use crate::ports::logger::{LiquidationLogger, ObservationEvent};
use crate::ports::oracle::PriceOracle;
use crate::ports::rpc::{
    ProgramAccount, RpcClient, RpcCommitment, SignatureStatusInfo, StreamingRpcClient,
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
use solana_sdk::message::v0::Message;
use solana_sdk::message::VersionedMessage;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::sysvar;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const KLEND_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const FARMS_PROGRAM: &str = "FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr";

// Solend
const SOLEND_PROGRAM: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const SOLEND_LIQUIDATE_FILTER: &str = "LiquidateWithoutReceivingCtokens";

// Kamino
const KAMINO_LIQUIDATE_FILTER: &str = "LiquidateObligationAndRedeemReserveCollateralV2";
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
const DEFAULT_OBLIGATION_DEDUP_MS: u128 = 3_000;

#[derive(Debug, Clone, Copy)]
struct HunterTxFetchConfig {
    attempts: usize,
    retry_delay_ms: u64,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct HunterRuntimeConfig {
    signal_commitment: RpcCommitment,
    tx_fetch: HunterTxFetchConfig,
    obligation_dedup_ms: u128,
    non_whitelist_cooldown_ms: u128,
    ws_idle_timeout_secs: u64,
    signal_lock_ms: u64,
    shortlist_enabled: bool,
    shortlist_max_obligations: usize,
    shortlist_refresh_secs: u64,
    shortlist_refresh_debounce_ms: u64,
    shortlist_cooling_down_ms: u64,
    shortlist_candidate_history_limit: usize,
    verbose: bool,
}

impl HunterTxFetchConfig {
    fn from_env(prefix: &str) -> Self {
        let attempts = std::env::var(format!("{prefix}_GET_TX_ATTEMPTS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_GET_TX_ATTEMPTS").ok())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(3);
        let retry_delay_ms = std::env::var(format!("{prefix}_GET_TX_RETRY_DELAY_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_GET_TX_RETRY_DELAY_MS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(40);
        let timeout_ms = std::env::var(format!("{prefix}_GET_TX_TIMEOUT_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_GET_TX_TIMEOUT_MS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(800);

        Self {
            attempts,
            retry_delay_ms,
            timeout_ms,
        }
    }
}

impl HunterRuntimeConfig {
    fn from_env(prefix: &str) -> Self {
        let signal_commitment = match std::env::var(format!("{prefix}_SIGNAL_COMMITMENT"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SIGNAL_COMMITMENT").ok())
            .unwrap_or_else(|| "confirmed".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "processed" => RpcCommitment::Processed,
            _ => RpcCommitment::Confirmed,
        };

        let obligation_dedup_ms = std::env::var(format!("{prefix}_OBLIGATION_DEDUP_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_OBLIGATION_DEDUP_MS").ok())
            .and_then(|v| v.parse::<u128>().ok())
            .unwrap_or(DEFAULT_OBLIGATION_DEDUP_MS);

        let non_whitelist_cooldown_ms =
            std::env::var(format!("{prefix}_NON_WHITELIST_COOLDOWN_MS"))
                .ok()
                .or_else(|| std::env::var("HUNTER_NON_WHITELIST_COOLDOWN_MS").ok())
                .and_then(|v| v.parse::<u128>().ok())
                .unwrap_or(30_000);

        let ws_idle_timeout_secs = std::env::var(format!("{prefix}_WS_IDLE_TIMEOUT_SECS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_WS_IDLE_TIMEOUT_SECS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(180);

        let signal_lock_ms = std::env::var(format!("{prefix}_SIGNAL_LOCK_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SIGNAL_LOCK_MS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_500);

        let verbose = std::env::var(format!("{prefix}_VERBOSE"))
            .ok()
            .or_else(|| std::env::var("HUNTER_VERBOSE").ok())
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);

        let shortlist_enabled = std::env::var(format!("{prefix}_SHORTLIST_ENABLED"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SHORTLIST_ENABLED").ok())
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);

        let shortlist_max_obligations =
            std::env::var(format!("{prefix}_SHORTLIST_MAX_OBLIGATIONS"))
                .ok()
                .or_else(|| std::env::var("HUNTER_SHORTLIST_MAX_OBLIGATIONS").ok())
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| v.clamp(1, 10))
                .unwrap_or(10);

        let shortlist_refresh_secs = std::env::var(format!("{prefix}_SHORTLIST_REFRESH_SECS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SHORTLIST_REFRESH_SECS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20);

        let shortlist_refresh_debounce_ms =
            std::env::var(format!("{prefix}_SHORTLIST_REFRESH_DEBOUNCE_MS"))
                .ok()
                .or_else(|| std::env::var("HUNTER_SHORTLIST_REFRESH_DEBOUNCE_MS").ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1_500);

        let shortlist_cooling_down_ms =
            std::env::var(format!("{prefix}_SHORTLIST_COOLDOWN_MS"))
                .ok()
                .or_else(|| std::env::var("HUNTER_SHORTLIST_COOLDOWN_MS").ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(20_000);

        let shortlist_candidate_history_limit =
            std::env::var(format!("{prefix}_SHORTLIST_CANDIDATE_HISTORY_LIMIT"))
                .ok()
                .or_else(|| std::env::var("HUNTER_SHORTLIST_CANDIDATE_HISTORY_LIMIT").ok())
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| v.max(shortlist_max_obligations))
                .unwrap_or(64);

        Self {
            signal_commitment,
            tx_fetch: HunterTxFetchConfig::from_env(prefix),
            obligation_dedup_ms,
            non_whitelist_cooldown_ms,
            ws_idle_timeout_secs,
            signal_lock_ms,
            shortlist_enabled,
            shortlist_max_obligations,
            shortlist_refresh_secs,
            shortlist_refresh_debounce_ms,
            shortlist_cooling_down_ms,
            shortlist_candidate_history_limit,
            verbose,
        }
    }
}

fn hunter_dry_run_enabled() -> bool {
    std::env::var("HUNTER_DRY_RUN")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn jito_send_max_attempts() -> usize {
    std::env::var("JITO_SEND_MAX_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 4))
        .unwrap_or(2)
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}

fn hunter_verbose_log(enabled: bool, protocol: &str, message: impl AsRef<str>) {
    if enabled {
        log_stderr(format!("[hunter-{protocol}] {}", message.as_ref()));
    }
}

fn format_signature_status(status: Option<&SignatureStatusInfo>) -> String {
    match status {
        Some(status) => format!(
            "status(slot={:?},confirmation={:?},has_error={})",
            status.slot, status.confirmation_status, status.has_error
        ),
        None => "status(absent)".to_string(),
    }
}

fn is_expired_blockhash_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains("expired blockhash")
}

fn is_retryable_jito_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("rate limited")
        || lower.contains("network congested")
        || lower.contains("too many requests")
        || lower.contains("temporarily unavailable")
        || lower.contains("timeout")
        || is_expired_blockhash_error(&lower)
}

fn retry_tip_lamports(base_tip_lamports: u64, attempt: usize) -> u64 {
    if attempt <= 1 {
        return base_tip_lamports;
    }
    let multiplier = 1.0 + 0.25 * (attempt.saturating_sub(1) as f64);
    ((base_tip_lamports as f64) * multiplier).round() as u64
}

fn retry_backoff_ms(attempt: usize) -> u64 {
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
struct HunterTraceEvent {
    timestamp: String,
    protocol: &'static str,
    stage: &'static str,
    signature: String,
    obligation: Option<String>,
    repay_mint: Option<String>,
    repay_symbol: Option<String>,
    reason: Option<String>,
    detail: Option<String>,
    ws_received_at_ms: Option<u64>,
    elapsed_ms: Option<u64>,
    bundle_id: Option<String>,
    shortlist_hit: Option<bool>,
    shortlist_state: Option<String>,
    shortlist_age_ms: Option<u64>,
    prepared_context_used: Option<bool>,
    candidate_score: Option<f64>,
    refresh_reason: Option<String>,
}

#[derive(Clone)]
struct HunterTraceLogger {
    writer: Option<Arc<std::sync::Mutex<std::fs::File>>>,
}

impl HunterTraceLogger {
    fn from_env() -> Self {
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

    fn log(&self, event: HunterTraceEvent) {
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
        firing_started_at_ms: u64,
    },
    Fired {
        winner_source: HunterSignalSource,
        acquired_at_ms: u64,
        firing_started_at_ms: u64,
        fired_at_ms: u64,
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

    fn transition_to_firing(&mut self, source: HunterSignalSource, now_ms: u64) -> bool {
        match &self.state {
            LockState::Held {
                winner_source,
                acquired_at_ms,
            } if *winner_source == source => {
                self.state = LockState::Firing {
                    winner_source: *winner_source,
                    acquired_at_ms: *acquired_at_ms,
                    firing_started_at_ms: now_ms,
                };
                true
            }
            _ => false,
        }
    }

    fn transition_to_fired(
        &mut self,
        source: HunterSignalSource,
        now_ms: u64,
        outcome: FireOutcome,
    ) -> bool {
        match &self.state {
            LockState::Firing {
                winner_source,
                acquired_at_ms,
                firing_started_at_ms,
            } if *winner_source == source => {
                self.state = LockState::Fired {
                    winner_source: *winner_source,
                    acquired_at_ms: *acquired_at_ms,
                    firing_started_at_ms: *firing_started_at_ms,
                    fired_at_ms: now_ms,
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
pub struct WalletToken {
    pub symbol: String,
    pub mint: String,
    pub decimals: u8,
    pub max_repay_native: u64,
}

#[derive(Debug, Clone)]
struct WalletTokenRuntime {
    symbol: String,
    mint: String,
    max_repay_native: u64,
    source_ata: Pubkey,
}

#[derive(Debug, Clone)]
struct KaminoReserveMeta {
    lending_market: Pubkey,
    pyth_oracle: Option<Pubkey>,
    switchboard_price_oracle: Option<Pubkey>,
    switchboard_twap_oracle: Option<Pubkey>,
    scope_prices: Option<Pubkey>,
    token_program: Pubkey,
}

#[derive(Debug, Clone)]
struct KaminoResolvedAccounts {
    obligation_pubkey: String,
    market: String,
    market_authority: String,
    repay_reserve: String,
    repay_mint: String,
    repay_supply: String,
    withdraw_reserve: String,
    withdraw_liquidity_mint: String,
    withdraw_collateral_mint: String,
    withdraw_collateral_supply: String,
    withdraw_liquidity_supply: String,
    withdraw_liquidity_fee_receiver: String,
}

#[derive(Debug, Clone)]
struct KaminoShortlistRuntime {
    candidates: HashMap<String, ShortlistCandidate>,
    active: HashMap<String, ShortlistEntry>,
    last_refresh_requested_at_ms: Option<u64>,
    last_refresh_completed_at_ms: Option<u64>,
}

impl KaminoShortlistRuntime {
    fn new() -> Self {
        Self {
            candidates: HashMap::new(),
            active: HashMap::new(),
            last_refresh_requested_at_ms: None,
            last_refresh_completed_at_ms: None,
        }
    }

    fn shortlist_entry(&self, obligation: &str) -> Option<ShortlistEntry> {
        self.active.get(obligation).cloned()
    }
}

#[derive(Debug, Clone)]
struct KaminoShortlistRefreshRequest {
    reason: String,
    prioritize_obligation: Option<String>,
}

/// Parses wallet.toml and returns the list of available tokens.
pub fn load_wallet_tokens(path: &str) -> Vec<WalletToken> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            log_stderr(format!("[hunter] wallet.toml not found at {}: {}", path, e));
            return vec![];
        }
    };

    let mut tokens = Vec::new();
    let mut current: Option<(String, String, u8, u64)> = None;

    for line in content.lines() {
        let line = line.trim();
        if line == "[[tokens]]" {
            if let Some((symbol, mint, decimals, max)) = current.take() {
                tokens.push(WalletToken {
                    symbol,
                    mint,
                    decimals,
                    max_repay_native: max,
                });
            }
            current = Some((String::new(), String::new(), 6, 0));
            continue;
        }
        if let Some(ref mut t) = current {
            if let Some(v) = parse_toml_str(line, "symbol") {
                t.0 = v;
            }
            if let Some(v) = parse_toml_str(line, "mint") {
                t.1 = v;
            }
            if let Some(v) = parse_toml_u8(line, "decimals") {
                t.2 = v;
            }
            if let Some(v) = parse_toml_u64(line, "max_repay_native") {
                t.3 = v;
            }
        }
    }
    if let Some((symbol, mint, decimals, max)) = current {
        tokens.push(WalletToken {
            symbol,
            mint,
            decimals,
            max_repay_native: max,
        });
    }

    tokens
}

fn parse_toml_str(line: &str, key: &str) -> Option<String> {
    let (lhs, rhs) = line.split_once('=')?;
    if lhs.trim() != key {
        return None;
    }
    Some(rhs.trim().trim_matches('"').to_string())
}

fn parse_toml_u8(line: &str, key: &str) -> Option<u8> {
    let (lhs, rhs) = line.split_once('=')?;
    if lhs.trim() != key {
        return None;
    }
    rhs.trim().parse().ok()
}

fn parse_toml_u64(line: &str, key: &str) -> Option<u64> {
    let (lhs, rhs) = line.split_once('=')?;
    if lhs.trim() != key {
        return None;
    }
    let clean: String = rhs.trim().chars().filter(|&c| c != '_').collect();
    let clean = clean.split('#').next().unwrap_or("").trim();
    clean.parse().ok()
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct BigFractionBytes {
    value: [u64; 4],
    padding: [u64; 2],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct LastUpdate {
    slot: u64,
    stale: u8,
    price_status: u8,
    placeholder: [u8; 6],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct PriceHeuristic {
    lower: u64,
    upper: u64,
    exp: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ScopeConfiguration {
    price_feed: [u8; 32],
    price_chain: [u16; 4],
    twap_chain: [u16; 4],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct SwitchboardConfiguration {
    price_aggregator: [u8; 32],
    twap_aggregator: [u8; 32],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct PythConfiguration {
    price: [u8; 32],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct TokenInfo {
    name: [u8; 32],
    heuristic: PriceHeuristic,
    max_twap_divergence_bps: u64,
    max_age_price_seconds: u64,
    max_age_twap_seconds: u64,
    scope_configuration: ScopeConfiguration,
    switchboard_configuration: SwitchboardConfiguration,
    pyth_configuration: PythConfiguration,
    block_price_usage: u8,
    reserved: [u8; 7],
    padding: [u64; 19],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ReserveFees {
    origination_fee_sf: u64,
    flash_loan_fee_sf: u64,
    padding: [u8; 8],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct CurvePoint {
    utilization_rate_bps: u32,
    borrow_rate_bps: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct BorrowRateCurve {
    points: [CurvePoint; 11],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct WithdrawalCaps {
    config_capacity: i64,
    current_total: i64,
    last_interval_start_timestamp: u64,
    config_interval_length_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ReserveConfig {
    status: u8,
    padding_deprecated_asset_tier: u8,
    host_fixed_interest_rate_bps: u16,
    min_deleveraging_bonus_bps: u16,
    block_ctoken_usage: u8,
    reserved1: [u8; 6],
    protocol_order_execution_fee_pct: u8,
    protocol_take_rate_pct: u8,
    protocol_liquidation_fee_pct: u8,
    loan_to_value_pct: u8,
    liquidation_threshold_pct: u8,
    min_liquidation_bonus_bps: u16,
    max_liquidation_bonus_bps: u16,
    bad_debt_liquidation_bonus_bps: u16,
    deleveraging_margin_call_period_secs: u64,
    deleveraging_threshold_decrease_bps_per_day: u64,
    fees: ReserveFees,
    borrow_rate_curve: BorrowRateCurve,
    borrow_factor_pct: u64,
    deposit_limit: u64,
    borrow_limit: u64,
    token_info: TokenInfo,
    deposit_withdrawal_cap: WithdrawalCaps,
    debt_withdrawal_cap: WithdrawalCaps,
    elevation_groups: [u8; 20],
    disable_usage_as_coll_outside_emode: u8,
    utilization_limit_block_borrowing_above_pct: u8,
    autodeleverage_enabled: u8,
    proposer_authority_locked: u8,
    borrow_limit_outside_elevation_group: u64,
    borrow_limit_against_this_collateral_in_elevation_group: [u64; 32],
    deleveraging_bonus_increase_bps_per_day: u64,
    debt_maturity_timestamp: u64,
    debt_term_seconds: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ReserveLiquidity {
    mint_pubkey: [u8; 32],
    supply_vault: [u8; 32],
    fee_vault: [u8; 32],
    total_available_amount: u64,
    borrowed_amount_sf: u128,
    market_price_sf: u128,
    market_price_last_updated_ts: u64,
    mint_decimals: u64,
    deposit_limit_crossed_timestamp: u64,
    borrow_limit_crossed_timestamp: u64,
    cumulative_borrow_rate_bsf: BigFractionBytes,
    accumulated_protocol_fees_sf: u128,
    accumulated_referrer_fees_sf: u128,
    pending_referrer_fees_sf: u128,
    absolute_referral_rate_sf: u128,
    token_program: [u8; 32],
    padding2: [u64; 51],
    padding3: [u128; 32],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ReserveCollateral {
    mint_pubkey: [u8; 32],
    mint_total_supply: u64,
    supply_vault: [u8; 32],
    padding1: [u128; 32],
    padding2: [u128; 32],
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct WithdrawQueue {
    queued_collateral_amount: u64,
    next_issued_ticket_sequence_number: u64,
    next_withdrawable_ticket_sequence_number: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct Reserve {
    version: u64,
    last_update: LastUpdate,
    lending_market: [u8; 32],
    farm_collateral: [u8; 32],
    farm_debt: [u8; 32],
    liquidity: ReserveLiquidity,
    reserve_liquidity_padding: [u64; 150],
    collateral: ReserveCollateral,
    reserve_collateral_padding: [u64; 150],
    config: ReserveConfig,
    config_padding: [u64; 114],
    borrowed_amount_outside_elevation_group: u64,
    borrowed_amounts_against_this_reserve_in_elevation_groups: [u64; 32],
    withdraw_queue: WithdrawQueue,
    padding: [u64; 204],
}

// ── Helper functions for instruction building ────────────────────────────────

fn discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{}", name);
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

fn get_ata_with_program(wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    let ata_program = Pubkey::from_str(ATA_PROGRAM).unwrap();
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    )
    .0
}

fn get_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    get_ata_with_program(wallet, mint, &token_program)
}

fn build_create_ata_idempotent_ix(
    funding_address: &Pubkey,
    wallet_address: &Pubkey,
    token_mint_address: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    let ata_address = get_ata_with_program(wallet_address, token_mint_address, token_program);
    Instruction {
        program_id: Pubkey::from_str(ATA_PROGRAM).unwrap(),
        accounts: vec![
            AccountMeta::new(*funding_address, true),
            AccountMeta::new(ata_address, false),
            AccountMeta::new_readonly(*wallet_address, false),
            AccountMeta::new_readonly(*token_mint_address, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(*token_program, false),
        ],
        data: vec![1],
    }
}

fn kamino_destination_ata_setup_enabled() -> bool {
    std::env::var("KAMINO_CREATE_DESTINATION_ATAS")
        .ok()
        .map(|value| {
            let value = value.trim();
            !(value.eq_ignore_ascii_case("0")
                || value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("off"))
        })
        .unwrap_or(true)
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

fn select_jito_tip_account(seed: &str) -> anyhow::Result<Pubkey> {
    let accounts = jito_tip_accounts();
    if accounts.is_empty() {
        anyhow::bail!("no valid Jito tip account configured");
    }

    let digest = Sha256::digest(seed.as_bytes());
    let idx = (digest[0] as usize) % accounts.len();
    Ok(accounts[idx])
}

fn optional_pubkey(bytes: [u8; 32]) -> Option<Pubkey> {
    if bytes.iter().all(|b| *b == 0) {
        None
    } else {
        Some(Pubkey::new_from_array(bytes))
    }
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

fn decode_kamino_reserve(data: &[u8]) -> anyhow::Result<Reserve> {
    if data.len() < 8 {
        anyhow::bail!("reserve account too small");
    }
    let mut cursor = &data[8..];
    Reserve::deserialize(&mut cursor).map_err(|e| anyhow::anyhow!("reserve decode failed: {}", e))
}

fn reserve_meta_from_account(data: &[u8]) -> anyhow::Result<KaminoReserveMeta> {
    let reserve = decode_kamino_reserve(data)?;
    Ok(KaminoReserveMeta {
        lending_market: Pubkey::new_from_array(reserve.lending_market),
        pyth_oracle: optional_pubkey(reserve.config.token_info.pyth_configuration.price),
        switchboard_price_oracle: optional_pubkey(
            reserve
                .config
                .token_info
                .switchboard_configuration
                .price_aggregator,
        ),
        switchboard_twap_oracle: optional_pubkey(
            reserve
                .config
                .token_info
                .switchboard_configuration
                .twap_aggregator,
        ),
        scope_prices: optional_pubkey(reserve.config.token_info.scope_configuration.price_feed),
        token_program: Pubkey::new_from_array(reserve.liquidity.token_program),
    })
}

fn ix_refresh_reserve(klend: &Pubkey, reserve: &Pubkey, meta: &KaminoReserveMeta) -> Instruction {
    let disc = discriminator("refresh_reserve");
    let mut accounts = vec![
        AccountMeta::new(*reserve, false),
        AccountMeta::new_readonly(meta.lending_market, false),
    ];
    if let Some(pk) = meta.pyth_oracle {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    if let Some(pk) = meta.switchboard_price_oracle {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    if let Some(pk) = meta.switchboard_twap_oracle {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    if let Some(pk) = meta.scope_prices {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    Instruction {
        program_id: *klend,
        accounts,
        data: disc.to_vec(),
    }
}

fn ix_refresh_obligation(
    klend: &Pubkey,
    lending_market: &Pubkey,
    obligation: &Pubkey,
    reserves: &[&Pubkey],
) -> Instruction {
    let disc = discriminator("refresh_obligation");
    let mut accounts = vec![
        AccountMeta::new_readonly(*lending_market, false),
        AccountMeta::new(*obligation, false),
    ];
    for r in reserves {
        accounts.push(AccountMeta::new_readonly(**r, false));
    }
    Instruction {
        program_id: *klend,
        accounts,
        data: disc.to_vec(),
    }
}

async fn get_or_fetch_kamino_reserve_meta<R: RpcClient>(
    rpc: &R,
    cache: &tokio::sync::RwLock<HashMap<String, KaminoReserveMeta>>,
    reserve_pk: &Pubkey,
) -> anyhow::Result<KaminoReserveMeta> {
    let key = reserve_pk.to_string();
    if let Some(meta) = cache.read().await.get(&key).cloned() {
        return Ok(meta);
    }

    let data = rpc.get_account_info(&key).await?;
    let meta = reserve_meta_from_account(&data)?;
    cache.write().await.insert(key, meta.clone());
    Ok(meta)
}

pub struct HunterService<
    R: RpcClient,
    JI: JitoPort,
    JU: JupiterPort,
    O: PriceOracle,
    C: ConfigPort + LiquidationLogger + Clone,
> {
    hunter_rpc: R,
    signal_secondary_rpc: Option<R>,
    jito: JI,
    _jupiter: JU,
    _oracle: O,
    _config: C,
    keypair: Arc<Keypair>,
    max_repay_usd: f64,
    trace_logger: HunterTraceLogger,
}

impl<
        R: RpcClient,
        JI: JitoPort,
        JU: JupiterPort,
        O: PriceOracle,
        C: ConfigPort + LiquidationLogger + Clone + 'static,
    > HunterService<R, JI, JU, O, C>
{
    pub fn new(
        hunter_rpc: R,
        signal_secondary_rpc: Option<R>,
        jito: JI,
        jupiter: JU,
        oracle: O,
        config: C,
        keypair: Arc<Keypair>,
        max_repay_usd: f64,
    ) -> Self {
        Self {
            hunter_rpc,
            signal_secondary_rpc,
            jito,
            _jupiter: jupiter,
            _oracle: oracle,
            _config: config,
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
    pub async fn run_kamino(&self, wallet_tokens: Vec<WalletToken>) -> anyhow::Result<()>
    where
        R: StreamingRpcClient + RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let runtime = HunterRuntimeConfig::from_env("KAMINO");
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
                self._config.clone(),
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
                    self._config.clone(),
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
                            trace_logger.log(HunterTraceEvent {
                                timestamp: crate::utils::utc_now(),
                                protocol: "kamino",
                                stage: "shortlist_refresh",
                                signature: request.prioritize_obligation.unwrap_or_else(|| {
                                    format!("shortlist:{}", request.reason)
                                }),
                                obligation: None,
                                repay_mint: None,
                                repay_symbol: None,
                                reason: None,
                                detail: Some(format!(
                                    "reason={} active={}",
                                    request.reason, active_count
                                )),
                                ws_received_at_ms: Some(now_ms()),
                                elapsed_ms: Some(0),
                                bundle_id: None,
                                shortlist_hit: None,
                                shortlist_state: None,
                                shortlist_age_ms: None,
                                prepared_context_used: None,
                                candidate_score: None,
                                refresh_reason: Some(request.reason),
                            });
                        }
                        Err(error) => {
                            trace_logger.log(HunterTraceEvent {
                                timestamp: crate::utils::utc_now(),
                                protocol: "kamino",
                                stage: "error",
                                signature: format!("shortlist:{}", request.reason),
                                obligation: None,
                                repay_mint: None,
                                repay_symbol: None,
                                reason: Some("shortlist_refresh_failed".to_string()),
                                detail: Some(error.to_string()),
                                ws_received_at_ms: Some(now_ms()),
                                elapsed_ms: Some(0),
                                bundle_id: None,
                                shortlist_hit: None,
                                shortlist_state: None,
                                shortlist_age_ms: None,
                                prepared_context_used: None,
                                candidate_score: None,
                                refresh_reason: Some(request.reason),
                            });
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

            self.trace_logger.log(HunterTraceEvent {
                timestamp: crate::utils::utc_now(),
                protocol: "kamino",
                stage: if won_lock {
                    "signal_accepted"
                } else {
                    "signal_rejected_duplicate"
                },
                signature: signal.signature.clone().unwrap_or_else(|| {
                    format!("{}:{}", signal.source.as_str(), signal.obligation_pubkey)
                }),
                obligation: Some(signal.obligation_pubkey.clone()),
                repay_mint: signal.repay_mint.clone(),
                repay_symbol: None,
                reason: if won_lock {
                    None
                } else {
                    Some("lock_held".to_string())
                },
                detail: Some(format!("source={}", signal.source.as_str())),
                ws_received_at_ms: Some(signal.received_at_ms),
                elapsed_ms: Some(0),
                bundle_id: None,
                shortlist_hit,
                shortlist_state: shortlist_state_value.clone(),
                shortlist_age_ms,
                prepared_context_used: Some(false),
                candidate_score: shortlist_score,
                refresh_reason: shortlist_refresh_reason.clone(),
            });

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
            let airtable_logger = self._config.clone();
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
                    trace_logger.log(HunterTraceEvent {
                        timestamp: crate::utils::utc_now(),
                        protocol: "kamino",
                        stage: "error",
                        signature: sig_for_error.clone(),
                        obligation: Some(signal.obligation_pubkey.clone()),
                        repay_mint: signal.repay_mint.clone(),
                        repay_symbol: None,
                        reason: Some("opportunity_error".to_string()),
                        detail: Some(format!("source={} {}", signal.source.as_str(), e)),
                        ws_received_at_ms: Some(signal.received_at_ms),
                        elapsed_ms: Some(elapsed_ms_since(signal.received_at_ms)),
                        bundle_id: None,
                        shortlist_hit: None,
                        shortlist_state: None,
                        shortlist_age_ms: None,
                        prepared_context_used: None,
                        candidate_score: None,
                        refresh_reason: None,
                    });
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
        self.trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "kamino",
            stage: "replay_start",
            signature: signature.clone(),
            obligation: None,
            repay_mint: None,
            repay_symbol: None,
            reason: None,
            detail: Some("manual replay".to_string()),
            ws_received_at_ms: Some(now_ms()),
            elapsed_ms: Some(0),
            bundle_id: None,
            shortlist_hit: None,
            shortlist_state: None,
            shortlist_age_ms: None,
            prepared_context_used: None,
            candidate_score: None,
            refresh_reason: None,
        });

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
            self._config.clone(),
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
    pub async fn run_solend(&self, wallet_tokens: Vec<WalletToken>) -> anyhow::Result<()>
    where
        R: StreamingRpcClient + RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let runtime = HunterRuntimeConfig::from_env("SOLEND");
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
                .subscribe_to_logs(SOLEND_PROGRAM, runtime.signal_commitment)
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
                let airtable_logger = self._config.clone();
                let hunter_wallet = self.keypair.pubkey().to_string();
                let err_sig = sig.clone();
                let err_trace_logger = trace_logger.clone();

                hunter_verbose_log(
                    runtime.verbose,
                    "solend",
                    format!("candidate | sig={}", sig),
                );

                trace_logger.log(HunterTraceEvent {
                    timestamp: crate::utils::utc_now(),
                    protocol: "solend",
                    stage: "ws_received",
                    signature: sig.clone(),
                    obligation: None,
                    repay_mint: None,
                    repay_symbol: None,
                    reason: None,
                    detail: None,
                    ws_received_at_ms: Some(entry.received_at_ms),
                    elapsed_ms: Some(0),
                    bundle_id: None,
                    shortlist_hit: None,
                    shortlist_state: None,
                    shortlist_age_ms: None,
                    prepared_context_used: None,
                    candidate_score: None,
                    refresh_reason: None,
                });
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
                        err_trace_logger.log(HunterTraceEvent {
                            timestamp: crate::utils::utc_now(),
                            protocol: "solend",
                            stage: "error",
                            signature: err_sig.clone(),
                            obligation: None,
                            repay_mint: None,
                            repay_symbol: None,
                            reason: Some("opportunity_error".to_string()),
                            detail: Some(e.to_string()),
                            ws_received_at_ms: Some(entry.received_at_ms),
                            elapsed_ms: Some(elapsed_ms_since(entry.received_at_ms)),
                            bundle_id: None,
                            shortlist_hit: None,
                            shortlist_state: None,
                            shortlist_age_ms: None,
                            prepared_context_used: None,
                            candidate_score: None,
                            refresh_reason: None,
                        });
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
        self.trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "solend",
            stage: "replay_start",
            signature: signature.clone(),
            obligation: None,
            repay_mint: None,
            repay_symbol: None,
            reason: None,
            detail: Some("manual replay".to_string()),
            ws_received_at_ms: Some(now_ms()),
            elapsed_ms: Some(0),
            bundle_id: None,
            shortlist_hit: None,
            shortlist_state: None,
            shortlist_age_ms: None,
            prepared_context_used: None,
            candidate_score: None,
            refresh_reason: None,
        });

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
            self._config.clone(),
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

fn resolve_kamino_accounts_from_tx_info(
    tx_info: &crate::ports::rpc::TransactionInfo,
    known_obligation: Option<&str>,
    known_repay_mint: Option<&str>,
) -> anyhow::Result<KaminoResolvedAccounts> {
    let liquidate_ix_idx = find_kamino_liquidate_ix(tx_info)
        .ok_or_else(|| anyhow::anyhow!("no KLEND liquidate instruction found"))?;
    let ix_accs = &tx_info.instruction_accounts[liquidate_ix_idx];
    if ix_accs.len() < 13 {
        anyhow::bail!(
            "liquidate instruction has too few accounts ({})",
            ix_accs.len()
        );
    }

    let resolve = |index: usize| -> anyhow::Result<String> {
        tx_info
            .account_keys
            .get(
                *ix_accs
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("missing liquidate account index {index}"))?,
            )
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing account key for liquidate account {index}"))
    };

    Ok(KaminoResolvedAccounts {
        obligation_pubkey: known_obligation
            .map(|value| value.to_string())
            .unwrap_or(resolve(1)?),
        market: resolve(2)?,
        market_authority: resolve(3)?,
        repay_reserve: resolve(4)?,
        repay_mint: known_repay_mint
            .map(|value| value.to_string())
            .unwrap_or(resolve(5)?),
        repay_supply: resolve(6)?,
        withdraw_reserve: resolve(7)?,
        withdraw_liquidity_mint: resolve(8)?,
        withdraw_collateral_mint: resolve(9)?,
        withdraw_collateral_supply: resolve(10)?,
        withdraw_liquidity_supply: resolve(11)?,
        withdraw_liquidity_fee_receiver: resolve(12)?,
    })
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
                .subscribe_to_logs(KLEND_PROGRAM, runtime.signal_commitment)
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
                    trace_logger.log(HunterTraceEvent {
                        timestamp: crate::utils::utc_now(),
                        protocol: "kamino",
                        stage: "skip",
                        signature: entry.signature.clone(),
                        obligation: extract_obligation_pda_from_logs(&entry.logs),
                        repay_mint: extract_log_field(&entry.logs, "repay_reserve:"),
                        repay_symbol: None,
                        reason: Some("source_obligation_healthy".to_string()),
                        detail: Some(format!("source={} {}", source.as_str(), detail)),
                        ws_received_at_ms: Some(entry.received_at_ms),
                        elapsed_ms: Some(0),
                        bundle_id: None,
                        shortlist_hit: None,
                        shortlist_state: None,
                        shortlist_age_ms: None,
                        prepared_context_used: None,
                        candidate_score: None,
                        refresh_reason: None,
                    });
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

                trace_logger.log(HunterTraceEvent {
                    timestamp: crate::utils::utc_now(),
                    protocol: "kamino",
                    stage: "ws_received",
                    signature: entry.signature.clone(),
                    obligation: extract_obligation_pda_from_logs(&entry.logs),
                    repay_mint: extract_log_field(&entry.logs, "repay_reserve:"),
                    repay_symbol: None,
                    reason: None,
                    detail: Some(format!("source={} {}", source.as_str(), detail)),
                    ws_received_at_ms: Some(entry.received_at_ms),
                    elapsed_ms: Some(0),
                    bundle_id: None,
                    shortlist_hit: None,
                    shortlist_state: None,
                    shortlist_age_ms: None,
                    prepared_context_used: None,
                    candidate_score: None,
                    refresh_reason: None,
                });
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
                        trace_logger.log(HunterTraceEvent {
                            timestamp: crate::utils::utc_now(),
                            protocol: "kamino",
                            stage: "error",
                            signature: entry.signature.clone(),
                            obligation: None,
                            repay_mint: None,
                            repay_symbol: None,
                            reason: Some("signal_resolution_failed".to_string()),
                            detail: Some(format!("source={} {}", source.as_str(), e)),
                            ws_received_at_ms: Some(entry.received_at_ms),
                            elapsed_ms: Some(elapsed_ms_since(entry.received_at_ms)),
                            bundle_id: None,
                            shortlist_hit: None,
                            shortlist_state: None,
                            shortlist_age_ms: None,
                            prepared_context_used: None,
                            candidate_score: None,
                            refresh_reason: None,
                        });
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KaminoSignalSourceConfig {
    primary_rpc_enabled: bool,
    secondary_rpc_enabled: bool,
    price_feed_enabled: bool,
}

fn read_kamino_signal_source_config(has_signal_secondary_rpc: bool) -> KaminoSignalSourceConfig {
    let primary_rpc_enabled = env_flag_aliases(
        &[
            "ENABLE_HUNTER_SIGNAL_PRIMARY",
            "ENABLE_HUNTER_SIGNAL_QUICKNODE",
        ],
        true,
    );
    let secondary_rpc_requested = env_flag_aliases(
        &[
            "ENABLE_HUNTER_SIGNAL_SECONDARY",
            "ENABLE_HUNTER_SIGNAL_HELIUS",
        ],
        true,
    );
    let price_feed_enabled = env_flag_aliases(
        &[
            "ENABLE_HUNTER_SIGNAL_PRICE_FEED",
            "ENABLE_HUNTER_SIGNAL_HERMES",
        ],
        false,
    );

    KaminoSignalSourceConfig {
        primary_rpc_enabled,
        secondary_rpc_enabled: secondary_rpc_requested && has_signal_secondary_rpc,
        price_feed_enabled,
    }
}

fn env_flag_aliases(names: &[&str], default: bool) -> bool {
    for name in names {
        if std::env::var(name).is_ok() {
            return env_flag(name, default);
        }
    }

    default
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
                    match rpc.get_program_accounts(KLEND_PROGRAM).await {
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
                                        trace_logger.log(HunterTraceEvent {
                                            timestamp: crate::utils::utc_now(),
                                            protocol: "kamino",
                                            stage: "signal_received",
                                            signature: format!(
                                                "hermes:{}",
                                                signal.obligation_pubkey
                                            ),
                                            obligation: Some(signal.obligation_pubkey.clone()),
                                            repay_mint: signal.repay_mint.clone(),
                                            repay_symbol: None,
                                            reason: None,
                                            detail: signal.detail.clone(),
                                            ws_received_at_ms: Some(chunk_received_at_ms),
                                            elapsed_ms: Some(0),
                                            bundle_id: None,
                                            shortlist_hit: None,
                                            shortlist_state: None,
                                            shortlist_age_ms: None,
                                            prepared_context_used: None,
                                            candidate_score: None,
                                            refresh_reason: None,
                                        });
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
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "kamino",
            stage: "skip",
            signature: sig.clone(),
            obligation: Some(obligation_str.to_string()),
            repay_mint: Some(repay_mint_str.to_string()),
            repay_symbol: None,
            reason: Some("token_not_whitelisted".to_string()),
            detail: Some("token not whitelisted".to_string()),
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
            shortlist_hit,
            shortlist_state: shortlist_state.clone(),
            shortlist_age_ms,
            prepared_context_used: Some(prepared_context_used),
            candidate_score: None,
            refresh_reason: refresh_reason.clone(),
        });
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
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "kamino",
            stage: "skip",
            signature: sig.clone(),
            obligation: Some(obligation_str.to_string()),
            repay_mint: Some(repay_mint_str.to_string()),
            repay_symbol: Some(repay_token.symbol.clone()),
            reason: Some("wallet_token_zero_cap".to_string()),
            detail: None,
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
            shortlist_hit,
            shortlist_state: shortlist_state.clone(),
            shortlist_age_ms,
            prepared_context_used: Some(prepared_context_used),
            candidate_score: None,
            refresh_reason: refresh_reason.clone(),
        });
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

    let klend_pk = Pubkey::from_str(KLEND_PROGRAM).unwrap();
    let farms_pk = Pubkey::from_str(FARMS_PROGRAM).unwrap();
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
    trace_logger.log(HunterTraceEvent {
        timestamp: crate::utils::utc_now(),
        protocol: "kamino",
        stage: "firing",
        signature: sig.clone(),
        obligation: Some(obligation_str.to_string()),
        repay_mint: Some(repay_mint_str.to_string()),
        repay_symbol: Some(repay_token.symbol.clone()),
        reason: None,
        detail: Some(format!(
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
        ws_received_at_ms: Some(ws_received_at_ms),
        elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
        bundle_id: None,
        shortlist_hit,
        shortlist_state: shortlist_state.clone(),
        shortlist_age_ms,
        prepared_context_used: Some(prepared_context_used),
        candidate_score: None,
        refresh_reason: refresh_reason.clone(),
    });
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

    let build_attempt_tx = |blockhash, tip_lamports| -> anyhow::Result<(VersionedTransaction, usize, bool)> {
        let mut attempt_instructions = instruction_prefix.clone();
        attempt_instructions.extend(ata_setup_instructions.clone());
        attempt_instructions.push(liquidation_ix.clone());
        attempt_instructions.push(solana_sdk::system_instruction::transfer(
            &liquidator,
            &tip_account,
            tip_lamports,
        ));

        let message = Message::try_compile(&liquidator, &attempt_instructions, &[], blockhash)
            .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
        let mut tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
            .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
        let mut tx_bytes = bincode::serialize(&tx)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        let mut ata_setup_dropped_for_size = false;

        if ata_setup_instruction_count > 0 && tx_bytes > max_tx_size_bytes {
            ata_setup_dropped_for_size = true;
            let mut fallback_instructions = instruction_prefix.clone();
            fallback_instructions.push(liquidation_ix.clone());
            fallback_instructions.push(solana_sdk::system_instruction::transfer(
                &liquidator,
                &tip_account,
                tip_lamports,
            ));
            let message = Message::try_compile(&liquidator, &fallback_instructions, &[], blockhash)
                .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
            tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
                .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
            tx_bytes = bincode::serialize(&tx)
                .map(|bytes| bytes.len())
                .unwrap_or_default();
        }

        Ok((tx, tx_bytes, ata_setup_dropped_for_size))
    };

    if hunter_dry_run_enabled() {
        let dry_run_blockhash = *cached_blockhash.read().await;
        let build_started_at = Instant::now();
        let (_tx, tx_bytes, ata_setup_dropped_for_size) =
            build_attempt_tx(dry_run_blockhash, base_tip_lamports)?;
        let build_ms = build_started_at.elapsed().as_millis();
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "kamino",
            stage: "dry_run",
            signature: sig.clone(),
            obligation: Some(obligation_str.to_string()),
            repay_mint: Some(repay_mint_str.to_string()),
            repay_symbol: Some(repay_token.symbol.clone()),
            reason: Some("dry_run_enabled".to_string()),
            detail: Some(format!(
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
            )),
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
            shortlist_hit,
            shortlist_state: shortlist_state.clone(),
            shortlist_age_ms,
            prepared_context_used: Some(prepared_context_used),
            candidate_score: None,
            refresh_reason: refresh_reason.clone(),
        });
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
        let (tx, tx_bytes, ata_setup_dropped_for_size) =
            build_attempt_tx(blockhash, tip_lamports)?;
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
                trace_logger.log(HunterTraceEvent {
                    timestamp: crate::utils::utc_now(),
                    protocol: "kamino",
                    stage: "bundle_sent",
                    signature: sig.clone(),
                    obligation: Some(obligation_str.to_string()),
                    repay_mint: Some(repay_mint_str.to_string()),
                    repay_symbol: Some(repay_token.symbol.clone()),
                    reason: None,
                    detail: Some(format!(
                        "source={} {}",
                        source.as_str(),
                        bundle_detail.clone()
                    )),
                    ws_received_at_ms: Some(ws_received_at_ms),
                    elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                    bundle_id: Some(bundle_id.clone()),
                    shortlist_hit,
                    shortlist_state: shortlist_state.clone(),
                    shortlist_age_ms,
                    prepared_context_used: Some(prepared_context_used),
                    candidate_score: None,
                    refresh_reason: refresh_reason.clone(),
                });
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
                    trace_logger.log(HunterTraceEvent {
                        timestamp: crate::utils::utc_now(),
                        protocol: "kamino",
                        stage: "bundle_retry",
                        signature: sig.clone(),
                        obligation: Some(obligation_str.to_string()),
                        repay_mint: Some(repay_mint_str.to_string()),
                        repay_symbol: Some(repay_token.symbol.clone()),
                        reason: Some(if is_expired_blockhash_error(&error_message) {
                            "expired_blockhash_retry".to_string()
                        } else {
                            "retryable_bundle_send_error".to_string()
                        }),
                        detail: Some(format!("source={} {}", source.as_str(), bundle_detail)),
                        ws_received_at_ms: Some(ws_received_at_ms),
                        elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                        bundle_id: None,
                        shortlist_hit,
                        shortlist_state: shortlist_state.clone(),
                        shortlist_age_ms,
                        prepared_context_used: Some(prepared_context_used),
                        candidate_score: None,
                        refresh_reason: refresh_reason.clone(),
                    });
                    let backoff_ms = retry_backoff_ms(attempt);
                    if backoff_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                    continue;
                }

                trace_logger.log(HunterTraceEvent {
                    timestamp: crate::utils::utc_now(),
                    protocol: "kamino",
                    stage: "error",
                    signature: sig.clone(),
                    obligation: Some(obligation_str.to_string()),
                    repay_mint: Some(repay_mint_str.to_string()),
                    repay_symbol: Some(repay_token.symbol.clone()),
                    reason: Some("bundle_send_failed".to_string()),
                    detail: Some(format!(
                        "source={} {}",
                        source.as_str(),
                        bundle_detail.clone()
                    )),
                    ws_received_at_ms: Some(ws_received_at_ms),
                    elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                    bundle_id: None,
                    shortlist_hit,
                    shortlist_state: shortlist_state.clone(),
                    shortlist_age_ms,
                    prepared_context_used: Some(prepared_context_used),
                    candidate_score: None,
                    refresh_reason: refresh_reason.clone(),
                });
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

// ── Solend opportunity execution (free function for tokio::spawn) ────────────
//
// Strategy: copy the competitor's tx instructions verbatim, replacing only
// the user-specific accounts (source/destination ATAs, signer) with ours.
// This means we don't need to know the Solend instruction discriminator or
// account layout — we just mirror the competitor's logic with our identity.
//
// For the liquidity_amount parameter (bytes 8-15 of Anchor instruction data),
// we substitute our max_repay_native so we never over-commit capital.
//
// Optimistic: the tx includes RefreshObligation copied from the competitor.
// If the obligation is already healthy, it fails cheaply.
async fn execute_solend_opportunity<R, JI>(
    sig: String,
    ws_received_at_ms: u64,
    rpc: R,
    jito: JI,
    keypair: Arc<Keypair>,
    wallet_index: Arc<HashMap<String, WalletTokenRuntime>>,
    cached_blockhash: Arc<tokio::sync::RwLock<solana_sdk::hash::Hash>>,
    cached_tip: Arc<std::sync::atomic::AtomicU64>,
    dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    tx_fetch: HunterTxFetchConfig,
    trace_logger: HunterTraceLogger,
    logger: impl LiquidationLogger,
) -> anyhow::Result<()>
where
    R: RpcClient,
    JI: JitoPort,
{
    let started_at = Instant::now();

    // ── 1. getTransaction — bounded retry window ─────────────────────────────
    let tx_fetch_started_at = Instant::now();
    let tx_info = match tokio::time::timeout(
        tokio::time::Duration::from_millis(tx_fetch.timeout_ms),
        rpc.get_transaction_with_retries(&sig, tx_fetch.attempts, tx_fetch.retry_delay_ms),
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
    };
    let tx_fetch_ms = tx_fetch_started_at.elapsed().as_millis();

    // ── 2. Find Solend liquidate instruction (most accounts) ─────────────────
    let resolve_started_at = Instant::now();
    let liq_ix_idx = tx_info
        .instruction_programs
        .iter()
        .enumerate()
        .filter(|(_, &prog_idx)| {
            tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(SOLEND_PROGRAM)
        })
        .max_by_key(|(ix_idx, _)| {
            tx_info
                .instruction_accounts
                .get(*ix_idx)
                .map(|a| a.len())
                .unwrap_or(0)
        })
        .map(|(ix_idx, _)| ix_idx)
        .ok_or_else(|| anyhow::anyhow!("no Solend liquidate instruction found"))?;

    let liq_accs = &tx_info.instruction_accounts[liq_ix_idx];
    let liq_data = &tx_info.instruction_data[liq_ix_idx];

    if liq_accs.len() < 9 || liq_data.len() < 16 {
        anyhow::bail!(
            "Solend liquidate instruction malformed (accs={} data={})",
            liq_accs.len(),
            liq_data.len()
        );
    }

    // ── 3. Competitor's wallet = account_keys[0] (fee payer / signer) ────────
    let competitor = tx_info
        .account_keys
        .get(0)
        .ok_or_else(|| anyhow::anyhow!("empty account_keys"))?
        .clone();

    // ── 4. Build: account_index → (mint, owner) from token balances ──────────
    // This lets us identify competitor ATAs and derive our equivalent ATAs.
    let balance_map: HashMap<usize, (String, String)> = tx_info
        .post_token_balances
        .iter()
        .chain(tx_info.pre_token_balances.iter())
        .map(|b| (b.account_index, (b.mint.clone(), b.owner.clone())))
        .collect();

    // ── 5. Find repay token: competitor ATA that decreased (owned by competitor)
    // We look for a wallet_token whose mint appears in the balances for an
    // account owned by the competitor. That's the token they repaid with.
    let repay_mint_str = balance_map
        .values()
        .find(|(_, owner)| owner == &competitor)
        .map(|(mint, _)| mint.clone())
        .ok_or_else(|| anyhow::anyhow!("could not identify repay mint for this liquidation"))?;

    let Some(repay_mint) = wallet_index.get(&repay_mint_str) else {
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "solend",
            stage: "skip",
            signature: sig.clone(),
            obligation: None,
            repay_mint: Some(repay_mint_str.clone()),
            repay_symbol: None,
            reason: Some("token_not_whitelisted".to_string()),
            detail: Some("token not whitelisted".to_string()),
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
            shortlist_hit: None,
            shortlist_state: None,
            shortlist_age_ms: None,
            prepared_context_used: None,
            candidate_score: None,
            refresh_reason: None,
        });
        log_stderr(format!(
            "[hunter-solend] skip: token not whitelisted | repay_mint={}",
            repay_mint_str
        ));
        return Ok(());
    };

    // ── 6. Dedup: skip if we fired on this obligation recently ───────────────
    // Obligation is at accounts[5] for LiquidateWithoutReceivingCtokens
    // (observer confirmed: accounts[5] = obligation, accounts[8] = liquidator).
    // We also fall back to checking a few positions to be safe.
    let obligation_key_idx = liq_accs
        .get(5)
        .and_then(|&i| tx_info.account_keys.get(i))
        .cloned()
        .unwrap_or_default();
    let resolve_ms = resolve_started_at.elapsed().as_millis();

    if obligation_key_idx.is_empty() {
        anyhow::bail!("could not extract obligation pubkey from Solend tx");
    }

    {
        let mut map = dedup.lock().unwrap();
        map.retain(|_, t| t.elapsed().as_millis() < DEFAULT_OBLIGATION_DEDUP_MS);
        if map.contains_key(&obligation_key_idx) {
            trace_logger.log(HunterTraceEvent {
                timestamp: crate::utils::utc_now(),
                protocol: "solend",
                stage: "skip",
                signature: sig.clone(),
                obligation: Some(obligation_key_idx.clone()),
                repay_mint: Some(repay_mint.mint.clone()),
                repay_symbol: Some(repay_mint.symbol.clone()),
                reason: Some("dedup".to_string()),
                detail: None,
                ws_received_at_ms: Some(ws_received_at_ms),
                elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                bundle_id: None,
                shortlist_hit: None,
                shortlist_state: None,
                shortlist_age_ms: None,
                prepared_context_used: None,
                candidate_score: None,
                refresh_reason: None,
            });
            return Ok(());
        }
        map.insert(obligation_key_idx.clone(), std::time::Instant::now());
    }
    if repay_mint.max_repay_native == 0 {
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "solend",
            stage: "skip",
            signature: sig.clone(),
            obligation: Some(obligation_key_idx.clone()),
            repay_mint: Some(repay_mint.mint.clone()),
            repay_symbol: Some(repay_mint.symbol.clone()),
            reason: Some("wallet_token_zero_cap".to_string()),
            detail: None,
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
            shortlist_hit: None,
            shortlist_state: None,
            shortlist_age_ms: None,
            prepared_context_used: None,
            candidate_score: None,
            refresh_reason: None,
        });
        return Ok(());
    }

    // ── 7. Build instruction list ────────────────────────────────────────────
    // ComputeBudget: 350k CU (sufficient for refresh x2 + liquidate without flash loan)
    let compute_unit_limit = std::env::var("SOLEND_COMPUTE_UNIT_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(400_000);
    let compute_unit_price = std::env::var("SOLEND_CU_PRICE_MICROLAMPORTS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5_000);

    let mut instructions: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
    ];

    // Copy all Solend non-liquidate instructions (RefreshReserve, RefreshObligation)
    // verbatim — they contain no user-specific accounts.
    let solend_pk = Pubkey::from_str(SOLEND_PROGRAM).unwrap();
    for (idx, (&prog_idx, accs)) in tx_info
        .instruction_programs
        .iter()
        .zip(tx_info.instruction_accounts.iter())
        .enumerate()
    {
        let prog_key = match tx_info.account_keys.get(prog_idx) {
            Some(k) => k.as_str(),
            None => continue,
        };
        if prog_key != SOLEND_PROGRAM {
            continue;
        }
        if idx == liq_ix_idx {
            continue;
        } // skip liquidate — we rebuild it below

        // Resolve account metas: assume all non-signer / non-writable for refresh
        let acc_metas: Vec<AccountMeta> = accs
            .iter()
            .filter_map(|&ai| {
                tx_info
                    .account_keys
                    .get(ai)
                    .and_then(|k| Pubkey::from_str(k).ok())
                    .map(|pk| AccountMeta::new_readonly(pk, false))
            })
            .collect();

        let data = tx_info
            .instruction_data
            .get(idx)
            .cloned()
            .unwrap_or_default();
        instructions.push(Instruction {
            program_id: solend_pk,
            accounts: acc_metas,
            data,
        });
    }

    // Rebuild the liquidate instruction, replacing competitor accounts with ours.
    {
        let liquidator = keypair.pubkey();

        let acc_metas: Vec<AccountMeta> = liq_accs
            .iter()
            .enumerate()
            .map(|(pos, &ai)| {
                let key_str = tx_info
                    .account_keys
                    .get(ai)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let pk = Pubkey::from_str(key_str).unwrap_or_default();

                // Determine if this is a competitor-owned ATA → replace with ours
                if let Some((mint_str, owner)) = balance_map.get(&ai) {
                    if owner == &competitor {
                        // It's the competitor's token account — use our ATA for the same mint
                        if let Some(runtime) = wallet_index.get(mint_str) {
                            return AccountMeta::new(runtime.source_ata, false);
                        }
                        if let Ok(mint_pk) = Pubkey::from_str(mint_str) {
                            return AccountMeta::new(get_ata(&liquidator, &mint_pk), false);
                        }
                    }
                }

                // It's the competitor's wallet (signer) — use our keypair
                if key_str == competitor {
                    return AccountMeta::new_readonly(liquidator, true);
                }

                // All other accounts (reserves, obligation, market, programs) are kept as-is.
                // Mark writable if it was writable in the original tx.
                // Heuristic: assume writable unless it's a program, sysvar, or readonly constant.
                let is_program_or_sysvar = pk == solana_sdk::system_program::id()
                    || pk == Pubkey::from_str(TOKEN_PROGRAM).unwrap_or_default()
                    || pk == sysvar::instructions::id()
                    || pk == solana_sdk::sysvar::clock::id()
                    || pk == solana_sdk::sysvar::rent::id()
                    || prog_idx_is_program(&tx_info, ai);

                // Readonly marker positions (lending market, lending market authority, token program)
                // are typically the last 3-4 accounts in Solend's liquidate instruction.
                let is_likely_readonly =
                    is_program_or_sysvar || pos >= liq_accs.len().saturating_sub(4);

                if is_likely_readonly {
                    AccountMeta::new_readonly(pk, false)
                } else {
                    AccountMeta::new(pk, false)
                }
            })
            .collect();

        // Copy instruction data, replacing liquidity_amount (bytes 8-15) with our cap.
        let mut data = liq_data.clone();
        let amount = repay_mint.max_repay_native;
        data[8..16].copy_from_slice(&amount.to_le_bytes());

        instructions.push(Instruction {
            program_id: solend_pk,
            accounts: acc_metas,
            data,
        });
    }

    // Jito tip
    let base_tip_lamports = cached_tip.load(Ordering::Relaxed);
    let tip_account = select_jito_tip_account(&sig)?;
    instructions.push(solana_sdk::system_instruction::transfer(
        &keypair.pubkey(),
        &tip_account,
        base_tip_lamports,
    ));
    let liquidator = keypair.pubkey();
    let tip_instruction_idx = instructions.len() - 1;
    let max_send_attempts = jito_send_max_attempts();
    let initial_timing_detail = format_stage_timings(
        tx_fetch_ms,
        resolve_ms,
        0,
        0,
        None,
        started_at.elapsed().as_millis(),
    );

    // ── 9. Send bundle ───────────────────────────────────────────────────────
    trace_logger.log(HunterTraceEvent {
        timestamp: crate::utils::utc_now(),
        protocol: "solend",
        stage: "firing",
        signature: sig.clone(),
        obligation: Some(obligation_key_idx.clone()),
        repay_mint: Some(repay_mint.mint.clone()),
        repay_symbol: Some(repay_mint.symbol.clone()),
        reason: None,
        detail: Some(format!(
            "tip={} tip_account={} cu_price={} max_send_attempts={} {}",
            base_tip_lamports,
            tip_account,
            compute_unit_price,
            max_send_attempts,
            initial_timing_detail
        )),
        ws_received_at_ms: Some(ws_received_at_ms),
        elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
        bundle_id: None,
        shortlist_hit: None,
        shortlist_state: None,
        shortlist_age_ms: None,
        prepared_context_used: None,
        candidate_score: None,
        refresh_reason: None,
    });
    let _ = log_hunter_observation(
        &logger,
        "Solend",
        "HUNTER_FIRING",
        &sig,
        Some(obligation_key_idx.clone()),
        Some(liquidator.to_string()),
        Some(repay_mint),
        Some(format!(
            "tip={} tip_account={} cu_price={} max_send_attempts={} {}",
            base_tip_lamports,
            tip_account,
            compute_unit_price,
            max_send_attempts,
            initial_timing_detail
        )),
        Some(elapsed_ms_since(ws_received_at_ms)),
    )
    .await;
    log_stderr(format!(
        "[hunter-solend] FIRING | obligation={} repay={} tip={} max_attempts={}",
        &obligation_key_idx[..8.min(obligation_key_idx.len())],
        repay_mint.symbol,
        base_tip_lamports,
        max_send_attempts,
    ));

    if hunter_dry_run_enabled() {
        let dry_run_blockhash = *cached_blockhash.read().await;
        let build_started_at = Instant::now();
        let message = Message::try_compile(&liquidator, &instructions, &[], dry_run_blockhash)
            .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
            .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
        let build_ms = build_started_at.elapsed().as_millis();
        let tx_bytes = bincode::serialize(&tx)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "solend",
            stage: "dry_run",
            signature: sig.clone(),
            obligation: Some(obligation_key_idx.clone()),
            repay_mint: Some(repay_mint.mint.clone()),
            repay_symbol: Some(repay_mint.symbol.clone()),
            reason: Some("dry_run_enabled".to_string()),
            detail: Some(format!(
                "tx_size_bytes={} tip={} cu_price={} attempt=1/{} {}",
                tx_bytes,
                base_tip_lamports,
                compute_unit_price,
                max_send_attempts,
                format_stage_timings(
                    tx_fetch_ms,
                    resolve_ms,
                    0,
                    build_ms,
                    None,
                    started_at.elapsed().as_millis(),
                )
            )),
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
            shortlist_hit: None,
            shortlist_state: None,
            shortlist_age_ms: None,
            prepared_context_used: None,
            candidate_score: None,
            refresh_reason: None,
        });
        log_stderr(format!(
            "[hunter-solend] DRY RUN | obligation={} repay={} tx_size={}",
            &obligation_key_idx[..8.min(obligation_key_idx.len())],
            repay_mint.symbol,
            tx_bytes
        ));
        return Ok(());
    }

    for attempt in 1..=max_send_attempts {
        let tip_lamports = retry_tip_lamports(base_tip_lamports, attempt);
        instructions[tip_instruction_idx] =
            solana_sdk::system_instruction::transfer(&liquidator, &tip_account, tip_lamports);

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
        let message = Message::try_compile(&liquidator, &instructions, &[], blockhash)
            .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
            .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
        let build_ms = build_started_at.elapsed().as_millis();

        let send_started_at = Instant::now();
        match jito.send_bundle(vec![tx]).await {
            Ok(bundle_id) => {
                let send_bundle_ms = send_started_at.elapsed().as_millis();
                let bundle_detail = format!(
                    "attempt={}/{} tip={} {}",
                    attempt,
                    max_send_attempts,
                    tip_lamports,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        0,
                        build_ms,
                        Some(send_bundle_ms),
                        started_at.elapsed().as_millis(),
                    )
                );
                trace_logger.log(HunterTraceEvent {
                    timestamp: crate::utils::utc_now(),
                    protocol: "solend",
                    stage: "bundle_sent",
                    signature: sig.clone(),
                    obligation: Some(obligation_key_idx.clone()),
                    repay_mint: Some(repay_mint.mint.clone()),
                    repay_symbol: Some(repay_mint.symbol.clone()),
                    reason: None,
                    detail: Some(bundle_detail.clone()),
                    ws_received_at_ms: Some(ws_received_at_ms),
                    elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                    bundle_id: Some(bundle_id.clone()),
                    shortlist_hit: None,
                    shortlist_state: None,
                    shortlist_age_ms: None,
                    prepared_context_used: None,
                    candidate_score: None,
                    refresh_reason: None,
                });
                let _ = log_hunter_observation(
                    &logger,
                    "Solend",
                    "HUNTER_BUNDLE_SENT",
                    &sig,
                    Some(obligation_key_idx.clone()),
                    Some(liquidator.to_string()),
                    Some(repay_mint),
                    Some(bundle_detail),
                    Some(elapsed_ms_since(ws_received_at_ms)),
                )
                .await;
                log_stderr(format!(
                    "[hunter-solend] BUNDLE SENT | obligation={} bundle={} attempt={}/{}",
                    &obligation_key_idx[..8.min(obligation_key_idx.len())],
                    &bundle_id[..12.min(bundle_id.len())],
                    attempt,
                    max_send_attempts
                ));
                return Ok(());
            }
            Err(error) => {
                let send_bundle_ms = send_started_at.elapsed().as_millis();
                let error_message = error.to_string();
                let bundle_detail = format!(
                    "attempt={}/{} tip={} {} | {}",
                    attempt,
                    max_send_attempts,
                    tip_lamports,
                    error_message,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        0,
                        build_ms,
                        Some(send_bundle_ms),
                        started_at.elapsed().as_millis(),
                    )
                );

                if attempt < max_send_attempts && is_retryable_jito_error(&error_message) {
                    trace_logger.log(HunterTraceEvent {
                        timestamp: crate::utils::utc_now(),
                        protocol: "solend",
                        stage: "bundle_retry",
                        signature: sig.clone(),
                        obligation: Some(obligation_key_idx.clone()),
                        repay_mint: Some(repay_mint.mint.clone()),
                        repay_symbol: Some(repay_mint.symbol.clone()),
                        reason: Some(if is_expired_blockhash_error(&error_message) {
                            "expired_blockhash_retry".to_string()
                        } else {
                            "retryable_bundle_send_error".to_string()
                        }),
                        detail: Some(bundle_detail),
                        ws_received_at_ms: Some(ws_received_at_ms),
                        elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                        bundle_id: None,
                        shortlist_hit: None,
                        shortlist_state: None,
                        shortlist_age_ms: None,
                        prepared_context_used: None,
                        candidate_score: None,
                        refresh_reason: None,
                    });
                    let backoff_ms = retry_backoff_ms(attempt);
                    if backoff_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                    continue;
                }

                trace_logger.log(HunterTraceEvent {
                    timestamp: crate::utils::utc_now(),
                    protocol: "solend",
                    stage: "error",
                    signature: sig.clone(),
                    obligation: Some(obligation_key_idx.clone()),
                    repay_mint: Some(repay_mint.mint.clone()),
                    repay_symbol: Some(repay_mint.symbol.clone()),
                    reason: Some("bundle_send_failed".to_string()),
                    detail: Some(bundle_detail.clone()),
                    ws_received_at_ms: Some(ws_received_at_ms),
                    elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                    bundle_id: None,
                    shortlist_hit: None,
                    shortlist_state: None,
                    shortlist_age_ms: None,
                    prepared_context_used: None,
                    candidate_score: None,
                    refresh_reason: None,
                });
                let _ = log_hunter_observation(
                    &logger,
                    "Solend",
                    "HUNTER_BUNDLE_FAILED",
                    &sig,
                    Some(obligation_key_idx.clone()),
                    Some(liquidator.to_string()),
                    Some(repay_mint),
                    Some(bundle_detail),
                    Some(elapsed_ms_since(ws_received_at_ms)),
                )
                .await;
                log_stderr(format!(
                    "[hunter-solend] bundle send failed (attempt={}/{}): {}",
                    attempt, max_send_attempts, error_message
                ));
                return Ok(());
            }
        }
    }

    Ok(())
}

/// Returns true if the given account index is used as a program id in any instruction.
/// Used to identify program accounts (always readonly) vs data accounts.
fn prog_idx_is_program(tx: &crate::ports::rpc::TransactionInfo, ai: usize) -> bool {
    tx.instruction_programs.contains(&ai)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn elapsed_ms_since(ws_received_at_ms: u64) -> u64 {
    now_ms().saturating_sub(ws_received_at_ms)
}

fn format_stage_timings(
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

async fn log_hunter_observation<L: LiquidationLogger>(
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

fn kamino_liquidate_discriminators() -> [[u8; 8]; 2] {
    [
        discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2"),
        discriminator("liquidate_obligation_and_redeem_reserve_collateral"),
    ]
}

fn find_kamino_liquidate_ix(tx_info: &crate::ports::rpc::TransactionInfo) -> Option<usize> {
    let expected_discriminators = kamino_liquidate_discriminators();
    let mut fallback_idx = None;

    for (ix_idx, &prog_idx) in tx_info.instruction_programs.iter().enumerate() {
        if tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) != Some(KLEND_PROGRAM) {
            continue;
        }

        if tx_info
            .instruction_data
            .get(ix_idx)
            .map(|data| {
                data.len() >= 8
                    && expected_discriminators
                        .iter()
                        .any(|expected| data[..8] == *expected)
            })
            .unwrap_or(false)
        {
            return Some(ix_idx);
        }

        let account_len = tx_info
            .instruction_accounts
            .get(ix_idx)
            .map(|accounts| accounts.len())
            .unwrap_or(0);
        if account_len >= 13 {
            fallback_idx = Some(ix_idx);
        }
    }

    fallback_idx
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
    use std::sync::{Mutex, OnceLock};
    use tokio::sync::{mpsc, Barrier};

    fn env_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env mutex poisoned")
    }

    #[test]
    fn parse_toml_u64_supports_underscores_and_comments() {
        assert_eq!(
            parse_toml_u64(
                "max_repay_native = 1_500_000_000  # 1.5 SOL",
                "max_repay_native"
            ),
            Some(1_500_000_000)
        );
    }

    #[test]
    fn finds_kamino_liquidate_instruction_by_discriminator() {
        let mut liquidate_data = kamino_liquidate_discriminators()[0].to_vec();
        liquidate_data.extend_from_slice(&[0; 24]);

        let tx = TransactionInfo {
            account_keys: vec![KLEND_PROGRAM.to_string(), "Other111".to_string()],
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
            account_keys: vec![KLEND_PROGRAM.to_string(), "Other111".to_string()],
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

        logger.log(HunterTraceEvent {
            timestamp: "2026-04-18T00:00:00Z".to_string(),
            protocol: "kamino",
            stage: "skip",
            signature: "sig".to_string(),
            obligation: Some("obl".to_string()),
            repay_mint: Some("mint".to_string()),
            repay_symbol: Some("USDC".to_string()),
            reason: Some("dedup".to_string()),
            detail: None,
            ws_received_at_ms: Some(1),
            elapsed_ms: Some(2),
            bundle_id: None,
            shortlist_hit: None,
            shortlist_state: None,
            shortlist_age_ms: None,
            prepared_context_used: None,
            candidate_score: None,
            refresh_reason: None,
        });

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
                firing_started_at_ms,
            } => {
                assert_eq!(*winner_source, HunterSignalSource::PrimaryRpc);
                assert_eq!(*acquired_at_ms, 100);
                assert_eq!(*firing_started_at_ms, 102);
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
                firing_started_at_ms,
                fired_at_ms,
                outcome,
            } => {
                assert_eq!(*winner_source, HunterSignalSource::PrimaryRpc);
                assert_eq!(*acquired_at_ms, 100);
                assert_eq!(*firing_started_at_ms, 102);
                assert_eq!(*fired_at_ms, 104);
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
    fn source_toggles_are_read_independently() {
        let _guard = env_test_guard();
        unsafe {
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRIMARY", "false");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_SECONDARY", "true");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED", "true");
        }
        let cfg = read_kamino_signal_source_config(true);
        assert!(!cfg.primary_rpc_enabled);
        assert!(cfg.secondary_rpc_enabled);
        assert!(cfg.price_feed_enabled);
        unsafe {
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRIMARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_SECONDARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED");
        }
    }

    #[test]
    fn primary_only_mode_disables_secondary_and_price_feed_effectively() {
        let _guard = env_test_guard();
        unsafe {
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRIMARY", "true");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_SECONDARY", "false");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED", "false");
        }
        let cfg = read_kamino_signal_source_config(true);
        assert!(cfg.primary_rpc_enabled);
        assert!(!cfg.secondary_rpc_enabled);
        assert!(!cfg.price_feed_enabled);
        unsafe {
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRIMARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_SECONDARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED");
        }
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
