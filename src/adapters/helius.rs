use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use solana_client::rpc_client::RpcClient as SolanaRpcClient;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::ports::rpc::{LogEntry, RpcClient, StreamingRpcClient};

pub struct HeliusAdapter {
    client: SolanaRpcClient,
    ws_url: String,
}

impl HeliusAdapter {
    /// Constructs the adapter from a raw URL string.
    /// Trailing slashes are stripped to avoid double-slash issues with QuickNode URLs.
    pub fn new(rpc_url: &str, ws_url: &str) -> Self {
        let url = rpc_url.trim_end_matches('/').to_string();
        let ws = ws_url.trim_end_matches('/').to_string();
        Self {
            client: SolanaRpcClient::new(url),
            ws_url: ws,
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
                                let received_at = std::time::Instant::now();
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
                                                    received_at,
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
