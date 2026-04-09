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
use tokio::sync::{mpsc, RwLock};
use tokio::time::{sleep, Duration};
use std::sync::Arc;
use std::str::FromStr;

const JITO_TIP_ACCOUNT: &str = "96g9sAg9u3P7Q9ebKsC6SA47cySvnV6S1S1K6ssB1vD"; // Example tip account

pub struct HunterService<R: RpcClient, JI: JitoPort, JU: JupiterPort, O: PriceOracle, C: ConfigPort + Clone> {
    hunter_rpc: R,
    jito: JI,
    jupiter: JU,
    _oracle: O,
    config: C,
    keypair: Arc<Keypair>,
    max_repay_usd: f64,
    whitelist: Arc<RwLock<Vec<String>>>,
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
            jupiter,
            _oracle: oracle,
            config,
            keypair,
            max_repay_usd,
            whitelist: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn run(&self, mut rx: mpsc::Receiver<LiquidationOpportunity>) -> anyhow::Result<()> {
        println!("[hunter] Hunter service started. Wallet: {} | Max repay: ${:.2}", self.keypair.pubkey(), self.max_repay_usd);
        
        // Initial whitelist fetch
        if let Ok(wl) = self.config.fetch_whitelist().await {
            let mut lock = self.whitelist.write().await;
            *lock = wl;
            println!("[hunter] Whitelist loaded: {} tokens", lock.len());
        }

        // Spawn background refresh for whitelist (every 5 minutes)
        let config_clone = self.config.clone();
        let whitelist_clone = self.whitelist.clone();
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(300)).await;
                match config_clone.fetch_whitelist().await {
                    Ok(wl) => {
                        let mut lock = whitelist_clone.write().await;
                        *lock = wl;
                        println!("[hunter] Whitelist refreshed: {} tokens", lock.len());
                    }
                    Err(e) => eprintln!("[hunter] Failed to refresh whitelist: {}", e),
                }
            }
        });

        while let Some(opportunity) = rx.recv().await {
            let _ = self.handle_opportunity(opportunity).await;
        }
        
        Ok(())
    }

    async fn handle_opportunity(&self, opportunity: LiquidationOpportunity) -> anyhow::Result<()> {
        // 1. Filter by amount
        if opportunity.position.debt_value_usd > self.max_repay_usd {
            return Ok(());
        }

        // 2. Filter by whitelist
        {
            let wl = self.whitelist.read().await;
            let repay_info = crate::domain::token::token_info(&opportunity.repay_mint);
            let withdraw_info = crate::domain::token::token_info(&opportunity.withdraw_mint);
            
            let is_whitelisted = match (repay_info, withdraw_info) {
                (Some(r), Some(w)) => wl.contains(&r.symbol.to_string()) && wl.contains(&w.symbol.to_string()),
                _ => false,
            };

            if !is_whitelisted {
                return Ok(());
            }
        }

        // 3. Profit Check (Basic Phase 2 calculation)
        // Bonus - Tip - Slippage
        let tip_lamports = self.jito.get_tip_recommendation().await.unwrap_or(100_000);
        let tip_usd = (tip_lamports as f64 / 1_000_000_000.0) * 150.0; // Hardcoded SOL price
        
        let estimated_profit_usd = (opportunity.position.collateral_value_usd - opportunity.position.debt_value_usd) - tip_usd;
        
        if estimated_profit_usd < 1.0 { // Minimum 1$ profit
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

        // 4a. Build Kamino Liquidation Instruction (Placeholder)
        // In prod, this would call build_kamino_liquidate_ix(&opportunity)
        
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
            Ok(bundle_id) => println!("[hunter] Bundle sent successfully! ID: {}", bundle_id),
            Err(e) => eprintln!("[hunter] Failed to send bundle: {}", e),
        }

        Ok(())
    }
}
