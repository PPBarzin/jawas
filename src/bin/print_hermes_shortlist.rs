use anyhow::{Context, Result};
use jawas::application::hermes_shortlist::{
    build_hermes_shortlist, decode_kamino_obligation, HermesRuntimeConfig,
};
use jawas::config::wallet::load_wallet_tokens;
use jawas::infrastructure::helius::HeliusAdapter;
use jawas::ports::rpc::RpcClient as _;
use sha2::{Digest, Sha256};

const KLEND_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";

fn anchor_account_discriminator_base58(name: &str) -> String {
    let preimage = format!("account:{name}");
    let hash = Sha256::digest(preimage.as_bytes());
    bs58::encode(&hash[..8]).into_string()
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn load_config() -> HermesRuntimeConfig {
    HermesRuntimeConfig::from_env()
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = std::env::var("HERMES_SHORTLIST_RPC_URL")
        .or_else(|_| std::env::var("OBSERVER_RPC_URL"))
        .context("missing HERMES_SHORTLIST_RPC_URL / OBSERVER_RPC_URL")?;
    let ws_url = std::env::var("OBSERVER_WS_URL")
        .unwrap_or_else(|_| "wss://api.mainnet-beta.solana.com".to_string());
    let wallet_path =
        std::env::var("WALLET_TOML_PATH").unwrap_or_else(|_| "wallet.toml".to_string());
    let refreshed_at_ms = env_u64("SHORTLIST_DEBUG_REFRESHED_AT_MS", 0);

    let config = load_config();
    let wallet_tokens = load_wallet_tokens(&wallet_path)?;
    let rpc = HeliusAdapter::new(&rpc_url, &ws_url);

    let obligation_disc = anchor_account_discriminator_base58("Obligation");
    let reserve_disc = anchor_account_discriminator_base58("Reserve");

    let mut obligation_accounts = rpc
        .get_program_accounts_with_memcmp_filters(
            KLEND_PROGRAM,
            &[(0, obligation_disc)],
        )
        .await?;
    let mut reserve_accounts = rpc
        .get_program_accounts_with_memcmp_filters(
            KLEND_PROGRAM,
            &[(0, reserve_disc)],
        )
        .await?;

    reserve_accounts.append(&mut obligation_accounts);

    let shortlist = build_hermes_shortlist(
        &wallet_tokens,
        reserve_accounts,
        config.shortlist_size,
        config.min_repay_usd,
        refreshed_at_ms,
    );

    println!(
        "shortlist_size={} min_repay_usd={} refresh_secs={}",
        config.shortlist_size, config.min_repay_usd, config.refresh_secs
    );
    println!(
        "{:<46} {:>10} {:>14} {:>12} {:>12} {:>12}",
        "obligation", "repay", "current_ltv", "unhealthy", "dist_liq", "debt_usd"
    );

    for entry in shortlist {
        let data = rpc.get_account_info(&entry.obligation_pubkey).await?;
        let obligation = decode_kamino_obligation(&data)
            .with_context(|| format!("failed to decode obligation {}", entry.obligation_pubkey))?;
        let repay_symbol = wallet_tokens
            .iter()
            .find(|token| token.mint == entry.repay_mint)
            .map(|token| token.symbol.as_str())
            .unwrap_or("?");
        println!(
            "{:<46} {:>10} {:>13.2}% {:>11.2}% {:>11.2}% {:>12.2}",
            entry.obligation_pubkey,
            repay_symbol,
            obligation.current_ltv() * 100.0,
            obligation.unhealthy_ltv() * 100.0,
            obligation.dist_to_liq() * 100.0,
            obligation.debt_value_usd(),
        );
    }

    Ok(())
}
