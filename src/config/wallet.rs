use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct WalletToken {
    pub symbol: String,
    pub mint: String,
    #[serde(default = "default_decimals")]
    pub decimals: u8,
    #[serde(default)]
    pub max_repay_native: u64,
}

#[derive(Debug, Deserialize)]
struct WalletConfig {
    #[serde(default)]
    tokens: Vec<WalletToken>,
}

pub fn load_wallet_tokens(path: &str) -> Result<Vec<WalletToken>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read wallet.toml at {}", path))?;
    let config: WalletConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse wallet.toml at {}", path))?;
    Ok(config.tokens)
}

const fn default_decimals() -> u8 {
    6
}

#[cfg(test)]
mod tests {
    use super::load_wallet_tokens;

    #[test]
    fn wallet_toml_supports_defaults_underscores_and_comments() {
        let temp_path = std::env::temp_dir().join(format!(
            "jawas-wallet-{}.toml",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        std::fs::write(
            &temp_path,
            r#"
[[tokens]]
symbol = "SOL"
mint = "So11111111111111111111111111111111111111112"
max_repay_native = 1_500_000_000 # 1.5 SOL

[[tokens]]
symbol = "USDC"
mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
decimals = 6
"#,
        )
        .unwrap();

        let tokens = load_wallet_tokens(temp_path.to_str().unwrap()).unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].decimals, 6);
        assert_eq!(tokens[0].max_repay_native, 1_500_000_000);
        assert_eq!(tokens[1].max_repay_native, 0);

        let _ = std::fs::remove_file(temp_path);
    }
}
