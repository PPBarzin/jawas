use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Data logged for every liquidation observed (Phase 1 — watch mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationEvent {
    /// UTC timestamp string (ISO 8601)
    pub timestamp: String,
    pub borrower: String,
    pub collateral_token: String,
    pub collateral_amount: f64,
    pub debt_repaid_usdc: f64,
    pub profit_estimated_usd: f64,
    pub ltv_at_liquidation: f64,
    /// Milliseconds between LTV threshold crossing and execution
    pub delay_ms: u64,
    pub competing_bots: u32,
    pub winner_tx: String,
}

/// Port (interface) for logging liquidation events.
/// Adapters must implement this trait; services depend only on this abstraction.
#[allow(async_fn_in_trait)]
pub trait LiquidationLogger: Send + Sync {
    /// Log a liquidation observed on-chain (Phase 1).
    async fn log_observation(&self, event: &ObservationEvent) -> Result<()>;
}
