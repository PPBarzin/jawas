/// Gross profit from a liquidation before fees.
/// debt_to_repay in USD, bonus_pct as a decimal (e.g. 0.05 for 5%).
#[allow(dead_code)]
pub fn gross_profit(debt_to_repay: f64, bonus_pct: f64) -> f64 {
    debt_to_repay * bonus_pct
}

/// Net profit after Jito tip and swap fees.
#[allow(dead_code)]
pub fn net_profit(gross: f64, jito_tip: f64, swap_fee: f64) -> f64 {
    gross - jito_tip - swap_fee
}

/// Returns true if the net profit exceeds the configured minimum.
#[allow(dead_code)]
pub fn is_worth_it(net: f64, min_profit: f64) -> bool {
    net >= min_profit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gross_profit_basic() {
        // Repay $1000 with 5% bonus → $50 gross
        assert!((gross_profit(1000.0, 0.05) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn net_profit_deducts_fees() {
        let gross = gross_profit(1000.0, 0.05); // $50
        let net = net_profit(gross, 5.0, 2.0);  // -$7 in fees
        assert!((net - 43.0).abs() < 1e-9);
    }

    #[test]
    fn is_worth_it_above_threshold() {
        assert!(is_worth_it(43.0, 20.0));
    }

    #[test]
    fn is_worth_it_below_threshold() {
        assert!(!is_worth_it(10.0, 20.0));
    }
}
