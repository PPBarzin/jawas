use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

/// Simplified Pubkey for domain (avoids solana-sdk dependency in domain)
pub type DomainPubkey = [u8; 32];

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct LastUpdate {
    pub slot: u64,
    pub stale: u8,
    pub padding: [u8; 7],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct BigFractionBytes {
    pub value: [u64; 4],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct ObligationCollateral {
    pub deposit_reserve: DomainPubkey,
    pub deposited_amount: u64,
    pub market_value_sf: u128,
    pub padding: [u64; 10],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct ObligationLiquidity {
    pub borrow_reserve: DomainPubkey,
    pub cumulative_borrow_rate_bsf: BigFractionBytes,
    pub borrowed_amount_sf: u128,
    pub market_value_sf: u128,
    pub borrow_factor_adjusted_market_value_sf: u128,
    pub padding2: [u64; 8],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct Obligation {
    pub tag: u64,
    pub last_update: LastUpdate,
    pub lending_market: DomainPubkey,
    pub owner: DomainPubkey,
    pub deposits: [ObligationCollateral; 8],
    pub deposited_value_sf: u128,
    pub borrows: [ObligationLiquidity; 5],
    pub borrow_factor_adjusted_debt_value_sf: u128,
    pub borrowed_assets_market_value_sf: u128,
    pub allowed_borrow_value_sf: u128,
    pub unhealthy_borrow_value_sf: u128,
    pub elevation_group: u8,
    pub padding: [u64; 51], // Estimated padding to match account size
}

impl Obligation {
    /// Calculate current LTV (Loan-to-Value)
    /// LTV = (Adjusted Debt Value) / (Deposited Value)
    pub fn current_ltv(&self) -> f64 {
        if self.deposited_value_sf == 0 {
            return f64::INFINITY;
        }
        (self.borrow_factor_adjusted_debt_value_sf as f64) / (self.deposited_value_sf as f64)
    }

    /// Check if the obligation is liquidatable
    pub fn is_liquidatable(&self) -> bool {
        self.borrow_factor_adjusted_debt_value_sf >= self.unhealthy_borrow_value_sf && self.unhealthy_borrow_value_sf > 0
    }
}
