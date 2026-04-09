use anyhow::Result;
use async_trait::async_trait;
use solana_sdk::transaction::VersionedTransaction;

#[async_trait]
pub trait JitoPort: Send + Sync {
    /// Sends a bundle of transactions to Jito Block Engine.
    /// Returns the bundle ID or an error.
    async fn send_bundle(&self, transactions: Vec<VersionedTransaction>) -> Result<String>;
    
    /// Returns the current tip recommendation (in lamports) for a given percentile.
    async fn get_tip_recommendation(&self) -> Result<u64>;
}
