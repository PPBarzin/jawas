/// One-shot Kamino liquidation script — Phase 1 monitoring validation.
///
/// Liquidates a single Kamino obligation and verifies that the Phase 1 observer
/// detects the event. Simulates the transaction before sending.
///
/// Usage:
///   cargo run --bin liquidate_one -- <OBLIGATION_PUBKEY> [--dry-run]
///
/// Required env vars (in .env):
///   RPC_URL        — Helius/Quicknode HTTP RPC endpoint
///   KEYPAIR_PATH   — path to the liquidator wallet JSON keypair

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{read_keypair_file, Signer},
    sysvar,
    transaction::Transaction,
};
use std::str::FromStr;

// ── Constants ──────────────────────────────────────────────────────────────────

const KLEND_PROGRAM: &str    = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const LENDING_MARKET: &str   = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
const TOKEN_PROGRAM: &str    = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str      = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const MSOL_MINT: &str        = "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So";
const JITOSOL_MINT: &str     = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";

// ── Anchor discriminator ───────────────────────────────────────────────────────

fn discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{}", name);
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

// ── ATA derivation ─────────────────────────────────────────────────────────────

fn get_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let ata_program   = Pubkey::from_str(ATA_PROGRAM).unwrap();
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    ).0
}

fn ix_create_ata(payer: &Pubkey, wallet: &Pubkey, mint: &Pubkey) -> Instruction {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let ata_program   = Pubkey::from_str(ATA_PROGRAM).unwrap();
    let ata = get_ata(wallet, mint);

    Instruction {
        program_id: ata_program,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*wallet, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data: vec![],
    }
}

// ── Reserve data ───────────────────────────────────────────────────────────────

#[derive(Debug)]
struct ReserveInfo {
    address:               Pubkey,
    liquidity_mint:        Pubkey,
    liquidity_supply:      Pubkey,
    collateral_mint:       Pubkey,
    collateral_supply:     Pubkey,
    liquidity_fee_receiver: Pubkey,
}

// ── Instructions ───────────────────────────────────────────────────────────────

fn ix_refresh_reserve(
    klend: &Pubkey,
    market: &Pubkey,
    reserve: &Pubkey,
    oracle: Option<&Pubkey>,
) -> Instruction {
    let disc = discriminator("refresh_reserve");
    let mut accounts = vec![
        AccountMeta::new(*reserve, false),
        AccountMeta::new_readonly(*market, false),
    ];
    // In KLend, optional accounts use the program ID as placeholder instead of null
    accounts.push(AccountMeta::new_readonly(*klend, false)); // 3. Pyth Placeholder
    accounts.push(AccountMeta::new_readonly(*klend, false)); // 4. Switchboard Price Placeholder
    accounts.push(AccountMeta::new_readonly(*klend, false)); // 5. Switchboard TWAP Placeholder
    
    if let Some(o) = oracle {
        accounts.push(AccountMeta::new_readonly(*o, false)); // 6. Scope
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

fn ix_liquidate(
    klend: &Pubkey,
    liquidator: &Pubkey,
    obligation: &Pubkey,
    lending_market: &Pubkey,
    lending_market_authority: &Pubkey,
    repay: &ReserveInfo,
    withdraw: &ReserveInfo,
    liquidity_amount: u64,
) -> Instruction {
    let disc = discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2");

    // Args: liquidityAmount u64 | minAcceptableReceivedLiquidityAmount u64 | maxAllowedLtvOverridePercent u64
    let mut data = disc.to_vec();
    data.extend_from_slice(&liquidity_amount.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // min acceptable = 0 (test)
    data.extend_from_slice(&0u64.to_le_bytes()); // ltv override = 0

    let token_program  = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let instructions_sysvar = sysvar::instructions::id();
    let farms_program = Pubkey::from_str("FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr").unwrap();

    let user_source_liquidity      = get_ata(liquidator, &repay.liquidity_mint);
    let user_dest_collateral       = get_ata(liquidator, &withdraw.collateral_mint);
    let user_dest_liquidity        = get_ata(liquidator, &withdraw.liquidity_mint);

    let mut accounts = vec![
        AccountMeta::new_readonly(*liquidator,                 true),
        AccountMeta::new(*obligation,                          false),
        AccountMeta::new_readonly(*lending_market,             false),
        AccountMeta::new_readonly(*lending_market_authority,   false),
        AccountMeta::new(repay.address,                        false),
        AccountMeta::new_readonly(repay.liquidity_mint,        false),
        AccountMeta::new(repay.liquidity_supply,               false),
        AccountMeta::new(withdraw.address,                     false),
        AccountMeta::new_readonly(withdraw.liquidity_mint,     false),
        AccountMeta::new(withdraw.collateral_mint,             false),
        AccountMeta::new(withdraw.collateral_supply,           false),
        AccountMeta::new(withdraw.liquidity_supply,            false),
        AccountMeta::new(withdraw.liquidity_fee_receiver,      false),
        AccountMeta::new(user_source_liquidity,                false),
        AccountMeta::new(user_dest_collateral,                 false),
        AccountMeta::new(user_dest_liquidity,                  false),
        AccountMeta::new_readonly(token_program,               false), // collateral token program
        AccountMeta::new_readonly(token_program,               false), // repay liquidity token program
        AccountMeta::new_readonly(token_program,               false), // withdraw liquidity token program
        AccountMeta::new_readonly(instructions_sysvar,         false),
    ];

    // Optional Farm accounts (use klend as placeholder)
    accounts.push(AccountMeta::new(*klend, false)); // collateral obligation farm user state
    accounts.push(AccountMeta::new(*klend, false)); // collateral reserve farm state
    accounts.push(AccountMeta::new(*klend, false)); // debt obligation farm user state
    accounts.push(AccountMeta::new(*klend, false)); // debt reserve farm state
    
    // Farms Program (required)
    accounts.push(AccountMeta::new_readonly(farms_program, false));

    Instruction { program_id: *klend, accounts, data }
}

// ── Main ───────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: liquidate_one <OBLIGATION_PUBKEY> [--dry-run]");
        std::process::exit(1);
    }
    let obligation_str = &args[1];
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let rpc_url = std::env::var("RPC_URL")
        .context("RPC_URL not set in .env")?;
    let keypair_path = std::env::var("KEYPAIR_PATH")
        .context("KEYPAIR_PATH not set in .env — path to your JSON keypair file")?;

    let keypair = read_keypair_file(&keypair_path)
        .map_err(|e| anyhow::anyhow!("Cannot read keypair from {}: {}", keypair_path, e))?;

    let rpc = RpcClient::new(rpc_url);
    let klend   = Pubkey::from_str(KLEND_PROGRAM)?;
    let market  = Pubkey::from_str(LENDING_MARKET)?;
    let obligation = Pubkey::from_str(obligation_str)
        .context("Invalid obligation pubkey")?;

    // Hardcoded Lending market authority (Main Market)
    let market_authority = Pubkey::from_str("9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo")?;

    println!("Liquidator : {}", keypair.pubkey());
    println!("Obligation : {}", obligation);
    println!("Market auth: {}", market_authority);
    println!("Dry-run    : {}", dry_run);
    println!();

    // Hardcoded reserve data (Kamino API is unreliable for this one-shot)
    let oracle = Pubkey::from_str("3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH")?;

    let repay_reserve = ReserveInfo {
        address:                Pubkey::from_str("FBSyPnxtHKLBZ4UeeUyAnbtFuAmTHLtso9YtsqRDRWpM")?,
        liquidity_mint:         Pubkey::from_str(MSOL_MINT)?,
        liquidity_supply:       Pubkey::from_str("CQWUdThEbNMjcoEjGyCMTGXHpKvW1aB8JF31hKa1FQQN")?,
        collateral_mint:        Pubkey::default(), // not used for repay
        collateral_supply:      Pubkey::default(), // not used for repay
        liquidity_fee_receiver: Pubkey::from_str("7Pj3hWhPjhd3mYYGyutQPSkXrm2wPN3VQvVzG3YBMfue")?,
    };

    let withdraw_reserve = ReserveInfo {
        address:                Pubkey::from_str("EVbyPKrHG6WBfm4dLxLMJpUDY43cCAcHSpV3KYjKsktW")?,
        liquidity_mint:         Pubkey::from_str(JITOSOL_MINT)?,
        liquidity_supply:       Pubkey::from_str("6sga1yRArgQRqa8Darhm54EBromEpV3z8iDAvMTVYXB3")?,
        collateral_mint:        Pubkey::from_str("9ucQp7thL38MDDTSER5ou24QnVSTZFLevDsZC1cAFkKy")?,
        collateral_supply:      Pubkey::from_str("7y5Nko765HcZiTd2gFtxorELuJZcbQqmrmTbUVoiwGyS")?,
        liquidity_fee_receiver: Pubkey::from_str("C2PyjpFRtbQjFjHNB3HDcoQoLP7VJ9NQn6NFJZMueWfB")?,
    };

    println!();
    println!("Repay reserve (mSOL):");
    println!("  address          : {}", repay_reserve.address);
    println!("  liquidity supply : {}", repay_reserve.liquidity_supply);
    println!("Withdraw reserve (JitoSOL):");
    println!("  address          : {}", withdraw_reserve.address);
    println!("  collateral mint  : {}", withdraw_reserve.collateral_mint);
    println!("  collateral supply: {}", withdraw_reserve.collateral_supply);
    println!("  liquidity supply : {}", withdraw_reserve.liquidity_supply);
    println!("  fee receiver     : {}", withdraw_reserve.liquidity_fee_receiver);
    println!();

    // Liquidator ATAs
    let msol_mint   = Pubkey::from_str(MSOL_MINT)?;
    let jitosol_mint = Pubkey::from_str(JITOSOL_MINT)?;
    let ata_msol    = get_ata(&keypair.pubkey(), &msol_mint);
    let ata_jitosol = get_ata(&keypair.pubkey(), &jitosol_mint);
    let ata_collat  = get_ata(&keypair.pubkey(), &withdraw_reserve.collateral_mint);
    println!("Liquidator mSOL ATA    : {}", ata_msol);
    println!("Liquidator JitoSOL ATA : {}", ata_jitosol);
    println!("Liquidator collat ATA  : {}", ata_collat);
    println!();

    // Build instructions
    let mut instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
        ComputeBudgetInstruction::set_compute_unit_price(100_000), // ~0.00001 SOL priority fee
    ];

    // Create ATAs if needed
    for mint in [&msol_mint, &jitosol_mint, &withdraw_reserve.collateral_mint] {
        let ata = get_ata(&keypair.pubkey(), mint);
        if rpc.get_account(&ata).is_err() {
            println!("Adding instruction to create ATA for mint: {}", mint);
            instructions.push(ix_create_ata(&keypair.pubkey(), &keypair.pubkey(), mint));
        }
    }

    // Refresh Reserves
    instructions.push(ix_refresh_reserve(&klend, &market, &repay_reserve.address, Some(&oracle)));
    instructions.push(ix_refresh_reserve(&klend, &market, &withdraw_reserve.address, Some(&oracle)));

    // Refresh Obligation (requires only reserves in V2, order usually Deposit then Borrow)
    let reserves_for_refresh = [&withdraw_reserve.address, &repay_reserve.address];
    instructions.push(ix_refresh_obligation(
        &klend,
        &market,
        &obligation,
        &reserves_for_refresh,
    ));

    instructions.push(ix_liquidate(
        &klend,
        &keypair.pubkey(),
        &obligation,
        &market,
        &market_authority,
        &repay_reserve,
        &withdraw_reserve,
        u64::MAX, // let Kamino determine amount (close factor)
    ));

    let blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &[&keypair],
        blockhash,
    );

    // Always simulate first
    println!("Simulating transaction...");
    match rpc.simulate_transaction(&tx) {
        Ok(result) => {
            if let Some(err) = &result.value.err {
                println!("Simulation returned an error: {:?}", err);
                if let Some(logs) = &result.value.logs {
                    for log in logs {
                        println!("  {}", log);
                    }
                }
                if dry_run {
                    println!("\n[dry-run] Simulation failed, stopping here.");
                    std::process::exit(1);
                } else {
                    println!("\n[LIVE] Simulation failed, but proceeding anyway (price might move)...");
                }
            } else {
                println!("Simulation OK — units consumed: {:?}", result.value.units_consumed);
            }
        }
        Err(e) => {
            eprintln!("Simulation RPC error: {}", e);
            if dry_run { std::process::exit(1); }
        }
    }

    if dry_run {
        println!("\n[dry-run] Transaction NOT sent.");
        return Ok(());
    }

    println!("\nSending transaction...");
    let sig = rpc.send_and_confirm_transaction(&tx)
        .context("Failed to send transaction")?;

    println!("Transaction confirmed!");
    println!("Signature: {}", sig);
    println!("Solscan  : https://solscan.io/tx/{}", sig);

    Ok(())
}
