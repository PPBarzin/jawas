use std::time::Duration;
use crate::ports::logger::{LiquidationLogger, ObservationEvent};
use crate::utils::utc_now;

/// Phase 1: Periodic heartbeat to ensure the bot is alive.
pub struct HeartbeatService<L: LiquidationLogger> {
    logger: L,
}

impl<L: LiquidationLogger> HeartbeatService<L> {
    pub fn new(logger: L) -> Self {
        Self { logger }
    }

    /// Periodically sends a "LIFEBIT" event to the logger.
    pub async fn run(&self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        
        // Skip the first immediate tick
        ticker.tick().await;

        loop {
            ticker.tick().await;

            let event = ObservationEvent {
                timestamp: utc_now(),
                signature: "LIFEBIT".to_string(),
                protocol: "N/A".to_string(),
                market: "N/A".to_string(),
                liquidated_user: "system-heartbeat".to_string(),
                liquidator: "N/A".to_string(),
                repay_mint: "N/A".to_string(),
                withdraw_mint: "N/A".to_string(),
                repay_symbol: "N/A".to_string(),
                withdraw_symbol: "N/A".to_string(),
                repay_amount: 0.0,
                withdraw_amount: 0.0,
                repaid_usd: 0.0,
                withdrawn_usd: 0.0,
                profit_usd: 0.0,
                delay_ms: 0,
                competing_bots: 0,
                status: "WATCHED".to_string(),
            };

            if let Err(e) = self.logger.log_observation(&event).await {
                eprintln!("[heartbeat] log failed: {}", e);
            } else {
                println!("[heartbeat] sent at {}", event.timestamp);
            }
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use tokio::time::{Duration, sleep};

    struct MockLogger {
        events: Arc<Mutex<Vec<ObservationEvent>>>,
    }

    impl MockLogger {
        fn new() -> (Self, Arc<Mutex<Vec<ObservationEvent>>>) {
            let events = Arc::new(Mutex::new(Vec::new()));
            (Self { events: events.clone() }, events)
        }
    }

    #[async_trait]
    impl LiquidationLogger for MockLogger {
        async fn log_observation(&self, event: &ObservationEvent) -> anyhow::Result<()> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_heartbeat_sends_event() {
        let (mock_logger, events_shared) = MockLogger::new();
        let service = HeartbeatService::new(mock_logger);
        
        // Run heartbeat in background
        let handle = tokio::spawn(async move {
            service.run(Duration::from_millis(50)).await;
        });

        // Sleep to let a few ticks happen (first tick is skipped by code)
        // Millis: 0 (start), 50 (tick 1 - skip? no, code skips FIRST immediate tick)
        // ticker.tick().await; // skip
        // loop { ticker.tick().await; ... }
        // So:
        // T=0: ticker.tick() returns immediately (skip)
        // T=50: ticker.tick() returns, sends LIFEBIT
        // T=100: ticker.tick() returns, sends LIFEBIT
        sleep(Duration::from_millis(120)).await;
        handle.abort();

        let events = events_shared.lock().unwrap();
        assert!(events.len() >= 2, "Expected at least 2 events, got {}", events.len());
        assert_eq!(events[0].signature, "LIFEBIT");
        assert_eq!(events[0].liquidated_user, "system-heartbeat");
    }
}
