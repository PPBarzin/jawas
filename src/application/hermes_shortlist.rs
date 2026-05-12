use crate::application::kamino_shortlist::{PreparedExecutionContext, ShortlistState};
use crate::application::kamino_tx::{decode_kamino_reserve, optional_pubkey};
use crate::config::wallet::WalletToken;
use crate::domain::protocol::KAMINO_PROGRAM_ID;
use crate::ports::rpc::{ProgramAccount, RpcClient};
use crate::utils::log_stderr;
use borsh::BorshDeserialize;
use futures_util::StreamExt;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct HermesRuntimeConfig {
    pub ws_url: String,
    pub refresh_secs: u64,
    pub shortlist_size: usize,
    pub trigger_buffer_bps: f64,
    pub armed_stale_ms: u64,
    pub cooldown_ms: u64,
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
}

#[derive(Debug, Clone)]
pub struct HermesReserveInfo {
    pub reserve_pubkey: String,
    pub mint: String,
    pub pyth_feed_id: Option<String>,
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
            .unwrap_or(20);
        let shortlist_size = std::env::var("HERMES_SHORTLIST_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|v| v.clamp(1, 512))
            .unwrap_or(10);
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

        Self {
            ws_url,
            refresh_secs,
            shortlist_size,
            trigger_buffer_bps,
            armed_stale_ms,
            cooldown_ms,
        }
    }
}

impl HermesShortlistRuntime {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            last_refresh_completed_at_ms: None,
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
        if is_entry_stale(entry, now_ms, config.armed_stale_ms) {
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

    pub fn apply_changed_feeds(
        &mut self,
        changed: &[String],
        received_at_ms: u64,
        config: &HermesRuntimeConfig,
    ) -> Vec<HermesSignalEvent> {
        let changed_set = changed.iter().cloned().collect::<HashSet<_>>();
        let mut signals = Vec::new();

        for entry in self.entries.values_mut() {
            reconcile_entry_state(entry, received_at_ms, config);
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
                entry.state = ShortlistState::Armed;
                entry.inclusion_reason = format!(
                    "hermes_feed_match buffer_bps={} matched_feeds={}",
                    (config.trigger_buffer_bps * 10_000.0).round() as u64,
                    feed_match_count
                );
                signals.push(HermesSignalEvent {
                    obligation_pubkey: entry.obligation_pubkey.clone(),
                    repay_mint: entry.repay_mint.clone(),
                    repay_symbol: entry.repay_symbol.clone(),
                    feed_match_count,
                    signal_received_at_ms: received_at_ms,
                    detail: format!(
                        "hermes_feed_update distance_to_liq={:.8} matched_feeds={} refresh_at_ms={}",
                        entry.distance_to_liq, feed_match_count, entry.last_refresh_at_ms
                    ),
                });
            } else {
                entry.state = ShortlistState::Warm;
            }
        }

        signals
    }
}

pub fn decode_kamino_obligation(data: &[u8]) -> Option<crate::domain::kamino::Obligation> {
    if data.len() < 8 {
        return None;
    }
    let mut cursor = &data[8..];
    crate::domain::kamino::Obligation::deserialize(&mut cursor).ok()
}

pub fn hermes_feed_id_from_pubkey(pk: Pubkey) -> String {
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

pub fn build_hermes_shortlist(
    wallet_tokens: &[WalletToken],
    program_accounts: Vec<ProgramAccount>,
    shortlist_size: usize,
    refreshed_at_ms: u64,
) -> Vec<HermesShortlistEntry> {
    let mut reserve_infos: HashMap<[u8; 32], HermesReserveInfo> = HashMap::new();
    let mut obligations = Vec::new();
    for account in &program_accounts {
        if let Ok(reserve) = decode_kamino_reserve(&account.data) {
            let mint = Pubkey::new_from_array(reserve.liquidity.mint_pubkey).to_string();
            let pyth_feed_id = optional_pubkey(reserve.config.token_info.pyth_configuration.price)
                .map(hermes_feed_id_from_pubkey);
            if let Ok(reserve_pubkey) = Pubkey::from_str(&account.pubkey) {
                reserve_infos.insert(
                    reserve_pubkey.to_bytes(),
                    HermesReserveInfo {
                        reserve_pubkey: account.pubkey.clone(),
                        mint,
                        pyth_feed_id,
                    },
                );
            }
        }
        if let Some(obligation) = decode_kamino_obligation(&account.data) {
            obligations.push((account.pubkey.clone(), obligation));
        }
    }

    let mut entries = build_hermes_shortlist_from_decoded(
        wallet_tokens,
        obligations,
        &reserve_infos,
        refreshed_at_ms,
    );
    entries.truncate(shortlist_size);
    entries
}

pub fn build_hermes_shortlist_from_decoded(
    wallet_tokens: &[WalletToken],
    obligations: Vec<(String, crate::domain::kamino::Obligation)>,
    reserve_infos: &HashMap<[u8; 32], HermesReserveInfo>,
    refreshed_at_ms: u64,
) -> Vec<HermesShortlistEntry> {
    let whitelist: HashMap<String, &WalletToken> = wallet_tokens
        .iter()
        .map(|token| (token.mint.clone(), token))
        .collect();
    let mut shortlist = Vec::new();

    for (obligation_pubkey, obligation) in obligations {
        if obligation.has_debt == 0 || obligation.borrowed_assets_market_value_sf == 0 {
            continue;
        }

        let distance_to_liq = obligation.dist_to_liq();
        let mut tracked_feed_ids = Vec::new();
        let mut active_reserve_pubkeys = Vec::new();
        let mut repay_choice: Option<(String, String, String, u128)> = None;
        let mut withdraw_choice: Option<(String, String, u128)> = None;

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
                    reserve.reserve_pubkey.clone(),
                    reserve.mint.clone(),
                    deposit.market_value_sf,
                );
                if withdraw_choice
                    .as_ref()
                    .is_none_or(|(_, _, current)| candidate.2 > *current)
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
                    let candidate = (
                        reserve.reserve_pubkey.clone(),
                        reserve.mint.clone(),
                        token.symbol.clone(),
                        borrow.market_value_sf,
                    );
                    if repay_choice
                        .as_ref()
                        .is_none_or(|(_, _, _, current)| candidate.3 > *current)
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

        let Some((repay_reserve, repay_mint, repay_symbol, _)) = repay_choice else {
            continue;
        };
        let Some((withdraw_reserve, withdraw_mint, _)) = withdraw_choice else {
            continue;
        };

        let inclusion_reason = format!(
            "wallet_repay_eligible distance_to_liq={:.8}",
            distance_to_liq
        );
        shortlist.push(HermesShortlistEntry {
            obligation_pubkey: obligation_pubkey.clone(),
            repay_mint: repay_mint.clone(),
            repay_symbol: repay_symbol.clone(),
            tracked_feed_ids,
            distance_to_liq,
            last_price_signal_at_ms: 0,
            last_refresh_at_ms: refreshed_at_ms,
            state: ShortlistState::Warm,
            inclusion_reason: inclusion_reason.clone(),
            prepared_context: PreparedExecutionContext {
                obligation_pubkey,
                repay_mint,
                repay_symbol,
                wallet_eligible: true,
                repay_reserve,
                withdraw_reserve,
                withdraw_mint,
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
    }
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

pub async fn spawn_price_feed_signal_source<R, F, Fut>(
    rpc: R,
    wallet_tokens: Vec<WalletToken>,
    runtime: std::sync::Arc<tokio::sync::RwLock<HermesShortlistRuntime>>,
    config: HermesRuntimeConfig,
    emit_signal: F,
) where
    R: RpcClient + Clone + Send + Sync + 'static,
    F: Fn(HermesSignalEvent) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    tokio::spawn(async move {
        {
            let rpc = rpc.clone();
            let wallet_tokens = wallet_tokens.clone();
            let runtime = runtime.clone();
            let config = config.clone();
            tokio::spawn(async move {
                loop {
                    let refreshed_at_ms = now_ms();
                    match rpc.get_program_accounts(KAMINO_PROGRAM_ID).await {
                        Ok(accounts) => {
                            let fresh_entries = build_hermes_shortlist(
                                &wallet_tokens,
                                accounts,
                                config.shortlist_size,
                                refreshed_at_ms,
                            );
                            let previous = runtime.read().await.clone();
                            *runtime.write().await = merge_shortlist_entries(
                                &previous,
                                fresh_entries,
                                &config,
                                refreshed_at_ms,
                            );
                        }
                        Err(error) => {
                            log_stderr(format!(
                                "[hunter-kamino] hermes shortlist refresh failed: {}",
                                error
                            ));
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(config.refresh_secs)).await;
                }
            });
        }

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
            match client.get(&url).send().await {
                Ok(resp) => {
                    let mut stream = resp.bytes_stream();
                    let mut buffer = String::new();
                    while let Some(item) = stream.next().await {
                        let received_at_ms = now_ms();
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
                                    let signals = {
                                        let mut runtime = runtime.write().await;
                                        runtime.apply_changed_feeds(
                                            &changed,
                                            received_at_ms,
                                            &config,
                                        )
                                    };
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
}

fn reconcile_entry_state(
    entry: &mut HermesShortlistEntry,
    now_ms: u64,
    config: &HermesRuntimeConfig,
) {
    if entry
        .cooldown_until_ms
        .is_some_and(|cooldown_until_ms| cooldown_until_ms > now_ms)
    {
        entry.state = ShortlistState::CoolingDown;
        return;
    }
    entry.cooldown_until_ms = None;

    if entry.last_price_signal_at_ms > 0
        && !is_entry_stale(entry, now_ms, config.armed_stale_ms)
        && entry.distance_to_liq <= config.trigger_buffer_bps
    {
        entry.state = ShortlistState::Armed;
    } else {
        entry.state = ShortlistState::Warm;
    }
}

fn is_entry_stale(entry: &HermesShortlistEntry, now_ms: u64, armed_stale_ms: u64) -> bool {
    now_ms.saturating_sub(entry.last_refresh_at_ms) > armed_stale_ms
        || now_ms.saturating_sub(entry.last_price_signal_at_ms) > armed_stale_ms
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

    fn wallet_token(symbol: &str, mint: &str) -> WalletToken {
        WalletToken {
            symbol: symbol.to_string(),
            mint: mint.to_string(),
            decimals: 6,
            max_repay_native: 1,
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
            trigger_buffer_bps: 0.0025,
            armed_stale_ms: 20_000,
            cooldown_ms: 20_000,
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
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: whitelisted_mint.to_string(),
                pyth_feed_id: Some(feed.clone()),
            },
        );
        reserve_infos.insert(
            blocked_reserve,
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: non_whitelisted_mint.to_string(),
                pyth_feed_id: Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            },
        );
        reserve_infos.insert(
            deposit_reserve,
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: Pubkey::new_unique().to_string(),
                pyth_feed_id: Some(hermes_feed_id_from_pubkey(Pubkey::new_unique())),
            },
        );

        let allowed_obligation =
            obligation_with_positions(repay_reserve, 1, deposit_reserve, 1, 1_000_000_000_000_000);
        let blocked_obligation = obligation_with_positions(
            blocked_reserve,
            1,
            deposit_reserve,
            1,
            1_000_000_000_000_000,
        );

        let shortlist = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &whitelisted_mint.to_string())],
            vec![
                ("allowed".to_string(), allowed_obligation),
                ("blocked".to_string(), blocked_obligation),
            ],
            &reserve_infos,
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
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: repay_mint.to_string(),
                pyth_feed_id: Some(feed.clone()),
            },
        );
        reserve_infos.insert(
            deposit_reserve,
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: deposit_mint.to_string(),
                pyth_feed_id: Some(deposit_feed.clone()),
            },
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "armed".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    1,
                    deposit_reserve,
                    1,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
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
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: repay_mint.to_string(),
                pyth_feed_id: Some(feed.clone()),
            },
        );
        reserve_infos.insert(
            deposit_reserve,
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: deposit_mint.to_string(),
                pyth_feed_id: None,
            },
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "warm".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    1,
                    deposit_reserve,
                    1,
                    50_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
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
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: repay_mint.to_string(),
                pyth_feed_id: Some(feed.clone()),
            },
        );
        reserve_infos.insert(
            deposit_reserve,
            HermesReserveInfo {
                reserve_pubkey: Pubkey::new_unique().to_string(),
                mint: deposit_mint.to_string(),
                pyth_feed_id: None,
            },
        );

        let entries = build_hermes_shortlist_from_decoded(
            &[wallet_token("USDC", &repay_mint.to_string())],
            vec![(
                "cycle".to_string(),
                obligation_with_positions(
                    repay_reserve,
                    1,
                    deposit_reserve,
                    1,
                    1_000_000_000_000_000,
                ),
            )],
            &reserve_infos,
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
}
