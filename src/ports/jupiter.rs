use anyhow::Result;
use async_trait::async_trait;
use solana_sdk::instruction::Instruction;

#[derive(Debug, Clone)]
pub struct QuoteResponse {
    pub in_amount: u64,
    pub out_amount: u64,
    pub price_impact_pct: f64,
    pub swap_transaction: Option<String>, // Base64 encoded transaction if we want Jupiter to build it
}

#[async_trait]
pub trait JupiterPort: Send + Sync {
    /// Gets a swap quote from Jupiter API.
    async fn get_quote(
        &self,
        input_mint: &str,
        output_mint: &str,
        amount: u64,
        slippage_bps: u16,
    ) -> Result<QuoteResponse>;

    /// Builds a swap instruction to be included in an atomic transaction.
    async fn build_swap_instructions(
        &self,
        input_mint: &str,
        output_mint: &str,
        amount: u64,
        slippage_bps: u16,
        user_public_key: &str,
    ) -> Result<Vec<Instruction>>;
}
