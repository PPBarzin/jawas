use crate::domain::token::token_info;
use crate::ports::oracle::PriceOracle;
use anyhow::Result;
use dashmap::DashMap;
use reqwest::Client;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct SimplePriceOracle {
    client: Client,
    base_url: String,
    quote_mint: String,
    cache_ttl: Duration,
    cache: Arc<DashMap<String, CachedPrice>>,
}

#[derive(Debug, Clone)]
struct CachedPrice {
    price_usd: f64,
    fetched_at: Instant,
}

impl SimplePriceOracle {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url
                .unwrap_or("https://quote-api.jup.ag/v6")
                .to_string(),
            quote_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string(),
            cache_ttl: Duration::from_secs(30),
            cache: Arc::new(DashMap::new()),
        }
    }

    async fn fetch_jupiter_price_usd(&self, mint_or_reserve: &str) -> Result<Option<f64>> {
        let info = match token_info(mint_or_reserve) {
            Some(info) => info,
            None => return Ok(None),
        };
        if matches!(info.symbol, "USDC" | "USDT" | "USDG") {
            return Ok(Some(1.0));
        }

        let amount = 10u64.saturating_pow(info.decimals as u32);
        if amount == 0 {
            return Ok(None);
        }

        let mint = normalize_mint(mint_or_reserve);
        let response = self
            .client
            .get(format!("{}/quote", self.base_url))
            .query(&[
                ("inputMint", mint),
                ("outputMint", self.quote_mint.as_str()),
                ("amount", &amount.to_string()),
                ("slippageBps", "50"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        let out_amount = response["outAmount"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
            .or_else(|| response["outAmount"].as_u64().map(|value| value as f64));

        Ok(out_amount.map(|amount_out| amount_out / 1_000_000.0))
    }
}

impl PriceOracle for SimplePriceOracle {
    async fn get_price_usd(&self, mint_or_reserve: &str) -> Result<f64> {
        if let Some(cached) = self.cache.get(mint_or_reserve) {
            if cached.fetched_at.elapsed() <= self.cache_ttl {
                return Ok(cached.price_usd);
            }
        }

        if let Some(price) = self.fetch_jupiter_price_usd(mint_or_reserve).await? {
            self.cache.insert(
                mint_or_reserve.to_string(),
                CachedPrice {
                    price_usd: price,
                    fetched_at: Instant::now(),
                },
            );
            return Ok(price);
        }

        Ok(static_fallback_price(mint_or_reserve))
    }
}

fn normalize_mint(mint_or_reserve: &str) -> &str {
    match mint_or_reserve {
        "8K9WC8xoh2rtQNY7iEGXtPvfbDCi563SdWhCAhuMP2xE" => {
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        }
        "8739Sstz9LueAnSgpKbaL6Z8atY6YdZPyv7mB7U75JAs" => {
            "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB"
        }
        "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36"
        | "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw" => {
            "So11111111111111111111111111111111111111112"
        }
        "6757fL8Y2Nf86QWp86Z99tWhUshAonqWfNnEAn85BPh" => {
            "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn"
        }
        other => other,
    }
}

fn static_fallback_price(mint_or_reserve: &str) -> f64 {
    let symbol = token_info(mint_or_reserve)
        .map(|info| info.symbol)
        .unwrap_or("UNKNOWN");
    match symbol {
        "USDC" | "USDT" | "USDG" => 1.0,
        "SOL" | "WSOL" => 145.0,
        "JitoSOL" => 165.0,
        "mSOL" => 170.0,
        "bSOL" => 165.0,
        "WIF" => 2.5,
        "BONK" => 0.000025,
        "tBTC" => 65_000.0,
        _ => 0.0,
    }
}
