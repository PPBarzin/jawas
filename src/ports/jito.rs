use anyhow::Result;
use async_trait::async_trait;
use solana_sdk::transaction::VersionedTransaction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JitoBundleStatus {
    pub bundle_id: String,
    pub status: String,
    pub landed_slot: Option<u64>,
}

#[async_trait]
pub trait JitoPort: Send + Sync {
    /// Sends a bundle of transactions to Jito Block Engine.
    /// Returns the bundle ID or an error.
    async fn send_bundle(&self, transactions: Vec<VersionedTransaction>) -> Result<String>;

    /// Returns inflight status information for bundles previously accepted by Jito.
    async fn get_inflight_bundle_statuses(
        &self,
        bundle_ids: Vec<String>,
    ) -> Result<Vec<JitoBundleStatus>>;

    /// Returns the current tip recommendation (in lamports) for a given percentile.
    async fn get_tip_recommendation(&self) -> Result<u64>;
}
