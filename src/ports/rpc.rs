use anyhow::Result;

/// A bundle of logs for a single Solana transaction.
pub struct LogEntry {
    pub signature: String,
    pub logs: Vec<String>,
    pub is_error: bool,
    /// Unix timestamp in milliseconds when the WebSocket notification was received.
    pub received_at_ms: u64,
}

/// Account keys and instruction layout extracted from a confirmed transaction.
pub struct TransactionInfo {
    /// All account pubkeys in the transaction (base58), in order.
    pub account_keys: Vec<String>,
    /// For each instruction: indices into `account_keys` (accounts it uses).
    pub instruction_accounts: Vec<Vec<usize>>,
    /// For each instruction: index into `account_keys` identifying the program.
    pub instruction_programs: Vec<usize>,
    /// On-chain block time (Unix seconds) from getTransaction, if available.
    pub block_time: Option<u64>,
}

/// Minimal RPC port for Phase 1 (read-only).
/// Expanded in Phase 2 as needed.
#[allow(async_fn_in_trait)]
pub trait RpcClient: Send + Sync {
    /// Returns the node version string (used as a connectivity health-check).
    async fn get_version(&self) -> Result<String>;
    /// Returns account keys and instruction accounts for a confirmed transaction.
    async fn get_transaction(&self, signature: &str) -> Result<TransactionInfo>;
}

/// Port for real-time streaming of Solana logs.
#[allow(async_fn_in_trait)]
pub trait StreamingRpcClient: Send + Sync {
    /// Subscribe to logs for a specific program.
    /// Returns a receiver of per-transaction log bundles.
    async fn subscribe_to_logs(&self, program_id: &str) -> Result<tokio::sync::mpsc::Receiver<LogEntry>>;
}
