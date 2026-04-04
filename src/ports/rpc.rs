use anyhow::Result;

/// A bundle of logs for a single Solana transaction.
pub struct LogEntry {
    pub signature: String,
    pub logs: Vec<String>,
    pub is_error: bool,
    pub received_at: std::time::Instant,
}

/// Minimal RPC port for Phase 1 (read-only).
/// Expanded in Phase 2 as needed.
#[allow(async_fn_in_trait)]
pub trait RpcClient: Send + Sync {
    /// Returns the node version string (used as a connectivity health-check).
    async fn get_version(&self) -> Result<String>;
}

/// Port for real-time streaming of Solana logs.
#[allow(async_fn_in_trait)]
pub trait StreamingRpcClient: Send + Sync {
    /// Subscribe to logs for a specific program.
    /// Returns a receiver of per-transaction log bundles.
    async fn subscribe_to_logs(&self, program_id: &str) -> Result<tokio::sync::mpsc::Receiver<LogEntry>>;
}
