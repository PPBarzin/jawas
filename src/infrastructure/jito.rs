use crate::ports::jito::{JitoBundleStatus, JitoPort};
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use solana_sdk::transaction::VersionedTransaction;

#[derive(Clone)]
pub struct JitoAdapter {
    client: Client,
    url: String,
}

impl JitoAdapter {
    pub fn new(url: &str) -> Self {
        Self {
            client: Client::new(),
            url: url.to_string(),
        }
    }

    async fn rpc_call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let response = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        if let Some(err) = response.get("error") {
            return Err(anyhow::anyhow!("Jito error: {:?}", err));
        }

        Ok(response)
    }
}

#[async_trait]
impl JitoPort for JitoAdapter {
    async fn send_bundle(&self, transactions: Vec<VersionedTransaction>) -> Result<String> {
        let serialized_txs: Vec<String> = transactions
            .iter()
            .map(|tx| {
                let bytes = bincode::serialize(tx)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize transaction: {}", e))?;
                Ok(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    bytes,
                ))
            })
            .collect::<Result<Vec<String>>>()?;

        let response = self
            .rpc_call("sendBundle", json!([serialized_txs, { "encoding": "base64" }]))
            .await?;

        response["result"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("Invalid Jito response: {:?}", response))
    }

    async fn get_inflight_bundle_statuses(
        &self,
        bundle_ids: Vec<String>,
    ) -> Result<Vec<JitoBundleStatus>> {
        if bundle_ids.is_empty() {
            return Ok(Vec::new());
        }

        let response = self
            .rpc_call("getInflightBundleStatuses", json!([bundle_ids]))
            .await?;

        let values = response["result"]["value"]
            .as_array()
            .or_else(|| response["result"].as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid Jito bundle status response: {:?}", response))?;

        let mut statuses = Vec::new();
        for value in values {
            if value.is_null() {
                continue;
            }

            let bundle_id = value["bundle_id"]
                .as_str()
                .or_else(|| value["bundleId"].as_str())
                .unwrap_or_default()
                .to_string();
            if bundle_id.is_empty() {
                continue;
            }

            let status = value["status"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let landed_slot = value["landed_slot"]
                .as_u64()
                .or_else(|| value["landedSlot"].as_u64());

            statuses.push(JitoBundleStatus {
                bundle_id,
                status,
                landed_slot,
            });
        }

        Ok(statuses)
    }

    async fn get_tip_recommendation(&self) -> Result<u64> {
        let response = self.rpc_call("getTipFloor", json!([])).await?;

        // Response example: [{"landed_tips_25th_percentile": 0, "landed_tips_50th_percentile": 0, ...}]
        let result = response["result"]
            .as_array()
            .and_then(|arr| arr.get(0))
            .ok_or_else(|| anyhow::anyhow!("Invalid Jito response: {:?}", response))?;

        result["landed_tips_50th_percentile"]
            .as_u64()
            .context("Missing landed_tips_50th_percentile in Jito response")
    }
}
