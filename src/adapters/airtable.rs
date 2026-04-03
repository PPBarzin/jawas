use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;

use crate::ports::logger::{LiquidationLogger, ObservationEvent};

pub struct AirtableAdapter {
    client: Client,
    api_token: String,
    base_id: String,
    table_watch: String,
}

impl AirtableAdapter {
    pub fn new(api_token: String, base_id: String, table_watch: String) -> Self {
        Self {
            client: Client::new(),
            api_token,
            base_id,
            table_watch,
        }
    }

    fn base_url(&self, table: &str) -> String {
        format!(
            "https://api.airtable.com/v0/{}/{}",
            self.base_id, table
        )
    }
}

impl LiquidationLogger for AirtableAdapter {
    async fn log_observation(&self, event: &ObservationEvent) -> Result<()> {
        let url = self.base_url(&self.table_watch);

        let body = json!({
            "records": [{
                "fields": {
                    "Name":                  format!("WATCH {}", event.timestamp),
                    "borrower":              event.borrower,
                    "collateral_token":      event.collateral_token,
                    "collateral_amount":     event.collateral_amount,
                    "debt_repaid_usdc":      event.debt_repaid_usdc,
                    "profit_estimated_usd":  event.profit_estimated_usd,
                    "ltv_at_liquidation":    event.ltv_at_liquidation,
                    "delay_ms":              event.delay_ms,
                    "competing_bots":        event.competing_bots,
                    "winner_tx":             event.winner_tx,
                    "timestamp":             event.timestamp,
                }
            }]
        });

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to reach Airtable API")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Airtable returned {}: {}", status, text);
        }

        Ok(())
    }
}
