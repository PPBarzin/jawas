use anyhow::Result;
use crate::ports::oracle::PriceOracle;
use crate::domain::token::token_info;

/// A simple, hardcoded price oracle for Phase 1.
///
/// In Phase 2, this will be replaced or supplemented by a real-time
/// oracle adapter (Pyth, Jupiter, etc.).
#[derive(Clone)]
pub struct SimplePriceOracle;

impl SimplePriceOracle {
    pub fn new() -> Self {
        Self
    }
}

impl PriceOracle for SimplePriceOracle {
    async fn get_price_usd(&self, mint_or_reserve: &str) -> Result<f64> {
        let symbol = token_info(mint_or_reserve).map(|i| i.symbol).unwrap_or("UNKNOWN");
        
        let price = match symbol {
            // USDC / USDT
            "USDC" | "USDT" => 1.0,
            // SOL
            "SOL" | "WSOL" => 145.0,
            // JitoSOL
            "JitoSOL" => 165.0,
            // mSOL
            "mSOL" => 170.0,
            // bSOL
            "bSOL" => 165.0,
            // WIF
            "WIF" => 2.5,
            // BONK (approx 0.000025)
            "BONK" => 0.000025,
            // USDG (approx 1.0)
            "USDG" => 1.0,
            // tBTC (approx 65k)
            "tBTC" => 65000.0,
            // Fallback for unknown mints
            _ => 0.0,
        };
        Ok(price)
    }
}
