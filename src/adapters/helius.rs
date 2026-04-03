use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient as SolanaRpcClient;

use crate::ports::rpc::RpcClient;

pub struct HeliusAdapter {
    client: SolanaRpcClient,
}

impl HeliusAdapter {
    /// Constructs the adapter from a raw URL string.
    /// Trailing slashes are stripped to avoid double-slash issues with QuickNode URLs.
    pub fn new(rpc_url: &str) -> Self {
        let url = rpc_url.trim_end_matches('/').to_string();
        Self {
            client: SolanaRpcClient::new(url),
        }
    }
}

impl RpcClient for HeliusAdapter {
    async fn get_version(&self) -> Result<String> {
        let version = self
            .client
            .get_version()
            .context("Failed to reach Solana RPC — check RPC_URL")?;
        Ok(version.solana_core)
    }
}
