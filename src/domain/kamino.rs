use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

/// Simplified Pubkey for domain (avoids solana-sdk dependency in domain)
pub type DomainPubkey = [u8; 32];

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct LastUpdate {
    pub slot: u64,
    pub stale: u8,
    pub price_status: u8,
    pub placeholder: [u8; 6],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct BigFractionBytes {
    pub value: [u64; 4],
    pub padding: [u64; 2],
}

// Borsh size: 32 + 8 + 16 + 80 = 136 bytes
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct ObligationCollateral {
    pub deposit_reserve: DomainPubkey,
    pub deposited_amount: u64,
    pub market_value_sf: u128,
    pub borrowed_amount_against_this_collateral_in_elevation_group: u64,
    pub padding: [u64; 9],
}

// Borsh size: 32 + 48 + 8 + 16 + 16 + 16 + 8 + 16 + 8 + 32 = 200 bytes
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct ObligationLiquidity {
    pub borrow_reserve: DomainPubkey,
    pub cumulative_borrow_rate_bsf: BigFractionBytes,
    pub last_borrowed_at_timestamp: u64,
    pub borrowed_amount_sf: u128,
    pub market_value_sf: u128,
    pub borrow_factor_adjusted_market_value_sf: u128,
    pub borrowed_amount_outside_elevation_groups: u64,
    pub fixed_term_borrow_rollover_config: FixedTermBorrowRolloverConfig,
    pub borrowed_amount_at_expiration: u64,
    pub padding2: [u64; 4],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct FixedTermBorrowRolloverConfig {
    pub auto_rollover_enabled: u8,
    pub open_term_allowed: u8,
    pub migration_to_fixed_enabled: u8,
    pub alignment_padding: [u8; 1],
    pub max_borrow_rate_bps: u32,
    pub min_debt_term_seconds: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct ObligationOrder {
    pub condition_threshold_sf: u128,
    pub opportunity_parameter_sf: u128,
    pub min_execution_bonus_bps: u16,
    pub max_execution_bonus_bps: u16,
    pub condition_type: u8,
    pub opportunity_type: u8,
    pub padding1: [u8; 10],
    pub padding2: [u128; 5],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
pub struct BorrowOrder {
    pub debt_liquidity_mint: DomainPubkey,
    pub remaining_debt_amount: u64,
    pub filled_debt_destination: DomainPubkey,
    pub min_debt_term_seconds: u64,
    pub fillable_until_timestamp: u64,
    pub placed_at_timestamp: u64,
    pub last_updated_at_timestamp: u64,
    pub requested_debt_amount: u64,
    pub max_borrow_rate_bps: u32,
    pub active: u8,
    pub enable_auto_rollover_on_filled_borrows: u8,
    pub padding1: [u8; 2],
    pub end_padding: [u64; 5],
}

// Matches the current Kamino IDL account layout.
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct Obligation {
    pub tag: u64,
    pub last_update: LastUpdate,
    pub lending_market: DomainPubkey,
    pub owner: DomainPubkey,
    pub deposits: [ObligationCollateral; 8],
    pub lowest_reserve_deposit_liquidation_ltv: u64,
    pub deposited_value_sf: u128,
    pub borrows: [ObligationLiquidity; 5],
    pub borrow_factor_adjusted_debt_value_sf: u128,
    pub borrowed_assets_market_value_sf: u128,
    pub allowed_borrow_value_sf: u128,
    pub unhealthy_borrow_value_sf: u128,
    pub padding_deprecated_asset_tiers: [u8; 13],
    pub elevation_group: u8,
    pub num_of_obsolete_deposit_reserves: u8,
    pub has_debt: u8,
    pub referrer: DomainPubkey,
    pub borrowing_disabled: u8,
    pub autodeleverage_target_ltv_pct: u8,
    pub lowest_reserve_deposit_max_ltv_pct: u8,
    pub num_of_obsolete_borrow_reserves: u8,
    pub reserved: [u8; 4],
    pub highest_borrow_factor_pct: u64,
    pub autodeleverage_margin_call_started_timestamp: u64,
    pub obligation_orders: [ObligationOrder; 2],
    pub borrow_order: BorrowOrder,
    pub padding3: [u64; 73],
}

impl Obligation {
    pub const SCALE_FACTOR: f64 = 1e18;

    pub fn sf_to_f64(value: u128) -> f64 {
        (value as f64) / Self::SCALE_FACTOR
    }

    pub fn deposited_value_usd(&self) -> f64 {
        Self::sf_to_f64(self.deposited_value_sf)
    }

    pub fn debt_value_usd(&self) -> f64 {
        Self::sf_to_f64(self.borrowed_assets_market_value_sf)
    }

    pub fn adjusted_debt_value_usd(&self) -> f64 {
        Self::sf_to_f64(self.borrow_factor_adjusted_debt_value_sf)
    }

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

    pub fn max_ltv(&self) -> f64 {
        if self.deposited_value_sf == 0 {
            return f64::INFINITY;
        }
        (self.allowed_borrow_value_sf as f64) / (self.deposited_value_sf as f64)
    }

    pub fn unhealthy_ltv(&self) -> f64 {
        if self.deposited_value_sf == 0 {
            return f64::INFINITY;
        }
        (self.unhealthy_borrow_value_sf as f64) / (self.deposited_value_sf as f64)
    }

    pub fn dist_to_liq(&self) -> f64 {
        self.unhealthy_ltv() - self.current_ltv()
    }

    pub fn net_value_usd(&self) -> f64 {
        self.deposited_value_usd() - self.debt_value_usd()
    }
}
