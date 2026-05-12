use crate::application::hunter::{
    elapsed_ms_since, format_signature_status, format_stage_timings, hunter_dry_run_enabled,
    is_expired_blockhash_error, is_retryable_jito_error, jito_send_max_attempts,
    log_hunter_observation, retry_backoff_ms, retry_tip_lamports, select_jito_tip_account,
    HunterTraceEvent, HunterTraceLogger, WalletTokenRuntime,
};
use crate::config::hunter::HunterTxFetchConfig;
use crate::domain::protocol::SOLEND_PROGRAM_ID;
use crate::ports::jito::JitoPort;
use crate::ports::logger::LiquidationLogger;
use crate::ports::rpc::RpcClient;
use crate::utils::log_stderr;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::message::v0::Message;
use solana_sdk::message::VersionedMessage;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::sysvar;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const DEFAULT_OBLIGATION_DEDUP_MS: u128 = 3_000;

pub(crate) async fn execute_solend_opportunity<R, JI>(
    sig: String,
    ws_received_at_ms: u64,
    rpc: R,
    jito: JI,
    keypair: Arc<Keypair>,
    wallet_index: Arc<HashMap<String, WalletTokenRuntime>>,
    cached_blockhash: Arc<tokio::sync::RwLock<solana_sdk::hash::Hash>>,
    cached_tip: Arc<std::sync::atomic::AtomicU64>,
    dedup: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    tx_fetch: HunterTxFetchConfig,
    trace_logger: HunterTraceLogger,
    logger: impl LiquidationLogger,
) -> anyhow::Result<()>
where
    R: RpcClient,
    JI: JitoPort,
{
    let started_at = Instant::now();

    let tx_fetch_started_at = Instant::now();
    let tx_info = match tokio::time::timeout(
        tokio::time::Duration::from_millis(tx_fetch.timeout_ms),
        rpc.get_transaction_with_retries(&sig, tx_fetch.attempts, tx_fetch.retry_delay_ms),
    )
    .await
    {
        Ok(Ok(tx_info)) => tx_info,
        Ok(Err(e)) => {
            let status = rpc.get_signature_status(&sig).await.ok().flatten();
            anyhow::bail!(
                "getTransaction failed after {}ms: {} | {}",
                tx_fetch_started_at.elapsed().as_millis(),
                e,
                format_signature_status(status.as_ref())
            );
        }
        Err(_) => {
            let status = rpc.get_signature_status(&sig).await.ok().flatten();
            anyhow::bail!(
                "getTransaction timeout after {}ms | {}",
                tx_fetch_started_at.elapsed().as_millis(),
                format_signature_status(status.as_ref())
            );
        }
    };
    let tx_fetch_ms = tx_fetch_started_at.elapsed().as_millis();

    let resolve_started_at = Instant::now();
    let liq_ix_idx = tx_info
        .instruction_programs
        .iter()
        .enumerate()
        .filter(|(_, &prog_idx)| {
            tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) == Some(SOLEND_PROGRAM_ID)
        })
        .max_by_key(|(ix_idx, _)| {
            tx_info
                .instruction_accounts
                .get(*ix_idx)
                .map(|a| a.len())
                .unwrap_or(0)
        })
        .map(|(ix_idx, _)| ix_idx)
        .ok_or_else(|| anyhow::anyhow!("no Solend liquidate instruction found"))?;

    let liq_accs = &tx_info.instruction_accounts[liq_ix_idx];
    let liq_data = &tx_info.instruction_data[liq_ix_idx];

    if liq_accs.len() < 9 || liq_data.len() < 16 {
        anyhow::bail!(
            "Solend liquidate instruction malformed (accs={} data={})",
            liq_accs.len(),
            liq_data.len()
        );
    }

    let competitor = tx_info
        .account_keys
        .get(0)
        .ok_or_else(|| anyhow::anyhow!("empty account_keys"))?
        .clone();

    let balance_map: HashMap<usize, (String, String)> = tx_info
        .post_token_balances
        .iter()
        .chain(tx_info.pre_token_balances.iter())
        .map(|b| (b.account_index, (b.mint.clone(), b.owner.clone())))
        .collect();

    let repay_mint_str = balance_map
        .values()
        .find(|(_, owner)| owner == &competitor)
        .map(|(mint, _)| mint.clone())
        .ok_or_else(|| anyhow::anyhow!("could not identify repay mint for this liquidation"))?;

    let Some(repay_mint) = wallet_index.get(&repay_mint_str) else {
        trace_logger.log(
            HunterTraceEvent::new("solend", "skip", sig.clone())
                .with_repay_mint(repay_mint_str.clone())
                .with_reason("token_not_whitelisted")
                .with_detail("token not whitelisted")
                .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms)),
        );
        log_stderr(format!(
            "[hunter-solend] skip: token not whitelisted | repay_mint={}",
            repay_mint_str
        ));
        return Ok(());
    };

    let obligation_key_idx = liq_accs
        .get(5)
        .and_then(|&i| tx_info.account_keys.get(i))
        .cloned()
        .unwrap_or_default();
    let resolve_ms = resolve_started_at.elapsed().as_millis();

    if obligation_key_idx.is_empty() {
        anyhow::bail!("could not extract obligation pubkey from Solend tx");
    }

    {
        let mut map = dedup.lock().expect("dedup mutex poisoned");
        map.retain(|_, t| t.elapsed().as_millis() < DEFAULT_OBLIGATION_DEDUP_MS);
        if map.contains_key(&obligation_key_idx) {
            trace_logger.log(
                HunterTraceEvent::new("solend", "skip", sig.clone())
                    .with_obligation(obligation_key_idx.clone())
                    .with_repay_mint(repay_mint.mint.clone())
                    .with_repay_symbol(repay_mint.symbol.clone())
                    .with_reason("dedup")
                    .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms)),
            );
            return Ok(());
        }
        map.insert(obligation_key_idx.clone(), std::time::Instant::now());
    }
    if repay_mint.max_repay_native == 0 {
        trace_logger.log(
            HunterTraceEvent::new("solend", "skip", sig.clone())
                .with_obligation(obligation_key_idx.clone())
                .with_repay_mint(repay_mint.mint.clone())
                .with_repay_symbol(repay_mint.symbol.clone())
                .with_reason("wallet_token_zero_cap")
                .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms)),
        );
        return Ok(());
    }

    let compute_unit_limit = std::env::var("SOLEND_COMPUTE_UNIT_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(400_000);
    let compute_unit_price = std::env::var("SOLEND_CU_PRICE_MICROLAMPORTS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5_000);

    let mut instructions: Vec<Instruction> = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
    ];

    let solend_pk = Pubkey::from_str(SOLEND_PROGRAM_ID).expect("static constant SOLEND_PROGRAM_ID");
    for (idx, (&prog_idx, accs)) in tx_info
        .instruction_programs
        .iter()
        .zip(tx_info.instruction_accounts.iter())
        .enumerate()
    {
        let prog_key = match tx_info.account_keys.get(prog_idx) {
            Some(k) => k.as_str(),
            None => continue,
        };
        if prog_key != SOLEND_PROGRAM_ID || idx == liq_ix_idx {
            continue;
        }

        let acc_metas: Vec<AccountMeta> = accs
            .iter()
            .filter_map(|&ai| {
                tx_info
                    .account_keys
                    .get(ai)
                    .and_then(|k| Pubkey::from_str(k).ok())
                    .map(|pk| AccountMeta::new_readonly(pk, false))
            })
            .collect();

        let data = tx_info
            .instruction_data
            .get(idx)
            .cloned()
            .unwrap_or_default();
        instructions.push(Instruction {
            program_id: solend_pk,
            accounts: acc_metas,
            data,
        });
    }

    {
        let liquidator = keypair.pubkey();

        let acc_metas: Vec<AccountMeta> = liq_accs
            .iter()
            .enumerate()
            .map(|(pos, &ai)| {
                let key_str = tx_info
                    .account_keys
                    .get(ai)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let pk = Pubkey::from_str(key_str).unwrap_or_default();

                if let Some((mint_str, owner)) = balance_map.get(&ai) {
                    if owner == &competitor {
                        if let Some(runtime) = wallet_index.get(mint_str) {
                            return AccountMeta::new(runtime.source_ata, false);
                        }
                        if let Ok(mint_pk) = Pubkey::from_str(mint_str) {
                            return AccountMeta::new(
                                crate::application::kamino_tx::get_ata(&liquidator, &mint_pk),
                                false,
                            );
                        }
                    }
                }

                if key_str == competitor {
                    return AccountMeta::new_readonly(liquidator, true);
                }

                let is_program_or_sysvar = pk == solana_sdk::system_program::id()
                    || pk == Pubkey::from_str(TOKEN_PROGRAM).unwrap_or_default()
                    || pk == sysvar::instructions::id()
                    || pk == solana_sdk::sysvar::clock::id()
                    || pk == solana_sdk::sysvar::rent::id()
                    || prog_idx_is_program(&tx_info, ai);
                let is_likely_readonly =
                    is_program_or_sysvar || pos >= liq_accs.len().saturating_sub(4);

                if is_likely_readonly {
                    AccountMeta::new_readonly(pk, false)
                } else {
                    AccountMeta::new(pk, false)
                }
            })
            .collect();

        let mut data = liq_data.clone();
        let amount = repay_mint.max_repay_native;
        data[8..16].copy_from_slice(&amount.to_le_bytes());

        instructions.push(Instruction {
            program_id: solend_pk,
            accounts: acc_metas,
            data,
        });
    }

    let base_tip_lamports = cached_tip.load(std::sync::atomic::Ordering::Relaxed);
    let tip_account = select_jito_tip_account(&sig)?;
    instructions.push(solana_sdk::system_instruction::transfer(
        &keypair.pubkey(),
        &tip_account,
        base_tip_lamports,
    ));
    let liquidator = keypair.pubkey();
    let tip_instruction_idx = instructions.len() - 1;
    let max_send_attempts = jito_send_max_attempts();
    let initial_timing_detail = format_stage_timings(
        tx_fetch_ms,
        resolve_ms,
        0,
        0,
        None,
        started_at.elapsed().as_millis(),
    );

    trace_logger.log(
        HunterTraceEvent::new("solend", "firing", sig.clone())
            .with_obligation(obligation_key_idx.clone())
            .with_repay_mint(repay_mint.mint.clone())
            .with_repay_symbol(repay_mint.symbol.clone())
            .with_detail(format!(
                "tip={} tip_account={} cu_price={} max_send_attempts={} {}",
                base_tip_lamports,
                tip_account,
                compute_unit_price,
                max_send_attempts,
                initial_timing_detail
            ))
            .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms)),
    );
    let _ = log_hunter_observation(
        &logger,
        "Solend",
        "HUNTER_FIRING",
        &sig,
        Some(obligation_key_idx.clone()),
        Some(liquidator.to_string()),
        Some(repay_mint),
        Some(format!(
            "tip={} tip_account={} cu_price={} max_send_attempts={} {}",
            base_tip_lamports,
            tip_account,
            compute_unit_price,
            max_send_attempts,
            initial_timing_detail
        )),
        Some(elapsed_ms_since(ws_received_at_ms)),
    )
    .await;
    log_stderr(format!(
        "[hunter-solend] FIRING | obligation={} repay={} tip={} max_attempts={}",
        &obligation_key_idx[..8.min(obligation_key_idx.len())],
        repay_mint.symbol,
        base_tip_lamports,
        max_send_attempts,
    ));

    if hunter_dry_run_enabled() {
        let dry_run_blockhash = *cached_blockhash.read().await;
        let build_started_at = Instant::now();
        let message = Message::try_compile(&liquidator, &instructions, &[], dry_run_blockhash)
            .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
            .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
        let build_ms = build_started_at.elapsed().as_millis();
        let tx_bytes = bincode::serialize(&tx)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        trace_logger.log(
            HunterTraceEvent::new("solend", "dry_run", sig.clone())
                .with_obligation(obligation_key_idx.clone())
                .with_repay_mint(repay_mint.mint.clone())
                .with_repay_symbol(repay_mint.symbol.clone())
                .with_reason("dry_run_enabled")
                .with_detail(format!(
                    "tx_size_bytes={} tip={} cu_price={} attempt=1/{} {}",
                    tx_bytes,
                    base_tip_lamports,
                    compute_unit_price,
                    max_send_attempts,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        0,
                        build_ms,
                        None,
                        started_at.elapsed().as_millis(),
                    )
                ))
                .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms)),
        );
        log_stderr(format!(
            "[hunter-solend] DRY RUN | obligation={} repay={} tx_size={}",
            &obligation_key_idx[..8.min(obligation_key_idx.len())],
            repay_mint.symbol,
            tx_bytes
        ));
        return Ok(());
    }

    for attempt in 1..=max_send_attempts {
        let tip_lamports = retry_tip_lamports(base_tip_lamports, attempt);
        instructions[tip_instruction_idx] =
            solana_sdk::system_instruction::transfer(&liquidator, &tip_account, tip_lamports);

        let blockhash = if attempt == 1 {
            *cached_blockhash.read().await
        } else {
            match rpc.get_latest_blockhash().await {
                Ok(latest_blockhash) => {
                    *cached_blockhash.write().await = latest_blockhash;
                    latest_blockhash
                }
                Err(_) => *cached_blockhash.read().await,
            }
        };

        let build_started_at = Instant::now();
        let message = Message::try_compile(&liquidator, &instructions, &[], blockhash)
            .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*keypair])
            .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
        let build_ms = build_started_at.elapsed().as_millis();

        let send_started_at = Instant::now();
        match jito.send_bundle(vec![tx]).await {
            Ok(bundle_id) => {
                let send_bundle_ms = send_started_at.elapsed().as_millis();
                let bundle_detail = format!(
                    "attempt={}/{} tip={} {}",
                    attempt,
                    max_send_attempts,
                    tip_lamports,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        0,
                        build_ms,
                        Some(send_bundle_ms),
                        started_at.elapsed().as_millis(),
                    )
                );
                trace_logger.log(
                    HunterTraceEvent::new("solend", "bundle_sent", sig.clone())
                        .with_obligation(obligation_key_idx.clone())
                        .with_repay_mint(repay_mint.mint.clone())
                        .with_repay_symbol(repay_mint.symbol.clone())
                        .with_detail(bundle_detail.clone())
                        .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms))
                        .with_optional_bundle_id(Some(bundle_id.clone())),
                );
                let _ = log_hunter_observation(
                    &logger,
                    "Solend",
                    "HUNTER_BUNDLE_SENT",
                    &sig,
                    Some(obligation_key_idx.clone()),
                    Some(liquidator.to_string()),
                    Some(repay_mint),
                    Some(bundle_detail),
                    Some(elapsed_ms_since(ws_received_at_ms)),
                )
                .await;
                log_stderr(format!(
                    "[hunter-solend] BUNDLE SENT | obligation={} bundle={} attempt={}/{}",
                    &obligation_key_idx[..8.min(obligation_key_idx.len())],
                    &bundle_id[..12.min(bundle_id.len())],
                    attempt,
                    max_send_attempts
                ));
                return Ok(());
            }
            Err(error) => {
                let send_bundle_ms = send_started_at.elapsed().as_millis();
                let error_message = error.to_string();
                let bundle_detail = format!(
                    "attempt={}/{} tip={} {} | {}",
                    attempt,
                    max_send_attempts,
                    tip_lamports,
                    error_message,
                    format_stage_timings(
                        tx_fetch_ms,
                        resolve_ms,
                        0,
                        build_ms,
                        Some(send_bundle_ms),
                        started_at.elapsed().as_millis(),
                    )
                );

                if attempt < max_send_attempts && is_retryable_jito_error(&error_message) {
                    trace_logger.log(
                        HunterTraceEvent::new("solend", "bundle_retry", sig.clone())
                            .with_obligation(obligation_key_idx.clone())
                            .with_repay_mint(repay_mint.mint.clone())
                            .with_repay_symbol(repay_mint.symbol.clone())
                            .with_reason(if is_expired_blockhash_error(&error_message) {
                                "expired_blockhash_retry"
                            } else {
                                "retryable_bundle_send_error"
                            })
                            .with_detail(bundle_detail)
                            .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms)),
                    );
                    let backoff_ms = retry_backoff_ms(attempt);
                    if backoff_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }
                    continue;
                }

                trace_logger.log(
                    HunterTraceEvent::new("solend", "error", sig.clone())
                        .with_obligation(obligation_key_idx.clone())
                        .with_repay_mint(repay_mint.mint.clone())
                        .with_repay_symbol(repay_mint.symbol.clone())
                        .with_reason("bundle_send_failed")
                        .with_detail(bundle_detail.clone())
                        .with_timing(ws_received_at_ms, elapsed_ms_since(ws_received_at_ms)),
                );
                let _ = log_hunter_observation(
                    &logger,
                    "Solend",
                    "HUNTER_BUNDLE_FAILED",
                    &sig,
                    Some(obligation_key_idx.clone()),
                    Some(liquidator.to_string()),
                    Some(repay_mint),
                    Some(bundle_detail),
                    Some(elapsed_ms_since(ws_received_at_ms)),
                )
                .await;
                log_stderr(format!(
                    "[hunter-solend] bundle send failed (attempt={}/{}): {}",
                    attempt, max_send_attempts, error_message
                ));
                return Ok(());
            }
        }
    }

    Ok(())
}

fn prog_idx_is_program(tx: &crate::ports::rpc::TransactionInfo, ai: usize) -> bool {
    tx.instruction_programs.contains(&ai)
}
