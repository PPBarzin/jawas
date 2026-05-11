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
        self.borrow_factor_adjusted_debt_value_sf >= self.unhealthy_borrow_value_sf
            && self.unhealthy_borrow_value_sf > 0
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

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct PriceHeuristic {
    pub lower: u64,
    pub upper: u64,
    pub exp: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ScopeConfiguration {
    pub price_feed: DomainPubkey,
    pub price_chain: [u16; 4],
    pub twap_chain: [u16; 4],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SwitchboardConfiguration {
    pub price_aggregator: DomainPubkey,
    pub twap_aggregator: DomainPubkey,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct PythConfiguration {
    pub price: DomainPubkey,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct TokenInfo {
    pub name: [u8; 32],
    pub heuristic: PriceHeuristic,
    pub max_twap_divergence_bps: u64,
    pub max_age_price_seconds: u64,
    pub max_age_twap_seconds: u64,
    pub scope_configuration: ScopeConfiguration,
    pub switchboard_configuration: SwitchboardConfiguration,
    pub pyth_configuration: PythConfiguration,
    pub block_price_usage: u8,
    pub reserved: [u8; 7],
    pub padding: [u64; 19],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveFees {
    pub origination_fee_sf: u64,
    pub flash_loan_fee_sf: u64,
    pub padding: [u8; 8],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CurvePoint {
    pub utilization_rate_bps: u32,
    pub borrow_rate_bps: u32,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct BorrowRateCurve {
    pub points: [CurvePoint; 11],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct WithdrawalCaps {
    pub config_capacity: i64,
    pub current_total: i64,
    pub last_interval_start_timestamp: u64,
    pub config_interval_length_seconds: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveConfig {
    pub status: u8,
    pub padding_deprecated_asset_tier: u8,
    pub host_fixed_interest_rate_bps: u16,
    pub min_deleveraging_bonus_bps: u16,
    pub block_ctoken_usage: u8,
    pub reserved1: [u8; 6],
    pub protocol_order_execution_fee_pct: u8,
    pub protocol_take_rate_pct: u8,
    pub protocol_liquidation_fee_pct: u8,
    pub loan_to_value_pct: u8,
    pub liquidation_threshold_pct: u8,
    pub min_liquidation_bonus_bps: u16,
    pub max_liquidation_bonus_bps: u16,
    pub bad_debt_liquidation_bonus_bps: u16,
    pub deleveraging_margin_call_period_secs: u64,
    pub deleveraging_threshold_decrease_bps_per_day: u64,
    pub fees: ReserveFees,
    pub borrow_rate_curve: BorrowRateCurve,
    pub borrow_factor_pct: u64,
    pub deposit_limit: u64,
    pub borrow_limit: u64,
    pub token_info: TokenInfo,
    pub deposit_withdrawal_cap: WithdrawalCaps,
    pub debt_withdrawal_cap: WithdrawalCaps,
    pub elevation_groups: [u8; 20],
    pub disable_usage_as_coll_outside_emode: u8,
    pub utilization_limit_block_borrowing_above_pct: u8,
    pub autodeleverage_enabled: u8,
    pub proposer_authority_locked: u8,
    pub borrow_limit_outside_elevation_group: u64,
    pub borrow_limit_against_this_collateral_in_elevation_group: [u64; 32],
    pub deleveraging_bonus_increase_bps_per_day: u64,
    pub debt_maturity_timestamp: u64,
    pub debt_term_seconds: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveLiquidity {
    pub mint_pubkey: DomainPubkey,
    pub supply_vault: DomainPubkey,
    pub fee_vault: DomainPubkey,
    pub total_available_amount: u64,
    pub borrowed_amount_sf: u128,
    pub market_price_sf: u128,
    pub market_price_last_updated_ts: u64,
    pub mint_decimals: u64,
    pub deposit_limit_crossed_timestamp: u64,
    pub borrow_limit_crossed_timestamp: u64,
    pub cumulative_borrow_rate_bsf: BigFractionBytes,
    pub accumulated_protocol_fees_sf: u128,
    pub accumulated_referrer_fees_sf: u128,
    pub pending_referrer_fees_sf: u128,
    pub absolute_referral_rate_sf: u128,
    pub token_program: DomainPubkey,
    pub padding2: [u64; 51],
    pub padding3: [u128; 32],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveCollateral {
    pub mint_pubkey: DomainPubkey,
    pub mint_total_supply: u64,
    pub supply_vault: DomainPubkey,
    pub padding1: [u128; 32],
    pub padding2: [u128; 32],
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct WithdrawQueue {
    pub queued_collateral_amount: u64,
    pub next_issued_ticket_sequence_number: u64,
    pub next_withdrawable_ticket_sequence_number: u64,
}

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct Reserve {
    pub version: u64,
    pub last_update: LastUpdate,
    pub lending_market: DomainPubkey,
    pub farm_collateral: DomainPubkey,
    pub farm_debt: DomainPubkey,
    pub liquidity: ReserveLiquidity,
    pub reserve_liquidity_padding: [u64; 150],
    pub collateral: ReserveCollateral,
    pub reserve_collateral_padding: [u64; 150],
    pub config: ReserveConfig,
    pub config_padding: [u64; 114],
    pub borrowed_amount_outside_elevation_group: u64,
    pub borrowed_amounts_against_this_reserve_in_elevation_groups: [u64; 32],
    pub withdraw_queue: WithdrawQueue,
    pub padding: [u64; 204],
}
