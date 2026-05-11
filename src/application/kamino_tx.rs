use crate::domain::kamino::Reserve;
use crate::domain::protocol::KAMINO_PROGRAM_ID;
use crate::ports::rpc::{RpcClient, TransactionInfo};
use borsh::BorshDeserialize;
use sha2::{Digest, Sha256};
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::message::v0::Message;
use solana_sdk::message::VersionedMessage;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::str::FromStr;
use tokio::sync::RwLock;

const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

#[derive(Debug, Clone)]
pub struct KaminoReserveMeta {
    pub lending_market: Pubkey,
    pub pyth_oracle: Option<Pubkey>,
    pub switchboard_price_oracle: Option<Pubkey>,
    pub switchboard_twap_oracle: Option<Pubkey>,
    pub scope_prices: Option<Pubkey>,
    pub token_program: Pubkey,
}

#[derive(Debug, Clone)]
pub struct KaminoResolvedAccounts {
    pub obligation_pubkey: String,
    pub repay_reserve: String,
    pub repay_mint: String,
    pub withdraw_reserve: String,
    pub withdraw_liquidity_mint: String,
}

#[derive(Debug)]
pub struct KaminoBuiltAttempt {
    pub tx: VersionedTransaction,
    pub tx_size_bytes: usize,
    pub ata_setup_dropped_for_size: bool,
    pub ata_setup_instruction_count: usize,
    pub full_refresh_context: bool,
}

pub fn decode_kamino_reserve(data: &[u8]) -> anyhow::Result<Reserve> {
    if data.len() < 8 {
        anyhow::bail!("reserve account too small");
    }
    let mut cursor = &data[8..];
    Reserve::deserialize(&mut cursor).map_err(|e| anyhow::anyhow!("reserve decode failed: {}", e))
}

pub fn reserve_meta_from_account(data: &[u8]) -> anyhow::Result<KaminoReserveMeta> {
    let reserve = decode_kamino_reserve(data)?;
    Ok(KaminoReserveMeta {
        lending_market: Pubkey::new_from_array(reserve.lending_market),
        pyth_oracle: optional_pubkey(reserve.config.token_info.pyth_configuration.price),
        switchboard_price_oracle: optional_pubkey(
            reserve
                .config
                .token_info
                .switchboard_configuration
                .price_aggregator,
        ),
        switchboard_twap_oracle: optional_pubkey(
            reserve
                .config
                .token_info
                .switchboard_configuration
                .twap_aggregator,
        ),
        scope_prices: optional_pubkey(reserve.config.token_info.scope_configuration.price_feed),
        token_program: Pubkey::new_from_array(reserve.liquidity.token_program),
    })
}

pub fn ix_refresh_reserve(
    klend: &Pubkey,
    reserve: &Pubkey,
    meta: &KaminoReserveMeta,
) -> Instruction {
    let disc = discriminator("refresh_reserve");
    let mut accounts = vec![
        AccountMeta::new(*reserve, false),
        AccountMeta::new_readonly(meta.lending_market, false),
    ];
    if let Some(pk) = meta.pyth_oracle {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    if let Some(pk) = meta.switchboard_price_oracle {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    if let Some(pk) = meta.switchboard_twap_oracle {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    if let Some(pk) = meta.scope_prices {
        accounts.push(AccountMeta::new_readonly(pk, false));
    }
    Instruction {
        program_id: *klend,
        accounts,
        data: disc.to_vec(),
    }
}

pub fn ix_refresh_obligation(
    klend: &Pubkey,
    lending_market: &Pubkey,
    obligation: &Pubkey,
    reserves: &[&Pubkey],
) -> Instruction {
    let disc = discriminator("refresh_obligation");
    let mut accounts = vec![
        AccountMeta::new_readonly(*lending_market, false),
        AccountMeta::new(*obligation, false),
    ];
    for reserve in reserves {
        accounts.push(AccountMeta::new_readonly(**reserve, false));
    }
    Instruction {
        program_id: *klend,
        accounts,
        data: disc.to_vec(),
    }
}

pub async fn get_or_fetch_kamino_reserve_meta<R: RpcClient>(
    rpc: &R,
    cache: &RwLock<HashMap<String, KaminoReserveMeta>>,
    reserve_pk: &Pubkey,
) -> anyhow::Result<KaminoReserveMeta> {
    let key = reserve_pk.to_string();
    if let Some(meta) = cache.read().await.get(&key).cloned() {
        return Ok(meta);
    }

    let data = rpc.get_account_info(&key).await?;
    let meta = reserve_meta_from_account(&data)?;
    cache.write().await.insert(key, meta.clone());
    Ok(meta)
}

pub fn resolve_kamino_accounts_from_tx_info(
    tx_info: &TransactionInfo,
    known_obligation: Option<&str>,
    known_repay_mint: Option<&str>,
) -> anyhow::Result<KaminoResolvedAccounts> {
    let liquidate_ix_idx = find_kamino_liquidate_ix(tx_info)
        .ok_or_else(|| anyhow::anyhow!("no KLEND liquidate instruction found"))?;
    let ix_accs = &tx_info.instruction_accounts[liquidate_ix_idx];
    if ix_accs.len() < 13 {
        anyhow::bail!(
            "liquidate instruction has too few accounts ({})",
            ix_accs.len()
        );
    }

    let resolve = |index: usize| -> anyhow::Result<String> {
        tx_info
            .account_keys
            .get(
                *ix_accs
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("missing liquidate account index {index}"))?,
            )
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing account key for liquidate account {index}"))
    };

    let _ = resolve(2)?;
    let _ = resolve(3)?;
    let _ = resolve(6)?;
    let _ = resolve(9)?;
    let _ = resolve(10)?;
    let _ = resolve(11)?;
    let _ = resolve(12)?;

    Ok(KaminoResolvedAccounts {
        obligation_pubkey: known_obligation
            .map(|value| value.to_string())
            .unwrap_or(resolve(1)?),
        repay_reserve: resolve(4)?,
        repay_mint: known_repay_mint
            .map(|value| value.to_string())
            .unwrap_or(resolve(5)?),
        withdraw_reserve: resolve(7)?,
        withdraw_liquidity_mint: resolve(8)?,
    })
}

#[derive(Debug, Clone)]
pub struct KaminoBuildRequest {
    pub liquidator: Pubkey,
    pub keypair: std::sync::Arc<Keypair>,
    pub blockhash: solana_sdk::hash::Hash,
    pub tip_account: Pubkey,
    pub tip_lamports: u64,
    pub instruction_prefix: Vec<Instruction>,
    pub ata_setup_instructions: Vec<Instruction>,
    pub liquidation_ix: Instruction,
    pub max_tx_size_bytes: usize,
    pub full_refresh_context: bool,
}

pub fn build_kamino_attempt_tx(
    request: KaminoBuildRequest,
) -> anyhow::Result<KaminoBuiltAttempt> {
    let ata_setup_instruction_count = request.ata_setup_instructions.len();
    let instruction_prefix = request.instruction_prefix;
    let ata_setup_instructions = request.ata_setup_instructions;
    let liquidation_ix = request.liquidation_ix;

    let mut instructions = instruction_prefix.clone();
    instructions.extend(ata_setup_instructions.clone());
    instructions.push(liquidation_ix.clone());
    instructions.push(solana_sdk::system_instruction::transfer(
        &request.liquidator,
        &request.tip_account,
        request.tip_lamports,
    ));

    let message = Message::try_compile(&request.liquidator, &instructions, &[], request.blockhash)
        .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
    let mut tx = VersionedTransaction::try_new(
        VersionedMessage::V0(message),
        &[&*request.keypair],
    )
    .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
    let mut tx_size_bytes = bincode::serialize(&tx)
        .map(|bytes| bytes.len())
        .unwrap_or_default();
    let mut ata_setup_dropped_for_size = false;

    if ata_setup_instruction_count > 0 && tx_size_bytes > request.max_tx_size_bytes {
        ata_setup_dropped_for_size = true;
        let mut fallback_instructions = instruction_prefix;
        fallback_instructions.push(liquidation_ix);
        fallback_instructions.push(instructions[instructions.len() - 1].clone());

        let message = Message::try_compile(
            &request.liquidator,
            &fallback_instructions,
            &[],
            request.blockhash,
        )
        .map_err(|e| anyhow::anyhow!("message compile: {}", e))?;
        tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &[&*request.keypair])
            .map_err(|e| anyhow::anyhow!("sign: {}", e))?;
        tx_size_bytes = bincode::serialize(&tx)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
    }

    Ok(KaminoBuiltAttempt {
        tx,
        tx_size_bytes,
        ata_setup_dropped_for_size,
        ata_setup_instruction_count,
        full_refresh_context: request.full_refresh_context,
    })
}

pub fn get_ata_with_program(wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    let ata_program = Pubkey::from_str(ATA_PROGRAM).expect("static constant ATA_PROGRAM");
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ata_program,
    )
    .0
}

pub fn get_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM).expect("static constant TOKEN_PROGRAM");
    get_ata_with_program(wallet, mint, &token_program)
}

pub fn build_create_ata_idempotent_ix(
    funding_address: &Pubkey,
    wallet_address: &Pubkey,
    token_mint_address: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    let ata_address = get_ata_with_program(wallet_address, token_mint_address, token_program);
    Instruction {
        program_id: Pubkey::from_str(ATA_PROGRAM).expect("static constant ATA_PROGRAM"),
        accounts: vec![
            AccountMeta::new(*funding_address, true),
            AccountMeta::new(ata_address, false),
            AccountMeta::new_readonly(*wallet_address, false),
            AccountMeta::new_readonly(*token_mint_address, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(*token_program, false),
        ],
        data: vec![1],
    }
}

pub fn kamino_destination_ata_setup_enabled() -> bool {
    std::env::var("KAMINO_CREATE_DESTINATION_ATAS")
        .ok()
        .map(|value| {
            let value = value.trim();
            !(value.eq_ignore_ascii_case("0")
                || value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("off"))
        })
        .unwrap_or(true)
}

pub fn discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{}", name);
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().expect("discriminator size")
}

pub fn kamino_liquidate_discriminators() -> [[u8; 8]; 2] {
    [
        discriminator("liquidate_obligation_and_redeem_reserve_collateral_v2"),
        discriminator("liquidate_obligation_and_redeem_reserve_collateral"),
    ]
}

pub fn find_kamino_liquidate_ix(tx_info: &TransactionInfo) -> Option<usize> {
    let expected_discriminators = kamino_liquidate_discriminators();
    let mut fallback_idx = None;

    for (ix_idx, &prog_idx) in tx_info.instruction_programs.iter().enumerate() {
        if tx_info.account_keys.get(prog_idx).map(|s| s.as_str()) != Some(KAMINO_PROGRAM_ID) {
            continue;
        }

        if tx_info
            .instruction_data
            .get(ix_idx)
            .map(|data| {
                data.len() >= 8
                    && expected_discriminators
                        .iter()
                        .any(|expected| data[..8] == *expected)
            })
            .unwrap_or(false)
        {
            return Some(ix_idx);
        }

        let account_len = tx_info
            .instruction_accounts
            .get(ix_idx)
            .map(|accounts| accounts.len())
            .unwrap_or(0);
        if account_len >= 13 {
            fallback_idx = Some(ix_idx);
        }
    }

    fallback_idx
}

pub(crate) fn optional_pubkey(bytes: [u8; 32]) -> Option<Pubkey> {
    if bytes.iter().all(|b| *b == 0) {
        None
    } else {
        Some(Pubkey::new_from_array(bytes))
    }
}
