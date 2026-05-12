use anyhow::{bail, Context, Result};
use borsh::BorshDeserialize;
use jawas::config::wallet::{load_wallet_tokens, WalletToken};
use jawas::domain::protocol::KAMINO_PROGRAM_ID;
use jawas::domain::{kamino::Obligation, token::token_info};
use jawas::infrastructure::jito::JitoAdapter;
use jawas::ports::jito::JitoPort;
use sha2::{Digest, Sha256};
use solana_client::{
    rpc_client::RpcClient, rpc_config::RpcSendTransactionConfig,
    rpc_response::RpcSimulateTransactionResult,
};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    message::{v0::Message, VersionedMessage},
    pubkey::Pubkey,
    signature::{read_keypair_file, Signature, Signer},
    sysvar,
    transaction::VersionedTransaction,
};
use std::{str::FromStr, time::Duration};

const LENDING_MARKET: &str = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
const MARKET_AUTHORITY: &str = "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo";
const ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const DEFAULT_TIP_ACCOUNT: &str = "96g9sAg9u3P7Q9ebKsC6SA47cySvnV6S1S1K6ssB1vD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendMode {
    Simulate,
    Rpc,
    Jito,
    RpcAndJito,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeStatus {
    StoppedHealthyBeforeSend,
    StoppedMissingWalletCoverage,
    SimulationFailed,
    RpcSendFailedBeforeSignature,
    RpcSignatureObtainedNotConfirmed,
    RpcConfirmed,
    JitoBundleRejectedApi,
    JitoBundleAcceptedApi,
}

impl SendMode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "simulate" => Ok(Self::Simulate),
            "rpc" => Ok(Self::Rpc),
            "jito" => Ok(Self::Jito),
            "rpc-and-jito" => Ok(Self::RpcAndJito),
            _ => bail!("unsupported mode '{value}'"),
        }
    }
}

#[derive(Debug, Clone)]
struct Cli {
    obligation: Pubkey,
    mode: SendMode,
    repay_native: u64,
    tip_lamports: u64,
    cu_limit: u32,
    cu_price: u64,
    tip_account: Pubkey,
    skip_preflight: bool,
}

#[derive(Debug, Clone)]
struct ReserveInfo {
    address: Pubkey,
    liquidity_mint: Pubkey,
    liquidity_supply: Pubkey,
    collateral_mint: Pubkey,
    collateral_supply: Pubkey,
    liquidity_fee_receiver: Pubkey,
    token_program: Pubkey,
    lending_market: Pubkey,
    pyth_oracle: Option<Pubkey>,
    switchboard_price_oracle: Option<Pubkey>,
    switchboard_twap_oracle: Option<Pubkey>,
    scope_prices: Option<Pubkey>,
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct BigFractionBytes {
    value: [u64; 4],
    padding: [u64; 2],
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct LastUpdate {
    slot: u64,
    stale: u8,
    price_status: u8,
    placeholder: [u8; 6],
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
struct WithdrawQueue {
    queued_collateral_amount: u64,
    next_issued_ticket_sequence_number: u64,
    next_withdrawable_ticket_sequence_number: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize)]
struct ReserveAccount {
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

#[derive(Debug, Clone)]
struct TransactionPlan {
    tx: VersionedTransaction,
    repay_token_symbol: String,
    repay_token_mint: String,
    wallet_max_repay_native: u64,
    effective_repay_native: u64,
}

fn discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{name}");
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

fn get_ata(wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    let ata_program = Pubkey::from_str(ATA_PROGRAM).expect("static constant ATA_PROGRAM");
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    )
    .0
}

fn build_create_ata_idempotent_ix(
    funding_address: &Pubkey,
    wallet_address: &Pubkey,
    token_mint_address: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    let ata_address = get_ata(wallet_address, token_mint_address, token_program);
    Instruction {
        program_id: Pubkey::from_str(ATA_PROGRAM).expect("static constant ATA_PROGRAM"),
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

fn optional_pubkey(bytes: [u8; 32]) -> Option<Pubkey> {
    if bytes.iter().all(|b| *b == 0) {
        None
    } else {
        Some(Pubkey::new_from_array(bytes))
    }
}

fn parse_cli() -> Result<Cli> {
    let mut args = std::env::args().skip(1);
    let obligation = match args.next() {
        Some(value) if !value.starts_with("--") => Pubkey::from_str(&value)
            .with_context(|| format!("invalid obligation pubkey '{value}'"))?,
        _ => {
            print_usage();
            bail!("missing obligation pubkey");
        }
    };

    let mut mode = SendMode::Simulate;
    let mut repay_native = u64::MAX;
    let mut tip_lamports = 100_000_u64;
    let mut cu_limit = std::env::var("KAMINO_COMPUTE_UNIT_LIMIT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(400_000);
    let mut cu_price = std::env::var("KAMINO_CU_PRICE_MICROLAMPORTS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5_000);
    let mut tip_account = Pubkey::from_str(DEFAULT_TIP_ACCOUNT)?;
    let mut skip_preflight = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" | "--simulate-only" => mode = SendMode::Simulate,
            "--send-rpc" => mode = SendMode::Rpc,
            "--send-jito" => mode = SendMode::Jito,
            "--mode" => {
                let value = args.next().context("missing value for --mode")?;
                mode = SendMode::parse(&value)?;
            }
            "--repay-native" => {
                let value = args.next().context("missing value for --repay-native")?;
                repay_native = value
                    .parse::<u64>()
                    .with_context(|| format!("invalid --repay-native value '{value}'"))?;
            }
            "--tip-lamports" => {
                let value = args.next().context("missing value for --tip-lamports")?;
                tip_lamports = value
                    .parse::<u64>()
                    .with_context(|| format!("invalid --tip-lamports value '{value}'"))?;
            }
            "--cu-limit" => {
                let value = args.next().context("missing value for --cu-limit")?;
                cu_limit = value
                    .parse::<u32>()
                    .with_context(|| format!("invalid --cu-limit value '{value}'"))?;
            }
            "--cu-price" => {
                let value = args.next().context("missing value for --cu-price")?;
                cu_price = value
                    .parse::<u64>()
                    .with_context(|| format!("invalid --cu-price value '{value}'"))?;
            }
            "--tip-account" => {
                let value = args.next().context("missing value for --tip-account")?;
                tip_account = Pubkey::from_str(&value)
                    .with_context(|| format!("invalid --tip-account value '{value}'"))?;
            }
            "--skip-preflight" => skip_preflight = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown argument '{arg}'"),
        }
    }

    Ok(Cli {
        obligation,
        mode,
        repay_native,
        tip_lamports,
        cu_limit,
        cu_price,
        tip_account,
        skip_preflight,
    })
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run --bin liquidate_one <OBLIGATION_PK> [options]

Options:
  --mode <simulate|rpc|jito|rpc-and-jito>  Send mode (default: simulate)
  --simulate-only                           Alias for --mode simulate
  --send-rpc                                Alias for --mode rpc
  --send-jito                               Alias for --mode jito
  --repay-native <u64>                      Liquidity amount to repay (default: u64::MAX)
  --tip-lamports <u64>                      Jito tip amount (default: 100000)
  --tip-account <pubkey>                    Tip account (default: {DEFAULT_TIP_ACCOUNT})
  --cu-limit <u32>                          Compute unit limit
  --cu-price <u64>                          Compute unit price in microlamports
  --skip-preflight                          Skip preflight when sending through RPC
  --dry-run                                 Backwards-compatible alias for simulate"
    );
}

fn active_reserve_pubkeys_in_refresh_order(obligation: &Obligation) -> Vec<Pubkey> {
    let mut reserve_pks = Vec::new();

    for deposit in obligation.deposits.iter() {
        if deposit.deposited_amount > 0 || deposit.market_value_sf > 0 {
            let reserve_pk = Pubkey::new_from_array(deposit.deposit_reserve);
            if !reserve_pks.contains(&reserve_pk) {
                reserve_pks.push(reserve_pk);
            }
        }
    }

    for borrow in obligation.borrows.iter() {
        if borrow.borrowed_amount_sf > 0 || borrow.market_value_sf > 0 {
            let reserve_pk = Pubkey::new_from_array(borrow.borrow_reserve);
            if !reserve_pks.contains(&reserve_pk) {
                reserve_pks.push(reserve_pk);
            }
        }
    }

    reserve_pks
}

fn fetch_reserve_metadata(rpc: &RpcClient, reserve_pk: &Pubkey) -> Result<ReserveInfo> {
    let data = rpc.get_account(reserve_pk)?.data;
    let mut cursor = data
        .get(8..)
        .context("reserve account too small for Anchor discriminator")?;
    let reserve = ReserveAccount::deserialize(&mut cursor)
        .map_err(|error| anyhow::anyhow!("reserve decode failed: {error}"))?;

    Ok(ReserveInfo {
        address: *reserve_pk,
        liquidity_mint: Pubkey::new_from_array(reserve.liquidity.mint_pubkey),
        liquidity_supply: Pubkey::new_from_array(reserve.liquidity.supply_vault),
        collateral_mint: Pubkey::new_from_array(reserve.collateral.mint_pubkey),
        collateral_supply: Pubkey::new_from_array(reserve.collateral.supply_vault),
        liquidity_fee_receiver: Pubkey::new_from_array(reserve.liquidity.fee_vault),
        token_program: Pubkey::new_from_array(reserve.liquidity.token_program),
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
    })
}

fn print_status(status: ProbeStatus) {
    println!("status: {:?}", status);
}

fn build_refresh_reserve_ix(reserve: &ReserveInfo, program_pk: &Pubkey) -> Instruction {
    let placeholder = *program_pk;
    let accounts = vec![
        AccountMeta::new(reserve.address, false),
        AccountMeta::new_readonly(reserve.lending_market, false),
        AccountMeta::new_readonly(reserve.pyth_oracle.unwrap_or(placeholder), false),
        AccountMeta::new_readonly(
            reserve.switchboard_price_oracle.unwrap_or(placeholder),
            false,
        ),
        AccountMeta::new_readonly(
            reserve.switchboard_twap_oracle.unwrap_or(placeholder),
            false,
        ),
        AccountMeta::new_readonly(reserve.scope_prices.unwrap_or(placeholder), false),
    ];
    Instruction {
        program_id: *program_pk,
        accounts,
        data: discriminator("refresh_reserve").to_vec(),
    }
}

fn build_refresh_obligation_ix(
    obligation_pk: &Pubkey,
    market_pk: &Pubkey,
    reserve_pks: &[Pubkey],
    program_pk: &Pubkey,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new_readonly(*market_pk, false),
        AccountMeta::new(*obligation_pk, false),
    ];
    for reserve_pk in reserve_pks {
        accounts.push(AccountMeta::new_readonly(*reserve_pk, false));
    }
    Instruction {
        program_id: *program_pk,
        accounts,
        data: discriminator("refresh_obligation").to_vec(),
    }
}

fn build_transaction(
    cli: &Cli,
    obligation: &Obligation,
    liquidator: &Pubkey,
    blockhash: solana_sdk::hash::Hash,
    keypair: &solana_sdk::signature::Keypair,
    rpc: &RpcClient,
    wallet_tokens: &[WalletToken],
) -> Result<TransactionPlan> {
    let deposit = obligation
        .deposits
        .iter()
        .find(|deposit| deposit.deposited_amount > 0)
        .context("no active deposit found in obligation")?;
    let withdraw_reserve_pk = Pubkey::new_from_array(deposit.deposit_reserve);
    let obligation_reserve_pks = active_reserve_pubkeys_in_refresh_order(obligation);
    let withdraw_info = fetch_reserve_metadata(rpc, &withdraw_reserve_pk)?;
    let obligation_reserve_infos = obligation_reserve_pks
        .iter()
        .map(|reserve_pk| fetch_reserve_metadata(rpc, reserve_pk))
        .collect::<Result<Vec<_>>>()?;
    let mut covered_borrows = Vec::new();
    let mut uncovered_mints = Vec::new();
    for borrow in obligation
        .borrows
        .iter()
        .filter(|borrow| borrow.borrowed_amount_sf > 0 || borrow.market_value_sf > 0)
    {
        let repay_reserve_pk = Pubkey::new_from_array(borrow.borrow_reserve);
        let repay_info = fetch_reserve_metadata(rpc, &repay_reserve_pk)?;
        let repay_mint = repay_info.liquidity_mint.to_string();
        let Some(wallet_token) = wallet_tokens.iter().find(|token| token.mint == repay_mint) else {
            uncovered_mints.push(repay_mint);
            continue;
        };
        if wallet_token.max_repay_native == 0 {
            uncovered_mints.push(format!(
                "{} ({}) configured with max_repay_native=0",
                wallet_token.symbol, wallet_token.mint
            ));
            continue;
        }
        covered_borrows.push((borrow, repay_info, wallet_token));
    }
    let Some((_borrow, repay_info, wallet_token)) = covered_borrows
        .into_iter()
        .max_by_key(|(borrow, _, _)| borrow.market_value_sf.max(borrow.borrowed_amount_sf))
    else {
        if uncovered_mints.is_empty() {
            bail!("no active borrow found in obligation");
        }
        bail!(
            "no active borrow is covered by wallet.toml; observed repay mints: {}",
            uncovered_mints.join(", ")
        );
    };

    let effective_repay_native = if cli.repay_native == u64::MAX {
        wallet_token.max_repay_native
    } else {
        cli.repay_native.min(wallet_token.max_repay_native)
    };

    let klend_pk = Pubkey::from_str(KAMINO_PROGRAM_ID)?;
    let market_pk = Pubkey::from_str(LENDING_MARKET)?;
    let market_authority_pk = Pubkey::from_str(MARKET_AUTHORITY)?;
    let mut instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(cli.cu_limit),
        ComputeBudgetInstruction::set_compute_unit_price(cli.cu_price),
    ];
    for reserve_info in &obligation_reserve_infos {
        instructions.push(build_refresh_reserve_ix(reserve_info, &klend_pk));
    }
    instructions.push(build_refresh_obligation_ix(
        &cli.obligation,
        &market_pk,
        &obligation_reserve_pks,
        &klend_pk,
    ));

    let user_src = get_ata(
        liquidator,
        &repay_info.liquidity_mint,
        &repay_info.token_program,
    );
    let user_dst_col = get_ata(
        liquidator,
        &withdraw_info.collateral_mint,
        &withdraw_info.token_program,
    );
    let user_dst_liq = get_ata(
        liquidator,
        &withdraw_info.liquidity_mint,
        &withdraw_info.token_program,
    );

    instructions.push(build_create_ata_idempotent_ix(
        liquidator,
        liquidator,
        &withdraw_info.collateral_mint,
        &withdraw_info.token_program,
    ));
    instructions.push(build_create_ata_idempotent_ix(
        liquidator,
        liquidator,
        &withdraw_info.liquidity_mint,
        &withdraw_info.token_program,
    ));

    let mut liquidation_data =
        discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2").to_vec();
    liquidation_data.extend_from_slice(&effective_repay_native.to_le_bytes());
    liquidation_data.extend_from_slice(&0_u64.to_le_bytes());
    liquidation_data.extend_from_slice(&0_u64.to_le_bytes());

    instructions.push(Instruction {
        program_id: klend_pk,
        accounts: vec![
            AccountMeta::new_readonly(*liquidator, true),
            AccountMeta::new(cli.obligation, false),
            AccountMeta::new_readonly(market_pk, false),
            AccountMeta::new_readonly(market_authority_pk, false),
            AccountMeta::new(repay_info.address, false),
            AccountMeta::new_readonly(repay_info.liquidity_mint, false),
            AccountMeta::new(repay_info.liquidity_supply, false),
            AccountMeta::new(withdraw_info.address, false),
            AccountMeta::new_readonly(withdraw_info.liquidity_mint, false),
            AccountMeta::new(withdraw_info.collateral_mint, false),
            AccountMeta::new(withdraw_info.collateral_supply, false),
            AccountMeta::new(withdraw_info.liquidity_supply, false),
            AccountMeta::new(withdraw_info.liquidity_fee_receiver, false),
            AccountMeta::new(user_src, false),
            AccountMeta::new(user_dst_col, false),
            AccountMeta::new(user_dst_liq, false),
            AccountMeta::new_readonly(withdraw_info.token_program, false),
            AccountMeta::new_readonly(repay_info.token_program, false),
            AccountMeta::new_readonly(withdraw_info.token_program, false),
            AccountMeta::new_readonly(sysvar::instructions::id(), false),
            AccountMeta::new(klend_pk, false),
            AccountMeta::new(klend_pk, false),
            AccountMeta::new(klend_pk, false),
            AccountMeta::new(klend_pk, false),
            AccountMeta::new_readonly(
                Pubkey::from_str("FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr")?,
                false,
            ),
        ],
        data: liquidation_data,
    });

    instructions.push(solana_sdk::system_instruction::transfer(
        liquidator,
        &cli.tip_account,
        cli.tip_lamports,
    ));

    let message = Message::try_compile(liquidator, &instructions, &[], blockhash)
        .context("failed to compile versioned message")?;
    let transaction = VersionedTransaction::try_new(VersionedMessage::V0(message), &[keypair])
        .context("failed to sign versioned transaction")?;

    Ok(TransactionPlan {
        tx: transaction,
        repay_token_symbol: wallet_token.symbol.clone(),
        repay_token_mint: wallet_token.mint.clone(),
        wallet_max_repay_native: wallet_token.max_repay_native,
        effective_repay_native,
    })
}

fn print_obligation_summary(obligation: &Obligation) {
    println!(
        "  owner             : {}",
        Pubkey::new_from_array(obligation.owner)
    );
    println!("Snapshot:");
    println!(
        "  collateral_usd    : {:.6}",
        obligation.deposited_value_usd()
    );
    println!("  debt_usd          : {:.6}", obligation.debt_value_usd());
    println!("  current_ltv       : {:.6}", obligation.current_ltv());
    println!("  unhealthy_ltv     : {:.6}", obligation.unhealthy_ltv());
    println!("  distance_to_liq   : {:.6}", obligation.dist_to_liq());
    println!("  is_liquidatable   : {}", obligation.is_liquidatable());
}

fn print_simulation(sim: &RpcSimulateTransactionResult) {
    println!("Simulation:");
    println!("  units_consumed    : {:?}", sim.units_consumed);
    println!("  replacement_hash  : {:?}", sim.replacement_blockhash);
    println!("  err               : {:?}", sim.err);
    if let Some(logs) = &sim.logs {
        println!("  logs:");
        for log in logs {
            println!("    {log}");
        }
    }
}

fn send_via_rpc(
    rpc: &RpcClient,
    tx: &VersionedTransaction,
    skip_preflight: bool,
) -> Result<Signature> {
    let signature = match rpc.send_transaction_with_config(
        tx,
        RpcSendTransactionConfig {
            skip_preflight,
            ..RpcSendTransactionConfig::default()
        },
    ) {
        Ok(signature) => signature,
        Err(error) => {
            print_status(ProbeStatus::RpcSendFailedBeforeSignature);
            return Err(error).context("rpc send_transaction_with_config failed");
        }
    };

    println!("RPC send:");
    println!("  signature         : {signature}");

    let confirmed = rpc
        .confirm_transaction_with_commitment(&signature, CommitmentConfig::confirmed())
        .context("rpc confirm_transaction_with_commitment failed")?;
    println!("  confirmed         : {}", confirmed.value);

    if !confirmed.value {
        print_status(ProbeStatus::RpcSignatureObtainedNotConfirmed);
        bail!("rpc signature was sent but not confirmed");
    }

    print_status(ProbeStatus::RpcConfirmed);
    Ok(signature)
}

fn load_obligation(rpc: &RpcClient, obligation_pk: &Pubkey) -> Result<Obligation> {
    let account = rpc
        .get_account(obligation_pk)
        .with_context(|| format!("failed to fetch obligation {obligation_pk}"))?;
    let mut cursor = &account.data[8..];
    Obligation::deserialize(&mut cursor)
        .map_err(|error| anyhow::anyhow!("borsh decode failed: {error}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    let cli = parse_cli()?;

    let rpc_url = std::env::var("HUNTER_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .context("HUNTER_RPC_URL or RPC_URL must be set")?;
    let keypair_path =
        std::env::var("SOLANA_KEYPAIR_PATH").context("SOLANA_KEYPAIR_PATH must be set")?;
    let jito_url = std::env::var("JITO_URL")
        .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf/api/v1/bundles".to_string());
    let wallet_toml_path =
        std::env::var("WALLET_TOML_PATH").unwrap_or_else(|_| "wallet.toml".to_string());

    let rpc = RpcClient::new(rpc_url);
    let keypair = read_keypair_file(&keypair_path)
        .map_err(|error| anyhow::anyhow!("failed to read keypair from {keypair_path}: {error}"))?;
    let liquidator = keypair.pubkey();

    println!("Liquidation probe");
    println!("  obligation        : {}", cli.obligation);
    println!("  liquidator        : {liquidator}");
    println!("  mode              : {:?}", cli.mode);
    println!("  repay_native      : {}", cli.repay_native);
    println!("  tip_lamports      : {}", cli.tip_lamports);
    println!("  tip_account       : {}", cli.tip_account);
    println!("  cu_limit          : {}", cli.cu_limit);
    println!("  cu_price          : {}", cli.cu_price);
    println!("  wallet_toml       : {}", wallet_toml_path);

    let wallet_tokens = load_wallet_tokens(&wallet_toml_path)?;
    println!("  wallet_tokens     : {}", wallet_tokens.len());

    let obligation = load_obligation(&rpc, &cli.obligation)?;
    print_obligation_summary(&obligation);

    if !obligation.is_liquidatable() {
        println!(
            "Raw obligation snapshot is not liquidatable. Continuing anyway so refresh + simulation can decide."
        );
    }

    let blockhash = rpc
        .get_latest_blockhash()
        .context("failed to fetch latest blockhash")?;
    let transaction_plan = match build_transaction(
        &cli,
        &obligation,
        &liquidator,
        blockhash,
        &keypair,
        &rpc,
        &wallet_tokens,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            print_status(ProbeStatus::StoppedMissingWalletCoverage);
            return Err(error);
        }
    };
    let tx = transaction_plan.tx.clone();

    println!("Wallet coverage:");
    println!(
        "  repay_symbol      : {}",
        transaction_plan.repay_token_symbol
    );
    println!(
        "  repay_mint        : {}",
        transaction_plan.repay_token_mint
    );
    println!(
        "  repay_decimals    : {}",
        wallet_tokens
            .iter()
            .find(|token| token.mint == transaction_plan.repay_token_mint)
            .map(|token| token.decimals)
            .or_else(|| token_info(&transaction_plan.repay_token_mint).map(|info| info.decimals))
            .unwrap_or(0)
    );
    println!(
        "  wallet_max_repay  : {}",
        transaction_plan.wallet_max_repay_native
    );
    println!(
        "  effective_repay   : {}",
        transaction_plan.effective_repay_native
    );

    let tx_size = bincode::serialize(&tx)
        .map(|bytes| bytes.len())
        .unwrap_or_default();
    println!("  tx_size_bytes     : {tx_size}");

    let simulation = rpc
        .simulate_transaction(&tx)
        .context("rpc simulate_transaction failed")?
        .value;
    print_simulation(&simulation);

    if simulation.err.is_some() {
        if simulation
            .logs
            .as_ref()
            .map(|logs| {
                logs.iter().any(|line| {
                    line.contains("ObligationHealthy")
                        || line.contains("healthy or not liquidatable")
                })
            })
            .unwrap_or(false)
        {
            println!("Simulation shows the obligation is still healthy after refresh.");
            print_status(ProbeStatus::StoppedHealthyBeforeSend);
            return Ok(());
        }
        println!("Aborting send because simulation failed.");
        print_status(ProbeStatus::SimulationFailed);
        return Ok(());
    }

    match cli.mode {
        SendMode::Simulate => {}
        SendMode::Rpc => {
            let _ = send_via_rpc(&rpc, &tx, cli.skip_preflight)?;
        }
        SendMode::Jito => {
            let jito = JitoAdapter::new(&jito_url);
            let bundle_id = match jito.send_bundle(vec![tx.clone()]).await {
                Ok(bundle_id) => bundle_id,
                Err(error) => {
                    print_status(ProbeStatus::JitoBundleRejectedApi);
                    return Err(error).context("jito sendBundle failed");
                }
            };
            println!("Jito send:");
            println!("  bundle_id         : {bundle_id}");
            println!("  note              : sendBundle accepted by Jito does not prove on-chain inclusion");
            print_status(ProbeStatus::JitoBundleAcceptedApi);
        }
        SendMode::RpcAndJito => {
            let rpc_sig = send_via_rpc(&rpc, &tx, cli.skip_preflight)?;
            let jito = JitoAdapter::new(&jito_url);
            let bundle_id = match jito.send_bundle(vec![tx.clone()]).await {
                Ok(bundle_id) => bundle_id,
                Err(error) => {
                    print_status(ProbeStatus::JitoBundleRejectedApi);
                    return Err(error).context("jito sendBundle failed");
                }
            };
            println!("Jito send:");
            println!("  bundle_id         : {bundle_id}");
            println!("  rpc_signature     : {rpc_sig}");
            println!("  note              : RPC confirmation proves inclusion better than sendBundle alone");
            print_status(ProbeStatus::JitoBundleAcceptedApi);
        }
    }

    if matches!(cli.mode, SendMode::Rpc | SendMode::RpcAndJito) {
        std::thread::sleep(Duration::from_millis(500));
    }

    Ok(())
}
