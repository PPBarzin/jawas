/// Static token catalogue for Phase 1 decimal normalization.
///
/// Prices are NO LONGER included here to avoid data corruption in Phase 1.
/// Real-time oracles (Pyth/Jupiter) will be used in Phase 2.
pub struct TokenInfo {
    pub symbol: &'static str,
    /// Number of decimal places for this token's native unit.
    pub decimals: u8,
}

/// Look up token metadata by mint address.
/// Returns `None` for unknown mints.
pub fn token_info(mint: &str) -> Option<TokenInfo> {
    match mint {
        // ── Stablecoins ───────────────────────────────────────────────────────
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => Some(TokenInfo {
            symbol: "USDC",
            decimals: 6,
        }),
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => Some(TokenInfo {
            symbol: "USDT",
            decimals: 6,
        }),
        // ── SOL and Liquid Staking Tokens ─────────────────────────────────────
        "So11111111111111111111111111111111111111112" => Some(TokenInfo {
            symbol: "SOL",
            decimals: 9,
        }),
        "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn" => Some(TokenInfo {
            symbol: "JitoSOL",
            decimals: 9,
        }),
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" => Some(TokenInfo {
            symbol: "mSOL",
            decimals: 9,
        }),
        "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1" => Some(TokenInfo {
            symbol: "bSOL",
            decimals: 9,
        }),
        // ── Other top Kamino collaterals ─────────────────────────────────────
        "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263" => Some(TokenInfo {
            symbol: "BONK",
            decimals: 5,
        }),
        "EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm" => Some(TokenInfo {
            symbol: "WIF",
            decimals: 6,
        }),
        _ => None,
    }
}

/// Convert a native (integer) token amount to a "human-readable" decimal amount.
///
/// Returns `None` if the mint is not in the catalogue (unknown decimals).
pub fn native_to_human(native_amount: u64, mint: &str) -> Option<f64> {
    let info = token_info(mint)?;
    Some(native_amount as f64 / 10f64.powi(info.decimals as i32))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usdc_normalization() {
        // 5_000_000 native USDC (6 decimals) → 5.0
        let val = native_to_human(5_000_000, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
            .unwrap();
        assert!((val - 5.0).abs() < 1e-9, "got {}", val);
    }

    #[test]
    fn sol_normalization() {
        // 1_000_000_000 lamports → 1.0 SOL
        let val =
            native_to_human(1_000_000_000, "So11111111111111111111111111111111111111112").unwrap();
        assert!((val - 1.0).abs() < 1e-9, "got {}", val);
    }

    #[test]
    fn unknown_mint_returns_none() {
        assert!(native_to_human(1_000_000, "UnknownMint111111111111111111111111111111").is_none());
    }
}
