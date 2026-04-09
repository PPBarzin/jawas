use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait ConfigPort: Send + Sync {
    /// Fetches the list of active token symbols from the whitelist.
    async fn fetch_whitelist(&self) -> Result<Vec<String>>;
}
