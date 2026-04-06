// Phase 1: Observes liquidations executed by other bots on Kamino.

use crate::domain::token::{native_to_human, token_info};
use crate::ports::logger::{LiquidationLogger, ObservationEvent};
use crate::ports::oracle::PriceOracle;
use crate::ports::rpc::StreamingRpcClient;
use crate::utils::utc_now;

const KAMINO_PROGRAM_ID: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const KAMINO_LIQUIDATE_INSTRUCTION: &str = "Instruction: LiquidateObligationAndRedeemReserveCollateral";

pub struct ObserverService<R: StreamingRpcClient, L: LiquidationLogger, O: PriceOracle> {
    rpc: R,
    logger: L,
    oracle: O,
}

impl<R: StreamingRpcClient, L: LiquidationLogger, O: PriceOracle> ObserverService<R, L, O> {
    pub fn new(rpc: R, logger: L, oracle: O) -> Self {
        Self { rpc, logger, oracle }
    }

    /// Subscribes to Kamino program logs and forwards each observed liquidation
    /// to the logger. Runs until the WebSocket stream closes.
    pub async fn watch(&self) -> anyhow::Result<()> {
        let mut rx = self.rpc.subscribe_to_logs(KAMINO_PROGRAM_ID).await?;

        while let Some(entry) = rx.recv().await {
            if entry.is_error {
                continue;
            }

            let is_liquidation = entry
                .logs
                .iter()
                .any(|log| log.contains(KAMINO_LIQUIDATE_INSTRUCTION));
            if !is_liquidation {
                continue;
            }

            let parsed = parse_liquidation_logs(&entry.logs);

            // Fetch prices for USD estimation
            let repay_price = self.oracle.get_price_usd(&parsed.repay_mint).await.unwrap_or(0.0);
            let withdraw_price = self.oracle.get_price_usd(&parsed.withdraw_mint).await.unwrap_or(0.0);

            let repaid_usd = parsed.repay_amount * repay_price;
            let withdrawn_usd = parsed.withdraw_amount * withdraw_price;
            let profit_usd = withdrawn_usd - repaid_usd;
            let delay_ms = entry.received_at.elapsed().as_millis() as u64;

            println!(
                "[observer] liquidation | sig={} market={} borrower={} liquidator={} \
                 repay={} {} (native={}, ${:.2}) withdraw={} {} (native={}, ${:.2}) profit=${:.2} delay={}ms",
                entry.signature,
                parsed.market,
                parsed.liquidated_user,
                parsed.liquidator,
                parsed.repay_amount,
                parsed.repay_symbol,
                parsed.repay_native,
                repaid_usd,
                parsed.withdraw_amount,
                parsed.withdraw_symbol,
                parsed.withdraw_native,
                withdrawn_usd,
                profit_usd,
                delay_ms,
            );

            let event = ObservationEvent {
                timestamp: utc_now(),
                signature: entry.signature.clone(),
                protocol: "Kamino".to_string(),
                market: parsed.market,
                liquidated_user: parsed.liquidated_user,
                liquidator: parsed.liquidator,
                repay_mint: parsed.repay_mint,
                withdraw_mint: parsed.withdraw_mint,
                repay_symbol: parsed.repay_symbol,
                withdraw_symbol: parsed.withdraw_symbol,
                repay_amount: parsed.repay_amount,
                withdraw_amount: parsed.withdraw_amount,
                repaid_usd,
                withdrawn_usd,
                profit_usd,
                delay_ms,
                competing_bots: 0,
                status: "WATCHED".to_string(),
            };

            if let Err(e) = self.logger.log_observation(&event).await {
                eprintln!("[observer] failed to log {}: {}", entry.signature, e);
            }
        }

        Ok(())
    }
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

    for line in logs {
        let content = strip_program_log_prefix(line);

        if market == "N/A" {
            if let Some(v) = extract_token(content, "lending_market:") {
                market = v;
            } else if let Some(v) = extract_token(content, "market:") {
                market = v;
            }
        }

        if liquidated_user == "N/A" {
            if let Some(v) = extract_token(content, "obligation_owner:") {
                liquidated_user = v;
            } else if let Some(v) = extract_token(content, "borrower:") {
                liquidated_user = v;
            }
            // `obligation:` is intentionally excluded — it is the PDA address, not the borrower's wallet
        }

        if liquidator == "N/A" {
            if let Some(v) = extract_token(content, "liquidator:") {
                liquidator = v;
            }
        }

        if repay_mint == "N/A" {
            if let Some(v) = extract_token(content, "repay_mint:") {
                repay_mint = v;
            } else if let Some(v) = extract_token(content, "repay_reserve:") {
                repay_mint = v;
            }
        }

        if withdraw_mint == "N/A" {
            if let Some(v) = extract_token(content, "withdraw_mint:") {
                withdraw_mint = v;
            } else if let Some(v) = extract_token(content, "withdraw_reserve:") {
                withdraw_mint = v;
            }
        }

        if repay_native.is_none() {
            if let Some(v) = extract_u64(content, "repay_amount:") {
                repay_native = Some(v);
            } else if let Some(v) = extract_u64(content, "repaid_amount:") {
                repay_native = Some(v);
            }
        }

        if withdraw_native.is_none() {
            if let Some(v) = extract_u64(content, "withdraw_amount:") {
                withdraw_native = Some(v);
            } else if let Some(v) = extract_u64(content, "withdrawn_amount:") {
                withdraw_native = Some(v);
            }
        }
    }

    // Normalization and symbols
    let repay_symbol = token_info(&repay_mint).map(|i| i.symbol.to_string()).unwrap_or_else(|| "UNKNOWN".to_string());
    let withdraw_symbol = token_info(&withdraw_mint).map(|i| i.symbol.to_string()).unwrap_or_else(|| "UNKNOWN".to_string());

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
    use crate::ports::rpc::LogEntry;

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

    #[tokio::test]
    async fn test_observer_full_cycle_with_mocks() {
        let (tx, rx) = mpsc::channel(1);
        let mock_rpc = MockRpcClient::new(rx);
        let (mock_logger, events_shared) = MockLogger::new();
        let mock_oracle = MockPriceOracle;
        let service = ObserverService::new(mock_rpc, mock_logger, mock_oracle);

        // Inject a valid Kamino liquidation log
        let log_entry = LogEntry {
            signature: "test_signature".to_string(),
            is_error: false,
            received_at: std::time::Instant::now(),
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

    /// Logs matching the actual KLend Anchor event format observed on mainnet.
    /// Uses `lending_market`, `obligation`, `repay_reserve`, `withdraw_reserve`
    /// as emitted by KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD.
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
        assert!((p.repay_amount - 10.0).abs() < 1e-9, "repay_amount={}", p.repay_amount);
        assert!((p.withdraw_amount - 0.5).abs() < 1e-9, "withdraw_amount={}", p.withdraw_amount);
    }

    /// repay_mint / withdraw_mint take priority over reserve fallbacks when both are present.
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

    /// Missing fields stay at their default "N/A" / 0 values without panic.
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

    /// Unknown mints produce symbol "UNKNOWN" and amount 0.0, but native amount is preserved.
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

        assert_eq!(p.repay_symbol, "UNKNOWN");
        assert_eq!(p.repay_native, 999_999);
        assert_eq!(p.repay_amount, 0.0, "unknown mint must yield 0.0 human amount");
        assert_eq!(p.withdraw_symbol, "SOL");
        assert!((p.withdraw_amount - 2.0).abs() < 1e-9);
    }

    /// `obligation:` is a PDA address, not the borrower's wallet — must not populate liquidated_user.
    #[test]
    fn obligation_pda_does_not_set_liquidated_user() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral",
            "Program log: obligation: SomePdaAddress1111111111111111111111111111111",
            "Program log: liquidator: BotLiquidatorPubkeyAbCdEfGhIjKlMnOpQrStUvWxYz",
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.liquidated_user, "N/A", "obligation PDA must not be used as liquidated_user");
        assert_eq!(p.liquidator, "BotLiquidatorPubkeyAbCdEfGhIjKlMnOpQrStUvWxYz");
    }

    /// A repay_amount of 0 must be captured, not skipped due to a zero guard.
    #[test]
    fn zero_repay_amount_is_captured() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateral",
            "Program log: repay_amount: 0",
            "Program log: repaid_amount: 9999999",  // fallback must NOT override the explicit 0
            "Program log: withdraw_amount: 500000000",
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.repay_native, 0, "explicit repay_amount: 0 must be stored, not skipped");
        assert_eq!(p.withdraw_native, 500_000_000);
    }

    /// jSOL / bSOL variants are recognized by the catalogue.
    #[test]
    fn parses_jito_sol_withdraw() {
        let logs = make_logs(&[
            "Program log: Instruction: LiquidateObligationAndRedeemReserveCollateralV2",
            "Program log: repay_reserve: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "Program log: repay_amount: 1000000",  // 1.0 USDC
            "Program log: withdraw_reserve: J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn",
            "Program log: withdraw_amount: 1000000000", // 1.0 JitoSOL
        ]);

        let p = parse_liquidation_logs(&logs);

        assert_eq!(p.repay_symbol, "USDC");
        assert_eq!(p.withdraw_symbol, "JitoSOL");
        assert!((p.repay_amount - 1.0).abs() < 1e-9);
        assert!((p.withdraw_amount - 1.0).abs() < 1e-9);
    }
}
