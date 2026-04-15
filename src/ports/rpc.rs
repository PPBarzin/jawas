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
    /// For each instruction: raw serialized data bytes (base58-decoded).
    /// Used to copy competitor instructions verbatim when building our tx.
    pub instruction_data: Vec<Vec<u8>>,
    /// On-chain block time (Unix seconds) from getTransaction, if available.
    pub block_time: Option<u64>,
    /// Token balances before the transaction.
    pub pre_token_balances: Vec<TokenBalance>,
    /// Token balances after the transaction.
    pub post_token_balances: Vec<TokenBalance>,
}

#[derive(Debug, Clone)]
pub struct TokenBalance {
    pub account_index: usize,
    pub mint: String,
    pub owner: String,
    pub ui_amount: f64,
}

use solana_sdk::hash::Hash;

/// Minimal RPC port for Phase 1 (read-only).
/// Expanded in Phase 2 as needed.
pub trait RpcClient: Send + Sync {
    /// Returns the node version string (used as a connectivity health-check).
    fn get_version(&self) -> impl std::future::Future<Output = Result<String>> + Send;
    /// Returns account keys and instruction accounts for a confirmed transaction.
    fn get_transaction(&self, signature: &str) -> impl std::future::Future<Output = Result<TransactionInfo>> + Send;
    /// Returns the latest blockhash from the network.
    fn get_latest_blockhash(&self) -> impl std::future::Future<Output = Result<Hash>> + Send;
    /// Returns the raw data bytes of an account (base64-decoded).
    fn get_account_info(&self, pubkey: &str) -> impl std::future::Future<Output = Result<Vec<u8>>> + Send;
}

/// Port for real-time streaming of Solana logs.
#[allow(async_fn_in_trait)]
pub trait StreamingRpcClient: Send + Sync {
    /// Subscribe to logs for a specific program.
    /// Returns a receiver of per-transaction log bundles.
    async fn subscribe_to_logs(&self, program_id: &str) -> Result<tokio::sync::mpsc::Receiver<LogEntry>>;
}
