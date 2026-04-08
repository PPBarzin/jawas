use anyhow::Result;
use crate::ports::oracle::PriceOracle;

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
    async fn get_price_usd(&self, mint: &str) -> Result<f64> {
        let price = match mint {
            // USDC / USDT
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" | "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => 1.0,
            // SOL
            "So11111111111111111111111111111111111111112" => 145.0,
            // JitoSOL
            "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn" => 165.0,
            // mSOL
            "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" => 170.0,
            // bSOL
            "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1" => 165.0,
            // WIF
            "EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm" => 2.5,
            // BONK (approx 0.000025)
            "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263" => 0.000025,
            // Fallback for unknown mints
            _ => 0.0,
        };
        Ok(price)
    }
}
