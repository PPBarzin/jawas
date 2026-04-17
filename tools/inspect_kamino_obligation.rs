#![allow(dead_code)]

use anyhow::{Context, Result};
use borsh::BorshDeserialize;
use jawas::domain::kamino::{BigFractionBytes, LastUpdate, Obligation};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_client::rpc_client::RpcClient;
use solana_rpc_client_types::config::{
    RpcSimulateTransactionAccountsConfig, RpcSimulateTransactionConfig,
};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    message::Message,
    transaction::Transaction,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::str::FromStr;

const KLEND_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";

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

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct WithdrawQueue {
    queued_collateral_amount: u64,
    next_issued_ticket_sequence_number: u64,
    next_withdrawable_ticket_sequence_number: u64,
}

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

fn fmt_pct(value: f64) -> String {
    format!("{:.2}", value * 100.0)
}

fn fmt_usd(value: f64) -> String {
    format!("{value:.2}")
}

fn discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{name}");
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

fn is_default_pubkey(bytes: &[u8; 32]) -> bool {
    bytes.iter().all(|b| *b == 0)
}

fn decode_anchor_account<T: BorshDeserialize>(data: &[u8]) -> Result<T> {
    if data.len() < 8 {
        anyhow::bail!("Compte trop petit pour contenir un discriminator Anchor");
    }
    let mut cursor = &data[8..];
    T::deserialize(&mut cursor).map_err(|e| anyhow::anyhow!("Borsh decode failed: {e}"))
}

fn ix_refresh_reserve(
    klend: &Pubkey,
    market: &Pubkey,
    reserve_pk: &Pubkey,
    reserve: &Reserve,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*reserve_pk, false),
        AccountMeta::new_readonly(*market, false),
    ];

    let pyth = Pubkey::new_from_array(reserve.config.token_info.pyth_configuration.price);
    let sb_price =
        Pubkey::new_from_array(reserve.config.token_info.switchboard_configuration.price_aggregator);
    let sb_twap =
        Pubkey::new_from_array(reserve.config.token_info.switchboard_configuration.twap_aggregator);
    let scope = Pubkey::new_from_array(reserve.config.token_info.scope_configuration.price_feed);

    let has_any_optional = !is_default_pubkey(&reserve.config.token_info.pyth_configuration.price)
        || !is_default_pubkey(&reserve.config.token_info.switchboard_configuration.price_aggregator)
        || !is_default_pubkey(&reserve.config.token_info.switchboard_configuration.twap_aggregator)
        || !is_default_pubkey(&reserve.config.token_info.scope_configuration.price_feed);

    if has_any_optional {
        accounts.push(AccountMeta::new_readonly(pyth, false));
        accounts.push(AccountMeta::new_readonly(sb_price, false));
        accounts.push(AccountMeta::new_readonly(sb_twap, false));
        accounts.push(AccountMeta::new_readonly(scope, false));
    }

    Instruction {
        program_id: *klend,
        accounts,
        data: discriminator("refresh_reserve").to_vec(),
    }
}

fn ix_refresh_obligation(
    klend: &Pubkey,
    market: &Pubkey,
    obligation_pk: &Pubkey,
    reserve_pks: &[Pubkey],
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new_readonly(*market, false),
        AccountMeta::new(*obligation_pk, false),
    ];
    for reserve_pk in reserve_pks {
        accounts.push(AccountMeta::new_readonly(*reserve_pk, false));
    }
    Instruction {
        program_id: *klend,
        accounts,
        data: discriminator("refresh_obligation").to_vec(),
    }
}

fn describe_oracles(reserve: &Reserve) -> String {
    let mut parts = Vec::new();
    if !is_default_pubkey(&reserve.config.token_info.pyth_configuration.price) {
        parts.push(format!(
            "pyth={}",
            Pubkey::new_from_array(reserve.config.token_info.pyth_configuration.price)
        ));
    }
    if !is_default_pubkey(&reserve.config.token_info.switchboard_configuration.price_aggregator) {
        parts.push(format!(
            "sb_price={}",
            Pubkey::new_from_array(reserve.config.token_info.switchboard_configuration.price_aggregator)
        ));
    }
    if !is_default_pubkey(&reserve.config.token_info.switchboard_configuration.twap_aggregator) {
        parts.push(format!(
            "sb_twap={}",
            Pubkey::new_from_array(reserve.config.token_info.switchboard_configuration.twap_aggregator)
        ));
    }
    if !is_default_pubkey(&reserve.config.token_info.scope_configuration.price_feed) {
        parts.push(format!(
            "scope={}",
            Pubkey::new_from_array(reserve.config.token_info.scope_configuration.price_feed)
        ));
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(" ")
    }
}

fn print_metrics(label: &str, obligation: &Obligation) {
    println!("\n--- {label} ---");
    println!("Owner                : {}", Pubkey::new_from_array(obligation.owner));
    println!("Collateral USD       : {}", fmt_usd(obligation.deposited_value_usd()));
    println!("Debt USD             : {}", fmt_usd(obligation.debt_value_usd()));
    println!("Net Value USD        : {}", fmt_usd(obligation.net_value_usd()));
    println!("Current LTV %        : {}", fmt_pct(obligation.current_ltv()));
    println!("Max LTV %            : {}", fmt_pct(obligation.max_ltv()));
    println!("Unhealthy LTV %      : {}", fmt_pct(obligation.unhealthy_ltv()));
    println!("Dist To Liq          : {}", fmt_pct(obligation.dist_to_liq()));
    println!("Liquidatable         : {}", obligation.is_liquidatable());
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: cargo run --bin inspect_kamino_obligation <OBLIGATION_PUBKEY>");
        return Ok(());
    }

    let pk_str = &args[1];
    let rpc_url = std::env::var("OBSERVER_RPC_URL").context("OBSERVER_RPC_URL not set")?;
    let rpc = RpcClient::new(rpc_url);

    println!("🔍 Inspection de l'obligation Kamino : {pk_str}");

    let pubkey = Pubkey::from_str(pk_str)?;
    let account = rpc.get_account(&pubkey)?;
    println!("Taille du compte : {} bytes", account.data.len());

    let obligation = decode_anchor_account::<Obligation>(&account.data)?;
    print_metrics("Snapshot RPC brut", &obligation);

    let mut reserve_pks = BTreeSet::new();
    for deposit in &obligation.deposits {
        if deposit.deposited_amount > 0 || deposit.market_value_sf > 0 {
            reserve_pks.insert(Pubkey::new_from_array(deposit.deposit_reserve));
        }
    }
    for borrow in &obligation.borrows {
        if borrow.borrowed_amount_sf > 0 || borrow.market_value_sf > 0 {
            reserve_pks.insert(Pubkey::new_from_array(borrow.borrow_reserve));
        }
    }

    let klend = Pubkey::from_str(KLEND_PROGRAM)?;
    let market = Pubkey::new_from_array(obligation.lending_market);
    let reserve_pks: Vec<Pubkey> = reserve_pks.into_iter().collect();

    println!("\n--- Réserves à refresh ---");
    let mut refresh_ixs = Vec::new();
    for reserve_pk in &reserve_pks {
        let reserve_account = rpc.get_account(reserve_pk)?;
        let reserve = decode_anchor_account::<Reserve>(&reserve_account.data)?;
        println!(
            "reserve={} ltv={} liq_threshold={} price_usd={} oracles=[{}]",
            reserve_pk,
            reserve.config.loan_to_value_pct,
            reserve.config.liquidation_threshold_pct,
            fmt_usd(Obligation::sf_to_f64(reserve.liquidity.market_price_sf)),
            describe_oracles(&reserve)
        );
        refresh_ixs.push(ix_refresh_reserve(&klend, &market, reserve_pk, &reserve));
    }
    refresh_ixs.push(ix_refresh_obligation(&klend, &market, &pubkey, &reserve_pks));

    let message = Message::new(&refresh_ixs, None);
    let tx = Transaction::new_unsigned(message);

    let sim = rpc.simulate_transaction_with_config(
        &tx,
        RpcSimulateTransactionConfig {
            sig_verify: false,
            replace_recent_blockhash: true,
            accounts: Some(RpcSimulateTransactionAccountsConfig {
                encoding: Some(UiAccountEncoding::Base64),
                addresses: vec![pubkey.to_string()],
            }),
            ..RpcSimulateTransactionConfig::default()
        },
    )?;

    if let Some(err) = &sim.value.err {
        println!("\n--- Simulation Refresh ---");
        println!("Échec simulation: {err:?}");
        if let Some(logs) = &sim.value.logs {
            for log in logs {
                println!("  {log}");
            }
        }
    } else if let Some(accounts) = &sim.value.accounts {
        if let Some(Some(ui_account)) = accounts.first() {
            let refreshed_data = ui_account
                .data
                .decode()
                .context("Impossible de décoder l'account renvoyé par la simulation")?;
            let refreshed = decode_anchor_account::<Obligation>(&refreshed_data)?;
            print_metrics("Après refresh simulé", &refreshed);
        }
    }

    println!("\n--- Dépôts ---");
    for (idx, deposit) in obligation.deposits.iter().enumerate() {
        if deposit.deposited_amount == 0 && deposit.market_value_sf == 0 {
            continue;
        }
        let reserve = Pubkey::new_from_array(deposit.deposit_reserve);
        println!(
            "#{idx} reserve={} deposited_amount={} market_value_usd={}",
            reserve,
            deposit.deposited_amount,
            fmt_usd(Obligation::sf_to_f64(deposit.market_value_sf))
        );
    }

    println!("\n--- Emprunts ---");
    for (idx, borrow) in obligation.borrows.iter().enumerate() {
        if borrow.borrowed_amount_sf == 0 && borrow.market_value_sf == 0 {
            continue;
        }
        let reserve = Pubkey::new_from_array(borrow.borrow_reserve);
        println!(
            "#{idx} reserve={} borrowed_amount_sf={} market_value_usd={} adjusted_market_value_usd={}",
            reserve,
            borrow.borrowed_amount_sf,
            fmt_usd(Obligation::sf_to_f64(borrow.market_value_sf)),
            fmt_usd(Obligation::sf_to_f64(
                borrow.borrow_factor_adjusted_market_value_sf
            ))
        );
    }

    Ok(())
}
