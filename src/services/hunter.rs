use crate::ports::jito::JitoPort;
use crate::ports::jupiter::JupiterPort;
use crate::ports::oracle::PriceOracle;
use crate::ports::rpc::{RpcClient, RpcCommitment, StreamingRpcClient};
use crate::ports::config::ConfigPort;
use borsh::BorshDeserialize;
use serde::Serialize;
use solana_sdk::signature::Keypair;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::message::VersionedMessage;
use solana_sdk::message::v0::Message;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::sysvar;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::collections::HashMap;
use std::io::Write;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const JITO_TIP_ACCOUNT: &str = "96g9sAg9u3P7Q9ebKsC6SA47cySvnV6S1S1K6ssB1vD";
const KLEND_PROGRAM: &str    = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const TOKEN_PROGRAM: &str    = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str      = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const FARMS_PROGRAM: &str    = "FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr";

// Solend
const SOLEND_PROGRAM: &str   = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const SOLEND_LIQUIDATE_FILTER: &str = "LiquidateWithoutReceivingCtokens";

// Kamino
const KAMINO_LIQUIDATE_FILTER: &str = "LiquidateObligationAndRedeemReserveCollateralV2";
pub const DEFAULT_KAMINO_REPLAY_SIGNATURE: &str =
    "3V11m9fyEiUqbrihZPF1QJdXW9g6tr4mHS9VtCS2BNSunUeQWvRTgXf48uoC7gXgij8bKp7hSERZ1CZvNhSYgCLA";

/// Tip is refreshed every 60s.
const TIP_REFRESH_SECS: u64 = 60;
/// An obligation that was fired on within this window is skipped (prevents burst duplicates).
const OBLIGATION_DEDUP_MS: u128 = 3_000;

#[derive(Debug, Clone, Copy)]
struct HunterTxFetchConfig {
    attempts: usize,
    retry_delay_ms: u64,
    timeout_ms: u64,
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

        Self { attempts, retry_delay_ms, timeout_ms }
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
}

#[derive(Clone)]
struct HunterTraceLogger {
    writer: Option<Arc<std::sync::Mutex<std::fs::File>>>,
}

impl HunterTraceLogger {
    fn from_env() -> Self {
        let path = std::env::var("HUNTER_LOG_FILE")
            .unwrap_or_else(|_| "hunter_trace.jsonl".to_string());

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
}

/// Parses wallet.toml and returns the list of available tokens.
pub fn load_wallet_tokens(path: &str) -> Vec<WalletToken> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[hunter] wallet.toml not found at {}: {}", path, e);
            return vec![];
        }
    };

    let mut tokens = Vec::new();
    let mut current: Option<(String, String, u8, u64)> = None;

    for line in content.lines() {
        let line = line.trim();
        if line == "[[tokens]]" {
            if let Some((symbol, mint, decimals, max)) = current.take() {
                tokens.push(WalletToken { symbol, mint, decimals, max_repay_native: max });
            }
            current = Some((String::new(), String::new(), 6, 0));
            continue;
        }
        if let Some(ref mut t) = current {
            if let Some(v) = parse_toml_str(line, "symbol")   { t.0 = v; }
            if let Some(v) = parse_toml_str(line, "mint")     { t.1 = v; }
            if let Some(v) = parse_toml_u8(line, "decimals")  { t.2 = v; }
            if let Some(v) = parse_toml_u64(line, "max_repay_native") { t.3 = v; }
        }
    }
    if let Some((symbol, mint, decimals, max)) = current {
        tokens.push(WalletToken { symbol, mint, decimals, max_repay_native: max });
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

fn get_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let ata_program   = Pubkey::from_str(ATA_PROGRAM).unwrap();
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    ).0
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
            reserve.config.token_info.switchboard_configuration.price_aggregator,
        ),
        switchboard_twap_oracle: optional_pubkey(
            reserve.config.token_info.switchboard_configuration.twap_aggregator,
        ),
        scope_prices: optional_pubkey(reserve.config.token_info.scope_configuration.price_feed),
    })
}

fn ix_refresh_reserve(
    klend: &Pubkey,
    reserve: &Pubkey,
    meta: &KaminoReserveMeta,
) -> Instruction {
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
    Instruction { program_id: *klend, accounts, data: disc.to_vec() }
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
    Instruction { program_id: *klend, accounts, data: disc.to_vec() }
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

pub struct HunterService<R: RpcClient, JI: JitoPort, JU: JupiterPort, O: PriceOracle, C: ConfigPort + Clone> {
    hunter_rpc: R,
    jito: JI,
    _jupiter: JU,
    _oracle: O,
    _config: C,
    keypair: Arc<Keypair>,
    max_repay_usd: f64,
    trace_logger: HunterTraceLogger,
}

impl<R: RpcClient, JI: JitoPort, JU: JupiterPort, O: PriceOracle, C: ConfigPort + Clone + 'static> HunterService<R, JI, JU, O, C> {
    pub fn new(
        hunter_rpc: R,
        jito: JI,
        jupiter: JU,
        oracle: O,
        config: C,
        keypair: Arc<Keypair>,
        max_repay_usd: f64,
    ) -> Self {
        Self {
            hunter_rpc,
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
    //   QuikNode WS notification (LiquidateObligationAndRedeemReserveCollateralV2)
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
        let wallet_index = Arc::new(build_wallet_token_index(&self.keypair.pubkey(), &wallet_tokens)?);
        let reserve_cache: Arc<tokio::sync::RwLock<HashMap<String, KaminoReserveMeta>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let tx_fetch = HunterTxFetchConfig::from_env("KAMINO");

        eprintln!(
            "[hunter-kamino] Starting autonomous hunter. Wallet: {} | max_repay: ${:.0} | tokens: {}",
            self.keypair.pubkey(),
            self.max_repay_usd,
            wallet_tokens.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>().join(", ")
        );

        // ── Hot cache: blockhash ─────────────────────────────────────────────
        let initial_blockhash = self.hunter_rpc.get_latest_blockhash().await
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
                    tokio::time::sleep(tokio::time::Duration::from_secs(blockhash_refresh_secs)).await;
                    match rpc.get_latest_blockhash().await {
                        Ok(hash) => { *bh.write().await = hash; }
                        Err(e) => eprintln!("[hunter-kamino] blockhash refresh failed: {}", e),
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

        // ── Obligation dedup: skip if fired within OBLIGATION_DEDUP_MS ───────
        let recent_obligations: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        // ── Main WS loop ─────────────────────────────────────────────────────
        loop {
            let mut rx = match self.hunter_rpc.subscribe_to_logs(KLEND_PROGRAM, RpcCommitment::Processed).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[hunter-kamino] WS subscribe failed: {}. Retrying in 2s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            eprintln!("[hunter-kamino] WebSocket connected.");

            loop {
                let entry = match rx.recv().await {
                    Some(e) => e,
                    None => {
                        eprintln!("[hunter-kamino] WS stream ended. Reconnecting...");
                        break;
                    }
                };

                // Ignore transactions that don't contain a liquidation call.
                if !entry.logs.iter().any(|l| l.contains(KAMINO_LIQUIDATE_FILTER)) {
                    continue;
                }

                // Spawn a task per event so the WS loop is never blocked.
                let sig         = entry.signature.clone();
                let rpc         = self.hunter_rpc.clone();
                let keypair     = self.keypair.clone();
                let jito        = self.jito.clone();
                let bh          = cached_blockhash.clone();
                let tip         = cached_tip.clone();
                let dedup       = recent_obligations.clone();
                let max_repay   = self.max_repay_usd;
                let wallet_idx  = wallet_index.clone();
                let reserve_cache = reserve_cache.clone();
                let trace_logger = self.trace_logger.clone();
                let tx_fetch_cfg = tx_fetch;
                let err_sig = sig.clone();
                let err_trace_logger = trace_logger.clone();

                trace_logger.log(HunterTraceEvent {
                    timestamp: crate::utils::utc_now(),
                    protocol: "kamino",
                    stage: "ws_received",
                    signature: sig.clone(),
                    obligation: extract_obligation_pda_from_logs(&entry.logs),
                    repay_mint: extract_log_field(&entry.logs, "repay_reserve:"),
                    repay_symbol: None,
                    reason: None,
                    detail: None,
                    ws_received_at_ms: Some(entry.received_at_ms),
                    elapsed_ms: Some(0),
                    bundle_id: None,
                });

                tokio::spawn(async move {
                    if let Err(e) = execute_kamino_opportunity(
                        sig,
                        entry.received_at_ms,
                        rpc,
                        jito,
                        keypair,
                        wallet_idx,
                        reserve_cache,
                        bh,
                        tip,
                        dedup,
                        max_repay,
                        tx_fetch_cfg,
                        trace_logger,
                    ).await {
                        err_trace_logger.log(HunterTraceEvent {
                            timestamp: crate::utils::utc_now(),
                            protocol: "kamino",
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
                        });
                        eprintln!("[hunter-kamino] opportunity error: {}", e);
                    }
                });
            }
        }
    }

    pub async fn replay_kamino(&self, wallet_tokens: Vec<WalletToken>, signature: String) -> anyhow::Result<()>
    where
        R: RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let wallet_index = Arc::new(build_wallet_token_index(&self.keypair.pubkey(), &wallet_tokens)?);
        let reserve_cache: Arc<tokio::sync::RwLock<HashMap<String, KaminoReserveMeta>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(
            self.hunter_rpc.get_latest_blockhash().await.unwrap_or_default(),
        ));
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(
            self.jito.get_tip_recommendation().await.unwrap_or(100_000),
        ));
        let tx_fetch = HunterTxFetchConfig::from_env("KAMINO");
        let dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        eprintln!("[hunter-kamino] REPLAY | signature={}", signature);
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
            dedup,
            self.max_repay_usd,
            tx_fetch,
            self.trace_logger.clone(),
        ).await
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
        let wallet_index = Arc::new(build_wallet_token_index(&self.keypair.pubkey(), &wallet_tokens)?);
        let tx_fetch = HunterTxFetchConfig::from_env("SOLEND");

        eprintln!(
            "[hunter-solend] Starting autonomous hunter. Wallet: {} | tokens: {}",
            self.keypair.pubkey(),
            wallet_tokens.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>().join(", ")
        );

        // ── Hot cache: blockhash ─────────────────────────────────────────────
        let initial_blockhash = self.hunter_rpc.get_latest_blockhash().await
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
                    tokio::time::sleep(tokio::time::Duration::from_secs(blockhash_refresh_secs)).await;
                    match rpc.get_latest_blockhash().await {
                        Ok(hash) => { *bh.write().await = hash; }
                        Err(e) => eprintln!("[hunter-solend] blockhash refresh failed: {}", e),
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
            let mut rx = match self.hunter_rpc.subscribe_to_logs(SOLEND_PROGRAM, RpcCommitment::Processed).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[hunter-solend] WS subscribe failed: {}. Retrying in 2s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            eprintln!("[hunter-solend] WebSocket connected.");

            loop {
                let entry = match rx.recv().await {
                    Some(e) => e,
                    None => {
                        eprintln!("[hunter-solend] WS stream ended. Reconnecting...");
                        break;
                    }
                };

                if !entry.logs.iter().any(|l| l.contains(SOLEND_LIQUIDATE_FILTER)) {
                    continue;
                }

                let sig       = entry.signature.clone();
                let rpc       = self.hunter_rpc.clone();
                let keypair   = self.keypair.clone();
                let jito      = self.jito.clone();
                let wallet_idx = wallet_index.clone();
                let bh        = cached_blockhash.clone();
                let tip       = cached_tip.clone();
                let dedup     = recent_obligations.clone();
                let trace_logger = self.trace_logger.clone();
                let tx_fetch_cfg = tx_fetch;
                let err_sig = sig.clone();
                let err_trace_logger = trace_logger.clone();

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
                });

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
                    ).await {
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
                        });
                        eprintln!("[hunter-solend] opportunity error: {}", e);
                    }
                });
            }
        }
    }

    pub async fn replay_solend(&self, wallet_tokens: Vec<WalletToken>, signature: String) -> anyhow::Result<()>
    where
        R: RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        let wallet_index = Arc::new(build_wallet_token_index(&self.keypair.pubkey(), &wallet_tokens)?);
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(
            self.hunter_rpc.get_latest_blockhash().await.unwrap_or_default(),
        ));
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(
            self.jito.get_tip_recommendation().await.unwrap_or(100_000),
        ));
        let tx_fetch = HunterTxFetchConfig::from_env("SOLEND");
        let dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        eprintln!("[hunter-solend] REPLAY | signature={}", signature);
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
        ).await
    }
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
    dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    max_repay_usd: f64,
    tx_fetch: HunterTxFetchConfig,
    trace_logger: HunterTraceLogger,
) -> anyhow::Result<()>
where
    R: RpcClient,
    JI: JitoPort,
{
    // ── 1. getTransaction — single attempt, 500ms hard timeout ───────────────
    let tx_info = tokio::time::timeout(
        tokio::time::Duration::from_millis(tx_fetch.timeout_ms),
        rpc.get_transaction_with_retries(&sig, tx_fetch.attempts, tx_fetch.retry_delay_ms),
    ).await
    .map_err(|_| anyhow::anyhow!("getTransaction timeout"))?
    .map_err(|e| anyhow::anyhow!("getTransaction failed: {}", e))?;

    // ── 2. Find the liquidate instruction ────────────────────────────────────
    let liquidate_ix_idx = find_kamino_liquidate_ix(&tx_info);

    let ix_idx = liquidate_ix_idx
        .ok_or_else(|| anyhow::anyhow!("no KLEND liquidate instruction found"))?;

    let ix_accs = &tx_info.instruction_accounts[ix_idx];
    if ix_accs.len() < 13 {
        anyhow::bail!("liquidate instruction has too few accounts ({})", ix_accs.len());
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
                .ok_or_else(|| anyhow::anyhow!("missing account at position {}", $i))?
        }
    }

    let obligation_str    = resolve!(1);
    let market_str        = resolve!(2);
    let market_auth_str   = resolve!(3);
    let repay_reserve_str = resolve!(4);
    let repay_mint_str    = resolve!(5);
    let repay_supply_str  = resolve!(6);
    let wdr_reserve_str   = resolve!(7);
    let wdr_liq_mint_str  = resolve!(8);
    let wdr_col_mint_str  = resolve!(9);
    let wdr_col_sup_str   = resolve!(10);
    let wdr_liq_sup_str   = resolve!(11);
    let wdr_fee_str       = resolve!(12);

    // ── 4. Dedup: skip if we fired on this obligation recently ───────────────
    {
        let mut map = dedup.lock().unwrap();
        map.retain(|_, t| t.elapsed().as_millis() < OBLIGATION_DEDUP_MS);
        if map.contains_key(obligation_str) {
            trace_logger.log(HunterTraceEvent {
                timestamp: crate::utils::utc_now(),
                protocol: "kamino",
                stage: "skip",
                signature: sig.clone(),
                obligation: Some(obligation_str.to_string()),
                repay_mint: Some(repay_mint_str.to_string()),
                repay_symbol: None,
                reason: Some("dedup".to_string()),
                detail: None,
                ws_received_at_ms: Some(ws_received_at_ms),
                elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                bundle_id: None,
            });
            return Ok(());
        }
        map.insert(obligation_str.to_string(), std::time::Instant::now());
    }

    // ── 5. Check we hold the repay token ────────────────────────────────────
    let repay_token = wallet_index.get(repay_mint_str)
        .ok_or_else(|| anyhow::anyhow!("no wallet token for repay mint {}", repay_mint_str))?;
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
        });
        return Ok(());
    }

    // Cap repay at max_repay_usd (approximate: we cap native amount, not USD)
    // The actual USD cap is enforced by wallet.toml max_repay_native.
    let _ = max_repay_usd; // available for future price-based capping

    // ── 6. Parse pubkeys ─────────────────────────────────────────────────────
    let obligation_pk    = Pubkey::from_str(obligation_str)?;
    let market_pk        = Pubkey::from_str(market_str)?;
    let market_auth_pk   = Pubkey::from_str(market_auth_str)?;
    let repay_reserve_pk = Pubkey::from_str(repay_reserve_str)?;
    let repay_mint_pk    = Pubkey::from_str(repay_mint_str)?;
    let repay_supply_pk  = Pubkey::from_str(repay_supply_str)?;
    let wdr_reserve_pk   = Pubkey::from_str(wdr_reserve_str)?;
    let wdr_liq_mint_pk  = Pubkey::from_str(wdr_liq_mint_str)?;
    let wdr_col_mint_pk  = Pubkey::from_str(wdr_col_mint_str)?;
    let wdr_col_sup_pk   = Pubkey::from_str(wdr_col_sup_str)?;
    let wdr_liq_sup_pk   = Pubkey::from_str(wdr_liq_sup_str)?;
    let wdr_fee_pk       = Pubkey::from_str(wdr_fee_str)?;

    let klend_pk      = Pubkey::from_str(KLEND_PROGRAM).unwrap();
    let token_prog_pk = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let farms_pk      = Pubkey::from_str(FARMS_PROGRAM).unwrap();
    let tip_account   = Pubkey::from_str(JITO_TIP_ACCOUNT).unwrap();

    let liquidator = keypair.pubkey();
    let repay_reserve_meta = get_or_fetch_kamino_reserve_meta(&rpc, &reserve_cache, &repay_reserve_pk).await?;
    let withdraw_reserve_meta = get_or_fetch_kamino_reserve_meta(&rpc, &reserve_cache, &wdr_reserve_pk).await?;

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

    let mut instructions: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
        ix_refresh_reserve(&klend_pk, &repay_reserve_pk, &repay_reserve_meta),
        ix_refresh_reserve(&klend_pk, &wdr_reserve_pk, &withdraw_reserve_meta),
        ix_refresh_obligation(&klend_pk, &market_pk, &obligation_pk, &[&repay_reserve_pk, &wdr_reserve_pk]),
    ];

    // Liquidate instruction
    {
        let disc = discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2");
        let liquidity_amount = repay_token.max_repay_native;

        let mut data = disc.to_vec();
        data.extend_from_slice(&liquidity_amount.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // minAcceptableReceivedLiquidityAmount
        data.extend_from_slice(&0u64.to_le_bytes()); // maxAllowedLtvOverridePercent

        let user_src     = repay_token.source_ata;
        let user_dst_col = get_ata(&liquidator, &wdr_col_mint_pk);
        let user_dst_liq = get_ata(&liquidator, &wdr_liq_mint_pk);

        let mut accounts = vec![
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
            AccountMeta::new_readonly(token_prog_pk, false), // collateral token program
            AccountMeta::new_readonly(token_prog_pk, false), // repay liquidity token program
            AccountMeta::new_readonly(token_prog_pk, false), // withdraw liquidity token program
            AccountMeta::new_readonly(sysvar::instructions::id(), false),
        ];
        if ix_accs.len() >= 25 {
            for &account_idx in &ix_accs[20..24] {
                let pk = Pubkey::from_str(
                    tx_info.account_keys
                        .get(account_idx)
                        .ok_or_else(|| anyhow::anyhow!("missing farm account"))?,
                )?;
                accounts.push(AccountMeta::new(pk, false));
            }
            let farms_program_idx = *ix_accs.get(24).ok_or_else(|| anyhow::anyhow!("missing farms program"))?;
            let farms_program_pk = Pubkey::from_str(
                tx_info.account_keys
                    .get(farms_program_idx)
                    .ok_or_else(|| anyhow::anyhow!("missing farms program key"))?,
            )?;
            accounts.push(AccountMeta::new_readonly(farms_program_pk, false));
        } else {
            accounts.push(AccountMeta::new(klend_pk, false));
            accounts.push(AccountMeta::new(klend_pk, false));
            accounts.push(AccountMeta::new(klend_pk, false));
            accounts.push(AccountMeta::new(klend_pk, false));
            accounts.push(AccountMeta::new_readonly(farms_pk, false));
        }

        instructions.push(Instruction { program_id: klend_pk, accounts, data });
    }

    // Jito tip (pre-cached)
    let tip_lamports = cached_tip.load(Ordering::Relaxed);
    instructions.push(solana_sdk::system_instruction::transfer(&liquidator, &tip_account, tip_lamports));

    // ── 8. Build and sign tx with pre-cached blockhash ───────────────────────
    let blockhash = *cached_blockhash.read().await;

    let message = Message::try_compile(&liquidator, &instructions, &[], blockhash)
        .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
        .map_err(|e| anyhow::anyhow!("sign: {}", e))?;

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
        detail: Some(format!("tip={} cu_price={}", tip_lamports, compute_unit_price)),
        ws_received_at_ms: Some(ws_received_at_ms),
        elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
        bundle_id: None,
    });
    eprintln!(
        "[hunter-kamino] FIRING | obligation={} repay={} tip={} cu_price={}",
        &obligation_str[..8], repay_token.symbol, tip_lamports, compute_unit_price
    );

    if hunter_dry_run_enabled() {
        let tx_bytes = bincode::serialize(&tx)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "kamino",
            stage: "dry_run",
            signature: sig,
            obligation: Some(obligation_str.to_string()),
            repay_mint: Some(repay_mint_str.to_string()),
            repay_symbol: Some(repay_token.symbol.clone()),
            reason: Some("dry_run_enabled".to_string()),
            detail: Some(format!("tx_size_bytes={} tip={} cu_price={}", tx_bytes, tip_lamports, compute_unit_price)),
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
        });
        eprintln!(
            "[hunter-kamino] DRY RUN | obligation={} repay={} tx_size={}",
            &obligation_str[..8], repay_token.symbol, tx_bytes
        );
        return Ok(());
    }

    match jito.send_bundle(vec![tx]).await {
        Ok(bundle_id) => {
            trace_logger.log(HunterTraceEvent {
                timestamp: crate::utils::utc_now(),
                protocol: "kamino",
                stage: "bundle_sent",
                signature: sig,
                obligation: Some(obligation_str.to_string()),
                repay_mint: Some(repay_mint_str.to_string()),
                repay_symbol: Some(repay_token.symbol.clone()),
                reason: None,
                detail: None,
                ws_received_at_ms: Some(ws_received_at_ms),
                elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                bundle_id: Some(bundle_id.clone()),
            });
            eprintln!(
                "[hunter-kamino] BUNDLE SENT | obligation={} bundle={}",
                &obligation_str[..8], &bundle_id[..12.min(bundle_id.len())]
            );
        }
        Err(e) => {
            trace_logger.log(HunterTraceEvent {
                timestamp: crate::utils::utc_now(),
                protocol: "kamino",
                stage: "error",
                signature: sig,
                obligation: Some(obligation_str.to_string()),
                repay_mint: Some(repay_mint_str.to_string()),
                repay_symbol: Some(repay_token.symbol.clone()),
                reason: Some("bundle_send_failed".to_string()),
                detail: Some(e.to_string()),
                ws_received_at_ms: Some(ws_received_at_ms),
                elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                bundle_id: None,
            });
            eprintln!("[hunter-kamino] bundle send failed: {}", e);
        }
    }

    Ok(())
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
) -> anyhow::Result<()>
where
    R: RpcClient,
    JI: JitoPort,
{
    // ── 1. getTransaction — single attempt, 500ms hard timeout ───────────────
    let tx_info = tokio::time::timeout(
        tokio::time::Duration::from_millis(tx_fetch.timeout_ms),
        rpc.get_transaction_with_retries(&sig, tx_fetch.attempts, tx_fetch.retry_delay_ms),
    ).await
    .map_err(|_| anyhow::anyhow!("getTransaction timeout"))?
    .map_err(|e| anyhow::anyhow!("getTransaction failed: {}", e))?;

    // ── 2. Find Solend liquidate instruction (most accounts) ─────────────────
    let liq_ix_idx = tx_info.instruction_programs.iter()
        .enumerate()
        .filter(|(_, &prog_idx)| {
            tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(SOLEND_PROGRAM)
        })
        .max_by_key(|(ix_idx, _)| {
            tx_info.instruction_accounts.get(*ix_idx).map(|a| a.len()).unwrap_or(0)
        })
        .map(|(ix_idx, _)| ix_idx)
        .ok_or_else(|| anyhow::anyhow!("no Solend liquidate instruction found"))?;

    let liq_accs = &tx_info.instruction_accounts[liq_ix_idx];
    let liq_data = &tx_info.instruction_data[liq_ix_idx];

    if liq_accs.len() < 9 || liq_data.len() < 16 {
        anyhow::bail!("Solend liquidate instruction malformed (accs={} data={})", liq_accs.len(), liq_data.len());
    }

    // ── 3. Competitor's wallet = account_keys[0] (fee payer / signer) ────────
    let competitor = tx_info.account_keys.get(0)
        .ok_or_else(|| anyhow::anyhow!("empty account_keys"))?
        .clone();

    // ── 4. Build: account_index → (mint, owner) from token balances ──────────
    // This lets us identify competitor ATAs and derive our equivalent ATAs.
    let balance_map: HashMap<usize, (String, String)> = tx_info.post_token_balances.iter()
        .chain(tx_info.pre_token_balances.iter())
        .map(|b| (b.account_index, (b.mint.clone(), b.owner.clone())))
        .collect();

    // ── 5. Find repay token: competitor ATA that decreased (owned by competitor)
    // We look for a wallet_token whose mint appears in the balances for an
    // account owned by the competitor. That's the token they repaid with.
    let repay_mint = balance_map.values()
        .find(|(_, owner)| owner == &competitor)
        .and_then(|(mint, _)| wallet_index.get(mint))
        .ok_or_else(|| anyhow::anyhow!("no matching wallet token for this liquidation"))?;

    // ── 6. Dedup: skip if we fired on this obligation recently ───────────────
    // Obligation is at accounts[5] for LiquidateWithoutReceivingCtokens
    // (observer confirmed: accounts[5] = obligation, accounts[8] = liquidator).
    // We also fall back to checking a few positions to be safe.
    let obligation_key_idx = liq_accs.get(5)
        .and_then(|&i| tx_info.account_keys.get(i))
        .cloned()
        .unwrap_or_default();

    if obligation_key_idx.is_empty() {
        anyhow::bail!("could not extract obligation pubkey from Solend tx");
    }

    {
        let mut map = dedup.lock().unwrap();
        map.retain(|_, t| t.elapsed().as_millis() < OBLIGATION_DEDUP_MS);
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
    for (idx, (&prog_idx, accs)) in tx_info.instruction_programs.iter()
        .zip(tx_info.instruction_accounts.iter())
        .enumerate()
    {
        let prog_key = match tx_info.account_keys.get(prog_idx) {
            Some(k) => k.as_str(),
            None => continue,
        };
        if prog_key != SOLEND_PROGRAM { continue; }
        if idx == liq_ix_idx { continue; } // skip liquidate — we rebuild it below

        // Resolve account metas: assume all non-signer / non-writable for refresh
        let acc_metas: Vec<AccountMeta> = accs.iter().filter_map(|&ai| {
            tx_info.account_keys.get(ai).and_then(|k| Pubkey::from_str(k).ok()).map(|pk| {
                AccountMeta::new_readonly(pk, false)
            })
        }).collect();

        let data = tx_info.instruction_data.get(idx).cloned().unwrap_or_default();
        instructions.push(Instruction { program_id: solend_pk, accounts: acc_metas, data });
    }

    // Rebuild the liquidate instruction, replacing competitor accounts with ours.
    {
        let liquidator = keypair.pubkey();

        let acc_metas: Vec<AccountMeta> = liq_accs.iter().enumerate().map(|(pos, &ai)| {
            let key_str = tx_info.account_keys.get(ai).map(|s| s.as_str()).unwrap_or("");
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
            let is_likely_readonly = is_program_or_sysvar || pos >= liq_accs.len().saturating_sub(4);

            if is_likely_readonly {
                AccountMeta::new_readonly(pk, false)
            } else {
                AccountMeta::new(pk, false)
            }
        }).collect();

        // Copy instruction data, replacing liquidity_amount (bytes 8-15) with our cap.
        let mut data = liq_data.clone();
        let amount = repay_mint.max_repay_native;
        data[8..16].copy_from_slice(&amount.to_le_bytes());

        instructions.push(Instruction { program_id: solend_pk, accounts: acc_metas, data });
    }

    // Jito tip
    let tip_lamports = cached_tip.load(Ordering::Relaxed);
    let tip_account = Pubkey::from_str(JITO_TIP_ACCOUNT).unwrap();
    instructions.push(solana_sdk::system_instruction::transfer(
        &keypair.pubkey(), &tip_account, tip_lamports,
    ));

    // ── 8. Build and sign tx with pre-cached blockhash ───────────────────────
    let blockhash = *cached_blockhash.read().await;
    let liquidator = keypair.pubkey();

    let message = Message::try_compile(&liquidator, &instructions, &[], blockhash)
        .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
        .map_err(|e| anyhow::anyhow!("sign: {}", e))?;

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
        detail: Some(format!("tip={} cu_price={}", tip_lamports, compute_unit_price)),
        ws_received_at_ms: Some(ws_received_at_ms),
        elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
        bundle_id: None,
    });
    eprintln!(
        "[hunter-solend] FIRING | obligation={} repay={} tip={}",
        &obligation_key_idx[..8.min(obligation_key_idx.len())],
        repay_mint.symbol,
        tip_lamports,
    );

    if hunter_dry_run_enabled() {
        let tx_bytes = bincode::serialize(&tx)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        trace_logger.log(HunterTraceEvent {
            timestamp: crate::utils::utc_now(),
            protocol: "solend",
            stage: "dry_run",
            signature: sig,
            obligation: Some(obligation_key_idx.clone()),
            repay_mint: Some(repay_mint.mint.clone()),
            repay_symbol: Some(repay_mint.symbol.clone()),
            reason: Some("dry_run_enabled".to_string()),
            detail: Some(format!("tx_size_bytes={} tip={} cu_price={}", tx_bytes, tip_lamports, compute_unit_price)),
            ws_received_at_ms: Some(ws_received_at_ms),
            elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
            bundle_id: None,
        });
        eprintln!(
            "[hunter-solend] DRY RUN | obligation={} repay={} tx_size={}",
            &obligation_key_idx[..8.min(obligation_key_idx.len())],
            repay_mint.symbol,
            tx_bytes
        );
        return Ok(());
    }

    match jito.send_bundle(vec![tx]).await {
        Ok(bundle_id) => {
            trace_logger.log(HunterTraceEvent {
                timestamp: crate::utils::utc_now(),
                protocol: "solend",
                stage: "bundle_sent",
                signature: sig,
                obligation: Some(obligation_key_idx.clone()),
                repay_mint: Some(repay_mint.mint.clone()),
                repay_symbol: Some(repay_mint.symbol.clone()),
                reason: None,
                detail: None,
                ws_received_at_ms: Some(ws_received_at_ms),
                elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                bundle_id: Some(bundle_id.clone()),
            });
            eprintln!(
                "[hunter-solend] BUNDLE SENT | obligation={} bundle={}",
                &obligation_key_idx[..8.min(obligation_key_idx.len())],
                &bundle_id[..12.min(bundle_id.len())]
            );
        }
        Err(e) => {
            trace_logger.log(HunterTraceEvent {
                timestamp: crate::utils::utc_now(),
                protocol: "solend",
                stage: "error",
                signature: sig,
                obligation: Some(obligation_key_idx.clone()),
                repay_mint: Some(repay_mint.mint.clone()),
                repay_symbol: Some(repay_mint.symbol.clone()),
                reason: Some("bundle_send_failed".to_string()),
                detail: Some(e.to_string()),
                ws_received_at_ms: Some(ws_received_at_ms),
                elapsed_ms: Some(elapsed_ms_since(ws_received_at_ms)),
                bundle_id: None,
            });
            eprintln!("[hunter-solend] bundle send failed: {}", e);
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

fn kamino_liquidate_discriminator() -> [u8; 8] {
    discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2")
}

fn find_kamino_liquidate_ix(
    tx_info: &crate::ports::rpc::TransactionInfo,
) -> Option<usize> {
    let expected_disc = kamino_liquidate_discriminator();
    tx_info
        .instruction_programs
        .iter()
        .enumerate()
        .find(|(ix_idx, &prog_idx)| {
            tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(KLEND_PROGRAM)
                && tx_info
                    .instruction_data
                    .get(*ix_idx)
                    .map(|data| data.len() >= 8 && data[..8] == expected_disc)
                    .unwrap_or(false)
        })
        .map(|(ix_idx, _)| ix_idx)
}

// ── Solend log helpers ────────────────────────────────────────────────────────

fn extract_obligation_pda_from_logs(logs: &[String]) -> Option<String> {
    for line in logs {
        let content = line.strip_prefix("Program log: ").unwrap_or(line);
        if let Some(rest) = content.strip_prefix("obligation_info:") {
            let pda = rest.trim().split_whitespace().next()?.to_string();
            if !pda.is_empty() { return Some(pda); }
        }
    }
    None
}

fn extract_log_field(logs: &[String], key: &str) -> Option<String> {
    for line in logs {
        let content = line.strip_prefix("Program log: ").unwrap_or(line);
        if let Some(rest) = content.strip_prefix(key) {
            let val = rest.trim().split_whitespace().next()?.to_string();
            if !val.is_empty() { return Some(val); }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::rpc::TransactionInfo;

    #[test]
    fn parse_toml_u64_supports_underscores_and_comments() {
        assert_eq!(
            parse_toml_u64("max_repay_native = 1_500_000_000  # 1.5 SOL", "max_repay_native"),
            Some(1_500_000_000)
        );
    }

    #[test]
    fn finds_kamino_liquidate_instruction_by_discriminator() {
        let mut liquidate_data = kamino_liquidate_discriminator().to_vec();
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
        });

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"stage\":\"skip\""));
        assert!(content.contains("\"reason\":\"dedup\""));

        let _ = std::fs::remove_file(path);
    }
}
