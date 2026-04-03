use serde::{Deserialize, Serialize};
use crate::domain::position::Position;

/// A liquidation opportunity detected on-chain.
/// Pure domain struct — zero external dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationOpportunity {
    pub position: Position,
    /// Kamino liquidation threshold (e.g. 0.85 for 85% LTV)
    pub liquidation_threshold: f64,
    /// Bonus percentage awarded to the liquidator (e.g. 0.05 for 5%)
    pub bonus_pct: f64,
    /// Unix timestamp (ms) when the opportunity was detected
    pub detected_at_ms: u64,
}
