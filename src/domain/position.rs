use serde::{Deserialize, Serialize};

/// Represents a borrower's position on Kamino Finance.
/// Pure domain logic — zero external dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub wallet: String,
    pub collateral_token: String,
    pub collateral_amount: f64,
    pub debt_token: String,
    pub debt_amount: f64,
}

impl Position {
    /// Loan-to-Value ratio given the current collateral price in USD.
    pub fn ltv(&self, collateral_price_usd: f64) -> f64 {
        let collateral_value = self.collateral_amount * collateral_price_usd;
        if collateral_value == 0.0 {
            return f64::INFINITY;
        }
        self.debt_amount / collateral_value
    }

    /// How far (in LTV units) before reaching the liquidation threshold.
    /// Positive → still safe. Negative → already past threshold (liquidatable).
    pub fn distance_to_liquidation(&self, threshold: f64, collateral_price_usd: f64) -> f64 {
        threshold - self.ltv(collateral_price_usd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_position() -> Position {
        Position {
            wallet: "wallet123".to_string(),
            collateral_token: "SOL".to_string(),
            collateral_amount: 10.0,
            debt_token: "USDC".to_string(),
            debt_amount: 800.0,
        }
    }

    #[test]
    fn ltv_basic() {
        // 10 SOL @ $100 = $1000 collateral, $800 debt → 80% LTV
        let pos = sample_position();
        let ltv = pos.ltv(100.0);
        assert!((ltv - 0.8).abs() < 1e-9);
    }

    #[test]
    fn ltv_zero_collateral_is_infinite() {
        let mut pos = sample_position();
        pos.collateral_amount = 0.0;
        assert_eq!(pos.ltv(100.0), f64::INFINITY);
    }

    #[test]
    fn distance_positive_means_safe() {
        let pos = sample_position();
        // threshold 85%, LTV 80% → distance +5%
        let dist = pos.distance_to_liquidation(0.85, 100.0);
        assert!((dist - 0.05).abs() < 1e-9);
    }

    #[test]
    fn distance_negative_means_liquidatable() {
        let pos = sample_position();
        // threshold 75%, LTV 80% → distance -5%
        let dist = pos.distance_to_liquidation(0.75, 100.0);
        assert!((dist - (-0.05)).abs() < 1e-9);
    }
}
