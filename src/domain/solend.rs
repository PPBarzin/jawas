/// Solend obligation account decoder.
///
/// Layout based on solend-program v1/v2 (Borsh serialization).
/// Decimal = u192 stored as [u64; 3], WAD = 10^18.

const WAD: u128 = 1_000_000_000_000_000_000;
const MAX_OBLIGATION_RESERVES: usize = 10;

/// u192 fixed-point with WAD = 10^18.
/// Stored as little-endian [lo: u64, mid: u64, hi: u64].
#[derive(Debug, Clone, Copy, Default)]
pub struct Decimal {
    lo: u64,
    mid: u64,
    hi: u64,
}

impl Decimal {
    pub fn to_u128(&self) -> u128 {
        (self.lo as u128) | ((self.mid as u128) << 64)
        // hi is beyond u128 range; for obligation values it's always 0
    }

    /// Returns the value as f64 (divided by WAD).
    pub fn to_f64(&self) -> f64 {
        self.to_u128() as f64 / WAD as f64
    }

    pub fn is_zero(&self) -> bool {
        self.lo == 0 && self.mid == 0 && self.hi == 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct ObligationCollateral {
    pub deposit_reserve: [u8; 32],
    pub deposited_amount: u64,
    pub market_value: Decimal,
}

#[derive(Debug, Clone, Default)]
pub struct ObligationLiquidity {
    pub borrow_reserve: [u8; 32],
    pub cumulative_borrow_rate_wads: Decimal,
    pub borrowed_amount_wads: Decimal,
    pub market_value: Decimal,
}

#[derive(Debug, Clone)]
pub struct SolendObligation {
    pub lending_market: [u8; 32],
    pub owner: [u8; 32],
    pub deposits: Vec<ObligationCollateral>,
    pub borrows: Vec<ObligationLiquidity>,
    pub deposited_value: Decimal,
    pub borrowed_value: Decimal,
    pub allowed_borrow_value: Decimal,
    pub unhealthy_borrow_value: Decimal,
}

impl SolendObligation {
    /// Returns true if the obligation can be liquidated right now.
    pub fn is_liquidatable(&self) -> bool {
        if self.unhealthy_borrow_value.is_zero() {
            return false;
        }
        self.borrowed_value.to_u128() >= self.unhealthy_borrow_value.to_u128()
    }

    /// Max amount repayable in a single liquidation call (Solend: 20% of borrowed value, WAD units).
    pub fn max_repay_wads(&self) -> u128 {
        self.borrowed_value.to_u128() / 5
    }
}

// ── Manual Borsh-style parser ─────────────────────────────────────────────────

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u8(&mut self) -> Option<u8> {
        let v = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn read_u64(&mut self) -> Option<u64> {
        let bytes: [u8; 8] = self.data.get(self.pos..self.pos + 8)?.try_into().ok()?;
        self.pos += 8;
        Some(u64::from_le_bytes(bytes))
    }

    fn read_u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.data.get(self.pos..self.pos + 4)?.try_into().ok()?;
        self.pos += 4;
        Some(u32::from_le_bytes(bytes))
    }

    fn read_pubkey(&mut self) -> Option<[u8; 32]> {
        let bytes: [u8; 32] = self.data.get(self.pos..self.pos + 32)?.try_into().ok()?;
        self.pos += 32;
        Some(bytes)
    }

    fn read_decimal(&mut self) -> Option<Decimal> {
        Some(Decimal {
            lo:  self.read_u64()?,
            mid: self.read_u64()?,
            hi:  self.read_u64()?,
        })
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        if self.pos + n > self.data.len() { return None; }
        self.pos += n;
        Some(())
    }
}

/// Decodes a raw Solend obligation account.
///
/// Byte layout:
///   1  version: u8
///   8  last_update.slot: u64
///   1  last_update.stale: u8 (bool)
///   7  last_update.padding
///  32  lending_market: Pubkey
///  32  owner: Pubkey
///   4  deposits.len: u32
///   N  deposits (each: 32 + 8 + 24 bytes = 64)
///   4  borrows.len: u32
///   M  borrows (each: 32 + 24 + 24 + 24 bytes = 104)
///  24  deposited_value: Decimal
///  24  borrowed_value: Decimal
///  24  allowed_borrow_value: Decimal
///  24  unhealthy_borrow_value: Decimal
pub fn decode_solend_obligation(data: &[u8]) -> Option<SolendObligation> {
    let mut c = Cursor::new(data);

    // version
    c.read_u8()?;

    // last_update: slot(8) + stale(1) + padding(7)
    c.skip(16)?;

    let lending_market = c.read_pubkey()?;
    let owner          = c.read_pubkey()?;

    // deposits
    let n_deposits = c.read_u32()? as usize;
    if n_deposits > MAX_OBLIGATION_RESERVES { return None; }
    let mut deposits = Vec::with_capacity(n_deposits);
    for _ in 0..n_deposits {
        let deposit_reserve  = c.read_pubkey()?;
        let deposited_amount = c.read_u64()?;
        let market_value     = c.read_decimal()?;
        deposits.push(ObligationCollateral { deposit_reserve, deposited_amount, market_value });
    }

    // borrows
    let n_borrows = c.read_u32()? as usize;
    if n_borrows > MAX_OBLIGATION_RESERVES { return None; }
    let mut borrows = Vec::with_capacity(n_borrows);
    for _ in 0..n_borrows {
        let borrow_reserve             = c.read_pubkey()?;
        let cumulative_borrow_rate_wads = c.read_decimal()?;
        let borrowed_amount_wads       = c.read_decimal()?;
        let market_value               = c.read_decimal()?;
        borrows.push(ObligationLiquidity {
            borrow_reserve,
            cumulative_borrow_rate_wads,
            borrowed_amount_wads,
            market_value,
        });
    }

    let deposited_value       = c.read_decimal()?;
    let borrowed_value        = c.read_decimal()?;
    let allowed_borrow_value  = c.read_decimal()?;
    let unhealthy_borrow_value = c.read_decimal()?;

    Some(SolendObligation {
        lending_market,
        owner,
        deposits,
        borrows,
        deposited_value,
        borrowed_value,
        allowed_borrow_value,
        unhealthy_borrow_value,
    })
}
