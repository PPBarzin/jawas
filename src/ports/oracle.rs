use anyhow::Result;

/// Port (interface) for fetching current token prices in USD.
#[allow(async_fn_in_trait)]
pub trait PriceOracle: Send + Sync {
    /// Returns the current price of the given token mint in USD.
    /// If price is unavailable, returns a Success with 0.0 or an Error.
    async fn get_price_usd(&self, mint: &str) -> Result<f64>;
}
