use crate::domain::opportunity::LiquidationOpportunity;
use crate::ports::jito::JitoPort;
use crate::ports::jupiter::JupiterPort;
use crate::ports::oracle::PriceOracle;
use crate::ports::rpc::RpcClient;
use crate::ports::config::ConfigPort;
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
use tokio::sync::{mpsc, RwLock};
use tokio::time::{sleep, Duration};
use std::sync::Arc;
use std::str::FromStr;

const JITO_TIP_ACCOUNT: &str = "96g9sAg9u3P7Q9ebKsC6SA47cySvnV6S1S1K6ssB1vD"; // Example tip account
const KLEND_PROGRAM: &str    = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const LENDING_MARKET: &str   = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
const LENDING_MARKET_AUTHORITY: &str = "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo";
const TOKEN_PROGRAM: &str    = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str      = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const FARMS_PROGRAM: &str    = "FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr";

#[derive(Debug, Clone)]
pub struct ReserveInfo {
    pub address: Pubkey,
    pub liquidity_mint: Pubkey,
    pub liquidity_supply: Pubkey,
    pub collateral_mint: Pubkey,
    pub collateral_supply: Pubkey,
    pub liquidity_fee_receiver: Pubkey,
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

use crate::ports::logger::{LiquidationLogger, ObservationEvent};

pub struct HunterService<R: RpcClient, JI: JitoPort, JU: JupiterPort, O: PriceOracle, C: ConfigPort + LiquidationLogger + Clone> {
    hunter_rpc: R,
    jito: JI,
    jupiter: JU,
    _oracle: O,
    config: C,
    keypair: Arc<Keypair>,
    max_repay_usd: f64,
}

impl<R: RpcClient, JI: JitoPort, JU: JupiterPort, O: PriceOracle, C: ConfigPort + LiquidationLogger + Clone + 'static> HunterService<R, JI, JU, O, C> {
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
            jupiter,
            _oracle: oracle,
            config,
            keypair,
            max_repay_usd,
        }
    }

    pub async fn run(&self, mut rx: mpsc::Receiver<LiquidationOpportunity>) -> anyhow::Result<()> {
        println!("[hunter] Hunter service started. Wallet: {} | Max repay: ${:.2}", self.keypair.pubkey(), self.max_repay_usd);
        
        while let Some(opportunity) = rx.recv().await {
            let _ = self.handle_opportunity(opportunity).await;
        }
        
        Ok(())
    }

    fn resolve_reserve(&self, mint: &str) -> Option<ReserveInfo> {
        match mint {
            // USDC
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => Some(ReserveInfo {
                address: Pubkey::from_str("D9hySTbuYpkiB9S7Y7XUsSaZ8vDSW9o8AeSxkYAmauHn").unwrap(),
                liquidity_mint: Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap(),
                liquidity_supply: Pubkey::from_str("2PF786D9S9SshF8S7799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
                collateral_mint: Pubkey::from_str("G9fvp9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
                collateral_supply: Pubkey::from_str("H9fvp9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
                liquidity_fee_receiver: Pubkey::from_str("ADov9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
            }),
            // SOL
            "So11111111111111111111111111111111111111112" => Some(ReserveInfo {
                address: Pubkey::from_str("D84Z9UkvYvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
                liquidity_mint: Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap(),
                liquidity_supply: Pubkey::from_str("E9fvp9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
                collateral_mint: Pubkey::from_str("F9fvp9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
                collateral_supply: Pubkey::from_str("G9fvp9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
                liquidity_fee_receiver: Pubkey::from_str("H9fvp9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6").unwrap(), // Placeholder
            }),
            _ => None,
        }
    }

    fn build_kamino_liquidate_ix(
        &self,
        liquidator: &Pubkey,
        obligation: &Pubkey,
        lending_market: &Pubkey,
        lending_market_authority: &Pubkey,
        repay: &ReserveInfo,
        withdraw: &ReserveInfo,
        liquidity_amount: u64,
    ) -> Instruction {
        let disc = discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2");
        let klend_program = Pubkey::from_str(KLEND_PROGRAM).unwrap();
        let farms_program = Pubkey::from_str(FARMS_PROGRAM).unwrap();
        let token_program = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
        let instructions_sysvar = sysvar::instructions::id();

        let mut data = disc.to_vec();
        data.extend_from_slice(&liquidity_amount.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // minAcceptableReceivedLiquidityAmount
        data.extend_from_slice(&0u64.to_le_bytes()); // maxAllowedLtvOverridePercent

        let user_source_liquidity = get_ata(liquidator, &repay.liquidity_mint);
        let user_dest_collateral = get_ata(liquidator, &withdraw.collateral_mint);
        let user_dest_liquidity = get_ata(liquidator, &withdraw.liquidity_mint);

        let accounts = vec![
            AccountMeta::new_readonly(*liquidator, true),
            AccountMeta::new(*obligation, false),
            AccountMeta::new_readonly(*lending_market, false),
            AccountMeta::new_readonly(*lending_market_authority, false),
            AccountMeta::new(repay.address, false),
            AccountMeta::new_readonly(repay.liquidity_mint, false),
            AccountMeta::new(repay.liquidity_supply, false),
            AccountMeta::new(withdraw.address, false),
            AccountMeta::new_readonly(withdraw.liquidity_mint, false),
            AccountMeta::new(withdraw.collateral_mint, false),
            AccountMeta::new(withdraw.collateral_supply, false),
            AccountMeta::new(withdraw.liquidity_supply, false),
            AccountMeta::new(withdraw.liquidity_fee_receiver, false),
            AccountMeta::new(user_source_liquidity, false),
            AccountMeta::new(user_dest_collateral, false),
            AccountMeta::new(user_dest_liquidity, false),
            AccountMeta::new_readonly(token_program, false), // collateral token program
            AccountMeta::new_readonly(token_program, false), // repay liquidity token program
            AccountMeta::new_readonly(token_program, false), // withdraw liquidity token program
            AccountMeta::new_readonly(instructions_sysvar, false),
            // Optional Farm accounts (use klend as placeholder)
            AccountMeta::new(klend_program, false), // collateral obligation farm user state
            AccountMeta::new(klend_program, false), // collateral reserve farm state
            AccountMeta::new(klend_program, false), // debt obligation farm user state
            AccountMeta::new(klend_program, false), // debt reserve farm state
            // Farms Program
            AccountMeta::new_readonly(farms_program, false),
        ];

        Instruction {
            program_id: klend_program,
            accounts,
            data,
        }
    }

    async fn handle_opportunity(&self, opportunity: LiquidationOpportunity) -> anyhow::Result<()> {
        let timestamp = crate::utils::utc_now();
        println!("[hunter] analyzing opportunity for user {} on {}...", opportunity.position.wallet, opportunity.market);
        
        // Base event for logging
        let mut event = ObservationEvent {
            timestamp: timestamp.clone(),
            signature: "N/A".to_string(),
            protocol: "Kamino".to_string(),
            market: opportunity.market.clone(),
            liquidated_user: opportunity.position.wallet.clone(),
            liquidator: self.keypair.pubkey().to_string(),
            repay_mint: opportunity.repay_mint.clone(),
            withdraw_mint: opportunity.withdraw_mint.clone(),
            repay_symbol: "N/A".to_string(),
            withdraw_symbol: "N/A".to_string(),
            repay_amount: opportunity.repay_amount_native as f64, // Placeholder native
            withdraw_amount: opportunity.withdraw_amount_native as f64,
            repaid_usd: opportunity.position.debt_value_usd,
            withdrawn_usd: opportunity.position.collateral_value_usd,
            profit_usd: 0.0,
            delay_ms: 0,
            competing_bots: 0,
            status: "WATCHED".to_string(),
        };

        // 1. Filter by amount
        if opportunity.position.debt_value_usd > self.max_repay_usd {
            println!("[hunter] opportunity ignored: debt ({:.2} USD) exceeds max_repay_usd ({:.2} USD)", opportunity.position.debt_value_usd, self.max_repay_usd);
            event.status = "IGNORED_CAPITAL".to_string();
            let _ = self.config.log_observation(&event).await;
            return Ok(());
        }

        // Fill symbols for logging
        {
            let repay_info = crate::domain::token::token_info(&opportunity.repay_mint);
            let withdraw_info = crate::domain::token::token_info(&opportunity.withdraw_mint);
            if let Some(r) = &repay_info { event.repay_symbol = r.symbol.to_string(); }
            if let Some(w) = &withdraw_info { event.withdraw_symbol = w.symbol.to_string(); }
        }

        // 2. Profit Check (Basic Phase 2 calculation)
        let tip_lamports = self.jito.get_tip_recommendation().await.unwrap_or(100_000);
        let tip_usd = (tip_lamports as f64 / 1_000_000_000.0) * 150.0; // Hardcoded SOL price
        
        let gross_profit = opportunity.position.collateral_value_usd - opportunity.position.debt_value_usd;
        let estimated_profit_usd = gross_profit - tip_usd;
        event.profit_usd = estimated_profit_usd;
        
        if estimated_profit_usd < 1.0 { // Minimum 1$ profit
            println!("[hunter] opportunity ignored: estimated profit ({:.2} USD) is too low (threshold: 1.00 USD)", estimated_profit_usd);
            event.status = "IGNORED_LOW_PROFIT".to_string();
            let _ = self.config.log_observation(&event).await;
            return Ok(());
        }

        println!(
            "[hunter] triggering liquidation: repay={:.2} USD | est_profit={:.2} USD",
            opportunity.position.debt_value_usd,
            estimated_profit_usd
        );

        // 4. Build Instructions
        let mut instructions = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
            ComputeBudgetInstruction::set_compute_unit_price(1),
        ];

        // 4a. Resolve Reserves (Prototype: Hardcoded for SOL/USDC)
        let repay_reserve = self.resolve_reserve(&opportunity.repay_mint);
        let withdraw_reserve = self.resolve_reserve(&opportunity.withdraw_mint);

        if let (Some(repay), Some(withdraw)) = (repay_reserve, withdraw_reserve) {
            let obligation_pubkey = Pubkey::from_str(&opportunity.position.wallet).unwrap();
            let market_pubkey = Pubkey::from_str(LENDING_MARKET).unwrap();
            let market_auth_pubkey = Pubkey::from_str(LENDING_MARKET_AUTHORITY).unwrap();

            // Add Refresh Reserve and Obligation instructions (required by Kamino before liquidate)
            instructions.push(ix_refresh_reserve(&Pubkey::from_str(KLEND_PROGRAM).unwrap(), &market_pubkey, &repay.address, None));
            instructions.push(ix_refresh_reserve(&Pubkey::from_str(KLEND_PROGRAM).unwrap(), &market_pubkey, &withdraw.address, None));
            instructions.push(ix_refresh_obligation(
                &Pubkey::from_str(KLEND_PROGRAM).unwrap(),
                &market_pubkey,
                &obligation_pubkey,
                &[&repay.address, &withdraw.address]
            ));

            let klend_ix = self.build_kamino_liquidate_ix(
                &self.keypair.pubkey(),
                &obligation_pubkey,
                &market_pubkey,
                &market_auth_pubkey,
                &repay,
                &withdraw,
                opportunity.repay_amount_native,
            );
            instructions.push(klend_ix);
        } else {
            event.status = "ERROR_RESERVE_RESOLVE".to_string();
            let _ = self.config.log_observation(&event).await;
            eprintln!("[hunter] could not resolve reserves for {}/{}", opportunity.repay_mint, opportunity.withdraw_mint);
            return Ok(());
        }

        // 4b. Add Jupiter Swap Instruction
        if opportunity.withdraw_mint != "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" { // Not USDC
            if let Ok(swap_ixs) = self.jupiter.build_swap_instructions(
                &opportunity.withdraw_mint,
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // To USDC
                opportunity.withdraw_amount_native,
                50, // 0.5% slippage
                &self.keypair.pubkey().to_string(),
            ).await {
                instructions.extend(swap_ixs);
            }
        }

        // 4c. Add Jito Tip
        let tip_account = Pubkey::from_str(JITO_TIP_ACCOUNT).unwrap();
        instructions.push(solana_sdk::system_instruction::transfer(
            &self.keypair.pubkey(),
            &tip_account,
            tip_lamports,
        ));

        // 5. Create Versioned Transaction
        let recent_blockhash = self.hunter_rpc.get_latest_blockhash().await
            .map_err(|e| anyhow::anyhow!("Failed to get blockhash: {}", e))?;
        
        let message = Message::try_compile(
            &self.keypair.pubkey(),
            &instructions,
            &[],
            recent_blockhash,
        ).map_err(|e| anyhow::anyhow!("Failed to compile message: {}", e))?;
        
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &[&*self.keypair],
        ).map_err(|e| anyhow::anyhow!("Failed to sign transaction: {}", e))?;

        // 6. Send Bundle
        match self.jito.send_bundle(vec![tx]).await {
            Ok(bundle_id) => {
                event.status = "HUNT_SENT".to_string();
                event.signature = bundle_id.clone();
                let _ = self.config.log_observation(&event).await;
                println!("[hunter] Bundle sent successfully! ID: {}", bundle_id);
            }
            Err(e) => {
                event.status = format!("HUNT_FAILED: {}", e);
                let _ = self.config.log_observation(&event).await;
                eprintln!("[hunter] Failed to send bundle: {}", e);
            }
        }

        Ok(())
    }
}
