use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client as HttpClient;
use serde_json::json;
use solana_client::rpc_client::RpcClient as SolanaRpcClient;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::ports::rpc::{
    LogEntry, ProgramAccount, RpcClient, RpcCommitment, SignatureStatusInfo, StreamingRpcClient,
    TransactionInfo,
};
use crate::utils::log_stderr;
use bs58;

use std::sync::Arc;

#[derive(Clone)]
pub struct HeliusAdapter {
    client: Arc<SolanaRpcClient>,
    ws_url: String,
    rpc_url: String,
    http_client: HttpClient,
    tx_commitment: String,
}

impl HeliusAdapter {
    /// Constructs the adapter from a raw URL string.
    /// Trailing slashes are stripped to avoid double-slash issues with QuickNode URLs.
    pub fn new(rpc_url: &str, ws_url: &str) -> Self {
        Self::with_tx_commitment(rpc_url, ws_url, "confirmed")
    }

    pub fn with_tx_commitment(rpc_url: &str, ws_url: &str, tx_commitment: &str) -> Self {
        let url = rpc_url.trim_end_matches('/').to_string();
        let ws = ws_url.trim_end_matches('/').to_string();
        Self {
            client: Arc::new(SolanaRpcClient::new(url.clone())),
            ws_url: ws,
            rpc_url: url,
            http_client: HttpClient::new(),
            tx_commitment: tx_commitment.to_string(),
        }
    }

    async fn get_transaction_with_commitment(
        &self,
        signature: &str,
        commitment: &str,
        max_attempts: usize,
        retry_delay_ms: u64,
    ) -> Result<Option<TransactionInfo>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTransaction",
            "params": [signature, {"encoding": "json", "maxSupportedTransactionVersion": 0, "commitment": commitment}]
        });

        let max_attempts = max_attempts.max(1);

        for attempt_idx in 0..max_attempts {
            let resp = self.http_client
                .post(&self.rpc_url)
                .json(&payload)
                .send()
                .await
                .with_context(|| format!("getTransaction HTTP request failed ({})", self.rpc_url))?;

            let body: serde_json::Value = resp
                .json()
                .await
                .with_context(|| format!("getTransaction response parse failed ({})", self.rpc_url))?;

            let result = &body["result"];

            if !result.is_null() {
                let account_keys = extract_account_keys(result);

                let raw_instructions = result["transaction"]["message"]["instructions"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .clone();

                let mut instruction_accounts: Vec<Vec<usize>> = Vec::new();
                let mut instruction_programs: Vec<usize> = Vec::new();
                let mut instruction_data: Vec<Vec<u8>> = Vec::new();

                for instr in &raw_instructions {
                    let prog_idx = instr["programIdIndex"].as_u64().unwrap_or(0) as usize;
                    let accounts: Vec<usize> = instr["accounts"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter_map(|v| v.as_u64().map(|n| n as usize))
                        .collect();
                    let data_bytes = instr["data"]
                        .as_str()
                        .and_then(|s| bs58::decode(s).into_vec().ok())
                        .unwrap_or_default();
                    instruction_programs.push(prog_idx);
                    instruction_accounts.push(accounts);
                    instruction_data.push(data_bytes);
                }

                let block_time = result["blockTime"].as_u64();
                let pre_token_balances = parse_json_token_balances(&result["meta"]["preTokenBalances"]);
                let post_token_balances = parse_json_token_balances(&result["meta"]["postTokenBalances"]);

                return Ok(Some(TransactionInfo {
                    account_keys,
                    instruction_accounts,
                    instruction_programs,
                    instruction_data,
                    block_time,
                    pre_token_balances,
                    post_token_balances,
                }));
            }

            if attempt_idx + 1 < max_attempts {
                tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms)).await;
            }
        }

        Ok(None)
    }
}

fn endpoint_host_label(raw: &str) -> String {
    raw.split("://")
        .nth(1)
        .unwrap_or(raw)
        .split('/')
        .next()
        .unwrap_or(raw)
        .to_string()
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
        self.get_transaction_with_retries(signature, 3, 500).await
    }

    async fn get_transaction_with_retries(
        &self,
        signature: &str,
        max_attempts: usize,
        retry_delay_ms: u64,
    ) -> Result<TransactionInfo> {
        if let Some(info) = self
            .get_transaction_with_commitment(signature, &self.tx_commitment, max_attempts, retry_delay_ms)
            .await?
        {
            return Ok(info);
        }

        if self.tx_commitment != "confirmed" {
            let fallback_attempts = (max_attempts / 2).max(1);
            if let Some(info) = self
                .get_transaction_with_commitment(signature, "confirmed", fallback_attempts, retry_delay_ms)
                .await?
            {
                return Ok(info);
            }
        }

        anyhow::bail!(
            "getTransaction returned null for signature {} after {} attempts (primary_commitment={} fallback=confirmed)",
            signature,
            max_attempts,
            self.tx_commitment,
        );
    }

    async fn get_account_info(&self, pubkey: &str) -> Result<Vec<u8>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getAccountInfo",
            "params": [pubkey, {"encoding": "base64"}]
        });

        let resp = self.http_client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("getAccountInfo HTTP request failed ({})", self.rpc_url))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("getAccountInfo response parse failed ({})", self.rpc_url))?;

        let data_arr = body["result"]["value"]["data"]
            .as_array()
            .context("getAccountInfo: missing data array")?;

        let b64 = data_arr.first()
            .and_then(|v| v.as_str())
            .context("getAccountInfo: missing base64 data")?;

        let bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            b64,
        ).context("getAccountInfo: base64 decode failed")?;

        Ok(bytes)
    }

    async fn get_program_accounts(&self, program_id: &str) -> Result<Vec<ProgramAccount>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getProgramAccounts",
            "params": [program_id, {"encoding": "base64"}]
        });

        let resp = self.http_client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("getProgramAccounts HTTP request failed ({})", self.rpc_url))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("getProgramAccounts response parse failed ({})", self.rpc_url))?;

        let accounts = body["result"]
            .as_array()
            .context("getProgramAccounts: missing result array")?;

        let mut out = Vec::with_capacity(accounts.len());
        for account in accounts {
            let Some(pubkey) = account["pubkey"].as_str() else {
                continue;
            };
            let data_arr = account["account"]["data"]
                .as_array()
                .context("getProgramAccounts: missing data array")?;
            let b64 = data_arr.first()
                .and_then(|v| v.as_str())
                .context("getProgramAccounts: missing base64 data")?;
            let bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                b64,
            ).context("getProgramAccounts: base64 decode failed")?;
            out.push(ProgramAccount {
                pubkey: pubkey.to_string(),
                data: bytes,
            });
        }

        Ok(out)
    }

    async fn get_signature_status(&self, signature: &str) -> Result<Option<SignatureStatusInfo>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSignatureStatuses",
            "params": [[signature], { "searchTransactionHistory": false }]
        });

        let resp = self.http_client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("getSignatureStatuses HTTP request failed ({})", self.rpc_url))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("getSignatureStatuses response parse failed ({})", self.rpc_url))?;
        let value = body["result"]["value"]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        if value.is_null() {
            return Ok(None);
        }

        Ok(Some(SignatureStatusInfo {
            slot: value["slot"].as_u64(),
            confirmation_status: value["confirmationStatus"].as_str().map(str::to_string),
            has_error: !value["err"].is_null(),
        }))
    }

    async fn get_latest_blockhash(&self) -> Result<solana_sdk::hash::Hash> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getLatestBlockhash",
            "params": []
        });

        let resp = self.http_client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("getLatestBlockhash HTTP request failed ({})", self.rpc_url))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("getLatestBlockhash response parse failed ({})", self.rpc_url))?;
        let hash_str = body["result"]["value"]["blockhash"]
            .as_str()
            .context("blockhash missing in response")?;

        use std::str::FromStr;
        let hash = solana_sdk::hash::Hash::from_str(hash_str)
            .map_err(|e| anyhow::anyhow!("Invalid blockhash format: {}", e))?;

        Ok(hash)
    }
}

fn parse_pubkey_entry(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.get("pubkey").and_then(|v| v.as_str()).map(str::to_string))
}

fn extract_account_keys(result: &serde_json::Value) -> Vec<String> {
    let mut account_keys: Vec<String> = result["transaction"]["message"]["accountKeys"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(parse_pubkey_entry)
        .collect();

    // For versioned v0 transactions, Solana RPC exposes ALT-resolved addresses under
    // meta.loadedAddresses. Instruction account indices are resolved against the full
    // account list: static keys first, then loaded writable, then loaded readonly.
    if let Some(writable) = result["meta"]["loadedAddresses"]["writable"].as_array() {
        account_keys.extend(writable.iter().filter_map(parse_pubkey_entry));
    }
    if let Some(readonly) = result["meta"]["loadedAddresses"]["readonly"].as_array() {
        account_keys.extend(readonly.iter().filter_map(parse_pubkey_entry));
    }

    account_keys
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

#[cfg(test)]
mod tests {
    use super::extract_account_keys;
    use serde_json::json;

    #[test]
    fn extract_account_keys_appends_loaded_addresses_for_v0() {
        let result = json!({
            "transaction": {
                "message": {
                    "accountKeys": [
                        "Static1111111111111111111111111111111111111",
                        "Static2222222222222222222222222222222222222"
                    ]
                }
            },
            "meta": {
                "loadedAddresses": {
                    "writable": [
                        "Writable33333333333333333333333333333333333"
                    ],
                    "readonly": [
                        "Readonly4444444444444444444444444444444444"
                    ]
                }
            }
        });

        let keys = extract_account_keys(&result);
        assert_eq!(
            keys,
            vec![
                "Static1111111111111111111111111111111111111".to_string(),
                "Static2222222222222222222222222222222222222".to_string(),
                "Writable33333333333333333333333333333333333".to_string(),
                "Readonly4444444444444444444444444444444444".to_string(),
            ]
        );
    }
}

impl StreamingRpcClient for HeliusAdapter {
    fn subscribe_to_logs(
        &self,
        program_id: &str,
        commitment: RpcCommitment,
    ) -> impl std::future::Future<Output = Result<tokio::sync::mpsc::Receiver<LogEntry>>> + Send {
        let this = self.clone();
        let program_id = program_id.to_string();
        async move {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let program_id_owned = program_id.to_string();
        let ws_url = this.ws_url.clone();
        let ws_label = endpoint_host_label(&ws_url);
        let commitment = match commitment {
            RpcCommitment::Processed => "processed",
            RpcCommitment::Confirmed => "confirmed",
        };

        tokio::spawn(async move {
            let mut backoff_secs: u64 = 1;

            loop {
                log_stderr(format!("[rpc-ws {ws_label}] connecting..."));
                match connect_async(&ws_url).await {
                    Ok((mut ws_stream, _)) => {
                        log_stderr(format!("[rpc-ws {ws_label}] connected."));
                        backoff_secs = 1; // reset backoff on successful connection

                        let subscribe_msg = json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "logsSubscribe",
                            "params": [
                                { "mentions": [program_id_owned] },
                                { "commitment": commitment }
                            ]
                        });

                        if let Err(e) = ws_stream
                            .send(Message::Text(subscribe_msg.to_string()))
                            .await
                        {
                            log_stderr(format!("[rpc-ws {ws_label}] failed to send subscribe: {e}"));
                        } else {
                            let mut subscribed = false;
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
                                            if !subscribed && value.get("id").and_then(|v| v.as_i64()) == Some(1) {
                                                if let Some(error) = value.get("error") {
                                                    log_stderr(format!(
                                                        "[rpc-ws {ws_label}] subscribe rejected: {}",
                                                        error
                                                    ));
                                                    break;
                                                }

                                                if let Some(subscription_id) = value.get("result") {
                                                    log_stderr(format!(
                                                        "[rpc-ws {ws_label}] subscribed successfully: subscription_id={}",
                                                        subscription_id
                                                    ));
                                                    subscribed = true;
                                                    continue;
                                                }

                                                log_stderr(format!(
                                                    "[rpc-ws {ws_label}] subscribe response missing result/error: {}",
                                                    value
                                                ));
                                                break;
                                            }

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
                                            } else if !subscribed {
                                                log_stderr(format!(
                                                    "[rpc-ws {ws_label}] unexpected pre-subscription message: {}",
                                                    value
                                                ));
                                            } else if let Some(method) =
                                                value.get("method").and_then(|m| m.as_str())
                                            {
                                                log_stderr(format!(
                                                    "[rpc-ws {ws_label}] ignoring WS message method={method}"
                                                ));
                                            }
                                        } else {
                                            log_stderr(format!(
                                                "[rpc-ws {ws_label}] failed to parse WS text message: {}",
                                                text
                                            ));
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

                        log_stderr(format!(
                            "[rpc-ws {ws_label}] stream ended. Reconnecting in {backoff_secs}s..."
                        ));
                    }
                    Err(e) => {
                        log_stderr(format!(
                            "[rpc-ws {ws_label}] connection error: {e}. Retrying in {backoff_secs}s..."
                        ));
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
}
