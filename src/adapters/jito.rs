use crate::ports::jito::JitoPort;
use anyhow::{Result, Context};
use async_trait::async_trait;
use reqwest::Client;
use solana_sdk::transaction::VersionedTransaction;
use serde_json::json;

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
}

#[async_trait]
impl JitoPort for JitoAdapter {
    async fn send_bundle(&self, transactions: Vec<VersionedTransaction>) -> Result<String> {
        let serialized_txs: Vec<String> = transactions
            .iter()
            .map(|tx| {
                let bytes = bincode::serialize(tx)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize transaction: {}", e))?;
                Ok(bs58::encode(bytes).into_string())
            })
            .collect::<Result<Vec<String>>>()?;

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendBundle",
            "params": [serialized_txs]
        });

        let response = self.client.post(&self.url)
            .json(&body)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        if let Some(err) = response.get("error") {
             return Err(anyhow::anyhow!("Jito error: {:?}", err));
        }

        response["result"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("Invalid Jito response: {:?}", response))
    }

    async fn get_tip_recommendation(&self) -> Result<u64> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTipFloor",
            "params": []
        });

        let response = self.client.post(&self.url)
            .json(&body)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        if let Some(err) = response.get("error") {
             return Err(anyhow::anyhow!("Jito error: {:?}", err));
        }

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
