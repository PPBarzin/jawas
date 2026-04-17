use anyhow::{Context, Result};
use jawas::domain::{solend::decode_solend_obligation, token::token_info};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;

fn fmt_pct(value: f64) -> String {
    format!("{:.2}", value * 100.0)
}

fn fmt_usd(value: f64) -> String {
    format!("{value:.2}")
}

fn symbol_or_fallback(key: &str) -> String {
    token_info(key)
        .map(|t| t.symbol.to_string())
        .unwrap_or_else(|| key.to_string())
}

fn get_solend_reserve_symbol(
    rpc: &RpcClient,
    cache: &mut HashMap<String, String>,
    reserve_pk: &Pubkey,
) -> String {
    let key = reserve_pk.to_string();
    if let Some(symbol) = cache.get(&key) {
        return symbol.clone();
    }

    let symbol = rpc
        .get_account(reserve_pk)
        .ok()
        .and_then(|acc| {
            if acc.data.len() < 74 {
                return None;
            }
            let mint_bytes: [u8; 32] = acc.data[42..74].try_into().ok()?;
            Some(Pubkey::new_from_array(mint_bytes))
        })
        .map(|mint| mint.to_string())
        .map(|mint| symbol_or_fallback(&mint))
        .unwrap_or_else(|| key.clone());

    cache.insert(key, symbol.clone());
    symbol
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: cargo run --bin inspect_solend_obligation <OBLIGATION_PUBKEY>");
        return Ok(());
    }

    let pk_str = &args[1];
    let rpc_url = std::env::var("OBSERVER_RPC_URL").context("OBSERVER_RPC_URL not set")?;
    let rpc = RpcClient::new(rpc_url);

    println!("🔍 Inspection de l'obligation Solend : {pk_str}");

    let pubkey = Pubkey::from_str(pk_str)?;
    let account = rpc.get_account(&pubkey)?;
    println!("Taille du compte : {} bytes", account.data.len());

    let obligation = decode_solend_obligation(&account.data)
        .context("Impossible de décoder l'obligation Solend")?;
    let mut reserve_cache = HashMap::new();

    let owner = Pubkey::new_from_array(obligation.owner);
    let market = Pubkey::new_from_array(obligation.lending_market);
    let collateral_usd = obligation.deposited_value.to_f64();
    let debt_usd = obligation.borrowed_value.to_f64();
    let max_borrow_usd = obligation.allowed_borrow_value.to_f64();
    let unhealthy_borrow_usd = obligation.unhealthy_borrow_value.to_f64();
    let net_value_usd = collateral_usd - debt_usd;

    let current_ltv = if collateral_usd > 0.0 {
        debt_usd / collateral_usd
    } else {
        f64::INFINITY
    };
    let max_ltv = if collateral_usd > 0.0 {
        max_borrow_usd / collateral_usd
    } else {
        f64::INFINITY
    };
    let unhealthy_ltv = if collateral_usd > 0.0 {
        unhealthy_borrow_usd / collateral_usd
    } else {
        f64::INFINITY
    };
    let dist_to_liq = unhealthy_ltv - current_ltv;

    println!("\n--- Valeurs agrégées Solend ---");
    println!("Owner                : {owner}");
    println!("Lending Market       : {market}");
    println!("Collateral USD       : {}", fmt_usd(collateral_usd));
    println!("Debt USD             : {}", fmt_usd(debt_usd));
    println!("Net Value USD        : {}", fmt_usd(net_value_usd));
    println!("Current LTV %        : {}", fmt_pct(current_ltv));
    println!("Max LTV %            : {}", fmt_pct(max_ltv));
    println!("Unhealthy LTV %      : {}", fmt_pct(unhealthy_ltv));
    println!("Dist To Liq          : {}", fmt_pct(dist_to_liq));
    println!("Liquidatable         : {}", obligation.is_liquidatable());
    println!("Max Repay Wads       : {}", obligation.max_repay_wads());

    println!("\n--- Dépôts ---");
    for (idx, deposit) in obligation.deposits.iter().enumerate() {
        let reserve = Pubkey::new_from_array(deposit.deposit_reserve);
        let token = get_solend_reserve_symbol(&rpc, &mut reserve_cache, &reserve);
        println!(
            "#{idx} reserve={} token={} deposited_amount={} market_value_usd={}",
            reserve,
            token,
            deposit.deposited_amount,
            fmt_usd(deposit.market_value.to_f64())
        );
    }

    println!("\n--- Emprunts ---");
    for (idx, borrow) in obligation.borrows.iter().enumerate() {
        let reserve = Pubkey::new_from_array(borrow.borrow_reserve);
        let token = get_solend_reserve_symbol(&rpc, &mut reserve_cache, &reserve);
        println!(
            "#{idx} reserve={} token={} borrowed_amount_wads={} market_value_usd={}",
            reserve,
            token,
            borrow.borrowed_amount_wads.to_u128(),
            fmt_usd(borrow.market_value.to_f64())
        );
    }

    Ok(())
}
