/// Static token catalogue for Phase 1 decimal normalization.
///
/// Prices are NO LONGER included here to avoid data corruption in Phase 1.
/// Real-time oracles (Pyth/Jupiter) will be used in Phase 2.
pub struct TokenInfo {
    pub symbol: &'static str,
    /// Number of decimal places for this token's native unit.
    pub decimals: u8,
}

/// Look up a token's mint address by its symbol.
/// Returns `None` for unknown symbols.
pub fn token_mint_by_symbol(symbol: &str) -> Option<&'static str> {
    match symbol {
        "USDC" => Some("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        "USDT" => Some("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB"),
        "SOL" | "WSOL" => Some("So11111111111111111111111111111111111111112"),
        "JitoSOL" | "JITOSOL" => Some("J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn"),
        "mSOL" => Some("mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So"),
        "bSOL" => Some("bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1"),
        "BONK" => Some("DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263"),
        "WIF" => Some("EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm"),
        "tBTC" => Some("6DNSN2BJsaPFdFFc1zP37kkeNe4Usc1Sqkzr9C9vPWcU"),
        "USDG" => Some("2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH"),
        _ => None,
    }
}

/// Look up token metadata by mint address or Solend reserve address.
/// Returns `None` for unknown mints/reserves.
pub fn token_info(mint_or_reserve: &str) -> Option<TokenInfo> {
    match mint_or_reserve {
        // ── Stablecoins ───────────────────────────────────────────────────────
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" | // USDC Mint
        "8K9WC8xoh2rtQNY7iEGXtPvfbDCi563SdWhCAhuMP2xE"   // Solend USDC Reserve
        => Some(TokenInfo {
            symbol: "USDC",
            decimals: 6,
        }),
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" | // USDT Mint
        "2p9S7YvU8o15H77799S7xH1L6N9C6n8T5X6n8T5X6n8T" | // Solend USDT Reserve (example)
        "8739Sstz9LueAnSgpKbaL6Z8atY6YdZPyv7mB7U75JAs"   // Solend USDT Reserve (Real)
        => Some(TokenInfo {
            symbol: "USDT",
            decimals: 6,
        }),
        // ── SOL and Liquid Staking Tokens ─────────────────────────────────────
        "So11111111111111111111111111111111111111112" | // SOL Mint
        "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36" | // Solend SOL Reserve
        "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw"   // Solend SOL Reserve (Turbo)
        => Some(TokenInfo {
            symbol: "SOL",
            decimals: 9,
        }),
        "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn" | // JitoSOL Mint
        "7v9ByZmcgp8iP9zG7m5U5W6zY6n8T5X6n8T5X6n8T5X6" | // Solend JitoSOL Reserve (example)
        "6757fL8Y2Nf86QWp86Z99tWhUshAonqWfNnEAn85BPh"   // Solend JitoSOL Reserve (Real)
        => Some(TokenInfo {
            symbol: "JitoSOL",
            decimals: 9,
        }),
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" | // mSOL Mint
        "CC98daE66SshF8S7799S7xH1L6N9C6n8T5X6n8T5X6n8T"   // Solend mSOL Reserve (example)
        => Some(TokenInfo {
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
        "6DNSN2BJsaPFdFFc1zP37kkeNe4Usc1Sqkzr9C9vPWcU" => Some(TokenInfo {
            symbol: "tBTC",
            decimals: 8,
        }),
        "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH" => Some(TokenInfo {
            symbol: "USDG",
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
