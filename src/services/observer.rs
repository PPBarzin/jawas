// Phase 1: Observes liquidations executed by other bots on Kamino.

use crate::domain::token::{native_to_human, token_info, token_mint_by_symbol};
use crate::ports::logger::{LiquidationLogger, ObservationEvent};
use crate::ports::oracle::PriceOracle;
use crate::ports::rpc::{RpcClient, StreamingRpcClient, TransactionInfo};
use crate::utils::utc_now;
use std::collections::VecDeque;
use std::io::Write;

const KAMINO_PROGRAM_ID: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const SOLEND_PROGRAM_ID: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const LIQUIDATE_FILTER: &str = "Liquidate";
const RPC_TIMEOUT_SECONDS: u64 = 120;
const COMPETING_BOTS_WINDOW_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    Kamino,
    Solend,
}

impl Protocol {
    pub fn program_id(&self) -> &'static str {
        match self {
            Protocol::Kamino => KAMINO_PROGRAM_ID,
            Protocol::Solend => SOLEND_PROGRAM_ID,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Protocol::Kamino => "Kamino",
            Protocol::Solend => "Solend",
        }
    }
}

/// Represents a failed liquidation attempt observed on-chain.
/// Used to estimate competition for successful liquidations.
struct FailedLiquidationAttempt {
    received_at_ms: u64,
    signature: String,
    market: String,
    repay_mint: String,
    withdraw_mint: String,
    repay_native: u64,
}

/// Buckets the repay amount to allow for small variations in failed attempts
/// while still matching the same liquidation opportunity.
///
/// Logic: Keeps the 2 most significant digits and truncates the rest.
/// Example: 1,234,567 -> 1,200,000.
fn bucket_repay_native(value: u64) -> u64 {
    if value == 0 {
        return 0;
    }
    let mut v = value;
    let mut scale = 1u64;
    while v >= 100 {
        v /= 10;
        scale *= 10;
    }
    v * scale
}

/// Removes entries from the buffer that are older than COMPETING_BOTS_WINDOW_MS.
fn purge_old_failed_attempts(buffer: &mut VecDeque<FailedLiquidationAttempt>, current_ts_ms: u64) {
    let cutoff = current_ts_ms.saturating_sub(COMPETING_BOTS_WINDOW_MS);
    while let Some(att) = buffer.front() {
        if att.received_at_ms < cutoff {
            buffer.pop_front();
        } else {
            break;
        }
    }
}

use std::collections::HashSet;

pub struct ObserverService<R: StreamingRpcClient + RpcClient, L: LiquidationLogger, O: PriceOracle> {
    rpc: R,
    logger: L,
    oracle: O,
    protocol: Protocol,
}

impl<R: StreamingRpcClient + RpcClient, L: LiquidationLogger, O: PriceOracle> ObserverService<R, L, O> {
    pub fn new(rpc: R, logger: L, oracle: O, protocol: Protocol) -> Self {
        Self { rpc, logger, oracle, protocol }
    }

    /// Subscribes to program logs and forwards each observed liquidation
    /// to the logger. Runs until the WebSocket stream closes.
    pub async fn watch(&self) -> anyhow::Result<()> {
        let program_id = self.protocol.program_id();
        let mut rx = self.rpc.subscribe_to_logs(program_id).await?;
        let mut liquidations_logged: u64 = 0;
        let mut failed_attempts: VecDeque<FailedLiquidationAttempt> = VecDeque::new();
        let mut seen_signatures: HashSet<String> = HashSet::new();

        // Raw event log — liquidation-related events appended as JSON lines.
        // Only active when LOG_FILE env var is explicitly set.
        // Not set → writes to /dev/null (no-op), so tests don't pollute the real file.
        let log_path = std::env::var("LOG_FILE").unwrap_or_else(|_| "/dev/null".to_string());
        let mut log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| anyhow::anyhow!("Cannot open log file {}: {}", log_path, e))?;

        let mut events_received: u64 = 0;
        loop {
            let next_entry = tokio::time::timeout(
                std::time::Duration::from_secs(RPC_TIMEOUT_SECONDS),
                rx.recv()
            ).await;

            let entry = match next_entry {
                Ok(Some(e)) => e,
                Ok(None) => break, // Stream closed
                Err(_) => {
                    let msg = format!("RPC stream timeout: no messages received for {} seconds", RPC_TIMEOUT_SECONDS);
                    eprintln!("[observer] {}", msg);
                    
                    // Log timeout event to Airtable for infrastructure monitoring
                    let timeout_event = ObservationEvent {
                        timestamp: utc_now(),
                        signature: format!("TIMEOUT_{}", utc_now()),
                        protocol: "SYSTEM".to_string(),
                        market: "N/A".to_string(),
                        liquidated_user: "N/A".to_string(),
                        liquidator: "N/A".to_string(),
                        repay_mint: "N/A".to_string(),
                        withdraw_mint: "N/A".to_string(),
                        repay_symbol: "TIMEOUT".to_string(),
                        withdraw_symbol: "WATCHDOG".to_string(),
                        repay_amount: 0.0,
                        withdraw_amount: 0.0,
                        repaid_usd: 0.0,
                        withdrawn_usd: 0.0,
                        profit_usd: 0.0,
                        delay_ms: 0,
                        competing_bots: 0,
                        status: "RPC_TIMEOUT".to_string(),
                    };
                    let _ = self.logger.log_observation(&timeout_event).await;

                    return Err(anyhow::anyhow!("{}. Force reconnecting...", msg));
                }
            };

            events_received += 1;

            if events_received % 1000 == 0 {
                eprintln!(
                    "[observer] {} {} events received ({} liquidations logged)",
                    events_received, self.protocol.name(), liquidations_logged
                );
            }

            let is_liquidation = entry.logs.iter().any(|log| log.contains(LIQUIDATE_FILTER));
            let is_truncated = entry.logs.iter().any(|log| log.contains("[truncated]"));

            // Append to log file only if any log line mentions liquidation OR if it's truncated (potential hidden liquidation)
            let has_liquidation_keyword = entry.logs.iter()
                .any(|l| l.to_ascii_lowercase().contains("liquidat"));
            if has_liquidation_keyword || is_truncated {
                let logs_json = entry.logs.iter()
                    .map(|l| format!("\"{}\"", l.replace('\\', "\\\\").replace('"', "\\\"")))
                    .collect::<Vec<_>>()
                    .join(",");
                let line = format!(
                    "{{\"ts\":\"{}\",\"sig\":\"{}\",\"err\":{},\"logs\":[{}]}}\n",
                    utc_now(),
                    entry.signature,
                    entry.is_error,
                    logs_json
                );
                if let Err(e) = log_file.write_all(line.as_bytes()) {
                    eprintln!("[observer] failed to write to log file: {}", e);
                }
                let _ = log_file.flush();
            }

            if !is_liquidation && !is_truncated {
                continue;
            }

            // Deduplicate: Helius WS can deliver the same tx twice (confirmed + finalized).
            if !seen_signatures.insert(entry.signature.clone()) {
                continue;
            }

            if entry.is_error {
                let parsed = parse_liquidation_logs(&entry.logs);
                // Only track if we have all fields for the fingerprint
                if parsed.market != "N/A" && parsed.repay_mint != "N/A" && parsed.withdraw_mint != "N/A" {
                    // Purge on addition to keep buffer size bounded
                    purge_old_failed_attempts(&mut failed_attempts, entry.received_at_ms);

                    // Minimal deduplication: don't add if already in buffer
                    // (30s buffer is small enough for a linear check)
                    let already_tracked = failed_attempts.iter().any(|att| att.signature == entry.signature);
                    if !already_tracked {
                        failed_attempts.push_back(FailedLiquidationAttempt {
                            received_at_ms: entry.received_at_ms,
                            signature: entry.signature.clone(),
                            market: parsed.market,
                            repay_mint: parsed.repay_mint,
                            withdraw_mint: parsed.withdraw_mint,
                            repay_native: parsed.repay_native,
                        });
                    }
                }
                continue;
            }

            let parsed = parse_liquidation_logs(&entry.logs);

            // Calculate competing bots
            purge_old_failed_attempts(&mut failed_attempts, entry.received_at_ms);

            let bucketed_repay = bucket_repay_native(parsed.repay_native);
            let competing_bots = failed_attempts.iter()
                .filter(|att| {
                    att.market == parsed.market &&
                    att.repay_mint == parsed.repay_mint &&
                    att.withdraw_mint == parsed.withdraw_mint &&
                    bucket_repay_native(att.repay_native) == bucketed_repay
                })
                .count();

            // Enrich liquidated_user, liquidator, delay_ms and amounts from getTransaction.
            // delay_ms = websocket receive time − on-chain block time (Phase 1 approximation).
            // Falls back to log-parsed values if getTransaction fails or accounts are missing.
            let (liquidated_user, liquidator, delay_ms, repay_amount, withdraw_amount) =
                match self.rpc.get_transaction(&entry.signature).await {
                    Ok(tx) => {
                        let delay = tx.block_time
                            .map(|bt| entry.received_at_ms.saturating_sub(bt * 1000))
                            .unwrap_or(0);
                        
                        let (_obligation_pda, owner, mut liq) = match self.protocol {
                            Protocol::Kamino => extract_klend_liquidation_accounts(&tx, KAMINO_PROGRAM_ID)
                                .unwrap_or(("N/A".to_string(), parsed.liquidated_user.clone(), parsed.liquidator.clone())),
                            Protocol::Solend => extract_solend_liquidation_accounts(&tx, SOLEND_PROGRAM_ID)
                                .unwrap_or(("N/A".to_string(), parsed.liquidated_user.clone(), parsed.liquidator.clone())),
                        };

                        // Fallback: If liquidator is still N/A (e.g. truncated logs), use the transaction signer
                        if (liq == "N/A" || liq.is_empty()) && !tx.account_keys.is_empty() {
                            liq = tx.account_keys[0].clone();
                        }

                        // Fallback for amounts: if logs gave 0.0, try to infer from token balance changes
                        let mut r_amt = parsed.repay_amount;
                        let mut w_amt = parsed.withdraw_amount;

                        if r_amt == 0.0 || w_amt == 0.0 {
                            let repay_info = token_info(&parsed.repay_mint);
                            let withdraw_info = token_info(&parsed.withdraw_mint);

                            // Find balance change for the liquidator wallet
                            for pre in &tx.pre_token_balances {
                                if pre.owner == liq {
                                    for post in &tx.post_token_balances {
                                        if post.owner == liq && post.mint == pre.mint {
                                            let diff = post.ui_amount - pre.ui_amount;
                                            
                                            // 1. Direct Mint match
                                            if post.mint == parsed.repay_mint && diff < 0.0 {
                                                r_amt = diff.abs();
                                            } else if post.mint == parsed.withdraw_mint && diff > 0.0 {
                                                w_amt = diff;
                                            } 
                                            // 2. Symbol match (handles Solend where log gives Reserve Address instead of Mint)
                                            else if let Some(post_info) = token_info(&post.mint) {
                                                if let Some(ri) = &repay_info {
                                                    if ri.symbol == post_info.symbol && diff < 0.0 {
                                                        r_amt = diff.abs();
                                                    }
                                                }
                                                if let Some(wi) = &withdraw_info {
                                                    if wi.symbol == post_info.symbol && diff > 0.0 {
                                                        w_amt = diff;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        (owner, liq, delay, r_amt, w_amt)
                    }
                    Err(e) => {
                        eprintln!("[observer] get_transaction failed for liquidation {}: {}", entry.signature, e);
                        (parsed.liquidated_user, parsed.liquidator, 0u64, parsed.repay_amount, parsed.withdraw_amount)
                    }
                };

            // Prices from KLend logs take priority (exact prices used at liquidation time);
            // fall back to oracle for tokens absent from the RefreshReservesBatch log.
            let repay_price = if parsed.repay_price_usd > 0.0 {
                parsed.repay_price_usd
            } else {
                self.oracle.get_price_usd(&parsed.repay_mint).await.unwrap_or(0.0)
            };
            let withdraw_price = if parsed.withdraw_price_usd > 0.0 {
                parsed.withdraw_price_usd
            } else {
                self.oracle.get_price_usd(&parsed.withdraw_mint).await.unwrap_or(0.0)
            };

            let repaid_usd = repay_amount * repay_price;
            let withdrawn_usd = withdraw_amount * withdraw_price;
            let profit_usd = withdrawn_usd - repaid_usd;

            println!(
                "[observer] liquidation | sig={} market={} borrower={} liquidator={} \
                 repay={} {} (native={}, ${:.2}) withdraw={} {} (native={}, ${:.2}) profit=${:.2} delay={}ms",
                entry.signature,
                parsed.market,
                liquidated_user,
                liquidator,
                repay_amount,
                parsed.repay_symbol,
                parsed.repay_native,
                repaid_usd,
                withdraw_amount,
                parsed.withdraw_symbol,
                parsed.withdraw_native,
                withdrawn_usd,
                profit_usd,
                delay_ms,
            );

            let event = ObservationEvent {
                timestamp: utc_now(),
                signature: entry.signature.clone(),
                protocol: self.protocol.name().to_string(),
                market: parsed.market.clone(),
                liquidated_user: liquidated_user.clone(),
                liquidator: liquidator.clone(),
                repay_mint: parsed.repay_mint.clone(),
                withdraw_mint: parsed.withdraw_mint.clone(),
                repay_symbol: parsed.repay_symbol.clone(),
                withdraw_symbol: parsed.withdraw_symbol.clone(),
                repay_amount,
                withdraw_amount,
                repaid_usd,
                withdrawn_usd,
                profit_usd,
                delay_ms,
                competing_bots: competing_bots as u32,
                status: "WATCHED".to_string(),
            };

            if let Err(e) = self.logger.log_observation(&event).await {
                eprintln!("[observer] failed to log {}: {}", entry.signature, e);
            } else {
                liquidations_logged += 1;
            }
        }

        Ok(())
    }
}

// ── Transaction account extraction ───────────────────────────────────────────

/// Extracts (obligation_pda, obligation_owner, liquidator) from a liquidation transaction.
///
/// KLend `LiquidateObligationAndRedeemReserveCollateralV2` account layout (IDL order):
///   accounts[0] = liquidator, accounts[1] = obligation, accounts[3] = obligation_owner
///
/// We identify the liquidation instruction as the KLend instruction with the most accounts
/// (it has ~17 accounts; RefreshObligation/RefreshReservesBatch have far fewer).
fn extract_klend_liquidation_accounts(
    tx: &TransactionInfo,
    klend_program_id: &str,
) -> Option<(String, String, String)> {
    let (instr_idx, _) = tx
        .instruction_programs
        .iter()
        .enumerate()
        .filter(|(_, &prog_idx)| {
            tx.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(klend_program_id)
        })
        .max_by_key(|(i, _)| tx.instruction_accounts.get(*i).map(|a| a.len()).unwrap_or(0))?;

    let accounts = &tx.instruction_accounts[instr_idx];
    let liquidator   = tx.account_keys.get(*accounts.get(0)?)?.clone();
    let obligation   = tx.account_keys.get(*accounts.get(1)?)?.clone();
    let owner        = tx.account_keys.get(*accounts.get(3)?)?.clone();

    Some((obligation, owner, liquidator))
}

/// Extracts (obligation_pda, obligation_owner, liquidator) from a Solend liquidation transaction.
///
/// Solend `LiquidateObligationAndRedeemReserveCollateral` account layout (IDL order):
///   accounts[5] = obligation, accounts[8] = liquidator (user_transfer_authority)
/// Note: Solend doesn't include the obligation_owner in the instruction accounts.
fn extract_solend_liquidation_accounts(
    tx: &TransactionInfo,
    solend_program_id: &str,
) -> Option<(String, String, String)> {
    let (instr_idx, _) = tx
        .instruction_programs
        .iter()
        .enumerate()
        .filter(|(_, &prog_idx)| {
            tx.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(solend_program_id)
        })
        .max_by_key(|(i, _)| tx.instruction_accounts.get(*i).map(|a| a.len()).unwrap_or(0))?;

    let accounts = &tx.instruction_accounts[instr_idx];
    let liquidator = tx.account_keys.get(*accounts.get(8)?)?.clone();
    let obligation = tx.account_keys.get(*accounts.get(5)?)?.clone();
    let owner      = "N/A".to_string(); // Not in accounts, logs might have it

    Some((obligation, owner, liquidator))
}

// ── Log parsing ───────────────────────────────────────────────────────────────

/// Intermediate result from a single log bundle.
struct ParsedLiquidation {
    market: String,
    liquidated_user: String,
    liquidator: String,
    repay_mint: String,
    withdraw_mint: String,
    repay_symbol: String,
    withdraw_symbol: String,
    repay_native: u64,
    withdraw_native: u64,
    repay_amount: f64,
    withdraw_amount: f64,
    /// Price extracted from KLend `Token: SYMBOL Price: X` log line (0.0 if absent).
    repay_price_usd: f64,
    /// Price extracted from KLend `Token: SYMBOL Price: X` log line (0.0 if absent).
    withdraw_price_usd: f64,
}

/// Scans KLend Anchor program logs for liquidation data.
fn parse_liquidation_logs(logs: &[String]) -> ParsedLiquidation {
    let mut market = "N/A".to_string();
    let mut liquidated_user = "N/A".to_string();
    let mut liquidator = "N/A".to_string();
    let mut repay_mint = "N/A".to_string();
    let mut withdraw_mint = "N/A".to_string();
    let mut repay_native: Option<u64> = None;
    let mut withdraw_native: Option<u64> = None;
    // Keyed by raw symbol from log (e.g. "tBTC", "SOL") — populated before mint lookup.
    let mut token_prices: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    // Raw symbols extracted from Borrow:/Deposit: lines — used for price lookup regardless of catalogue.
    let mut repay_log_symbol: Option<String> = None;
    let mut withdraw_log_symbol: Option<String> = None;

    for line in logs {
        let content = strip_program_log_prefix(line);

        if market == "N/A" {
            if let Some(v) = extract_token(content, "lending_market:") {
                market = v;
            } else if let Some(v) = extract_token(content, "market:") {
                market = v;
            } else if let Some(v) = extract_token(content, "lending_market_info:") {
                market = v;
            }
        }

        if liquidated_user == "N/A" {
            if let Some(v) = extract_token(content, "obligation_owner:") {
                liquidated_user = v;
            } else if let Some(v) = extract_token(content, "borrower:") {
                liquidated_user = v;
            } else if let Some(v) = extract_token(content, "obligation_info:") {
                liquidated_user = v;
            }
            // `obligation:` is intentionally excluded — it is the PDA address, not the borrower's wallet
        }

        if liquidator == "N/A" {
            if let Some(v) = extract_token(content, "liquidator:") {
                liquidator = v;
            } else if let Some(v) = extract_token(content, "user_transfer_authority_info:") {
                liquidator = v;
            }
        }

        if repay_mint == "N/A" {
            if let Some(v) = extract_token(content, "repay_mint:") {
                repay_mint = v;
            } else if let Some(v) = extract_token(content, "repay_reserve:") {
                repay_mint = v;
            } else if let Some(v) = extract_token(content, "repay_reserve_info:") {
                repay_mint = v;
            } else if let Some(rest) = content.strip_prefix("Borrow: ") {
                // Real KLend production format: "Borrow: SYMBOL amount: X value: Y ..."
                if let Some(sym) = rest.split_whitespace().next() {
                    repay_log_symbol = Some(sym.to_string());
                    if let Some(mint) = token_mint_by_symbol(sym) {
                        repay_mint = mint.to_string();
                    }
                }
            }
        }

        if withdraw_mint == "N/A" {
            if let Some(v) = extract_token(content, "withdraw_mint:") {
                withdraw_mint = v;
            } else if let Some(v) = extract_token(content, "withdraw_reserve:") {
                withdraw_mint = v;
            } else if let Some(v) = extract_token(content, "withdraw_reserve_info:") {
                withdraw_mint = v;
            } else if let Some(rest) = content.strip_prefix("Deposit: ") {
                // Real KLend production format: "Deposit: SYMBOL amount: X value: Y ..."
                if let Some(sym) = rest.split_whitespace().next() {
                    withdraw_log_symbol = Some(sym.to_string());
                    if let Some(mint) = token_mint_by_symbol(sym) {
                        withdraw_mint = mint.to_string();
                    }
                }
            }
        }

        // Real KLend production format: "Token: SYMBOL Price: X.XXXX"
        if let Some(rest) = content.strip_prefix("Token: ") {
            let mut parts = rest.splitn(3, ' ');
            if let (Some(sym), Some("Price:"), Some(price_str)) = (parts.next(), parts.next(), parts.next()) {
                if let Ok(price) = price_str.trim().parse::<f64>() {
                    token_prices.insert(sym.to_string(), price);
                }
            }
        }

        if repay_native.is_none() {
            if let Some(v) = extract_u64(content, "repay_amount:") {
                repay_native = Some(v);
            } else if let Some(v) = extract_u64(content, "repaid_amount:") {
                repay_native = Some(v);
            } else if let Some(v) = extract_u64(content, "repaid ") {
                // Real KLend production format: "pnl: Liquidator repaid <N> and withdrew <M> ..."
                repay_native = Some(v);
            }
        }

        if withdraw_native.is_none() {
            if let Some(v) = extract_u64(content, "withdraw_amount:") {
                withdraw_native = Some(v);
            } else if let Some(v) = extract_u64(content, "withdrawn_amount:") {
                withdraw_native = Some(v);
            } else if let Some(v) = extract_u64(content, "and withdrew ") {
                // Real KLend production format: "pnl: Liquidator repaid <N> and withdrew <M> collateral ..."
                // Note: this is the net amount to the liquidator (total minus protocol fee)
                withdraw_native = Some(v);
            }
        }
    }

    // Normalization and symbols
    let repay_symbol = token_info(&repay_mint)
        .map(|i| i.symbol.to_string())
        .unwrap_or_else(|| {
            if repay_mint != "N/A" {
                format!("UNK-{}", &repay_mint[..repay_mint.len().min(4)])
            } else {
                "N/A".to_string()
            }
        });
    let withdraw_symbol = token_info(&withdraw_mint)
        .map(|i| i.symbol.to_string())
        .unwrap_or_else(|| {
            if withdraw_mint != "N/A" {
                format!("UNK-{}", &withdraw_mint[..withdraw_mint.len().min(4)])
            } else {
                "N/A".to_string()
            }
        });

    // Use raw log symbol for price lookup — works even for tokens absent from the catalogue.
    let repay_price_usd = repay_log_symbol.as_deref()
        .and_then(|s| token_prices.get(s))
        .copied()
        .unwrap_or_else(|| token_prices.get(&repay_symbol).copied().unwrap_or(0.0));
    let withdraw_price_usd = withdraw_log_symbol.as_deref()
        .and_then(|s| token_prices.get(s))
        .copied()
        .unwrap_or_else(|| token_prices.get(&withdraw_symbol).copied().unwrap_or(0.0));

    let repay_native = repay_native.unwrap_or(0);
    let withdraw_native = withdraw_native.unwrap_or(0);

    let repay_amount = native_to_human(repay_native, &repay_mint).unwrap_or_else(|| {
        if repay_native > 0 {
            println!("[parser] repay_mint='{}' not in catalogue — decimals unknown", repay_mint);
        }
        0.0
    });

    let withdraw_amount = native_to_human(withdraw_native, &withdraw_mint).unwrap_or_else(|| {
        if withdraw_native > 0 {
            println!("[parser] withdraw_mint='{}' not in catalogue — decimals unknown", withdraw_mint);
        }
        0.0
    });

    ParsedLiquidation {
        market,
        liquidated_user,
        liquidator,
        repay_mint,
        withdraw_mint,
        repay_symbol,
        withdraw_symbol,
        repay_native,
        withdraw_native,
        repay_amount,
        withdraw_amount,
        repay_price_usd,
        withdraw_price_usd,
    }
}

// ── String helpers ────────────────────────────────────────────────────────────

fn strip_program_log_prefix(line: &str) -> &str {
    line.strip_prefix("Program log: ").unwrap_or(line)
}

fn extract_token(content: &str, keyword: &str) -> Option<String> {
    let start = content.find(keyword)?;
    let rest = content[start + keyword.len()..].trim_start();
    let token = rest.split_whitespace().next()?;
    if token.is_empty() { None } else { Some(token.to_string()) }
}

fn extract_u64(content: &str, keyword: &str) -> Option<u64> {
    extract_token(content, keyword)?.parse::<u64>().ok()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use crate::ports::rpc::{LogEntry, TransactionInfo};

    struct MockRpcClient {
        rx: Arc<Mutex<Option<mpsc::Receiver<LogEntry>>>>,
    }

    impl MockRpcClient {
        fn new(rx: mpsc::Receiver<LogEntry>) -> Self {
            Self {
                rx: Arc::new(Mutex::new(Some(rx))),
            }
        }
    }

    impl RpcClient for MockRpcClient {
        async fn get_version(&self) -> anyhow::Result<String> {
            Ok("mock-1.0".to_string())
        }
        async fn get_transaction(&self, _signature: &str) -> anyhow::Result<TransactionInfo> {
            Ok(TransactionInfo {
                account_keys: vec![],
                instruction_accounts: vec![],
                instruction_programs: vec![],
                instruction_data: vec![],
                block_time: None,
                pre_token_balances: vec![],
                post_token_balances: vec![],
            })
        }
        async fn get_account_info(&self, _pubkey: &str) -> anyhow::Result<Vec<u8>> {
            Ok(vec![])
        }
        async fn get_latest_blockhash(&self) -> anyhow::Result<solana_sdk::hash::Hash> {
            Ok(solana_sdk::hash::Hash::default())
        }
    }

    impl StreamingRpcClient for MockRpcClient {
        async fn subscribe_to_logs(&self, _program_id: &str) -> anyhow::Result<mpsc::Receiver<LogEntry>> {
            let mut rx_lock = self.rx.lock().unwrap();
            rx_lock.take().ok_or_else(|| anyhow::anyhow!("Stream already consumed"))
        }
    }

    struct MockLogger {
        events: Arc<Mutex<Vec<ObservationEvent>>>,
    }

    impl MockLogger {
        fn new() -> (Self, Arc<Mutex<Vec<ObservationEvent>>>) {
            let events = Arc::new(Mutex::new(Vec::new()));
            (Self { events: events.clone() }, events)
        }
    }

    impl LiquidationLogger for MockLogger {
        async fn log_observation(&self, event: &ObservationEvent) -> anyhow::Result<()> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    struct MockPriceOracle;
    impl PriceOracle for MockPriceOracle {
        async fn get_price_usd(&self, mint: &str) -> anyhow::Result<f64> {
            match mint {
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => Ok(1.0),
                "So11111111111111111111111111111111111111112" => Ok(150.0),
                _ => Ok(0.0),
            }
        }
    }

    #[test]
    fn test_purge_old_failed_attempts() {
        let mut buffer = VecDeque::new();
        buffer.push_back(FailedLiquidationAttempt {
            received_at_ms: 1000,
            signature: "1".to_string(),
            market: "M".to_string(),
            repay_mint: "R".to_string(),
            withdraw_mint: "W".to_string(),
            repay_native: 100,
        });
        buffer.push_back(FailedLiquidationAttempt {
            received_at_ms: 40_000,
            signature: "2".to_string(),
            market: "M".to_string(),
            repay_mint: "R".to_string(),
            withdraw_mint: "W".to_string(),
            repay_native: 100,
        });

        // Window is 30s. At 40_001ms, cutoff is 10_001. Entry "1" should be purged.
        purge_old_failed_attempts(&mut buffer, 40_001);
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].signature, "2");
    }

    #[test]
    fn test_bucket_repay_native() {
        assert_eq!(bucket_repay_native(0), 0);
        assert_eq!(bucket_repay_native(50), 50);
        assert_eq!(bucket_repay_native(99), 99);
        assert_eq!(bucket_repay_native(100), 100);
        assert_eq!(bucket_repay_native(123), 120);
        assert_eq!(bucket_repay_native(999), 990);
        assert_eq!(bucket_repay_native(1000), 1000);
        assert_eq!(bucket_repay_native(1234567), 1200000);
    }

    #[tokio::test]
    async fn test_competing_bots_counting() {
        let (tx, rx) = mpsc::channel(10);
        let mock_rpc = MockRpcClient::new(rx);
        let (mock_logger, events_shared) = MockLogger::new();
        let mock_oracle = MockPriceOracle;
        let service = ObserverService::new(mock_rpc, mock_logger, mock_oracle, Protocol::Kamino);

        let market = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
        let repay_mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let withdraw_mint = "So11111111111111111111111111111111111111112";

        // 1. Failed attempt 1: Out of window (will be purged on next insertion or success)
        tx.send(LogEntry {
            signature: "failed_out".to_string(),
            is_error: true,
            received_at_ms: 500,
            logs: vec![
                "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral".to_string(),
                format!("Program log: lending_market: {}", market),
                format!("Program log: repay_reserve: {}", repay_mint),
                format!("Program log: withdraw_reserve: {}", withdraw_mint),
                "Program log: repay_amount: 10000000".to_string(),
            ],
        }).await.unwrap();

        // 2. Failed attempt 2: Duplicate signature (should be ignored)
        tx.send(LogEntry {
            signature: "failed_out".to_string(),
            is_error: true,
            received_at_ms: 600,
            logs: vec![
                "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral".to_string(),
                format!("Program log: lending_market: {}", market),
                format!("Program log: repay_reserve: {}", repay_mint),
                format!("Program log: withdraw_reserve: {}", withdraw_mint),
                "Program log: repay_amount: 10000000".to_string(),
            ],
        }).await.unwrap();

        // 3. Failed attempt 3: Matching
        tx.send(LogEntry {
            signature: "failed_1".to_string(),
            is_error: true,
            received_at_ms: 1000,
            logs: vec![
                "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral".to_string(),
                format!("Program log: lending_market: {}", market),
                format!("Program log: repay_reserve: {}", repay_mint),
                format!("Program log: withdraw_reserve: {}", withdraw_mint),
                "Program log: repay_amount: 10000000".to_string(),
            ],
        }).await.unwrap();

        // 4. Failed attempt 4: Matching (slight variation in repay_amount, should bucket to same)
        tx.send(LogEntry {
            signature: "failed_2".to_string(),
            is_error: true,
            received_at_ms: 2000,
            logs: vec![
                "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral".to_string(),
                format!("Program log: lending_market: {}", market),
                format!("Program log: repay_reserve: {}", repay_mint),
                format!("Program log: withdraw_reserve: {}", withdraw_mint),
                "Program log: repay_amount: 10000001".to_string(),
            ],
        }).await.unwrap();

        // 5. Failed attempt 5: Missing withdraw_mint (should be ignored)
        tx.send(LogEntry {
            signature: "failed_no_withdraw".to_string(),
            is_error: true,
            received_at_ms: 4000,
            logs: vec![
                "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral".to_string(),
                format!("Program log: lending_market: {}", market),
                format!("Program log: repay_reserve: {}", repay_mint),
                // Missing withdraw_reserve
                "Program log: repay_amount: 10000000".to_string(),
            ],
        }).await.unwrap();

        // 6. Successful liquidation
        tx.send(LogEntry {
            signature: "success".to_string(),
            is_error: false,
            received_at_ms: 31000, // 30.5s after failed_out (out), 30s after failed_1 (in)
            logs: vec![
                "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral".to_string(),
                format!("Program log: lending_market: {}", market),
                format!("Program log: repay_reserve: {}", repay_mint),
                format!("Program log: withdraw_reserve: {}", withdraw_mint),
                "Program log: repay_amount: 10000000".to_string(),
                "Program log: withdraw_amount: 500000000".to_string(),
            ],
        }).await.unwrap();

        drop(tx);
        service.watch().await.expect("watch() failed");

        let events = events_shared.lock().unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.signature, "success");
        // Should match failed_1 and failed_2.
        // failed_out is purged.
        assert_eq!(event.competing_bots, 2);
    }

    #[tokio::test]
    async fn test_observer_full_cycle_with_mocks() {
        let (tx, rx) = mpsc::channel(1);
        let mock_rpc = MockRpcClient::new(rx);
        let (mock_logger, events_shared) = MockLogger::new();
        let mock_oracle = MockPriceOracle;
        let service = ObserverService::new(mock_rpc, mock_logger, mock_oracle, Protocol::Kamino);

        // Inject a valid Kamino liquidation log
        let log_entry = LogEntry {
            signature: "test_signature".to_string(),
            is_error: false,
            received_at_ms: 0,
            logs: vec![
                "Program KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD invoke [1]".to_string(),
                "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral".to_string(),
                "Program log: lending_market: 7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF".to_string(),
                "Program log: obligation_owner: 9XCpqnGzLLLrHDHJPBHHHJDDabcLiquidatedUser1111".to_string(),
                "Program log: liquidator: BotLiquidatorPubkeyAbCdEfGhIjKlMnOpQrStUvWxYz".to_string(),
                "Program log: repay_reserve: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string(),
                "Program log: withdraw_reserve: So11111111111111111111111111111111111111112".to_string(),
                "Program log: repay_amount: 10000000".to_string(),   // 10.0 USDC (6 decimals)
                "Program log: withdraw_amount: 500000000".to_string(), // 0.5 SOL (9 decimals)
            ],
        };
        tx.send(log_entry).await.unwrap();
        // Drop tx so the watch() loop terminates after processing the message
        drop(tx);

        service.watch().await.expect("watch() failed");

        let events = events_shared.lock().unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.signature, "test_signature");
        assert_eq!(event.protocol, "Kamino");
        assert_eq!(event.market, "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF");
        assert_eq!(event.repay_symbol, "USDC");
        assert_eq!(event.withdraw_symbol, "SOL");
        assert!((event.repay_amount - 10.0).abs() < 1e-9);
        assert!((event.withdraw_amount - 0.5).abs() < 1e-9);
        assert!((event.repaid_usd - 10.0).abs() < 1e-9); // 10.0 * 1.0
        assert!((event.withdrawn_usd - 75.0).abs() < 1e-9); // 0.5 * 150.0
        assert!((event.profit_usd - 65.0).abs() < 1e-9); // 75.0 - 10.0
    }

    fn make_logs(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_actual_klend_log_format() {
        let logs = make_logs(&[
            "Program KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD invoke [1]",
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateralV2",
            "Program log: lending_market: 7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF",
            "Program log: obligation_owner: 9XCpqnGzLLLrHDHJPBHHHJDDabcLiquidatedUser1111",
            "Program log: liquidator: BotLiquidatorPubkeyAbCdEfGhIjKlMnOpQrStUvWxYz",
            "Program log: repay_reserve: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "Program log: withdraw_reserve: So11111111111111111111111111111111111111112",
            "Program log: repay_amount: 10000000",   // 10.0 USDC (6 decimals)
            "Program log: withdraw_amount: 500000000", // 0.5 SOL (9 decimals)
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.market, "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF");
        assert_eq!(p.liquidated_user, "9XCpqnGzLLLrHDHJPBHHHJDDabcLiquidatedUser1111");
        assert_eq!(p.liquidator, "BotLiquidatorPubkeyAbCdEfGhIjKlMnOpQrStUvWxYz");
        assert_eq!(p.repay_mint, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(p.withdraw_mint, "So11111111111111111111111111111111111111112");
        assert_eq!(p.repay_symbol, "USDC");
        assert_eq!(p.withdraw_symbol, "SOL");
        assert_eq!(p.repay_native, 10_000_000);
        assert_eq!(p.withdraw_native, 500_000_000);
        assert!((p.repay_amount - 10.0).abs() < 1e-9);
        assert!((p.withdraw_amount - 0.5).abs() < 1e-9);
    }

    #[test]
    fn parses_full_liquidation_normalization() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateralV2",
            "Program log: repay_mint: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "Program log: repay_amount: 5000000",    // 5.0 USDC
            "Program log: withdraw_mint: So11111111111111111111111111111111111111112",
            "Program log: withdraw_amount: 100000000", // 0.1 SOL
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.repay_symbol, "USDC");
        assert_eq!(p.withdraw_symbol, "SOL");
        assert!((p.repay_amount - 5.0).abs() < 1e-9);
        assert!((p.withdraw_amount - 0.1).abs() < 1e-9);
    }

    #[test]
    fn falls_back_gracefully_when_logs_missing() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral",
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.market, "N/A");
        assert_eq!(p.liquidated_user, "N/A");
        assert_eq!(p.liquidator, "N/A");
        assert_eq!(p.repay_native, 0);
        assert_eq!(p.withdraw_native, 0);
        assert_eq!(p.repay_amount, 0.0);
        assert_eq!(p.withdraw_amount, 0.0);
    }

    #[test]
    fn unknown_mint_keeps_native_amount() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateralV2",
            "Program log: repay_mint: UnknownMint111111111111111111111111111111111",
            "Program log: repay_amount: 999999",
            "Program log: withdraw_mint: So11111111111111111111111111111111111111112",
            "Program log: withdraw_amount: 2000000000", // 2.0 SOL
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.repay_symbol, "UNK-Unkn");
        assert_eq!(p.repay_native, 999_999);
        assert_eq!(p.repay_amount, 0.0);
        assert_eq!(p.withdraw_symbol, "SOL");
        assert!((p.withdraw_amount - 2.0).abs() < 1e-9);
    }

    #[test]
    fn obligation_pda_does_not_set_liquidated_user() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral",
            "Program log: obligation: SomePdaAddress1111111111111111111111111111111",
            "Program log: liquidator: BotLiquidatorPubkeyAbCdEfGhIjKlMnOpQrStUvWxYz",
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.liquidated_user, "N/A");
        assert_eq!(p.liquidator, "BotLiquidatorPubkeyAbCdEfGhIjKlMnOpQrStUvWxYz");
    }

    #[test]
    fn zero_repay_amount_is_captured() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral",
            "Program log: repay_amount: 0",
            "Program log: repaid_amount: 9999999",
            "Program log: withdraw_amount: 500000000",
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.repay_native, 0);
        assert_eq!(p.withdraw_native, 500_000_000);
    }

    #[test]
    fn parses_jito_sol_withdraw() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateralV2",
            "Program log: repay_reserve: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "Program log: repay_amount: 1000000",
            "Program log: withdraw_reserve: J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn",
            "Program log: withdraw_amount: 1000000000",
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.repay_symbol, "USDC");
        assert_eq!(p.withdraw_symbol, "JitoSOL");
        assert!((p.repay_amount - 1.0).abs() < 1e-9);
        assert!((p.withdraw_amount - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    #[ignore]
    async fn integration_real_liquidation_apr05_2026() {
        dotenv::dotenv().ok();
        let rpc_url = std::env::var("RPC_URL")
            .expect("RPC_URL must be set in .env to run integration tests");

        let signature = "2VwLWgu9zRrDjE5jF5WxvnizCAWqf231Sb43EA7WpWHWECbVdZNgtc6vRCD12yBzCgVfdwEYaigQkhDq3jdmw8J6";

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTransaction",
            "params": [
                signature,
                {"encoding": "json", "maxSupportedTransactionVersion": 0}
            ]
        });

        let client = reqwest::Client::new();
        let resp = client
            .post(&rpc_url)
            .json(&payload)
            .send()
            .await
            .expect("RPC request failed");

        let json: serde_json::Value = resp.json().await.expect("Failed to parse RPC response");
        let result = json.get("result").expect("No 'result' field in RPC response");
        assert!(!result.is_null());

        let err = &result["meta"]["err"];
        assert!(err.is_null());

        let logs: Vec<String> = result["meta"]["logMessages"]
            .as_array()
            .expect("logMessages must be an array")
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();

        let detected = logs.iter().any(|log| log.contains(LIQUIDATE_FILTER));
        assert!(detected);

        let parsed = parse_liquidation_logs(&logs);
        assert_eq!(parsed.repay_native, 67_014_738);
        assert_eq!(parsed.withdraw_native, 860_053_474);
    }
}
