use crate::application::kamino_shortlist::{PreparedExecutionContext, ShortlistState};
use crate::application::kamino_tx::decode_kamino_reserve;
use crate::config::wallet::WalletToken;
use crate::domain::protocol::KAMINO_PROGRAM_ID;
use crate::ports::rpc::{ProgramAccount, RpcClient};
use crate::utils::{log_stderr, log_stdout_at, RuntimeLogVerbosity};
use borsh::BorshDeserialize;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;

const DEFAULT_KAMINO_LENDING_MARKET: &str = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
const DEFAULT_KAMINO_MARKET_AUTHORITY: &str = "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo";
const KAMINO_OBLIGATION_HAS_DEBT_OFFSET: usize = 2287;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HermesExecutionMode {
    Prepare,
    Hybrid,
    Only,
}

impl HermesExecutionMode {
    pub fn from_env() -> Self {
        match std::env::var("HERMES_EXECUTION_MODE")
            .unwrap_or_else(|_| "prepare".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "hybrid" => Self::Hybrid,
            "only" => Self::Only,
            _ => Self::Prepare,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Hybrid => "hybrid",
            Self::Only => "only",
        }
    }

    pub fn allows_hermes_firing(self) -> bool {
        matches!(self, Self::Hybrid | Self::Only)
    }

    pub fn reactive_observe_only(self) -> bool {
        matches!(self, Self::Hybrid)
    }
}

#[derive(Debug, Clone)]
pub struct HermesRuntimeConfig {
    pub ws_url: String,
    pub refresh_secs: u64,
    pub shortlist_size: usize,
    pub min_repay_usd: f64,
    pub max_signals_per_batch: usize,
    pub trigger_buffer_bps: f64,
    pub armed_stale_ms: u64,
    pub cooldown_ms: u64,
    pub execution_mode: HermesExecutionMode,
    pub fire_enabled: bool,
    pub fire_confirmation_window_ms: u64,
    pub fire_max_context_age_ms: u64,
    pub fire_cooldown_ms: u64,
    pub fire_min_feed_match_count: usize,
    pub fire_require_persistence: bool,
    pub fireability_invalid_threshold: u32,
    pub fireability_invalid_cooldown_ms: u64,
    pub fireability_account_missing_cooldown_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HermesSignalEvent {
    pub obligation_pubkey: String,
    pub repay_mint: String,
    pub repay_symbol: String,
    pub feed_match_count: usize,
    pub signal_received_at_ms: u64,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HermesFastLaneContext {
    pub prepared_context: PreparedExecutionContext,
    pub state: ShortlistState,
    pub last_price_signal_at_ms: u64,
    pub last_refresh_at_ms: u64,
    pub feed_match_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HermesShortlistEntry {
    pub obligation_pubkey: String,
    pub repay_mint: String,
    pub repay_symbol: String,
    pub tracked_feed_ids: Vec<String>,
    pub distance_to_liq: f64,
    pub last_price_signal_at_ms: u64,
    pub last_signal_emitted_at_ms: u64,
    pub last_refresh_at_ms: u64,
    pub state: ShortlistState,
    pub inclusion_reason: String,
    pub prepared_context: PreparedExecutionContext,
    pub last_feed_match_count: usize,
    cooldown_until_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct HermesShortlistRuntime {
    pub entries: HashMap<String, HermesShortlistEntry>,
    pub last_refresh_completed_at_ms: Option<u64>,
    fireability: HashMap<String, HermesFireabilityRecord>,
}

#[derive(Debug, Clone, Default)]
struct HermesFireabilityRecord {
    invalid_bundle_count: u32,
    invalid_blocked_until_ms: Option<u64>,
    account_missing_blocked_until_ms: Option<u64>,
    last_rejection_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HermesReserveInfo {
    pub reserve_pubkey: String,
    pub mint: String,
    pub pyth_feed_id: Option<String>,
    pub lending_market: String,
    pub market_price_sf: u128,
    pub liquidity_supply: String,
    pub collateral_mint: String,
    pub collateral_supply: String,
    pub liquidity_fee_receiver: String,
}

#[derive(Debug, Clone, Default)]
struct HermesShortlistDiagnostics {
    reserves_decoded: usize,
    reserves_with_pyth: usize,
    obligations_decoded: usize,
    skipped_no_debt: usize,
    skipped_no_market_value: usize,
    skipped_no_wallet_repay: usize,
    skipped_wallet_repay_cap: usize,
    skipped_small_repay_usd: usize,
    skipped_no_withdraw: usize,
    skipped_no_price_feed: usize,
    skipped_market_mismatch: usize,
    skipped_unsupported_market: usize,
    eligible: usize,
}

impl HermesShortlistDiagnostics {
    fn summarize(&self) -> String {
        format!(
            "reserves_decoded={} reserves_with_pyth={} obligations_decoded={} eligible={} skipped_no_debt={} skipped_no_market_value={} skipped_no_wallet_repay={} skipped_wallet_repay_cap={} skipped_small_repay_usd={} skipped_no_withdraw={} skipped_no_price_feed={} skipped_market_mismatch={} skipped_unsupported_market={}",
            self.reserves_decoded,
            self.reserves_with_pyth,
            self.obligations_decoded,
            self.eligible,
            self.skipped_no_debt,
            self.skipped_no_market_value,
            self.skipped_no_wallet_repay,
            self.skipped_wallet_repay_cap,
            self.skipped_small_repay_usd,
            self.skipped_no_withdraw,
            self.skipped_no_price_feed,
            self.skipped_market_mismatch,
            self.skipped_unsupported_market
        )
    }
}

fn wallet_token_max_repay_market_value_sf(token: &WalletToken, reserve: &HermesReserveInfo) -> u128 {
    if token.max_repay_native == 0 {
        return 0;
    }

    let token_decimals = token.decimals.min(18) as u32;
    let scale = 10u128.saturating_pow(token_decimals);
    (token.max_repay_native as u128)
        .saturating_mul(reserve.market_price_sf)
        .saturating_div(scale.max(1))
}

impl HermesRuntimeConfig {
    pub fn from_env() -> Self {
        let ws_url = std::env::var("SIGNAL_FEED_WS_URL")
            .or_else(|_| std::env::var("HERMES_WS_URL"))
            .unwrap_or_else(|_| "https://hermes.pyth.network".to_string())
            .trim_end_matches('/')
            .to_string();
        let refresh_secs = std::env::var("HERMES_REFRESH_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_200);
        let shortlist_size = std::env::var("HERMES_SHORTLIST_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|v| v.clamp(1, 512))
            .unwrap_or(10);
        let min_repay_usd = std::env::var("HERMES_SHORTLIST_MIN_REPAY_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map(|v| v.max(0.0))
            .unwrap_or(0.5);
        let max_signals_per_batch = std::env::var("HERMES_MAX_SIGNALS_PER_BATCH")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|v| v.max(1))
            .unwrap_or(1);
        let trigger_buffer_bps = std::env::var("HERMES_TRIGGER_BUFFER_BPS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(25) as f64
            / 10_000.0;
        let armed_stale_ms = std::env::var("HERMES_ARMED_STALE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(refresh_secs.saturating_mul(1_000));
        let cooldown_ms = std::env::var("HERMES_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20_000);
        let execution_mode = HermesExecutionMode::from_env();
        let fire_enabled = std::env::var("HERMES_FIRE_ENABLE")
            .ok()
            .map(|value| {
                let value = value.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);
        let fire_confirmation_window_ms = std::env::var("HERMES_FIRE_CONFIRMATION_WINDOW_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);
        let fire_max_context_age_ms = std::env::var("HERMES_FIRE_MAX_CONTEXT_AGE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2_000);
        let fire_cooldown_ms = std::env::var("HERMES_FIRE_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20_000);
        let fire_min_feed_match_count = std::env::var("HERMES_FIRE_MIN_FEED_MATCH_COUNT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);
        let fire_require_persistence = std::env::var("HERMES_FIRE_REQUIRE_PERSISTENCE")
            .ok()
            .map(|value| {
                let value = value.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);
        let fireability_invalid_threshold = std::env::var("HERMES_FIREABILITY_INVALID_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .map(|v| v.max(1))
            .unwrap_or(3);
        let fireability_invalid_cooldown_ms =
            std::env::var("HERMES_FIREABILITY_INVALID_COOLDOWN_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(15 * 60 * 1_000);
        let fireability_account_missing_cooldown_ms =
            std::env::var("HERMES_FIREABILITY_ACCOUNT_MISSING_COOLDOWN_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(15 * 60 * 1_000);

        Self {
            ws_url,
            refresh_secs,
            shortlist_size,
            min_repay_usd,
            max_signals_per_batch,
            trigger_buffer_bps,
            armed_stale_ms,
            cooldown_ms,
            execution_mode,
            fire_enabled,
            fire_confirmation_window_ms,
            fire_max_context_age_ms,
            fire_cooldown_ms,
            fire_min_feed_match_count,
            fire_require_persistence,
            fireability_invalid_threshold,
            fireability_invalid_cooldown_ms,
            fireability_account_missing_cooldown_ms,
        }
    }
}

impl HermesShortlistRuntime {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            last_refresh_completed_at_ms: None,
            fireability: HashMap::new(),
        }
    }

    pub fn shortlisted_entry(&self, obligation: &str) -> Option<HermesShortlistEntry> {
        self.entries.get(obligation).cloned()
    }

    pub fn fast_lane_context(
        &self,
        obligation: &str,
        now_ms: u64,
        config: &HermesRuntimeConfig,
    ) -> Option<HermesFastLaneContext> {
        let entry = self.entries.get(obligation)?;
        if entry.state != ShortlistState::Armed {
            return None;
        }
        if is_entry_stale(entry, now_ms, config) {
            return None;
        }
        Some(HermesFastLaneContext {
            prepared_context: entry.prepared_context.clone(),
            state: entry.state,
            last_price_signal_at_ms: entry.last_price_signal_at_ms,
            last_refresh_at_ms: entry.last_refresh_at_ms,
            feed_match_count: entry.last_feed_match_count,
        })
    }

    pub fn note_reactive_hit(
        &mut self,
        obligation: &str,
        now_ms: u64,
        config: &HermesRuntimeConfig,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(obligation) else {
            return false;
        };
        entry.state = ShortlistState::CoolingDown;
        entry.cooldown_until_ms = Some(now_ms.saturating_add(config.cooldown_ms));
        true
    }

    pub fn note_hermes_fire(
        &mut self,
        obligation: &str,
        now_ms: u64,
        config: &HermesRuntimeConfig,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(obligation) else {
            return false;
        };
        entry.state = ShortlistState::CoolingDown;
        entry.cooldown_until_ms = Some(now_ms.saturating_add(config.fire_cooldown_ms));
        true
    }

    pub fn note_invalid_bundle(
        &mut self,
        obligation: &str,
        now_ms: u64,
        config: &HermesRuntimeConfig,
    ) -> bool {
        let record = self.fireability.entry(obligation.to_string()).or_default();
        record.invalid_bundle_count = record.invalid_bundle_count.saturating_add(1);
        record.last_rejection_reason = Some("invalid_bundle_history".to_string());
        if record.invalid_bundle_count >= config.fireability_invalid_threshold {
            record.invalid_blocked_until_ms =
                Some(now_ms.saturating_add(config.fireability_invalid_cooldown_ms));
            if let Some(entry) = self.entries.get_mut(obligation) {
                entry.state = ShortlistState::Dropped;
                entry.cooldown_until_ms = record.invalid_blocked_until_ms;
                entry.inclusion_reason = format!(
                    "invalid_bundle_cooldown count={} cooldown_ms={}",
                    record.invalid_bundle_count, config.fireability_invalid_cooldown_ms
                );
            }
        }
        true
    }

    fn note_account_missing_refresh_rejection(
        &mut self,
        obligation: &str,
        now_ms: u64,
        config: &HermesRuntimeConfig,
        reason: String,
    ) {
        let record = self.fireability.entry(obligation.to_string()).or_default();
        record.account_missing_blocked_until_ms =
            Some(now_ms.saturating_add(config.fireability_account_missing_cooldown_ms));
        record.last_rejection_reason = Some(reason);
    }

    fn note_refresh_validation_success(&mut self, obligation: &str) {
        if let Some(record) = self.fireability.get_mut(obligation) {
            record.account_missing_blocked_until_ms = None;
            if record.invalid_blocked_until_ms.is_none() {
                record.last_rejection_reason = None;
            }
        }
    }

    fn shortlist_block_reason(&self, obligation: &str, now_ms: u64) -> Option<String> {
        let record = self.fireability.get(obligation)?;
        if let Some(until_ms) = record.account_missing_blocked_until_ms {
            if now_ms < until_ms {
                return Some(format!(
                    "account_missing cooldown_ms={}",
                    until_ms.saturating_sub(now_ms)
                ));
            }
        }
        if let Some(until_ms) = record.invalid_blocked_until_ms {
            if now_ms < until_ms {
                return Some(format!(
                    "invalid_history count={} cooldown_ms={}",
                    record.invalid_bundle_count,
                    until_ms.saturating_sub(now_ms)
                ));
            }
        }
        None
    }

    pub fn apply_changed_feeds(
        &mut self,
        changed: &[String],
        received_at_ms: u64,
        config: &HermesRuntimeConfig,
    ) -> Vec<HermesSignalEvent> {
        let changed_set = changed.iter().cloned().collect::<HashSet<_>>();
        let mut pending = Vec::new();

        for entry in self.entries.values_mut() {
            reconcile_entry_state(entry, received_at_ms, config);
            if entry.state == ShortlistState::Dropped {
                continue;
            }
            let feed_match_count = entry
                .tracked_feed_ids
                .iter()
                .filter(|feed_id| changed_set.contains(*feed_id))
                .count();
            if feed_match_count == 0 {
                continue;
            }

            entry.last_price_signal_at_ms = received_at_ms;
            entry.last_feed_match_count = feed_match_count;

            if entry.state == ShortlistState::CoolingDown {
                continue;
            }

            if entry.distance_to_liq <= config.trigger_buffer_bps {
                let should_emit = entry.state != ShortlistState::Armed
                    || entry.last_signal_emitted_at_ms == 0
                    || received_at_ms.saturating_sub(entry.last_signal_emitted_at_ms)
                        >= config.fire_cooldown_ms;
                entry.state = ShortlistState::Armed;
                entry.inclusion_reason = format!(
                    "hermes_feed_match buffer_bps={} matched_feeds={}",
                    (config.trigger_buffer_bps * 10_000.0).round() as u64,
                    feed_match_count
                );
                if should_emit {
                    pending.push((
                        entry.obligation_pubkey.clone(),
                        entry.distance_to_liq,
                        HermesSignalEvent {
                            obligation_pubkey: entry.obligation_pubkey.clone(),
                            repay_mint: entry.repay_mint.clone(),
                            repay_symbol: entry.repay_symbol.clone(),
                            feed_match_count,
                            signal_received_at_ms: received_at_ms,
                            detail: format!(
                                "hermes_feed_update distance_to_liq={:.8} matched_feeds={} refresh_at_ms={}",
                                entry.distance_to_liq, feed_match_count, entry.last_refresh_at_ms
                            ),
                        },
                    ));
                }
            } else {
                entry.state = ShortlistState::Warm;
            }
        }

        pending.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let selected = pending
            .into_iter()
            .take(config.max_signals_per_batch)
            .collect::<Vec<_>>();

        for (obligation, _, _) in &selected {
            if let Some(entry) = self.entries.get_mut(obligation) {
                entry.last_signal_emitted_at_ms = received_at_ms;
            }
        }

        selected
            .into_iter()
            .map(|(_, _, signal)| signal)
            .collect()
    }
}

pub fn decode_kamino_obligation(data: &[u8]) -> Option<crate::domain::kamino::Obligation> {
    if data.len() < 8 {
        return None;
    }
    let mut cursor = &data[8..];
    crate::domain::kamino::Obligation::deserialize(&mut cursor).ok()
}

fn obligation_lending_market(obligation: &crate::domain::kamino::Obligation) -> String {
    Pubkey::new_from_array(obligation.lending_market).to_string()
}

fn supported_hermes_lending_market() -> String {
    std::env::var("KAMINO_LENDING_MARKET")
        .unwrap_or_else(|_| DEFAULT_KAMINO_LENDING_MARKET.to_string())
}

fn supported_hermes_market_authority() -> String {
    std::env::var("KAMINO_MARKET_AUTHORITY")
        .unwrap_or_else(|_| DEFAULT_KAMINO_MARKET_AUTHORITY.to_string())
}

pub fn hermes_feed_id_from_pubkey(pk: Pubkey) -> String {
    format!("0x{}", hex_encode_lower(&pk.to_bytes()))
}

fn hermes_price_feed_id_for_mint(mint: &str) -> Option<String> {
    let feed = match mint {
        // Pyth Core stable price feed IDs.
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => {
            "0xeaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a"
        }
        "So11111111111111111111111111111111111111112" => {
            "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d"
        }
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => {
            "0x2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b"
        }
        "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs" => {
            "0xff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace"
        }
        "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn" => {
            "0x67be9f519b95cf24338801051f9a808eff0a578ccb388db73b7f6fe1de019ffb"
        }
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" => {
            "0xc2289a6a43d2ce91c6f55caec370f4acc38a2ed477f58813334c6d03749ff2a4"
        }
        _ => return None,
    };
    Some(feed.to_string())
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

fn anchor_account_discriminator_base58(name: &str) -> String {
    let preimage = format!("account:{name}");
    let hash = Sha256::digest(preimage.as_bytes());
    bs58::encode(&hash[..8]).into_string()
}

pub fn build_hermes_shortlist(
    wallet_tokens: &[WalletToken],
    program_accounts: Vec<ProgramAccount>,
    shortlist_size: usize,
    min_repay_usd: f64,
    refreshed_at_ms: u64,
) -> Vec<HermesShortlistEntry> {
    let (mut entries, _) = build_hermes_shortlist_with_diagnostics(
        wallet_tokens,
        program_accounts,
        shortlist_size,
        min_repay_usd,
        refreshed_at_ms,
    )
    ;
    entries.truncate(shortlist_size);
    entries
}

fn build_hermes_shortlist_with_diagnostics(
    wallet_tokens: &[WalletToken],
    program_accounts: Vec<ProgramAccount>,
    shortlist_size: usize,
    min_repay_usd: f64,
    refreshed_at_ms: u64,
) -> (Vec<HermesShortlistEntry>, HermesShortlistDiagnostics) {
    let mut reserve_infos: HashMap<[u8; 32], HermesReserveInfo> = HashMap::new();
    let mut obligations = Vec::new();
    let mut diagnostics = HermesShortlistDiagnostics::default();
    for account in &program_accounts {
        if let Ok(reserve) = decode_kamino_reserve(&account.data) {
            diagnostics.reserves_decoded += 1;
            let mint = Pubkey::new_from_array(reserve.liquidity.mint_pubkey).to_string();
            let pyth_feed_id = hermes_price_feed_id_for_mint(&mint);
            if pyth_feed_id.is_some() {
                diagnostics.reserves_with_pyth += 1;
            }
            if let Ok(reserve_pubkey) = Pubkey::from_str(&account.pubkey) {
                reserve_infos.insert(
                    reserve_pubkey.to_bytes(),
                    HermesReserveInfo {
                        reserve_pubkey: account.pubkey.clone(),
                        mint,
                        pyth_feed_id,
                        lending_market: Pubkey::new_from_array(reserve.lending_market).to_string(),
                        market_price_sf: reserve.liquidity.market_price_sf,
                        liquidity_supply: Pubkey::new_from_array(reserve.liquidity.supply_vault)
                            .to_string(),
                        collateral_mint: Pubkey::new_from_array(reserve.collateral.mint_pubkey)
                            .to_string(),
                        collateral_supply: Pubkey::new_from_array(reserve.collateral.supply_vault)
                            .to_string(),
                        liquidity_fee_receiver: Pubkey::new_from_array(reserve.liquidity.fee_vault)
                            .to_string(),
                    },
                );
            }
        }
        if let Some(obligation) = decode_kamino_obligation(&account.data) {
            diagnostics.obligations_decoded += 1;
            obligations.push((account.pubkey.clone(), obligation));
        }
    }

    let entries = build_hermes_shortlist_from_decoded_with_diagnostics(
        wallet_tokens,
        obligations,
        &reserve_infos,
        min_repay_usd,
        refreshed_at_ms,
        &mut diagnostics,
    );
    let _ = shortlist_size;
    (entries, diagnostics)
}

pub fn build_hermes_shortlist_from_decoded(
    wallet_tokens: &[WalletToken],
    obligations: Vec<(String, crate::domain::kamino::Obligation)>,
    reserve_infos: &HashMap<[u8; 32], HermesReserveInfo>,
    min_repay_usd: f64,
    refreshed_at_ms: u64,
) -> Vec<HermesShortlistEntry> {
    let mut diagnostics = HermesShortlistDiagnostics::default();
    build_hermes_shortlist_from_decoded_with_diagnostics(
        wallet_tokens,
        obligations,
        reserve_infos,
        min_repay_usd,
        refreshed_at_ms,
        &mut diagnostics,
    )
}

fn build_hermes_shortlist_from_decoded_with_diagnostics(
    wallet_tokens: &[WalletToken],
    obligations: Vec<(String, crate::domain::kamino::Obligation)>,
    reserve_infos: &HashMap<[u8; 32], HermesReserveInfo>,
    min_repay_usd: f64,
    refreshed_at_ms: u64,
    diagnostics: &mut HermesShortlistDiagnostics,
) -> Vec<HermesShortlistEntry> {
    let whitelist: HashMap<String, &WalletToken> = wallet_tokens
        .iter()
        .map(|token| (token.mint.clone(), token))
        .collect();
    let mut shortlist = Vec::new();

    for (obligation_pubkey, obligation) in obligations {
        if obligation.has_debt == 0 {
            diagnostics.skipped_no_debt += 1;
            continue;
        }
        if obligation.borrowed_assets_market_value_sf == 0 {
            diagnostics.skipped_no_market_value += 1;
            continue;
        }

        let distance_to_liq = obligation.dist_to_liq();
        if distance_to_liq <= 0.0 {
            continue;
        }
        let mut tracked_feed_ids = Vec::new();
        let mut active_reserve_pubkeys = Vec::new();
        let mut repay_choice: Option<([u8; 32], String, String, String, u128, f64)> = None;
        let mut withdraw_choice: Option<([u8; 32], String, String, u128)> = None;

        for deposit in &obligation.deposits {
            if deposit.deposited_amount == 0 && deposit.market_value_sf == 0 {
                continue;
            }
            if let Some(reserve) = reserve_infos.get(&deposit.deposit_reserve) {
                active_reserve_pubkeys.push(reserve.reserve_pubkey.clone());
                if let Some(feed_id) = &reserve.pyth_feed_id {
                    tracked_feed_ids.push(feed_id.clone());
                }
                let candidate = (
                    deposit.deposit_reserve,
                    reserve.reserve_pubkey.clone(),
                    reserve.mint.clone(),
                    deposit.market_value_sf,
                );
                if withdraw_choice
                    .as_ref()
                    .is_none_or(|(_, _, _, current)| candidate.3 > *current)
                {
                    withdraw_choice = Some(candidate);
                }
            }
        }

        for borrow in &obligation.borrows {
            if borrow.borrowed_amount_sf == 0 && borrow.market_value_sf == 0 {
                continue;
            }
            if let Some(reserve) = reserve_infos.get(&borrow.borrow_reserve) {
                active_reserve_pubkeys.push(reserve.reserve_pubkey.clone());
                if let Some(feed_id) = &reserve.pyth_feed_id {
                    tracked_feed_ids.push(feed_id.clone());
                }
                if let Some(token) = whitelist.get(&reserve.mint) {
                    let max_repay_market_value_sf =
                        wallet_token_max_repay_market_value_sf(token, reserve);
                    if max_repay_market_value_sf == 0
                        || borrow.market_value_sf > max_repay_market_value_sf
                    {
                        diagnostics.skipped_wallet_repay_cap += 1;
                        continue;
                    }
                    let candidate = (
                        borrow.borrow_reserve,
                        reserve.reserve_pubkey.clone(),
                        reserve.mint.clone(),
                        token.symbol.clone(),
                        borrow.market_value_sf,
                        crate::domain::kamino::Obligation::sf_to_f64(borrow.market_value_sf),
                    );
                    if repay_choice
                        .as_ref()
                        .is_none_or(|(_, _, _, _, current, _)| candidate.4 > *current)
                    {
                        repay_choice = Some(candidate);
                    }
                }
            }
        }

        tracked_feed_ids.sort();
        tracked_feed_ids.dedup();
        active_reserve_pubkeys.sort();
        active_reserve_pubkeys.dedup();
        if tracked_feed_ids.is_empty() {
            diagnostics.skipped_no_price_feed += 1;
            continue;
        }

        let Some((repay_reserve_key, repay_reserve, repay_mint, repay_symbol, _, repay_usd)) =
            repay_choice
        else {
            diagnostics.skipped_no_wallet_repay += 1;
            continue;
        };
        if repay_usd < min_repay_usd {
            diagnostics.skipped_small_repay_usd += 1;
            continue;
        }
        let Some((withdraw_reserve_key, withdraw_reserve, withdraw_mint, _)) = withdraw_choice
        else {
            diagnostics.skipped_no_withdraw += 1;
            continue;
        };
        let lending_market = reserve_infos[&repay_reserve_key].lending_market.clone();
        if reserve_infos[&withdraw_reserve_key].lending_market != lending_market {
            diagnostics.skipped_market_mismatch += 1;
            continue;
        }
        let lending_market_authority = supported_hermes_market_authority();
        let supported_lending_market = supported_hermes_lending_market();
        if lending_market != supported_lending_market {
            diagnostics.skipped_unsupported_market += 1;
            continue;
        }

        diagnostics.eligible += 1;
        let inclusion_reason = format!(
            "wallet_repay_eligible positive_distance_only min_repay_usd_passed repay_usd={:.8} distance_to_liq={:.8}",
            repay_usd, distance_to_liq
        );
        shortlist.push(HermesShortlistEntry {
            obligation_pubkey: obligation_pubkey.clone(),
            repay_mint: repay_mint.clone(),
            repay_symbol: repay_symbol.clone(),
            tracked_feed_ids,
            distance_to_liq,
            last_price_signal_at_ms: 0,
            last_signal_emitted_at_ms: 0,
            last_refresh_at_ms: refreshed_at_ms,
            state: ShortlistState::Warm,
            inclusion_reason: inclusion_reason.clone(),
            prepared_context: PreparedExecutionContext {
                obligation_pubkey,
                repay_mint,
                repay_symbol,
                wallet_eligible: true,
                lending_market,
                lending_market_authority,
                repay_reserve,
                repay_supply: reserve_infos[&repay_reserve_key].liquidity_supply.clone(),
                withdraw_reserve,
                withdraw_mint,
                withdraw_collateral_mint: reserve_infos[&withdraw_reserve_key]
                    .collateral_mint
                    .clone(),
                withdraw_collateral_supply: reserve_infos[&withdraw_reserve_key]
                    .collateral_supply
                    .clone(),
                withdraw_liquidity_supply: reserve_infos[&withdraw_reserve_key]
                    .liquidity_supply
                    .clone(),
                withdraw_liquidity_fee_receiver: reserve_infos[&withdraw_reserve_key]
                    .liquidity_fee_receiver
                    .clone(),
                active_reserve_pubkeys,
                inclusion_reason,
            },
            last_feed_match_count: 0,
            cooldown_until_ms: None,
        });
    }

    shortlist.sort_by(|a, b| {
        a.distance_to_liq
            .partial_cmp(&b.distance_to_liq)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.obligation_pubkey.cmp(&b.obligation_pubkey))
    });
    shortlist
}

pub fn merge_shortlist_entries(
    previous: &HermesShortlistRuntime,
    fresh_entries: Vec<HermesShortlistEntry>,
    config: &HermesRuntimeConfig,
    refreshed_at_ms: u64,
) -> HermesShortlistRuntime {
    let mut entries = HashMap::new();
    for mut entry in fresh_entries {
        if let Some(previous_entry) = previous.entries.get(&entry.obligation_pubkey) {
            entry.last_price_signal_at_ms = previous_entry.last_price_signal_at_ms;
            entry.last_signal_emitted_at_ms = previous_entry.last_signal_emitted_at_ms;
            entry.last_feed_match_count = previous_entry.last_feed_match_count;
            entry.cooldown_until_ms = previous_entry.cooldown_until_ms;
            entry.state = previous_entry.state;
            reconcile_entry_state(&mut entry, refreshed_at_ms, config);
        }
        entries.insert(entry.obligation_pubkey.clone(), entry);
    }

    HermesShortlistRuntime {
        entries,
        last_refresh_completed_at_ms: Some(refreshed_at_ms),
        fireability: previous.fireability.clone(),
    }
}

async fn filter_fireable_shortlist_entries<R: RpcClient>(
    rpc: &R,
    runtime: &mut HermesShortlistRuntime,
    fresh_entries: Vec<HermesShortlistEntry>,
    config: &HermesRuntimeConfig,
    refreshed_at_ms: u64,
) -> (Vec<HermesShortlistEntry>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();

    for entry in fresh_entries {
        if accepted.len() >= config.shortlist_size {
            break;
        }

        if let Some(reason) = runtime.shortlist_block_reason(&entry.obligation_pubkey, refreshed_at_ms)
        {
            rejected.push(format!(
                "{}:shortlist_candidate_rejected_invalid_history:{}",
                shorten_pubkey(&entry.obligation_pubkey),
                reason
            ));
            continue;
        }

        match rpc.get_account_info(&entry.obligation_pubkey).await {
            Ok(data) => {
                let Some(obligation) = decode_kamino_obligation(&data) else {
                    runtime.note_account_missing_refresh_rejection(
                        &entry.obligation_pubkey,
                        refreshed_at_ms,
                        config,
                        "decode_failed".to_string(),
                    );
                    rejected.push(format!(
                        "{}:shortlist_candidate_rejected_not_fireable:decode_failed",
                        shorten_pubkey(&entry.obligation_pubkey)
                    ));
                    continue;
                };
                let actual_market = obligation_lending_market(&obligation);
                if actual_market != entry.prepared_context.lending_market {
                    let reason = format!(
                        "market_mismatch expected={} actual={}",
                        entry.prepared_context.lending_market, actual_market
                    );
                    runtime.note_account_missing_refresh_rejection(
                        &entry.obligation_pubkey,
                        refreshed_at_ms,
                        config,
                        reason.clone(),
                    );
                    rejected.push(format!(
                        "{}:shortlist_candidate_rejected_not_fireable:{}",
                        shorten_pubkey(&entry.obligation_pubkey),
                        reason
                    ));
                    continue;
                }
                runtime.note_refresh_validation_success(&entry.obligation_pubkey);
                accepted.push(entry);
            }
            Err(error) => {
                runtime.note_account_missing_refresh_rejection(
                    &entry.obligation_pubkey,
                    refreshed_at_ms,
                    config,
                    format!("account_lookup_failed:{error}"),
                );
                rejected.push(format!(
                    "{}:shortlist_candidate_rejected_not_fireable:{}",
                    shorten_pubkey(&entry.obligation_pubkey),
                    error
                ));
            }
        }
    }

    (accepted, rejected)
}

pub fn parse_hermes_changed_feed_ids(payload: &str) -> Vec<String> {
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

fn unique_feed_count(entries: &[HermesShortlistEntry]) -> usize {
    entries
        .iter()
        .flat_map(|entry| entry.tracked_feed_ids.iter())
        .collect::<HashSet<_>>()
        .len()
}

fn unique_runtime_feed_count(runtime: &HermesShortlistRuntime) -> usize {
    runtime
        .entries
        .values()
        .flat_map(|entry| entry.tracked_feed_ids.iter())
        .collect::<HashSet<_>>()
        .len()
}

fn summarize_shortlist_entries(entries: &[HermesShortlistEntry], limit: usize) -> String {
    if entries.is_empty() {
        return "none".to_string();
    }
    entries
        .iter()
        .take(limit)
        .map(|entry| {
            format!(
                "{}:{}:dist={:.8}:feeds={}",
                shorten_pubkey(&entry.obligation_pubkey),
                entry.repay_symbol,
                entry.distance_to_liq,
                entry.tracked_feed_ids.len()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn summarize_runtime_entries(runtime: &HermesShortlistRuntime, limit: usize) -> String {
    if runtime.entries.is_empty() {
        return "none".to_string();
    }
    let mut entries = runtime.entries.values().collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        a.distance_to_liq
            .partial_cmp(&b.distance_to_liq)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.obligation_pubkey.cmp(&b.obligation_pubkey))
    });
    entries
        .iter()
        .take(limit)
        .map(|entry| {
            format!(
                "{}:{}:{}:dist={:.8}:feeds={}:match={}",
                shorten_pubkey(&entry.obligation_pubkey),
                entry.repay_symbol,
                entry.state.as_str(),
                entry.distance_to_liq,
                entry.tracked_feed_ids.len(),
                entry.last_feed_match_count
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn summarize_runtime_states(runtime: &HermesShortlistRuntime) -> String {
    let mut warm = 0usize;
    let mut armed = 0usize;
    let mut cooling_down = 0usize;
    let mut dropped = 0usize;
    for entry in runtime.entries.values() {
        match entry.state {
            ShortlistState::Warm => warm += 1,
            ShortlistState::Armed => armed += 1,
            ShortlistState::CoolingDown => cooling_down += 1,
            ShortlistState::Dropped => dropped += 1,
        }
    }
    format!(
        "warm={} armed={} cooling_down={} dropped={}",
        warm, armed, cooling_down, dropped
    )
}

fn shorten_pubkey(pubkey: &str) -> String {
    if pubkey.len() <= 10 {
        return pubkey.to_string();
    }
    format!("{}..{}", &pubkey[..4], &pubkey[pubkey.len() - 4..])
}

pub async fn spawn_price_feed_signal_source<R, F, Fut>(
    rpc: R,
    wallet_tokens: Vec<WalletToken>,
    runtime: std::sync::Arc<tokio::sync::RwLock<HermesShortlistRuntime>>,
    config: HermesRuntimeConfig,
    global_fire_block_until_ms: Arc<AtomicU64>,
    emit_signal: F,
) -> Vec<JoinHandle<()>>
where
    R: RpcClient + Clone + Send + Sync + 'static,
    F: Fn(HermesSignalEvent) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    let refresh_handle = tokio::spawn({
        let rpc = rpc.clone();
        let wallet_tokens = wallet_tokens.clone();
        let runtime = runtime.clone();
        let config = config.clone();
        async move {
            loop {
                let refreshed_at_ms = now_ms();
                let obligation_disc = anchor_account_discriminator_base58("Obligation");
                let reserve_disc = anchor_account_discriminator_base58("Reserve");
                let has_debt = bs58::encode([1u8]).into_string();
                let obligation_accounts = rpc
                    .get_program_accounts_with_memcmp_filters(
                        KAMINO_PROGRAM_ID,
                        &[
                            (0, obligation_disc),
                            (KAMINO_OBLIGATION_HAS_DEBT_OFFSET, has_debt),
                        ],
                    )
                    .await;
                let reserve_accounts = rpc
                    .get_program_accounts_with_memcmp(KAMINO_PROGRAM_ID, 0, &reserve_disc)
                    .await;
                match (obligation_accounts, reserve_accounts) {
                    (Ok(mut obligation_accounts), Ok(mut reserve_accounts)) => {
                        log_stdout_at(RuntimeLogVerbosity::Medium, format!(
                            "[hunter-kamino] hermes shortlist refresh accounts obligations={} reserves={}",
                            obligation_accounts.len(),
                            reserve_accounts.len()
                        ));
                        reserve_accounts.append(&mut obligation_accounts);
                        let (fresh_entries, diagnostics) = build_hermes_shortlist_with_diagnostics(
                            &wallet_tokens,
                            reserve_accounts,
                            config.shortlist_size,
                            config.min_repay_usd,
                            refreshed_at_ms,
                        );
                        log_stdout_at(RuntimeLogVerbosity::Medium, format!(
                            "[hunter-kamino] hermes shortlist build fresh={} feeds={} {} top={}",
                            fresh_entries.len(),
                            unique_feed_count(&fresh_entries),
                            diagnostics.summarize(),
                            summarize_shortlist_entries(&fresh_entries, 3)
                        ));
                        let mut previous = runtime.read().await.clone();
                        let (fireable_entries, rejected) = filter_fireable_shortlist_entries(
                            &rpc,
                            &mut previous,
                            fresh_entries,
                            &config,
                            refreshed_at_ms,
                        )
                        .await;
                        if !rejected.is_empty() {
                            log_stdout_at(
                                RuntimeLogVerbosity::Low,
                                format!(
                                    "[hunter-kamino] hermes shortlist fireability rejected={} details={}",
                                    rejected.len(),
                                    rejected
                                        .iter()
                                        .take(5)
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(",")
                                ),
                            );
                        }
                        let merged = merge_shortlist_entries(
                            &previous,
                            fireable_entries,
                            &config,
                            refreshed_at_ms,
                        );
                        log_stdout_at(RuntimeLogVerbosity::Medium, format!(
                            "[hunter-kamino] hermes shortlist runtime active={} feeds={} states={} top={}",
                            merged.entries.len(),
                            unique_runtime_feed_count(&merged),
                            summarize_runtime_states(&merged),
                            summarize_runtime_entries(&merged, 3)
                        ));
                        *runtime.write().await = merged;
                    }
                    (Err(error), _) => {
                        log_stderr(format!(
                            "[hunter-kamino] hermes obligation shortlist refresh failed: {}",
                            error
                        ));
                    }
                    (_, Err(error)) => {
                        log_stderr(format!(
                            "[hunter-kamino] hermes reserve shortlist refresh failed: {}",
                            error
                        ));
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(config.refresh_secs)).await;
            }
        }
    });

    let stream_handle = tokio::spawn(async move {
        loop {
            let current = runtime.read().await.clone();
            if current.entries.is_empty() {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }

            let mut feed_ids = current
                .entries
                .values()
                .flat_map(|entry| entry.tracked_feed_ids.iter().cloned())
                .collect::<Vec<_>>();
            feed_ids.sort();
            feed_ids.dedup();

            let mut url = format!("{}/v2/updates/price/stream", config.ws_url);
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
            log_stdout_at(RuntimeLogVerbosity::Medium, format!(
                "[hunter-kamino] hermes stream connecting feeds={} url={}",
                feed_ids.len(),
                url
            ));
            match client.get(&url).send().await {
                Ok(resp) => {
                    log_stdout_at(RuntimeLogVerbosity::Medium, format!(
                        "[hunter-kamino] hermes stream connected status={} feeds={}",
                        resp.status(),
                        feed_ids.len()
                    ));
                    let mut stream = resp.bytes_stream();
                    let mut buffer = String::new();
                    while let Some(item) = stream.next().await {
                        let received_at_ms = now_ms();
                        let Ok(chunk) = item else {
                            log_stderr("[hunter-kamino] hermes stream chunk read failed");
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
                                    if global_fire_block_until_ms.load(Ordering::Relaxed)
                                        > received_at_ms
                                    {
                                        continue;
                                    }
                                    let signals = {
                                        let mut runtime = runtime.write().await;
                                        runtime.apply_changed_feeds(
                                            &changed,
                                            received_at_ms,
                                            &config,
                                        )
                                    };
                                    if !signals.is_empty() {
                                        log_stdout_at(RuntimeLogVerbosity::Low, format!(
                                            "[hunter-kamino] hermes stream matched changed_feeds={} signals={}",
                                            changed.len(),
                                            signals.len()
                                        ));
                                    }
                                    for signal in signals {
                                        if !emit_signal.clone()(signal).await {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    log_stderr(format!("[hunter-kamino] hermes stream error: {}", error));
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    });

    vec![refresh_handle, stream_handle]
}

fn reconcile_entry_state(
    entry: &mut HermesShortlistEntry,
    now_ms: u64,
    config: &HermesRuntimeConfig,
) {
    if entry.state == ShortlistState::Dropped {
        return;
    }
    if entry
        .cooldown_until_ms
        .is_some_and(|cooldown_until_ms| cooldown_until_ms > now_ms)
    {
        entry.state = ShortlistState::CoolingDown;
        return;
    }
    entry.cooldown_until_ms = None;

    if entry.last_price_signal_at_ms > 0
        && !is_entry_stale(entry, now_ms, config)
        && entry.distance_to_liq <= config.trigger_buffer_bps
    {
        entry.state = ShortlistState::Armed;
    } else {
        entry.state = ShortlistState::Warm;
    }
}

fn refresh_stale_grace_ms(config: &HermesRuntimeConfig) -> u64 {
    config
        .armed_stale_ms
        .max(config.refresh_secs.saturating_mul(2_000))
}

fn is_entry_stale(entry: &HermesShortlistEntry, now_ms: u64, config: &HermesRuntimeConfig) -> bool {
    now_ms.saturating_sub(entry.last_refresh_at_ms) > refresh_stale_grace_ms(config)
        || now_ms.saturating_sub(entry.last_price_signal_at_ms) > config.armed_stale_ms
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{
        build_hermes_shortlist_from_decoded, hermes_feed_id_from_pubkey, merge_shortlist_entries,
        parse_hermes_changed_feed_ids, HermesReserveInfo, HermesRuntimeConfig,
        HermesShortlistRuntime,
    };
    use crate::application::kamino_shortlist::ShortlistState;
    use crate::config::wallet::WalletToken;
    use crate::domain::kamino::Obligation;
    use solana_sdk::pubkey::Pubkey;
    use std::collections::HashMap;

    const ONE_USD_SF: u128 = 1_000_000_000_000_000_000;

    fn wallet_token(symbol: &str, mint: &str) -> WalletToken {
        WalletToken {
            symbol: symbol.to_string(),
            mint: mint.to_string(),
            decimals: 6,
            max_repay_native: 1_000_000_000_000,
        }
    }

    fn obligation_with_positions(
        borrow_reserve: [u8; 32],
        borrow_market_value_sf: u128,
        deposit_reserve: [u8; 32],
        deposit_market_value_sf: u128,
        unhealthy_delta_sf: u128,
    ) -> Obligation {
        let mut obligation = Obligation::default();
        obligation.has_debt = 1;
        obligation.deposited_value_sf = 1_000_000_000_000_000_000;
        obligation.borrow_factor_adjusted_debt_value_sf =
            700_000_000_000_000_000u128.saturating_sub(unhealthy_delta_sf);
        obligation.borrowed_assets_market_value_sf = borrow_market_value_sf;
        obligation.unhealthy_borrow_value_sf = 700_000_000_000_000_000;
        obligation.borrows[0].borrow_reserve = borrow_reserve;
        obligation.borrows[0].borrowed_amount_sf = 1;
        obligation.borrows[0].market_value_sf = borrow_market_value_sf;
        obligation.deposits[0].deposit_reserve = deposit_reserve;
        obligation.deposits[0].deposited_amount = 1;
        obligation.deposits[0].market_value_sf = deposit_market_value_sf;
        obligation
    }

    impl Default for Obligation {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }

    fn config() -> HermesRuntimeConfig {
        HermesRuntimeConfig {
            ws_url: "https://hermes.pyth.network".to_string(),
            refresh_secs: 20,
            shortlist_size: 10,
            min_repay_usd: 0.5,
            max_signals_per_batch: 1,
            trigger_buffer_bps: 0.0025,
            armed_stale_ms: 20_000,
            cooldown_ms: 20_000,
            execution_mode: super::HermesExecutionMode::Prepare,
            fire_enabled: true,
            fire_confirmation_window_ms: 120,
            fire_max_context_age_ms: 2_000,
            fire_cooldown_ms: 20_000,
            fire_min_feed_match_count: 1,
            fire_require_persistence: true,
            fireability_invalid_threshold: 3,
            fireability_invalid_cooldown_ms: 15 * 60 * 1_000,
            fireability_account_missing_cooldown_ms: 15 * 60 * 1_000,
        }
    }

    fn reserve_info(mint: String, pyth_feed_id: Option<String>) -> HermesReserveInfo {
        HermesReserveInfo {
            reserve_pubkey: Pubkey::new_unique().to_string(),
            mint,
            pyth_feed_id,
            lending_market: super::supported_hermes_lending_market(),
            market_price_sf: ONE_USD_SF,
            liquidity_supply: Pubkey::new_unique().to_string(),
            collateral_mint: Pubkey::new_unique().to_string(),
            collateral_supply: Pubkey::new_unique().to_string(),
            liquidity_fee_receiver: Pubkey::new_unique().to_string(),
        }
    }

    #[test]
    fn hermes_shortlist_filters_non_whitelisted_repay_assets() {
        let whitelisted_mint = Pubkey::new_unique();
        let non_whitelisted_mint = Pubkey::new_unique();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let blocked_reserve = Pubkey::new_unique().to_bytes();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(whitelisted_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            blocked_reserve,
            reserve_info(
                non_whitelisted_mint.to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(
                Pubkey::new_unique().to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );

        let allowed_obligation =
            obligation_with_positions(
                repay_reserve,
                ONE_USD_SF,
                deposit_reserve,
                ONE_USD_SF,
                1_000_000_000_000_000,
            );
        let blocked_obligation = obligation_with_positions(
            blocked_reserve,
            ONE_USD_SF,
            deposit_reserve,
            ONE_USD_SF,
            1_000_000_000_000_000,
        );

        let shortlist = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &whitelisted_mint.to_string())],
            vec![
                ("allowed".to_string(), allowed_obligation),
                ("blocked".to_string(), blocked_obligation),
            ],
            &reserve_infos,
            0.5,
            100,
        );

        assert_eq!(shortlist.len(), 1);
        assert_eq!(shortlist[0].obligation_pubkey, "allowed");
        assert_eq!(shortlist[0].repay_mint, whitelisted_mint.to_string());
    }

    #[test]
    fn hermes_changed_feed_parser_extracts_ids() {
        let ids = parse_hermes_changed_feed_ids(r#"{"parsed":[{"id":"0xabc"},{"id":"def"}]}"#);
        assert_eq!(ids, vec!["0xabc".to_string(), "0xdef".to_string()]);
    }

    #[test]
    fn hermes_matching_feed_under_buffer_emits_signal_and_arms() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let deposit_feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(repay_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), Some(deposit_feed.clone())),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "armed".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    ONE_USD_SF,
                    deposit_reserve,
                    ONE_USD_SF,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);

        let signals = runtime.apply_changed_feeds(&[feed], 120, &config());

        assert_eq!(signals.len(), 1);
        assert_eq!(
            runtime.shortlisted_entry("armed").unwrap().state,
            ShortlistState::Armed
        );
    }

    #[test]
    fn hermes_non_matching_or_out_of_buffer_does_not_emit() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(repay_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), None),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "warm".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    ONE_USD_SF,
                    deposit_reserve,
                    ONE_USD_SF,
                    50_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);

        assert!(runtime
            .apply_changed_feeds(&["0xdeadbeef".to_string()], 120, &config())
            .is_empty());
        assert!(runtime
            .apply_changed_feeds(&[feed], 140, &config())
            .is_empty());
        assert_eq!(
            runtime.shortlisted_entry("warm").unwrap().state,
            ShortlistState::Warm
        );
    }

    #[test]
    fn hermes_transitions_warm_to_armed_to_cooling_down() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(repay_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), None),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "cycle".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    ONE_USD_SF,
                    deposit_reserve,
                    ONE_USD_SF,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);

        runtime.apply_changed_feeds(&[feed], 120, &config());
        assert_eq!(
            runtime.shortlisted_entry("cycle").unwrap().state,
            ShortlistState::Armed
        );

        assert!(runtime.note_reactive_hit("cycle", 130, &config()));
        assert_eq!(
            runtime.shortlisted_entry("cycle").unwrap().state,
            ShortlistState::CoolingDown
        );
    }

    #[test]
    fn hermes_does_not_reemit_while_already_armed() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(repay_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), None),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "sticky".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    ONE_USD_SF,
                    deposit_reserve,
                    ONE_USD_SF,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);

        let first = runtime.apply_changed_feeds(std::slice::from_ref(&feed), 120, &config());
        assert_eq!(first.len(), 1);
        assert_eq!(
            runtime.shortlisted_entry("sticky").unwrap().state,
            ShortlistState::Armed
        );

        let second = runtime.apply_changed_feeds(&[feed], 140, &config());
        assert!(second.is_empty());
        let entry = runtime.shortlisted_entry("sticky").unwrap();
        assert_eq!(entry.state, ShortlistState::Armed);
        assert_eq!(entry.last_price_signal_at_ms, 140);
        assert_eq!(entry.last_feed_match_count, 1);
    }

    #[test]
    fn hermes_recent_price_signal_survives_single_refresh_gap() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(repay_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), None),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "gap".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    ONE_USD_SF,
                    deposit_reserve,
                    ONE_USD_SF,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);
        let config = config();

        let first = runtime.apply_changed_feeds(std::slice::from_ref(&feed), 120, &config);
        assert_eq!(first.len(), 1);

        let entry = runtime.entries.get_mut("gap").unwrap();
        entry.last_refresh_at_ms = 100;
        entry.last_price_signal_at_ms = 121;
        entry.state = ShortlistState::Armed;

        let second = runtime.apply_changed_feeds(&[feed], 126, &config);
        assert!(second.is_empty());
        assert_eq!(runtime.shortlisted_entry("gap").unwrap().state, ShortlistState::Armed);
    }

    #[test]
    fn hermes_limits_batch_to_closest_candidate() {
        let repay_reserve_a = Pubkey::new_unique().to_bytes();
        let repay_reserve_b = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint_a = Pubkey::new_unique();
        let repay_mint_b = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve_a,
            reserve_info(repay_mint_a.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            repay_reserve_b,
            reserve_info(repay_mint_b.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), None),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[
                wallet_token("USDC", &repay_mint_a.to_string()),
                wallet_token("USDT", &repay_mint_b.to_string()),
            ],
            vec![
                (
                    "closer".to_string(),
                    obligation_with_positions(
                        repay_reserve_a,
                        ONE_USD_SF,
                        deposit_reserve,
                        ONE_USD_SF,
                        1_000_000_000_000_000,
                    ),
                ),
                (
                    "farther".to_string(),
                    obligation_with_positions(
                        repay_reserve_b,
                        ONE_USD_SF,
                        deposit_reserve,
                        ONE_USD_SF,
                        2_000_000_000_000_000,
                    ),
                ),
            ],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);

        let signals = runtime.apply_changed_feeds(&[feed], 120, &config());
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].obligation_pubkey, "closer");
        assert_eq!(
            runtime.shortlisted_entry("closer").unwrap().last_signal_emitted_at_ms,
            120
        );
        assert_eq!(
            runtime.shortlisted_entry("farther").unwrap().last_signal_emitted_at_ms,
            0
        );
    }

    #[test]
    fn hermes_fire_keeps_entry_recoverable_after_cooldown() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(repay_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), None),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "recoverable".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    ONE_USD_SF,
                    deposit_reserve,
                    ONE_USD_SF,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);
        let config = config();

        runtime.apply_changed_feeds(&[feed], 120, &config);
        assert_eq!(
            runtime.shortlisted_entry("recoverable").unwrap().state,
            ShortlistState::Armed
        );

        assert!(runtime.note_hermes_fire("recoverable", 130, &config));
        let entry = runtime.shortlisted_entry("recoverable").unwrap();
        assert_eq!(entry.state, ShortlistState::CoolingDown);
        assert_eq!(entry.cooldown_until_ms, Some(130 + config.fire_cooldown_ms));

        let merged = merge_shortlist_entries(&runtime, vec![entry], &config, 140);
        assert_eq!(
            merged.shortlisted_entry("recoverable").unwrap().state,
            ShortlistState::CoolingDown
        );
    }

    #[test]
    fn hermes_invalid_history_drops_entry_from_fireable_pool() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let feed = hermes_feed_id_from_pubkey(Pubkey::new_unique());
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(repay_mint.to_string(), Some(feed.clone())),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(deposit_mint.to_string(), None),
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "invalid".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    ONE_USD_SF,
                    deposit_reserve,
                    ONE_USD_SF,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
            0.5,
            100,
        );
        let mut runtime =
            merge_shortlist_entries(&HermesShortlistRuntime::new(), entries, &config(), 100);
        let config = config();

        assert!(runtime.note_invalid_bundle("invalid", 120, &config));
        assert!(runtime.note_invalid_bundle("invalid", 140, &config));
        assert!(runtime.note_invalid_bundle("invalid", 160, &config));

        assert!(runtime
            .shortlist_block_reason("invalid", 200)
            .is_some_and(|reason| reason.contains("invalid_history")));
        assert_eq!(
            runtime.shortlisted_entry("invalid").unwrap().state,
            ShortlistState::Dropped
        );
    }

    #[test]
    fn hermes_shortlist_excludes_non_positive_distances() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(
                repay_mint.to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(
                deposit_mint.to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );

        let shortlist = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![
                (
                    "negative".to_string(),
                    obligation_with_positions(
                        repay_reserve,
                        1_000_000_000_000_000_000,
                        deposit_reserve,
                        1_000_000_000_000_000_000,
                        0,
                    ),
                ),
                (
                    "positive".to_string(),
                    obligation_with_positions(
                        repay_reserve,
                        1_000_000_000_000_000_000,
                        deposit_reserve,
                        1_000_000_000_000_000_000,
                        1_000_000_000_000_000,
                    ),
                ),
            ],
            &reserve_infos,
            0.5,
            100,
        );

        assert_eq!(shortlist.len(), 1);
        assert_eq!(shortlist[0].obligation_pubkey, "positive");
    }

    #[test]
    fn hermes_shortlist_excludes_micro_repay_positions() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(
                repay_mint.to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(
                deposit_mint.to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );

        let shortlist = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![
                (
                    "dust".to_string(),
                    obligation_with_positions(
                        repay_reserve,
                        10_000_000_000_000_000,
                        deposit_reserve,
                        1_000_000_000_000_000_000,
                        1_000_000_000_000_000,
                    ),
                ),
                (
                    "real".to_string(),
                    obligation_with_positions(
                        repay_reserve,
                        1_000_000_000_000_000_000,
                        deposit_reserve,
                        1_000_000_000_000_000_000,
                        1_000_000_000_000_000,
                    ),
                ),
            ],
            &reserve_infos,
            0.5,
            100,
        );

        assert_eq!(shortlist.len(), 1);
        assert_eq!(shortlist[0].obligation_pubkey, "real");
    }

    #[test]
    fn hermes_shortlist_excludes_positions_above_wallet_repay_cap() {
        let repay_reserve = Pubkey::new_unique().to_bytes();
        let deposit_reserve = Pubkey::new_unique().to_bytes();
        let repay_mint = Pubkey::new_unique();
        let deposit_mint = Pubkey::new_unique();

        let mut reserve_infos = HashMap::new();
        reserve_infos.insert(
            repay_reserve,
            reserve_info(
                repay_mint.to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );
        reserve_infos.insert(
            deposit_reserve,
            reserve_info(
                deposit_mint.to_string(),
                Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            ),
        );

        let mut too_large = obligation_with_positions(
            repay_reserve,
            3 * ONE_USD_SF,
            deposit_reserve,
            4 * ONE_USD_SF,
            1_000_000_000_000_000,
        );
        too_large.borrows[0].borrowed_amount_sf = ONE_USD_SF;

        let mut covered = obligation_with_positions(
            repay_reserve,
            ONE_USD_SF,
            deposit_reserve,
            2 * ONE_USD_SF,
            2_000_000_000_000_000,
        );
        covered.borrows[0].borrowed_amount_sf = 400_000_000_000_000_000;

        let shortlist = build_hermes_shortlist_from_decoded(
            &[WalletToken {
                symbol: "USDC".to_string(),
                mint: repay_mint.to_string(),
                decimals: 6,
                max_repay_native: 2_000_000,
            }],
            vec![
                ("too-large".to_string(), too_large),
                ("covered".to_string(), covered),
            ],
            &reserve_infos,
            0.5,
            100,
        );

        assert_eq!(shortlist.len(), 1);
        assert_eq!(shortlist[0].obligation_pubkey, "covered");
    }
}
