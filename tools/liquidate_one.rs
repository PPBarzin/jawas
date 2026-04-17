use anyhow::{Context, Result};
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
use jawas::domain::kamino::Obligation;
use borsh::BorshDeserialize;
use sha2::{Digest, Sha256};

// ── Constants ──────────────────────────────────────────────────────────────────
const KLEND_PROGRAM: &str    = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const LENDING_MARKET: &str   = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
const MARKET_AUTHORITY: &str = "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo";
const TOKEN_PROGRAM: &str    = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str      = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

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

#[derive(Debug, Clone)]
struct ReserveInfo {
    address:               Pubkey,
    liquidity_mint:        Pubkey,
    liquidity_supply:      Pubkey,
    collateral_mint:       Pubkey,
    collateral_supply:     Pubkey,
    liquidity_fee_receiver: Pubkey,
}

async fn fetch_reserve_metadata(rpc: &RpcClient, reserve_pk: &Pubkey) -> Result<ReserveInfo> {
    let data = rpc.get_account(reserve_pk)?.data;
    // Offsets standards Kamino K-Lend
    let liquidity_mint = Pubkey::new_from_array(data[128..160].try_into()?);
    let liquidity_supply = Pubkey::new_from_array(data[160..192].try_into()?);
    let liquidity_fee_receiver = Pubkey::new_from_array(data[192..224].try_into()?);
    let collateral_mint = Pubkey::new_from_array(data[280..312].try_into()?);
    let collateral_supply = Pubkey::new_from_array(data[312..344].try_into()?);

    Ok(ReserveInfo {
        address: *reserve_pk,
        liquidity_mint,
        liquidity_supply,
        collateral_mint,
        collateral_supply,
        liquidity_fee_receiver,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: cargo run --bin liquidate_one <OBLIGATION_PK> [--dry-run]");
        return Ok(());
    }

    let obligation_pk = Pubkey::from_str(&args[1])?;
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let rpc_url = std::env::var("OBSERVER_RPC_URL").expect("OBSERVER_RPC_URL not set");
    let keypair_path = std::env::var("SOLANA_KEYPAIR_PATH").expect("SOLANA_KEYPAIR_PATH not set");
    let rpc = RpcClient::new(rpc_url);
    let keypair = read_keypair_file(&keypair_path).expect("Failed to read keypair");

    println!("🚀 DÉMARRAGE DE LA LIQUIDATION CIBLÉE");
    println!("Cible      : {}", obligation_pk);
    println!("Liquidateur: {}", keypair.pubkey());

    // 1. Lire l'obligation
    let obs_data = rpc.get_account(&obligation_pk)?.data;
    let mut cursor = &obs_data[8..];
    let obligation = Obligation::deserialize(&mut cursor).map_err(|e| anyhow::anyhow!("Borsh fail: {}", e))?;

    // 2. Trouver le premier dépôt et le premier emprunt
    let deposit = obligation.deposits.iter().find(|d| d.deposited_amount > 0)
        .context("Aucun dépôt trouvé")?;
    let borrow = obligation.borrows.iter().find(|b| b.borrowed_amount_sf > 0)
        .context("Aucun emprunt trouvé")?;

    let repay_reserve_pk = Pubkey::new_from_array(borrow.borrow_reserve);
    let withdraw_reserve_pk = Pubkey::new_from_array(deposit.deposit_reserve);

    println!("Action: Rembourser réserve {} | Retirer réserve {}", repay_reserve_pk, withdraw_reserve_pk);

    // 3. Chercher les métadonnées des réserves
    let repay_info = fetch_reserve_metadata(&rpc, &repay_reserve_pk).await?;
    let withdraw_info = fetch_reserve_metadata(&rpc, &withdraw_reserve_pk).await?;

    // 4. Construire les instructions
    let mut ixs = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(1_000_000),
        ComputeBudgetInstruction::set_compute_unit_price(10_000),
    ];

    let klend_pk = Pubkey::from_str(KLEND_PROGRAM)?;
    let market_pk = Pubkey::from_str(LENDING_MARKET)?;
    let market_auth = Pubkey::from_str(MARKET_AUTHORITY)?;

    // Refresh Obligation + Reserves
    ixs.push(Instruction {
        program_id: klend_pk,
        accounts: vec![AccountMeta::new(repay_reserve_pk, false), AccountMeta::new_readonly(market_pk, false), AccountMeta::new_readonly(klend_pk, false), AccountMeta::new_readonly(klend_pk, false), AccountMeta::new_readonly(klend_pk, false)],
        data: discriminator("refresh_reserve").to_vec(),
    });
    ixs.push(Instruction {
        program_id: klend_pk,
        accounts: vec![AccountMeta::new(withdraw_reserve_pk, false), AccountMeta::new_readonly(market_pk, false), AccountMeta::new_readonly(klend_pk, false), AccountMeta::new_readonly(klend_pk, false), AccountMeta::new_readonly(klend_pk, false)],
        data: discriminator("refresh_reserve").to_vec(),
    });
    ixs.push(Instruction {
        program_id: klend_pk,
        accounts: vec![AccountMeta::new_readonly(market_pk, false), AccountMeta::new(obligation_pk, false), AccountMeta::new_readonly(withdraw_reserve_pk, false), AccountMeta::new_readonly(repay_reserve_pk, false)],
        data: discriminator("refresh_obligation").to_vec(),
    });

    // Liquidate Instruction
    let disc = discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2");
    let mut data = disc.to_vec();
    data.extend_from_slice(&u64::MAX.to_le_bytes()); // Montant max
    data.extend_from_slice(&0u64.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());

    let liquidator = keypair.pubkey();
    let accounts = vec![
        AccountMeta::new_readonly(liquidator, true),
        AccountMeta::new(obligation_pk, false),
        AccountMeta::new_readonly(market_pk, false),
        AccountMeta::new_readonly(market_auth, false),
        AccountMeta::new(repay_info.address, false),
        AccountMeta::new_readonly(repay_info.liquidity_mint, false),
        AccountMeta::new(repay_info.liquidity_supply, false),
        AccountMeta::new(withdraw_info.address, false),
        AccountMeta::new_readonly(withdraw_info.liquidity_mint, false),
        AccountMeta::new(withdraw_info.collateral_mint, false),
        AccountMeta::new(withdraw_info.collateral_supply, false),
        AccountMeta::new(withdraw_info.liquidity_supply, false),
        AccountMeta::new(withdraw_info.liquidity_fee_receiver, false),
        AccountMeta::new(get_ata(&liquidator, &repay_info.liquidity_mint), false),
        AccountMeta::new(get_ata(&liquidator, &withdraw_info.collateral_mint), false),
        AccountMeta::new(get_ata(&liquidator, &withdraw_info.liquidity_mint), false),
        AccountMeta::new_readonly(Pubkey::from_str(TOKEN_PROGRAM)?, false),
        AccountMeta::new_readonly(Pubkey::from_str(TOKEN_PROGRAM)?, false),
        AccountMeta::new_readonly(Pubkey::from_str(TOKEN_PROGRAM)?, false),
        AccountMeta::new_readonly(sysvar::instructions::id(), false),
        AccountMeta::new(klend_pk, false), AccountMeta::new(klend_pk, false), AccountMeta::new(klend_pk, false), AccountMeta::new(klend_pk, false),
        AccountMeta::new_readonly(Pubkey::from_str("FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr")?, false),
    ];

    ixs.push(Instruction { program_id: klend_pk, accounts, data });

    // Jito Tip
    ixs.push(solana_sdk::system_instruction::transfer(&liquidator, &Pubkey::from_str("96g9sAg9u3P7Q9ebKsC6SA47cySvnV6S1S1K6ssB1vD")?, 100_000));

    let blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&liquidator), &[&keypair], blockhash);

    println!("Simulation en cours...");
    let sim = rpc.simulate_transaction(&tx)?;
    if let Some(err) = sim.value.err {
        println!("❌ ÉCHEC SIMULATION : {:?}", err);
        if let Some(logs) = sim.value.logs {
            for log in logs { println!("  {}", log); }
        }
    } else {
        println!("✅ SIMULATION RÉUSSIE !");
        if !dry_run {
            println!("Envoi de la transaction réelle...");
            let sig = rpc.send_and_confirm_transaction(&tx)?;
            println!("Transaction confirmée : {}", sig);
        }
    }

    Ok(())
}
