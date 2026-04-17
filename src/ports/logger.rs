use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Data logged for every liquidation observed (Phase 1 — watch mode).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationEvent {
    pub timestamp: String,
    pub signature: String,
    pub protocol: String,
    pub market: String,
    pub liquidated_user: String,
    pub liquidator: String,
    pub repay_mint: String,
    pub withdraw_mint: String,
    pub repay_symbol: String,
    pub withdraw_symbol: String,
    pub repay_amount: f64,
    pub withdraw_amount: f64,
    /// Estimated USD values (legacy/Phase 2). Will be 0.0 in Phase 1 without Oracle.
    pub repaid_usd: f64,
    pub withdrawn_usd: f64,
    pub profit_usd: f64,
    pub delay_ms: u64,
    pub competing_bots: u32,
    pub status: String,
}

/// Port (interface) for logging liquidation events.
/// Adapters must implement this trait; services depend only on this abstraction.
#[allow(async_fn_in_trait)]
pub trait LiquidationLogger: Send + Sync {
    /// Log a liquidation observed on-chain (Phase 1).
    async fn log_observation(&self, event: &ObservationEvent) -> Result<()>;
}
