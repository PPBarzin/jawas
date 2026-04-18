use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use async_trait::async_trait;

use crate::ports::logger::{LiquidationLogger, ObservationEvent};
use crate::ports::config::ConfigPort;

#[derive(Clone)]
pub struct AirtableAdapter {
    tx: mpsc::Sender<ObservationEvent>,
    api_token: Arc<String>,
    base_id: Arc<String>,
    client: Client,
}

impl AirtableAdapter {
    pub fn new(api_token: String, base_id: String, table_watch: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<ObservationEvent>(100);
        let client = Client::new();
        let api_token_arc = Arc::new(api_token.clone());
        let base_id_arc = Arc::new(base_id.clone());
        let table_watch_arc = Arc::new(table_watch);

        // Spawn background worker for batching
        let worker_client = client.clone();
        let worker_token = api_token_arc.clone();
        let worker_base = base_id_arc.clone();
        tokio::spawn(async move {
            let mut buffer = Vec::with_capacity(10);
            let mut last_flush = tokio::time::Instant::now();

            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        buffer.push(event);
                        if buffer.len() >= 10 {
                            let _ = flush_batch(&worker_client, &worker_token, &worker_base, &table_watch_arc, &mut buffer).await;
                            last_flush = tokio::time::Instant::now();
                        }
                    }
                    _ = sleep(Duration::from_secs(30)) => {
                        if !buffer.is_empty() && last_flush.elapsed() >= Duration::from_secs(30) {
                            let _ = flush_batch(&worker_client, &worker_token, &worker_base, &table_watch_arc, &mut buffer).await;
                            last_flush = tokio::time::Instant::now();
                        }
                    }
                }
            }
        });

        Self { tx, api_token: api_token_arc, base_id: base_id_arc, client }
    }

    /// Fetches the list of active token symbols from the jawas-whitelist table.
    pub async fn fetch_whitelist_internal(&self) -> Result<Vec<String>> {
        let url = format!(
            "https://api.airtable.com/v0/{}/jawas-whitelist",
            self.base_id
        );

        let response = self.client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .query(&[("filterByFormula", "{Active}=1")])
            .send()
            .await
            .context("Failed to reach Airtable API for whitelist")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Airtable whitelist error {}: {}", status, text);
        }

        let json: serde_json::Value = response.json().await?;
        let mut whitelist = Vec::new();

        if let Some(records) = json["records"].as_array() {
            for rec in records {
                if let Some(symbol) = rec["fields"]["Symbol"].as_str() {
                    whitelist.push(symbol.to_string());
                }
            }
        }

        Ok(whitelist)
    }
}

#[async_trait]
impl ConfigPort for AirtableAdapter {
    async fn fetch_whitelist(&self) -> Result<Vec<String>> {
        self.fetch_whitelist_internal().await
    }
}

async fn flush_batch(
    client: &Client,
    api_token: &str,
    base_id: &str,
    table_watch: &str,
    buffer: &mut Vec<ObservationEvent>,
) -> Result<()> {
    let mut deduped: Vec<ObservationEvent> = Vec::with_capacity(buffer.len());
    let mut local_seen_tx_signatures: HashSet<String> = HashSet::new();

    for event in buffer.drain(..) {
        if !is_tx_signature_like(&event.signature) {
            deduped.push(event);
            continue;
        }

        if local_seen_tx_signatures.insert(event.signature.clone()) {
            deduped.push(event);
        }
    }

    if deduped.is_empty() {
        return Ok(());
    }

    let tx_signatures: Vec<String> = deduped
        .iter()
        .filter(|event| is_tx_signature_like(&event.signature))
        .map(|event| event.signature.clone())
        .collect();

    let existing_signatures = if tx_signatures.is_empty() {
        HashSet::new()
    } else {
        fetch_existing_tx_signatures(client, api_token, base_id, table_watch, &tx_signatures).await?
    };

    let url = format!("https://api.airtable.com/v0/{}/{}", base_id, table_watch);

    let records: Vec<_> = deduped
        .into_iter()
        .filter(|event| {
            !is_tx_signature_like(&event.signature) || !existing_signatures.contains(&event.signature)
        })
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
                    "Timestamp":             event.timestamp,
                    "Delay MS":              event.delay_ms,
                    "Competing Bots":        event.competing_bots.to_string(),
                    "Status":                event.status,
                }
            })
        })
        .collect();

    if records.is_empty() {
        return Ok(());
    }

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

fn is_tx_signature_like(signature: &str) -> bool {
    let len_ok = (64..=88).contains(&signature.len());
    let base58_ok = signature
        .bytes()
        .all(|b| matches!(b,
            b'1'..=b'9' |
            b'A'..=b'H' |
            b'J'..=b'N' |
            b'P'..=b'Z' |
            b'a'..=b'k' |
            b'm'..=b'z'
        ));

    len_ok && base58_ok
}

fn airtable_formula_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\"', "\\\"")
}

async fn fetch_existing_tx_signatures(
    client: &Client,
    api_token: &str,
    base_id: &str,
    table_watch: &str,
    signatures: &[String],
) -> Result<HashSet<String>> {
    let unique_signatures: HashSet<&str> = signatures.iter().map(String::as_str).collect();
    if unique_signatures.is_empty() {
        return Ok(HashSet::new());
    }

    let mut formulas: Vec<String> = unique_signatures
        .into_iter()
        .map(|signature| format!("{{Tx Signature}}=\"{}\"", airtable_formula_string(signature)))
        .collect();
    formulas.sort();
    let filter_formula = if formulas.len() == 1 {
        formulas.remove(0)
    } else {
        format!("OR({})", formulas.join(","))
    };

    let url = format!("https://api.airtable.com/v0/{}/{}", base_id, table_watch);
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .query(&[
            ("filterByFormula", filter_formula.as_str()),
            ("fields[]", "Tx Signature"),
            ("pageSize", "100"),
        ])
        .send()
        .await
        .context("Failed to reach Airtable API for duplicate check")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("Airtable duplicate check error {}: {}", status, text);
    }

    let payload: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse Airtable duplicate-check response")?;

    let mut found = HashSet::new();
    if let Some(records) = payload["records"].as_array() {
        for record in records {
            if let Some(signature) = record["fields"]["Tx Signature"].as_str() {
                found.insert(signature.to_string());
            }
        }
    }

    Ok(found)
}

#[async_trait]
impl LiquidationLogger for AirtableAdapter {
    async fn log_observation(&self, event: &ObservationEvent) -> Result<()> {
        self.tx
            .send(event.clone())
            .await
            .context("Failed to send event to batcher")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_tx_signature_like;

    #[test]
    fn recognizes_realistic_transaction_signatures() {
        assert!(is_tx_signature_like(
            "4QRFN9kUnhGbEMAm1BC5ApaRFGzC4gGpyG2qw6UJVi9FRnh6Ma8Ln5EkC5xrHWVVvaooxNmqfAVVA88mtHBBgQFZ"
        ));
        assert!(!is_tx_signature_like("Jawas Kamino is alive"));
        assert!(!is_tx_signature_like("TIMEOUT_2026-04-18T08:38:08Z"));
    }
}
