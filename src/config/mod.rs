use crate::application::observer::Protocol;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub observer: RpcConfig,
    pub hunter: HunterConfig,
    pub airtable: AirtableConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Debug, Clone)]
pub struct RpcConfig {
    pub rpc_url: String,
    pub ws_url: String,
    pub tx_commitment: String,
}

#[derive(Debug, Clone)]
pub struct HunterConfig {
    pub rpc_url: String,
    pub ws_url: String,
    pub tx_commitment: String,
    pub keypair_path: Option<String>,
    pub jito_url: String,
    pub max_repay_usd: f64,
    pub wallet_toml_path: String,
    pub replay_enabled: bool,
    pub replay_signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AirtableConfig {
    pub token: String,
    pub base_id: String,
    pub watch_table: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub target_protocol: String,
    pub enable_hunter: bool,
    pub enable_observer: bool,
}

impl RuntimeConfig {
    pub fn protocol(&self) -> Protocol {
        match self.target_protocol.to_uppercase().as_str() {
            "SOLEND" | "SAVE" => Protocol::Solend,
            _ => Protocol::Kamino,
        }
    }
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            observer: RpcConfig {
                rpc_url: required_env_with_fallback("OBSERVER_RPC_URL", "RPC_URL")?,
                ws_url: required_env_with_fallback("OBSERVER_WS_URL", "WS_URL")?,
                tx_commitment: env_string("OBSERVER_TX_COMMITMENT", "confirmed"),
            },
            hunter: HunterConfig {
                rpc_url: required_env_with_fallback("HUNTER_RPC_URL", "RPC_URL")?,
                ws_url: required_env_with_fallback("HUNTER_WS_URL", "WS_URL")?,
                tx_commitment: env_string("HUNTER_TX_COMMITMENT", "confirmed"),
                keypair_path: std::env::var("SOLANA_KEYPAIR_PATH").ok(),
                jito_url: env_string(
                    "JITO_URL",
                    "https://mainnet.block-engine.jito.wtf/api/v1/bundles",
                ),
                max_repay_usd: env_string("MAX_REPAY_USD", "300.0")
                    .parse::<f64>()
                    .unwrap_or(300.0),
                wallet_toml_path: env_string("WALLET_TOML_PATH", "wallet.toml"),
                replay_enabled: env_flag("HUNTER_REPLAY", false),
                replay_signature: std::env::var("HUNTER_REPLAY_SIGNATURE").ok(),
            },
            airtable: AirtableConfig {
                token: required_env("AIRTABLE_TOKEN")?,
                base_id: required_env("AIRTABLE_BASE_ID")?,
                watch_table: env_string("AIRTABLE_TABLE_WATCH", "Jawas-Watch"),
            },
            runtime: RuntimeConfig {
                target_protocol: env_string("TARGET_PROTOCOL", "KAMINO"),
                enable_hunter: env_flag("ENABLE_HUNTER", true),
                enable_observer: env_flag("ENABLE_OBSERVER", true),
            },
        })
    }
}

pub fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

pub fn env_string(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn required_env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| anyhow::anyhow!("{name} must be set"))
}

fn required_env_with_fallback(primary: &str, fallback: &str) -> anyhow::Result<String> {
    std::env::var(primary)
        .or_else(|_| std::env::var(fallback))
        .map_err(|_| anyhow::anyhow!("{primary} or {fallback} must be set"))
}

#[cfg(test)]
mod tests {
    use super::env_flag;

    #[test]
    fn env_flag_handles_truthy_and_default_values() {
        unsafe { std::env::set_var("JAWAS_TEST_FLAG", "yes") };
        assert!(env_flag("JAWAS_TEST_FLAG", false));

        unsafe { std::env::remove_var("JAWAS_TEST_FLAG") };
        assert!(!env_flag("JAWAS_TEST_FLAG", false));
        assert!(env_flag("JAWAS_TEST_FLAG", true));
    }
}
