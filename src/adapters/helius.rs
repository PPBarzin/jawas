use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client as HttpClient;
use serde_json::json;
use solana_client::rpc_client::RpcClient as SolanaRpcClient;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::ports::rpc::{LogEntry, RpcClient, StreamingRpcClient, TransactionInfo};

pub struct HeliusAdapter {
    client: SolanaRpcClient,
    ws_url: String,
    rpc_url: String,
    http_client: HttpClient,
}

impl HeliusAdapter {
    /// Constructs the adapter from a raw URL string.
    /// Trailing slashes are stripped to avoid double-slash issues with QuickNode URLs.
    pub fn new(rpc_url: &str, ws_url: &str) -> Self {
        let url = rpc_url.trim_end_matches('/').to_string();
        let ws = ws_url.trim_end_matches('/').to_string();
        Self {
            client: SolanaRpcClient::new(url.clone()),
            ws_url: ws,
            rpc_url: url,
            http_client: HttpClient::new(),
        }
    }
}

impl RpcClient for HeliusAdapter {
    async fn get_version(&self) -> Result<String> {
        let version = self
            .client
            .get_version()
            .context("Failed to reach Solana RPC — check RPC_URL")?;
        Ok(version.solana_core)
    }

    async fn get_transaction(&self, signature: &str) -> Result<TransactionInfo> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTransaction",
            "params": [signature, {"encoding": "json", "maxSupportedTransactionVersion": 0}]
        });

        let resp = self.http_client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .context("getTransaction HTTP request failed")?;

        let body: serde_json::Value = resp.json().await.context("getTransaction response parse failed")?;

        let result = &body["result"];
        if result.is_null() {
            anyhow::bail!("getTransaction returned null for signature {}", signature);
        }

        let account_keys: Vec<String> = result["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();

        let raw_instructions = result["transaction"]["message"]["instructions"]
            .as_array()
            .unwrap_or(&vec![])
            .clone();

        let mut instruction_accounts: Vec<Vec<usize>> = Vec::new();
        let mut instruction_programs: Vec<usize> = Vec::new();

        for instr in &raw_instructions {
            let prog_idx = instr["programIdIndex"].as_u64().unwrap_or(0) as usize;
            let accounts: Vec<usize> = instr["accounts"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect();
            instruction_programs.push(prog_idx);
            instruction_accounts.push(accounts);
        }

        let block_time = result["blockTime"].as_u64();

        let pre_token_balances = parse_json_token_balances(&result["meta"]["preTokenBalances"]);
        let post_token_balances = parse_json_token_balances(&result["meta"]["postTokenBalances"]);

        Ok(TransactionInfo {
            account_keys,
            instruction_accounts,
            instruction_programs,
            block_time,
            pre_token_balances,
            post_token_balances,
        })
    }
}

fn parse_json_token_balances(value: &serde_json::Value) -> Vec<crate::ports::rpc::TokenBalance> {
    value
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let account_index = v["accountIndex"].as_u64()? as usize;
            let mint = v["mint"].as_str()?.to_string();
            let owner = v["owner"].as_str()?.to_string();
            let ui_amount = v["uiTokenAmount"]["uiAmount"].as_f64().unwrap_or(0.0);
            Some(crate::ports::rpc::TokenBalance {
                account_index,
                mint,
                owner,
                ui_amount,
            })
        })
        .collect()
}

impl StreamingRpcClient for HeliusAdapter {
    async fn subscribe_to_logs(
        &self,
        program_id: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<LogEntry>> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let program_id_owned = program_id.to_string();
        let ws_url = self.ws_url.clone();

        tokio::spawn(async move {
            let mut backoff_secs: u64 = 1;

            loop {
                eprintln!("[helius] connecting to WebSocket...");
                match connect_async(&ws_url).await {
                    Ok((mut ws_stream, _)) => {
                        eprintln!("[helius] WebSocket connected.");
                        backoff_secs = 1; // reset backoff on successful connection

                        let subscribe_msg = json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "logsSubscribe",
                            "params": [
                                { "mentions": [program_id_owned] },
                                { "commitment": "confirmed" }
                            ]
                        });

                        if let Err(e) = ws_stream
                            .send(Message::Text(subscribe_msg.to_string()))
                            .await
                        {
                            eprintln!("[helius] failed to send subscribe: {e}");
                        } else {
                            while let Some(msg) = ws_stream.next().await {
                                let received_at_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                                match msg {
                                    Ok(Message::Text(text)) => {
                                        if let Ok(value) =
                                            serde_json::from_str::<serde_json::Value>(&text)
                                        {
                                            if value
                                                .get("method")
                                                .and_then(|m| m.as_str())
                                                == Some("logsNotification")
                                            {
                                                let v = &value["params"]["result"]["value"];
                                                let signature = v["signature"]
                                                    .as_str()
                                                    .unwrap_or("")
                                                    .to_string();
                                                let is_error = !v["err"].is_null();
                                                let logs = v["logs"]
                                                    .as_array()
                                                    .map(|arr| {
                                                        arr.iter()
                                                            .filter_map(|l| {
                                                                l.as_str().map(str::to_string)
                                                            })
                                                            .collect()
                                                    })
                                                    .unwrap_or_default();

                                                let entry = LogEntry {
                                                    signature,
                                                    logs,
                                                    is_error,
                                                    received_at_ms,
                                                };
                                                if tx.send(entry).await.is_err() {
                                                    // Downstream receiver dropped — stop.
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    Ok(Message::Ping(data)) => {
                                        let _ = ws_stream.send(Message::Pong(data)).await;
                                    }
                                    Ok(Message::Close(_)) | Err(_) => break,
                                    _ => {}
                                }
                            }
                        }

                        eprintln!(
                            "[helius] WebSocket stream ended. Reconnecting in {backoff_secs}s..."
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[helius] WebSocket connection error: {e}. Retrying in {backoff_secs}s..."
                        );
                    }
                }

                if tx.is_closed() {
                    return;
                }

                sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(30);
            }
        });

        Ok(rx)
    }
}
