use anyhow::Result;

/// Minimal RPC port for Phase 1 (read-only).
/// Expanded in Phase 2 as needed.
#[allow(async_fn_in_trait)]
pub trait RpcClient: Send + Sync {
    /// Returns the node version string (used as a connectivity health-check).
    async fn get_version(&self) -> Result<String>;
}
