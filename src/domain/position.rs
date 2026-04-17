use serde::{Deserialize, Serialize};

/// Represents a borrower's position on a lending protocol.
/// Pure domain logic — zero external dependencies.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub wallet: String,
    pub collateral_token: String,
    pub collateral_amount: f64,
    pub collateral_value_usd: f64,
    pub debt_token: String,
    pub debt_amount: f64,
    pub debt_value_usd: f64,
    pub last_updated_slot: u64,
}

impl Position {
    /// Loan-to-Value ratio given the current collateral price in USD.
    pub fn ltv(&self, collateral_price_usd: f64) -> f64 {
        let collateral_value = if self.collateral_value_usd > 0.0 {
            self.collateral_value_usd
        } else {
            self.collateral_amount * collateral_price_usd
        };

        if collateral_value == 0.0 {
            return f64::INFINITY;
        }

        let debt_value = if self.debt_value_usd > 0.0 {
            self.debt_value_usd
        } else {
            self.debt_amount // Assuming debt token is 1$ if not provided (simplified)
        };

        debt_value / collateral_value
    }

    /// How far (in LTV units) before reaching the liquidation threshold.
    pub fn distance_to_liquidation(&self, threshold: f64, collateral_price_usd: f64) -> f64 {
        threshold - self.ltv(collateral_price_usd)
    }
}
