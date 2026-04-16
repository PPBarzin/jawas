use crate::ports::jito::JitoPort;
use crate::ports::jupiter::JupiterPort;
use crate::ports::oracle::PriceOracle;
use crate::ports::rpc::{RpcClient, StreamingRpcClient};
use crate::ports::config::ConfigPort;
use solana_sdk::signature::Keypair;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::message::VersionedMessage;
use solana_sdk::message::v0::Message;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::sysvar;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::collections::HashMap;

const JITO_TIP_ACCOUNT: &str = "96g9sAg9u3P7Q9ebKsC6SA47cySvnV6S1S1K6ssB1vD";
const KLEND_PROGRAM: &str    = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const LENDING_MARKET: &str   = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
const LENDING_MARKET_AUTHORITY: &str = "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo";
const TOKEN_PROGRAM: &str    = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str      = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const FARMS_PROGRAM: &str    = "FarmsPZpWu9i7Kky8tPN37rs2TpmMrAZrC7S7vJa91Hr";

// Solend
const SOLEND_PROGRAM: &str   = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const SOLEND_LIQUIDATE_FILTER: &str = "LiquidateWithoutReceivingCtokens";

// Kamino
const KAMINO_LIQUIDATE_FILTER: &str = "LiquidateObligationAndRedeemReserveCollateralV2";

/// Tip is refreshed every 60s.
const TIP_REFRESH_SECS: u64 = 60;
/// An obligation that was fired on within this window is skipped (prevents burst duplicates).
const OBLIGATION_DEDUP_MS: u128 = 3_000;

/// Token available in the hunter wallet (loaded from wallet.toml at startup).
#[derive(Debug, Clone)]
pub struct WalletToken {
    pub symbol: String,
    pub mint: String,
    pub decimals: u8,
    pub max_repay_native: u64,
}

/// Parses wallet.toml and returns the list of available tokens.
pub fn load_wallet_tokens(path: &str) -> Vec<WalletToken> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[hunter] wallet.toml not found at {}: {}", path, e);
            return vec![];
        }
    };

    let mut tokens = Vec::new();
    let mut current: Option<(String, String, u8, u64)> = None;

    for line in content.lines() {
        let line = line.trim();
        if line == "[[tokens]]" {
            if let Some((symbol, mint, decimals, max)) = current.take() {
                tokens.push(WalletToken { symbol, mint, decimals, max_repay_native: max });
            }
            current = Some((String::new(), String::new(), 6, 0));
            continue;
        }
        if let Some(ref mut t) = current {
            if let Some(v) = parse_toml_str(line, "symbol")   { t.0 = v; }
            if let Some(v) = parse_toml_str(line, "mint")     { t.1 = v; }
            if let Some(v) = parse_toml_u8(line, "decimals")  { t.2 = v; }
            if let Some(v) = parse_toml_u64(line, "max_repay_native") { t.3 = v; }
        }
    }
    if let Some((symbol, mint, decimals, max)) = current {
        tokens.push(WalletToken { symbol, mint, decimals, max_repay_native: max });
    }

    tokens
}

fn parse_toml_str(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{} =", key);
    if !line.starts_with(&prefix) { return None; }
    let rest = line[prefix.len()..].trim();
    Some(rest.trim_matches('"').to_string())
}

fn parse_toml_u8(line: &str, key: &str) -> Option<u8> {
    let prefix = format!("{} =", key);
    if !line.starts_with(&prefix) { return None; }
    line[prefix.len()..].trim().parse().ok()
}

fn parse_toml_u64(line: &str, key: &str) -> Option<u64> {
    let prefix = format!("{} =", key);
    if !line.starts_with(&prefix) { return None; }
    let clean: String = line[prefix.len()..].trim().chars().filter(|&c| c != '_').collect();
    let clean = clean.split('#').next().unwrap_or("").trim();
    clean.parse().ok()
}

// ── Helper functions for instruction building ────────────────────────────────

fn discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{}", name);
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

fn get_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let ata_program   = Pubkey::from_str(ATA_PROGRAM).unwrap();
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    ).0
}

fn ix_refresh_reserve(
    klend: &Pubkey,
    market: &Pubkey,
    reserve: &Pubkey,
) -> Instruction {
    let disc = discriminator("refresh_reserve");
    let accounts = vec![
        AccountMeta::new(*reserve, false),
        AccountMeta::new_readonly(*market, false),
        AccountMeta::new_readonly(*klend, false), // Pyth placeholder
        AccountMeta::new_readonly(*klend, false), // Switchboard Price placeholder
        AccountMeta::new_readonly(*klend, false), // Switchboard TWAP placeholder
    ];
    Instruction { program_id: *klend, accounts, data: disc.to_vec() }
}

fn ix_refresh_obligation(
    klend: &Pubkey,
    lending_market: &Pubkey,
    obligation: &Pubkey,
    reserves: &[&Pubkey],
) -> Instruction {
    let disc = discriminator("refresh_obligation");
    let mut accounts = vec![
        AccountMeta::new_readonly(*lending_market, false),
        AccountMeta::new(*obligation, false),
    ];
    for r in reserves {
        accounts.push(AccountMeta::new_readonly(**r, false));
    }
    Instruction { program_id: *klend, accounts, data: disc.to_vec() }
}

pub struct HunterService<R: RpcClient, JI: JitoPort, JU: JupiterPort, O: PriceOracle, C: ConfigPort + Clone> {
    hunter_rpc: R,
    jito: JI,
    _jupiter: JU,
    _oracle: O,
    _config: C,
    keypair: Arc<Keypair>,
    max_repay_usd: f64,
}

impl<R: RpcClient, JI: JitoPort, JU: JupiterPort, O: PriceOracle, C: ConfigPort + Clone + 'static> HunterService<R, JI, JU, O, C> {
    pub fn new(
        hunter_rpc: R,
        jito: JI,
        jupiter: JU,
        oracle: O,
        config: C,
        keypair: Arc<Keypair>,
        max_repay_usd: f64,
    ) -> Self {
        Self {
            hunter_rpc,
            jito,
            _jupiter: jupiter,
            _oracle: oracle,
            _config: config,
            keypair,
            max_repay_usd,
        }
    }

    // ── Kamino autonomous hunter ─────────────────────────────────────────────
    //
    // Flow:
    //   QuikNode WS notification (LiquidateObligationAndRedeemReserveCollateralV2)
    //   → getTransaction (single attempt, 500ms timeout)
    //   → extract obligation PDA + reserve addresses from competitor's tx accounts
    //   → build optimistic tx (RefreshReserve x2 + RefreshObligation + Liquidate)
    //     using pre-cached blockhash and tip
    //   → sendBundle (Jito)
    //
    // The observer is NOT involved in this cycle. It logs independently.
    pub async fn run_kamino(&self, wallet_tokens: Vec<WalletToken>) -> anyhow::Result<()>
    where
        R: StreamingRpcClient + RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        eprintln!(
            "[hunter-kamino] Starting autonomous hunter. Wallet: {} | max_repay: ${:.0} | tokens: {}",
            self.keypair.pubkey(),
            self.max_repay_usd,
            wallet_tokens.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>().join(", ")
        );

        // ── Hot cache: blockhash ─────────────────────────────────────────────
        let initial_blockhash = self.hunter_rpc.get_latest_blockhash().await
            .unwrap_or_default();
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(initial_blockhash));
        let blockhash_refresh_secs = std::env::var("BLOCKHASH_REFRESH_SECS")
            .unwrap_or_else(|_| "12".to_string())
            .parse::<u64>()
            .unwrap_or(12);

        {
            let rpc = self.hunter_rpc.clone();
            let bh = cached_blockhash.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(blockhash_refresh_secs)).await;
                    match rpc.get_latest_blockhash().await {
                        Ok(hash) => { *bh.write().await = hash; }
                        Err(e) => eprintln!("[hunter-kamino] blockhash refresh failed: {}", e),
                    }
                }
            });
        }

        // ── Hot cache: Jito tip ──────────────────────────────────────────────
        let initial_tip = self.jito.get_tip_recommendation().await.unwrap_or(100_000);
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(initial_tip));
        {
            let jito = self.jito.clone();
            let tip = cached_tip.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(TIP_REFRESH_SECS)).await;
                    if let Ok(t) = jito.get_tip_recommendation().await {
                        tip.store(t, Ordering::Relaxed);
                    }
                }
            });
        }

        // ── Obligation dedup: skip if fired within OBLIGATION_DEDUP_MS ───────
        let recent_obligations: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        // ── Main WS loop ─────────────────────────────────────────────────────
        loop {
            let mut rx = match self.hunter_rpc.subscribe_to_logs(KLEND_PROGRAM).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[hunter-kamino] WS subscribe failed: {}. Retrying in 2s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            eprintln!("[hunter-kamino] WebSocket connected.");

            loop {
                let entry = match rx.recv().await {
                    Some(e) => e,
                    None => {
                        eprintln!("[hunter-kamino] WS stream ended. Reconnecting...");
                        break;
                    }
                };

                // Ignore transactions that don't contain a liquidation call.
                if !entry.logs.iter().any(|l| l.contains(KAMINO_LIQUIDATE_FILTER)) {
                    continue;
                }

                // Spawn a task per event so the WS loop is never blocked.
                let sig         = entry.signature.clone();
                let rpc         = self.hunter_rpc.clone();
                let keypair     = self.keypair.clone();
                let jito        = self.jito.clone();
                let wt          = wallet_tokens.clone();
                let bh          = cached_blockhash.clone();
                let tip         = cached_tip.clone();
                let dedup       = recent_obligations.clone();
                let max_repay   = self.max_repay_usd;

                tokio::spawn(async move {
                    if let Err(e) = execute_kamino_opportunity(
                        sig, rpc, jito, keypair, wt, bh, tip, dedup, max_repay,
                    ).await {
                        eprintln!("[hunter-kamino] opportunity error: {}", e);
                    }
                });
            }
        }
    }

    // ── Solend autonomous hunter ─────────────────────────────────────────────
    //
    // Flow (identical spirit to Kamino):
    //   QuikNode WS notification (LiquidateWithoutReceivingCtokens)
    //   → getTransaction (single attempt, 500ms timeout)
    //   → copy competitor's refresh instructions verbatim
    //   → copy competitor's liquidate instruction, replacing user accounts
    //     with our own, and setting our own liquidity_amount
    //   → build tx with pre-cached blockhash + tip → sendBundle (Jito)
    //
    // No getAccountInfo, no obligation decode, no is_liquidatable() check.
    // Optimistic: include RefreshObligation in tx, let Solend decide on-chain.
    pub async fn run_solend(&self, wallet_tokens: Vec<WalletToken>) -> anyhow::Result<()>
    where
        R: StreamingRpcClient + RpcClient + Clone + Send + Sync + 'static,
        JI: Clone + Send + Sync + 'static,
    {
        eprintln!(
            "[hunter-solend] Starting autonomous hunter. Wallet: {} | tokens: {}",
            self.keypair.pubkey(),
            wallet_tokens.iter().map(|t| t.symbol.as_str()).collect::<Vec<_>>().join(", ")
        );

        // ── Hot cache: blockhash ─────────────────────────────────────────────
        let initial_blockhash = self.hunter_rpc.get_latest_blockhash().await
            .unwrap_or_default();
        let cached_blockhash = Arc::new(tokio::sync::RwLock::new(initial_blockhash));
        let blockhash_refresh_secs = std::env::var("BLOCKHASH_REFRESH_SECS")
            .unwrap_or_else(|_| "12".to_string())
            .parse::<u64>()
            .unwrap_or(12);

        {
            let rpc = self.hunter_rpc.clone();
            let bh = cached_blockhash.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(blockhash_refresh_secs)).await;
                    match rpc.get_latest_blockhash().await {
                        Ok(hash) => { *bh.write().await = hash; }
                        Err(e) => eprintln!("[hunter-solend] blockhash refresh failed: {}", e),
                    }
                }
            });
        }

        // ── Hot cache: Jito tip ──────────────────────────────────────────────
        let initial_tip = self.jito.get_tip_recommendation().await.unwrap_or(100_000);
        let cached_tip = Arc::new(std::sync::atomic::AtomicU64::new(initial_tip));
        {
            let jito = self.jito.clone();
            let tip = cached_tip.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(TIP_REFRESH_SECS)).await;
                    if let Ok(t) = jito.get_tip_recommendation().await {
                        tip.store(t, Ordering::Relaxed);
                    }
                }
            });
        }

        // ── Obligation dedup ─────────────────────────────────────────────────
        let recent_obligations: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        // ── Main WS loop ─────────────────────────────────────────────────────
        loop {
            let mut rx = match self.hunter_rpc.subscribe_to_logs(SOLEND_PROGRAM).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[hunter-solend] WS subscribe failed: {}. Retrying in 2s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            eprintln!("[hunter-solend] WebSocket connected.");

            loop {
                let entry = match rx.recv().await {
                    Some(e) => e,
                    None => {
                        eprintln!("[hunter-solend] WS stream ended. Reconnecting...");
                        break;
                    }
                };

                if !entry.logs.iter().any(|l| l.contains(SOLEND_LIQUIDATE_FILTER)) {
                    continue;
                }

                let sig       = entry.signature.clone();
                let rpc       = self.hunter_rpc.clone();
                let keypair   = self.keypair.clone();
                let jito      = self.jito.clone();
                let wt        = wallet_tokens.clone();
                let bh        = cached_blockhash.clone();
                let tip       = cached_tip.clone();
                let dedup     = recent_obligations.clone();

                tokio::spawn(async move {
                    if let Err(e) = execute_solend_opportunity(
                        sig, rpc, jito, keypair, wt, bh, tip, dedup,
                    ).await {
                        eprintln!("[hunter-solend] opportunity error: {}", e);
                    }
                });
            }
        }
    }
}

// ── Kamino opportunity execution (free function for tokio::spawn) ────────────
//
// Runs in its own task. Uses pre-cached blockhash and tip.
// Optimistic: does NOT call getAccountInfo — includes RefreshObligation in the tx
// and lets Kamino decide on-chain. If the position is already healthy, the tx
// fails cheaply (5000 lamports priority fee lost). If liquidatable, we win.
//
// Reserve addresses are extracted directly from the competitor's transaction,
// which means this works for ANY token pair without a hardcoded lookup table.
async fn execute_kamino_opportunity<R, JI>(
    sig: String,
    rpc: R,
    jito: JI,
    keypair: Arc<Keypair>,
    wallet_tokens: Vec<WalletToken>,
    cached_blockhash: Arc<tokio::sync::RwLock<solana_sdk::hash::Hash>>,
    cached_tip: Arc<std::sync::atomic::AtomicU64>,
    dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    max_repay_usd: f64,
) -> anyhow::Result<()>
where
    R: RpcClient,
    JI: JitoPort,
{
    // ── 1. getTransaction — single attempt, 500ms hard timeout ───────────────
    let tx_info = tokio::time::timeout(
        tokio::time::Duration::from_millis(500),
        rpc.get_transaction(&sig),
    ).await
    .map_err(|_| anyhow::anyhow!("getTransaction timeout"))?
    .map_err(|e| anyhow::anyhow!("getTransaction failed: {}", e))?;

    // ── 2. Find the liquidate instruction ────────────────────────────────────
    // The liquidate instruction is the KLend instruction with the most accounts.
    let liquidate_ix_idx = tx_info.instruction_programs.iter()
        .enumerate()
        .filter(|(_, &prog_idx)| {
            tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(KLEND_PROGRAM)
        })
        .max_by_key(|(ix_idx, _)| {
            tx_info.instruction_accounts.get(*ix_idx).map(|a| a.len()).unwrap_or(0)
        })
        .map(|(ix_idx, _)| ix_idx);

    let ix_idx = liquidate_ix_idx
        .ok_or_else(|| anyhow::anyhow!("no KLEND liquidate instruction found"))?;

    let ix_accs = &tx_info.instruction_accounts[ix_idx];
    if ix_accs.len() < 13 {
        anyhow::bail!("liquidate instruction has too few accounts ({})", ix_accs.len());
    }

    // ── 3. Resolve account pubkeys from the competitor's instruction ─────────
    // Account layout of LiquidateObligationAndRedeemReserveCollateralV2:
    //   0  liquidator (competitor's, we replace with ours)
    //   1  obligation PDA
    //   2  lending_market
    //   3  lending_market_authority
    //   4  repay_reserve
    //   5  repay_liquidity_mint
    //   6  repay_liquidity_supply
    //   7  withdraw_reserve
    //   8  withdraw_liquidity_mint
    //   9  withdraw_collateral_mint
    //   10 withdraw_collateral_supply
    //   11 withdraw_liquidity_supply
    //   12 withdraw_liquidity_fee_receiver
    macro_rules! resolve {
        ($i:expr) => {
            tx_info.account_keys
                .get(ix_accs[$i])
                .map(|s| s.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing account at position {}", $i))?
        }
    }

    let obligation_str    = resolve!(1);
    let repay_reserve_str = resolve!(4);
    let repay_mint_str    = resolve!(5);
    let repay_supply_str  = resolve!(6);
    let wdr_reserve_str   = resolve!(7);
    let wdr_liq_mint_str  = resolve!(8);
    let wdr_col_mint_str  = resolve!(9);
    let wdr_col_sup_str   = resolve!(10);
    let wdr_liq_sup_str   = resolve!(11);
    let wdr_fee_str       = resolve!(12);

    // ── 4. Dedup: skip if we fired on this obligation recently ───────────────
    {
        let mut map = dedup.lock().unwrap();
        map.retain(|_, t| t.elapsed().as_millis() < OBLIGATION_DEDUP_MS);
        if map.contains_key(obligation_str) {
            return Ok(());
        }
        map.insert(obligation_str.to_string(), std::time::Instant::now());
    }

    // ── 5. Check we hold the repay token ────────────────────────────────────
    let repay_token = wallet_tokens.iter()
        .find(|t| t.mint == repay_mint_str)
        .ok_or_else(|| anyhow::anyhow!("no wallet token for repay mint {}", &repay_mint_str[..8]))?;

    // Cap repay at max_repay_usd (approximate: we cap native amount, not USD)
    // The actual USD cap is enforced by wallet.toml max_repay_native.
    let _ = max_repay_usd; // available for future price-based capping

    // ── 6. Parse pubkeys ─────────────────────────────────────────────────────
    let obligation_pk    = Pubkey::from_str(obligation_str)?;
    let repay_reserve_pk = Pubkey::from_str(repay_reserve_str)?;
    let repay_mint_pk    = Pubkey::from_str(repay_mint_str)?;
    let repay_supply_pk  = Pubkey::from_str(repay_supply_str)?;
    let wdr_reserve_pk   = Pubkey::from_str(wdr_reserve_str)?;
    let wdr_liq_mint_pk  = Pubkey::from_str(wdr_liq_mint_str)?;
    let wdr_col_mint_pk  = Pubkey::from_str(wdr_col_mint_str)?;
    let wdr_col_sup_pk   = Pubkey::from_str(wdr_col_sup_str)?;
    let wdr_liq_sup_pk   = Pubkey::from_str(wdr_liq_sup_str)?;
    let wdr_fee_pk       = Pubkey::from_str(wdr_fee_str)?;

    let klend_pk      = Pubkey::from_str(KLEND_PROGRAM).unwrap();
    let market_pk     = Pubkey::from_str(LENDING_MARKET).unwrap();
    let market_auth_pk = Pubkey::from_str(LENDING_MARKET_AUTHORITY).unwrap();
    let token_prog_pk = Pubkey::from_str(TOKEN_PROGRAM).unwrap();
    let farms_pk      = Pubkey::from_str(FARMS_PROGRAM).unwrap();
    let tip_account   = Pubkey::from_str(JITO_TIP_ACCOUNT).unwrap();

    let liquidator = keypair.pubkey();

    // ── 7. Build instructions ────────────────────────────────────────────────
    // Compute budget: 350k CU is sufficient for refresh x2 + refresh_obligation + liquidate.
    // Optimistic: we include the refresh instructions so on-chain state is fresh.
    // If ObligationHealthy, tx fails and we lose only the priority fee.
    let mut instructions: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(350_000),
        ComputeBudgetInstruction::set_compute_unit_price(1),
        ix_refresh_reserve(&klend_pk, &market_pk, &repay_reserve_pk),
        ix_refresh_reserve(&klend_pk, &market_pk, &wdr_reserve_pk),
        ix_refresh_obligation(&klend_pk, &market_pk, &obligation_pk, &[&repay_reserve_pk, &wdr_reserve_pk]),
    ];

    // Liquidate instruction
    {
        let disc = discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2");
        let liquidity_amount = repay_token.max_repay_native;

        let mut data = disc.to_vec();
        data.extend_from_slice(&liquidity_amount.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // minAcceptableReceivedLiquidityAmount
        data.extend_from_slice(&0u64.to_le_bytes()); // maxAllowedLtvOverridePercent

        let user_src     = get_ata(&liquidator, &repay_mint_pk);
        let user_dst_col = get_ata(&liquidator, &wdr_col_mint_pk);
        let user_dst_liq = get_ata(&liquidator, &wdr_liq_mint_pk);

        let accounts = vec![
            AccountMeta::new_readonly(liquidator, true),
            AccountMeta::new(obligation_pk, false),
            AccountMeta::new_readonly(market_pk, false),
            AccountMeta::new_readonly(market_auth_pk, false),
            AccountMeta::new(repay_reserve_pk, false),
            AccountMeta::new_readonly(repay_mint_pk, false),
            AccountMeta::new(repay_supply_pk, false),
            AccountMeta::new(wdr_reserve_pk, false),
            AccountMeta::new_readonly(wdr_liq_mint_pk, false),
            AccountMeta::new(wdr_col_mint_pk, false),
            AccountMeta::new(wdr_col_sup_pk, false),
            AccountMeta::new(wdr_liq_sup_pk, false),
            AccountMeta::new(wdr_fee_pk, false),
            AccountMeta::new(user_src, false),
            AccountMeta::new(user_dst_col, false),
            AccountMeta::new(user_dst_liq, false),
            AccountMeta::new_readonly(token_prog_pk, false), // collateral token program
            AccountMeta::new_readonly(token_prog_pk, false), // repay liquidity token program
            AccountMeta::new_readonly(token_prog_pk, false), // withdraw liquidity token program
            AccountMeta::new_readonly(sysvar::instructions::id(), false),
            // Farm accounts (klend_pk as placeholder — positions without farms use it as no-op)
            AccountMeta::new(klend_pk, false),
            AccountMeta::new(klend_pk, false),
            AccountMeta::new(klend_pk, false),
            AccountMeta::new(klend_pk, false),
            AccountMeta::new_readonly(farms_pk, false),
        ];

        instructions.push(Instruction { program_id: klend_pk, accounts, data });
    }

    // Jito tip (pre-cached)
    let tip_lamports = cached_tip.load(Ordering::Relaxed);
    instructions.push(solana_sdk::system_instruction::transfer(&liquidator, &tip_account, tip_lamports));

    // ── 8. Build and sign tx with pre-cached blockhash ───────────────────────
    let blockhash = *cached_blockhash.read().await;

    let message = Message::try_compile(&liquidator, &instructions, &[], blockhash)
        .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
        .map_err(|e| anyhow::anyhow!("sign: {}", e))?;

    // ── 9. Send bundle ───────────────────────────────────────────────────────
    eprintln!(
        "[hunter-kamino] FIRING | obligation={} repay={} tip={}",
        &obligation_str[..8], repay_token.symbol, tip_lamports
    );

    match jito.send_bundle(vec![tx]).await {
        Ok(bundle_id) => eprintln!(
            "[hunter-kamino] BUNDLE SENT | obligation={} bundle={}",
            &obligation_str[..8], &bundle_id[..12.min(bundle_id.len())]
        ),
        Err(e) => eprintln!("[hunter-kamino] bundle send failed: {}", e),
    }

    Ok(())
}

// ── Solend opportunity execution (free function for tokio::spawn) ────────────
//
// Strategy: copy the competitor's tx instructions verbatim, replacing only
// the user-specific accounts (source/destination ATAs, signer) with ours.
// This means we don't need to know the Solend instruction discriminator or
// account layout — we just mirror the competitor's logic with our identity.
//
// For the liquidity_amount parameter (bytes 8-15 of Anchor instruction data),
// we substitute our max_repay_native so we never over-commit capital.
//
// Optimistic: the tx includes RefreshObligation copied from the competitor.
// If the obligation is already healthy, it fails cheaply.
async fn execute_solend_opportunity<R, JI>(
    sig: String,
    rpc: R,
    jito: JI,
    keypair: Arc<Keypair>,
    wallet_tokens: Vec<WalletToken>,
    cached_blockhash: Arc<tokio::sync::RwLock<solana_sdk::hash::Hash>>,
    cached_tip: Arc<std::sync::atomic::AtomicU64>,
    dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
) -> anyhow::Result<()>
where
    R: RpcClient,
    JI: JitoPort,
{
    // ── 1. getTransaction — single attempt, 500ms hard timeout ───────────────
    let tx_info = tokio::time::timeout(
        tokio::time::Duration::from_millis(500),
        rpc.get_transaction(&sig),
    ).await
    .map_err(|_| anyhow::anyhow!("getTransaction timeout"))?
    .map_err(|e| anyhow::anyhow!("getTransaction failed: {}", e))?;

    // ── 2. Find Solend liquidate instruction (most accounts) ─────────────────
    let liq_ix_idx = tx_info.instruction_programs.iter()
        .enumerate()
        .filter(|(_, &prog_idx)| {
            tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(SOLEND_PROGRAM)
        })
        .max_by_key(|(ix_idx, _)| {
            tx_info.instruction_accounts.get(*ix_idx).map(|a| a.len()).unwrap_or(0)
        })
        .map(|(ix_idx, _)| ix_idx)
        .ok_or_else(|| anyhow::anyhow!("no Solend liquidate instruction found"))?;

    let liq_accs = &tx_info.instruction_accounts[liq_ix_idx];
    let liq_data = &tx_info.instruction_data[liq_ix_idx];

    if liq_accs.len() < 9 || liq_data.len() < 16 {
        anyhow::bail!("Solend liquidate instruction malformed (accs={} data={})", liq_accs.len(), liq_data.len());
    }

    // ── 3. Competitor's wallet = account_keys[0] (fee payer / signer) ────────
    let competitor = tx_info.account_keys.get(0)
        .ok_or_else(|| anyhow::anyhow!("empty account_keys"))?
        .clone();

    // ── 4. Build: account_index → (mint, owner) from token balances ──────────
    // This lets us identify competitor ATAs and derive our equivalent ATAs.
    let balance_map: HashMap<usize, (String, String)> = tx_info.post_token_balances.iter()
        .chain(tx_info.pre_token_balances.iter())
        .map(|b| (b.account_index, (b.mint.clone(), b.owner.clone())))
        .collect();

    // ── 5. Find repay token: competitor ATA that decreased (owned by competitor)
    // We look for a wallet_token whose mint appears in the balances for an
    // account owned by the competitor. That's the token they repaid with.
    let repay_token = wallet_tokens.iter().find(|wt| {
        balance_map.values().any(|(mint, owner)| mint == &wt.mint && owner == &competitor)
    }).ok_or_else(|| anyhow::anyhow!("no matching wallet token for this liquidation"))?;

    // ── 6. Dedup: skip if we fired on this obligation recently ───────────────
    // Obligation is at accounts[5] for LiquidateWithoutReceivingCtokens
    // (observer confirmed: accounts[5] = obligation, accounts[8] = liquidator).
    // We also fall back to checking a few positions to be safe.
    let obligation_key_idx = liq_accs.get(5)
        .and_then(|&i| tx_info.account_keys.get(i))
        .cloned()
        .unwrap_or_default();

    if obligation_key_idx.is_empty() {
        anyhow::bail!("could not extract obligation pubkey from Solend tx");
    }

    {
        let mut map = dedup.lock().unwrap();
        map.retain(|_, t| t.elapsed().as_millis() < OBLIGATION_DEDUP_MS);
        if map.contains_key(&obligation_key_idx) {
            return Ok(());
        }
        map.insert(obligation_key_idx.clone(), std::time::Instant::now());
    }

    // ── 7. Build instruction list ────────────────────────────────────────────
    // ComputeBudget: 350k CU (sufficient for refresh x2 + liquidate without flash loan)
    let mut instructions: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(350_000),
        ComputeBudgetInstruction::set_compute_unit_price(1),
    ];

    // Copy all Solend non-liquidate instructions (RefreshReserve, RefreshObligation)
    // verbatim — they contain no user-specific accounts.
    let solend_pk = Pubkey::from_str(SOLEND_PROGRAM).unwrap();
    for (idx, (&prog_idx, accs)) in tx_info.instruction_programs.iter()
        .zip(tx_info.instruction_accounts.iter())
        .enumerate()
    {
        let prog_key = match tx_info.account_keys.get(prog_idx) {
            Some(k) => k.as_str(),
            None => continue,
        };
        if prog_key != SOLEND_PROGRAM { continue; }
        if idx == liq_ix_idx { continue; } // skip liquidate — we rebuild it below

        // Resolve account metas: assume all non-signer / non-writable for refresh
        let acc_metas: Vec<AccountMeta> = accs.iter().filter_map(|&ai| {
            tx_info.account_keys.get(ai).and_then(|k| Pubkey::from_str(k).ok()).map(|pk| {
                AccountMeta::new_readonly(pk, false)
            })
        }).collect();

        let data = tx_info.instruction_data.get(idx).cloned().unwrap_or_default();
        instructions.push(Instruction { program_id: solend_pk, accounts: acc_metas, data });
    }

    // Rebuild the liquidate instruction, replacing competitor accounts with ours.
    {
        let liquidator = keypair.pubkey();

        let acc_metas: Vec<AccountMeta> = liq_accs.iter().enumerate().map(|(pos, &ai)| {
            let key_str = tx_info.account_keys.get(ai).map(|s| s.as_str()).unwrap_or("");
            let pk = Pubkey::from_str(key_str).unwrap_or_default();

            // Determine if this is a competitor-owned ATA → replace with ours
            if let Some((mint_str, owner)) = balance_map.get(&ai) {
                if owner == &competitor {
                    // It's the competitor's token account — use our ATA for the same mint
                    if let Ok(mint_pk) = Pubkey::from_str(mint_str) {
                        let our_ata = get_ata(&liquidator, &mint_pk);
                        return AccountMeta::new(our_ata, false);
                    }
                }
            }

            // It's the competitor's wallet (signer) — use our keypair
            if key_str == competitor {
                return AccountMeta::new_readonly(liquidator, true);
            }

            // All other accounts (reserves, obligation, market, programs) are kept as-is.
            // Mark writable if it was writable in the original tx.
            // Heuristic: assume writable unless it's a program, sysvar, or readonly constant.
            let is_program_or_sysvar = pk == solana_sdk::system_program::id()
                || pk == Pubkey::from_str(TOKEN_PROGRAM).unwrap_or_default()
                || pk == sysvar::instructions::id()
                || pk == solana_sdk::sysvar::clock::id()
                || pk == solana_sdk::sysvar::rent::id()
                || prog_idx_is_program(&tx_info, ai);

            // Readonly marker positions (lending market, lending market authority, token program)
            // are typically the last 3-4 accounts in Solend's liquidate instruction.
            let is_likely_readonly = is_program_or_sysvar || pos >= liq_accs.len().saturating_sub(4);

            if is_likely_readonly {
                AccountMeta::new_readonly(pk, false)
            } else {
                AccountMeta::new(pk, false)
            }
        }).collect();

        // Copy instruction data, replacing liquidity_amount (bytes 8-15) with our cap.
        let mut data = liq_data.clone();
        let amount = repay_token.max_repay_native;
        data[8..16].copy_from_slice(&amount.to_le_bytes());

        instructions.push(Instruction { program_id: solend_pk, accounts: acc_metas, data });
    }

    // Jito tip
    let tip_lamports = cached_tip.load(Ordering::Relaxed);
    let tip_account = Pubkey::from_str(JITO_TIP_ACCOUNT).unwrap();
    instructions.push(solana_sdk::system_instruction::transfer(
        &keypair.pubkey(), &tip_account, tip_lamports,
    ));

    // ── 8. Build and sign tx with pre-cached blockhash ───────────────────────
    let blockhash = *cached_blockhash.read().await;
    let liquidator = keypair.pubkey();

    let message = Message::try_compile(&liquidator, &instructions, &[], blockhash)
        .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;

    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
        .map_err(|e| anyhow::anyhow!("sign: {}", e))?;

    // ── 9. Send bundle ───────────────────────────────────────────────────────
    eprintln!(
        "[hunter-solend] FIRING | obligation={} repay={} tip={}",
        &obligation_key_idx[..8.min(obligation_key_idx.len())],
        repay_token.symbol,
        tip_lamports,
    );

    match jito.send_bundle(vec![tx]).await {
        Ok(bundle_id) => eprintln!(
            "[hunter-solend] BUNDLE SENT | obligation={} bundle={}",
            &obligation_key_idx[..8.min(obligation_key_idx.len())],
            &bundle_id[..12.min(bundle_id.len())]
        ),
        Err(e) => eprintln!("[hunter-solend] bundle send failed: {}", e),
    }

    Ok(())
}

/// Returns true if the given account index is used as a program id in any instruction.
/// Used to identify program accounts (always readonly) vs data accounts.
fn prog_idx_is_program(tx: &crate::ports::rpc::TransactionInfo, ai: usize) -> bool {
    tx.instruction_programs.contains(&ai)
}

// ── Solend log helpers ────────────────────────────────────────────────────────

fn extract_obligation_pda_from_logs(logs: &[String]) -> Option<String> {
    for line in logs {
        let content = line.strip_prefix("Program log: ").unwrap_or(line);
        if let Some(rest) = content.strip_prefix("obligation_info:") {
            let pda = rest.trim().split_whitespace().next()?.to_string();
            if !pda.is_empty() { return Some(pda); }
        }
    }
    None
}

fn extract_log_field(logs: &[String], key: &str) -> Option<String> {
    for line in logs {
        let content = line.strip_prefix("Program log: ").unwrap_or(line);
        if let Some(rest) = content.strip_prefix(key) {
            let val = rest.trim().split_whitespace().next()?.to_string();
            if !val.is_empty() { return Some(val); }
        }
    }
    None
}
