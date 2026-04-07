use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

use crate::ports::logger::{LiquidationLogger, ObservationEvent};

#[derive(Clone)]
pub struct AirtableAdapter {
    tx: mpsc::Sender<ObservationEvent>,
}

impl AirtableAdapter {
    pub fn new(api_token: String, base_id: String, table_watch: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<ObservationEvent>(100);
        let client = Client::new();
        let api_token = Arc::new(api_token);
        let base_id = Arc::new(base_id);
        let table_watch = Arc::new(table_watch);

        // Spawn background worker for batching
        tokio::spawn(async move {
            let mut buffer = Vec::with_capacity(10);
            let mut last_flush = tokio::time::Instant::now();

            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        buffer.push(event);
                        if buffer.len() >= 10 {
                            let _ = flush_batch(&client, &api_token, &base_id, &table_watch, &mut buffer).await;
                            last_flush = tokio::time::Instant::now();
                        }
                    }
                    _ = sleep(Duration::from_secs(30)) => {
                        if !buffer.is_empty() && last_flush.elapsed() >= Duration::from_secs(30) {
                            let _ = flush_batch(&client, &api_token, &base_id, &table_watch, &mut buffer).await;
                            last_flush = tokio::time::Instant::now();
                        }
                    }
                }
            }
        });

        Self { tx }
    }
}

async fn flush_batch(
    client: &Client,
    api_token: &str,
    base_id: &str,
    table_watch: &str,
    buffer: &mut Vec<ObservationEvent>,
) -> Result<()> {
    let url = format!("https://api.airtable.com/v0/{}/{}", base_id, table_watch);

    let records: Vec<_> = buffer
        .drain(..)
        .map(|event| {
            json!({
                "fields": {
                    "Name":                  format!("WATCH {}", event.timestamp),
                    "Tx Signature":          event.signature,
                    "Protocol":              event.protocol,
                    "Market":                event.market,
                    "Liquidated User":       event.liquidated_user,
                    "Liquidator":            event.liquidator,
                    "Repay Mint":            event.repay_mint,
                    "Withdraw Mint":         event.withdraw_mint,
                    "Repay Symbol":          event.repay_symbol,
                    "Withdraw Symbol":       event.withdraw_symbol,
                    "Repay Amount":          event.repay_amount,
                    "Withdraw Amount":       event.withdraw_amount,
                    "Repaid USD":            event.repaid_usd,
                    "Withdrawn USD":         event.withdrawn_usd,
                    "Profit USD":            event.profit_usd,
                    "Net_Profit_Liquidation": event.profit_usd,
                    "Timestamp":             event.timestamp,
                    "Delay MS":              event.delay_ms,
                    "Competing Bots":        event.competing_bots.to_string(),
                    "Status":                event.status,
                }
            })
        })
        .collect();

    let body = json!({ "records": records });

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to reach Airtable API")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        eprintln!("Airtable batch error {}: {}", status, text);
        anyhow::bail!("Airtable returned {}: {}", status, text);
    }

    Ok(())
}

impl LiquidationLogger for AirtableAdapter {
    async fn log_observation(&self, event: &ObservationEvent) -> Result<()> {
        self.tx
            .send(event.clone())
            .await
            .context("Failed to send event to batcher")?;
        Ok(())
    }
}
