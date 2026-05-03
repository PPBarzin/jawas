#![allow(dead_code)]

use anyhow::{Context, Result};
use borsh::BorshDeserialize;
use jawas::domain::{
    kamino::{BigFractionBytes, LastUpdate, Obligation as KaminoObligation},
    solend::decode_solend_obligation,
    token::token_info,
};
use jawas::utils::utc_now;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::str::FromStr;

const KLEND_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const SOLEND_PROGRAM: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const MAX_PLAUSIBLE_POSITION_USD: f64 = 10_000_000_000.0;

#[derive(Debug, Clone)]
struct CandidatePosition {
    protocol: &'static str,
    obligation: String,
    owner: String,
    repay_mint: String,
    repay_symbol: String,
    collateral_mint: String,
    collateral_symbol: String,
    pair: String,
    collateral_usd: f64,
    debt_usd: f64,
    current_ltv: f64,
    unhealthy_ltv: f64,
    dist_to_liq: f64,
}

#[derive(Debug, Clone)]
struct TokenRef {
    symbol: String,
    mint: String,
}

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

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ReserveCollateral {
    mint_pubkey: [u8; 32],
    mint_total_supply: u64,
    supply_vault: [u8; 32],
    padding1: [u128; 32],
    padding2: [u128; 32],
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ReserveFees {
    origination_fee_sf: u64,
    flash_loan_fee_sf: u64,
    padding: [u8; 8],
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct CurvePoint {
    utilization_rate_bps: u32,
    borrow_rate_bps: u32,
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct BorrowRateCurve {
    points: [CurvePoint; 11],
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct WithdrawalCaps {
    config_capacity: i64,
    current_total: i64,
    last_interval_start_timestamp: u64,
    config_interval_length_seconds: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct PriceHeuristic {
    lower: u64,
    upper: u64,
    exp: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ScopeConfiguration {
    price_feed: [u8; 32],
    price_chain: [u16; 4],
    twap_chain: [u16; 4],
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct SwitchboardConfiguration {
    price_aggregator: [u8; 32],
    twap_aggregator: [u8; 32],
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct PythConfiguration {
    price: [u8; 32],
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct TokenInfoConfig {
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
    token_info: TokenInfoConfig,
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

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct WithdrawQueue {
    queued_collateral_amount: u64,
    next_issued_ticket_sequence_number: u64,
    next_withdrawable_ticket_sequence_number: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct KaminoReserve {
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

fn today_date() -> String {
    utc_now()[0..10].to_string()
}

fn fmt_pct(v: f64) -> String {
    format!("{:.2}", v * 100.0)
}

fn decode_kamino_anchor<T: BorshDeserialize>(data: &[u8]) -> Option<T> {
    if data.len() < 8 {
        return None;
    }
    let mut cursor = &data[8..];
    T::deserialize(&mut cursor).ok()
}

fn anchor_account_discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("account:{name}");
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

fn is_default_pubkey(bytes: &[u8; 32]) -> bool {
    bytes.iter().all(|b| *b == 0)
}

fn symbol_or_fallback(key: &str) -> String {
    token_info(key)
        .map(|t| t.symbol.to_string())
        .unwrap_or_else(|| key.to_string())
}

fn get_kamino_reserve_token(
    rpc: &RpcClient,
    cache: &mut HashMap<String, TokenRef>,
    reserve_pk: &Pubkey,
) -> Option<TokenRef> {
    let key = reserve_pk.to_string();
    if let Some(token) = cache.get(&key) {
        return Some(token.clone());
    }

    let token = rpc
        .get_account(reserve_pk)
        .ok()
        .and_then(|acc| decode_kamino_anchor::<KaminoReserve>(&acc.data))
        .map(|reserve| Pubkey::new_from_array(reserve.liquidity.mint_pubkey))
        .map(|mint| mint.to_string())
        .map(|mint| TokenRef {
            symbol: symbol_or_fallback(&mint),
            mint,
        })?;

    cache.insert(key, token.clone());
    Some(token)
}

fn get_solend_reserve_token(
    rpc: &RpcClient,
    cache: &mut HashMap<String, TokenRef>,
    reserve_pk: &Pubkey,
) -> Option<TokenRef> {
    let key = reserve_pk.to_string();
    if let Some(token) = cache.get(&key) {
        return Some(token.clone());
    }

    let token = rpc
        .get_account(reserve_pk)
        .ok()
        .and_then(|acc| {
            if acc.data.len() < 74 {
                return None;
            }
            let mint_bytes: [u8; 32] = acc.data[42..74].try_into().ok()?;
            Some(Pubkey::new_from_array(mint_bytes))
        })
        .map(|mint| mint.to_string())
        .map(|mint| TokenRef {
            symbol: symbol_or_fallback(&mint),
            mint,
        })?;

    cache.insert(key, token.clone());
    Some(token)
}

fn top_kamino_collateral_and_borrow(
    obligation: &KaminoObligation,
    rpc: &RpcClient,
    reserve_cache: &mut HashMap<String, TokenRef>,
) -> Option<(TokenRef, TokenRef)> {
    let deposit = obligation
        .deposits
        .iter()
        .filter(|d| d.deposited_amount > 0 || d.market_value_sf > 0)
        .max_by_key(|d| d.market_value_sf)?;
    let borrow = obligation
        .borrows
        .iter()
        .filter(|b| b.borrowed_amount_sf > 0 || b.market_value_sf > 0)
        .max_by_key(|b| b.market_value_sf)?;

    let collateral = get_kamino_reserve_token(
        rpc,
        reserve_cache,
        &Pubkey::new_from_array(deposit.deposit_reserve),
    )?;
    let repay = get_kamino_reserve_token(
        rpc,
        reserve_cache,
        &Pubkey::new_from_array(borrow.borrow_reserve),
    )?;

    Some((repay, collateral))
}

fn is_plausible_kamino_position(obligation: &KaminoObligation) -> bool {
    if obligation.has_debt == 0
        || is_default_pubkey(&obligation.owner)
        || is_default_pubkey(&obligation.lending_market)
    {
        return false;
    }

    let collateral_usd = obligation.deposited_value_usd();
    let debt_usd = obligation.debt_value_usd();
    let max_ltv = obligation.max_ltv();
    let unhealthy_ltv = obligation.unhealthy_ltv();
    let current_ltv = obligation.current_ltv();

    collateral_usd.is_finite()
        && debt_usd.is_finite()
        && current_ltv.is_finite()
        && max_ltv.is_finite()
        && unhealthy_ltv.is_finite()
        && collateral_usd > 0.0
        && debt_usd > 0.0
        && collateral_usd < MAX_PLAUSIBLE_POSITION_USD
        && debt_usd < MAX_PLAUSIBLE_POSITION_USD
        && current_ltv >= 0.0
        && current_ltv <= 1.5
        && max_ltv > 0.0
        && max_ltv <= 1.5
        && unhealthy_ltv > 0.0
        && unhealthy_ltv <= 1.5
}

fn is_valid_kamino_obligation_account(data: &[u8]) -> bool {
    let disc = anchor_account_discriminator("Obligation");
    data.len() >= 8 && data[..8] == disc
}

fn scan_kamino_positions(
    rpc: &RpcClient,
    min_collateral_usd: f64,
) -> Result<Vec<CandidatePosition>> {
    let program = Pubkey::from_str(KLEND_PROGRAM)?;
    let accounts = rpc.get_program_accounts(&program)?;
    let mut out = Vec::new();
    let mut reserve_cache = HashMap::new();

    for (pubkey, account) in accounts {
        if !is_valid_kamino_obligation_account(&account.data) {
            continue;
        }

        let Some(obligation) = decode_kamino_anchor::<KaminoObligation>(&account.data) else {
            continue;
        };
        if !is_plausible_kamino_position(&obligation) {
            continue;
        }

        let collateral_usd = obligation.deposited_value_usd();
        let debt_usd = obligation.debt_value_usd();
        if collateral_usd < min_collateral_usd || debt_usd <= 0.0 {
            continue;
        }

        let current_ltv = obligation.current_ltv();
        let unhealthy_ltv = obligation.unhealthy_ltv();
        let dist_to_liq = obligation.dist_to_liq();
        let Some((repay_token, collateral_token)) =
            top_kamino_collateral_and_borrow(&obligation, rpc, &mut reserve_cache)
        else {
            continue;
        };

        out.push(CandidatePosition {
            protocol: "Kamino",
            obligation: pubkey.to_string(),
            owner: Pubkey::new_from_array(obligation.owner).to_string(),
            repay_mint: repay_token.mint.clone(),
            repay_symbol: repay_token.symbol.clone(),
            collateral_mint: collateral_token.mint.clone(),
            collateral_symbol: collateral_token.symbol.clone(),
            pair: format!("{} -> {}", repay_token.symbol, collateral_token.symbol),
            collateral_usd,
            debt_usd,
            current_ltv,
            unhealthy_ltv,
            dist_to_liq,
        });
    }

    Ok(out)
}

fn scan_solend_positions(
    rpc: &RpcClient,
    min_collateral_usd: f64,
) -> Result<Vec<CandidatePosition>> {
    let program = Pubkey::from_str(SOLEND_PROGRAM)?;
    let accounts = rpc.get_program_accounts(&program)?;
    let mut out = Vec::new();
    let mut reserve_cache = HashMap::new();

    for (pubkey, account) in accounts {
        let Some(obligation) = decode_solend_obligation(&account.data) else {
            continue;
        };

        let collateral_usd = obligation.deposited_value.to_f64();
        let debt_usd = obligation.borrowed_value.to_f64();
        if collateral_usd < min_collateral_usd
            || debt_usd <= 0.0
            || !collateral_usd.is_finite()
            || !debt_usd.is_finite()
            || collateral_usd >= MAX_PLAUSIBLE_POSITION_USD
            || debt_usd >= MAX_PLAUSIBLE_POSITION_USD
        {
            continue;
        }

        let Some(top_deposit) = obligation
            .deposits
            .iter()
            .max_by(|a, b| a.market_value.to_f64().partial_cmp(&b.market_value.to_f64()).unwrap_or(Ordering::Equal))
        else {
            continue;
        };
        let Some(top_borrow) = obligation
            .borrows
            .iter()
            .max_by(|a, b| a.market_value.to_f64().partial_cmp(&b.market_value.to_f64()).unwrap_or(Ordering::Equal))
        else {
            continue;
        };

        let Some(collateral_token) = get_solend_reserve_token(
            rpc,
            &mut reserve_cache,
            &Pubkey::new_from_array(top_deposit.deposit_reserve),
        ) else {
            continue;
        };
        let Some(repay_token) = get_solend_reserve_token(
            rpc,
            &mut reserve_cache,
            &Pubkey::new_from_array(top_borrow.borrow_reserve),
        ) else {
            continue;
        };
        let current_ltv = if collateral_usd > 0.0 {
            debt_usd / collateral_usd
        } else {
            f64::INFINITY
        };
        let unhealthy_ltv = if collateral_usd > 0.0 {
            obligation.unhealthy_borrow_value.to_f64() / collateral_usd
        } else {
            f64::INFINITY
        };
        if !current_ltv.is_finite()
            || !unhealthy_ltv.is_finite()
            || current_ltv < 0.0
            || current_ltv > 1.5
            || unhealthy_ltv <= 0.0
            || unhealthy_ltv > 1.5
        {
            continue;
        }
        let dist_to_liq = unhealthy_ltv - current_ltv;

        out.push(CandidatePosition {
            protocol: "Solend",
            obligation: pubkey.to_string(),
            owner: Pubkey::new_from_array(obligation.owner).to_string(),
            repay_mint: repay_token.mint.clone(),
            repay_symbol: repay_token.symbol.clone(),
            collateral_mint: collateral_token.mint.clone(),
            collateral_symbol: collateral_token.symbol.clone(),
            pair: format!("{} -> {}", repay_token.symbol, collateral_token.symbol),
            collateral_usd,
            debt_usd,
            current_ltv,
            unhealthy_ltv,
            dist_to_liq,
        });
    }

    Ok(out)
}

fn top_key(map: &HashMap<String, usize>) -> String {
    map.iter()
        .max_by_key(|(_, count)| *count)
        .map(|(key, _)| key.clone())
        .unwrap_or_default()
}

fn top_pairs_string(pair_counts: &HashMap<String, usize>, limit: usize) -> String {
    let mut pairs: Vec<_> = pair_counts.iter().collect();
    pairs.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    pairs.into_iter()
        .take(limit)
        .map(|(pair, count)| format!("{pair} x{count}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn repay_shortlist_string(
    repay_counts: &HashMap<String, (usize, String)>,
    total_positions: usize,
    limit: usize,
) -> String {
    let mut tokens: Vec<_> = repay_counts.iter().collect();
    tokens.sort_by(|a, b| {
        b.1.0.cmp(&a.1.0)
            .then_with(|| a.0.cmp(b.0))
    });

    tokens
        .into_iter()
        .take(limit)
        .map(|(_mint, (count, symbol))| {
            let share = if total_positions > 0 {
                (*count as f64 / total_positions as f64) * 100.0
            } else {
                0.0
            };
            format!("{symbol} {:.1}% ({count})", share)
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn repay_mint_shortlist_string(
    repay_counts: &HashMap<String, (usize, String)>,
    total_positions: usize,
    limit: usize,
) -> String {
    let mut tokens: Vec<_> = repay_counts.iter().collect();
    tokens.sort_by(|a, b| {
        b.1.0.cmp(&a.1.0)
            .then_with(|| a.0.cmp(b.0))
    });

    tokens
        .into_iter()
        .take(limit)
        .map(|(mint, (count, symbol))| {
            let share = if total_positions > 0 {
                (*count as f64 / total_positions as f64) * 100.0
            } else {
                0.0
            };
            format!("{symbol} [{mint}] {:.1}% ({count})", share)
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

async fn write_weekly_report(
    client: &Client,
    api_token: &str,
    base_id: &str,
    table_name: &str,
    fields: Value,
) -> Result<()> {
    let url = format!("https://api.airtable.com/v0/{}/{}", base_id, table_name);
    let body = json!({ "records": [{ "fields": fields }] });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to write weekly token report to Airtable")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Airtable weekly report error {}: {}", status, text);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = std::env::var("OBSERVER_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .context("OBSERVER_RPC_URL or RPC_URL must be set")?;
    let rpc = RpcClient::new(rpc_url);

    let api_token = std::env::var("AIRTABLE_TOKEN").context("AIRTABLE_TOKEN must be set")?;
    let base_id = std::env::var("AIRTABLE_BASE_ID").context("AIRTABLE_BASE_ID must be set")?;
    let weekly_table = std::env::var("AIRTABLE_TABLE_WEEKLY_TOKEN")
        .unwrap_or_else(|_| "jawas-weekly-token".to_string());
    let min_collateral_usd = std::env::var("WEEKLY_REPORT_MIN_COLLATERAL_USD")
        .ok()
        .or_else(|| std::env::var("WEEKLY_REPORT_MIN_BORROW").ok())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0);
    let top_n = std::env::var("WEEKLY_REPORT_TOP_N")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let shortlist_size = std::env::var("WEEKLY_REPORT_SHORTLIST_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(10);

    println!("📊 Génération du rapport hebdo on-chain...");
    println!("Table cible  : {}", weekly_table);
    println!("Filtre min collateral USD : {}", min_collateral_usd);
    println!("Positions analysées top N : {}", top_n);
    println!("Shortlist repay size      : {}", shortlist_size);

    let mut positions = Vec::new();
    positions.extend(scan_kamino_positions(&rpc, min_collateral_usd)?);
    positions.extend(scan_solend_positions(&rpc, min_collateral_usd)?);

    positions.sort_by(|a, b| {
        a.dist_to_liq
            .partial_cmp(&b.dist_to_liq)
            .unwrap_or(Ordering::Equal)
    });

    let shortlisted: Vec<CandidatePosition> = positions.into_iter().take(top_n).collect();

    if shortlisted.is_empty() {
        println!("Aucune obligation proche trouvée avec le filtre actuel.");
        write_weekly_report(
            &Client::new(),
            &api_token,
            &base_id,
            &weekly_table,
            json!({
                "Date": today_date(),
                "top_repay_token": "",
                "top_collateral_token": "",
                "Next_position": "N/A",
                "Frequence_paire": "Aucune obligation proche trouvée",
                "Shortlist": "",
            }),
        ).await?;
        return Ok(());
    }

    let mut repay_counts = HashMap::<String, (usize, String)>::new();
    let mut collateral_counts = HashMap::<String, usize>::new();
    let mut pair_counts = HashMap::<String, usize>::new();

    for pos in &shortlisted {
        let entry = repay_counts
            .entry(pos.repay_mint.clone())
            .or_insert((0, pos.repay_symbol.clone()));
        entry.0 += 1;
        *collateral_counts.entry(pos.collateral_symbol.clone()).or_insert(0) += 1;
        *pair_counts.entry(pos.pair.clone()).or_insert(0) += 1;
    }

    let top_repay = repay_counts
        .iter()
        .max_by_key(|(_, (count, _))| *count)
        .map(|(mint, (_, symbol))| format!("{symbol} [{mint}]"))
        .unwrap_or_default();
    let top_collateral = top_key(&collateral_counts);
    let next = &shortlisted[0];
    let next_position = format!(
        "{} | obligation={} | owner={} | {} | dist={} | ltv={}/{} | debt=${:.2} | collat=${:.2}",
        next.protocol,
        next.obligation,
        next.owner,
        next.pair,
        fmt_pct(next.dist_to_liq),
        fmt_pct(next.current_ltv),
        fmt_pct(next.unhealthy_ltv),
        next.debt_usd,
        next.collateral_usd
    );
    let pair_frequency = top_pairs_string(&pair_counts, 5);
    let shortlist = repay_mint_shortlist_string(&repay_counts, shortlisted.len(), shortlist_size);

    println!("Top repay token      : {}", top_repay);
    println!("Top collateral token : {}", top_collateral);
    println!("Next position        : {}", next_position);
    println!("Fréquence paire      : {}", pair_frequency);
    println!("Shortlist repay      : {}", shortlist);

    write_weekly_report(
        &Client::new(),
        &api_token,
        &base_id,
        &weekly_table,
        json!({
            "Date": today_date(),
            "top_repay_token": top_repay,
            "top_collateral_token": top_collateral,
            "Next_position": next_position,
            "Frequence_paire": pair_frequency,
            "Shortlist": shortlist,
        }),
    ).await?;

    println!("✅ Rapport hebdo on-chain inséré dans Airtable.");
    Ok(())
}
