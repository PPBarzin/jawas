mod domain;
mod ports;
mod adapters;
mod services;
mod utils;

use adapters::{
    airtable::AirtableAdapter, 
    helius::HeliusAdapter, 
    oracle::SimplePriceOracle,
    jito::JitoAdapter,
    jupiter::JupiterAdapter,
};
use ports::rpc::RpcClient;
use ports::logger::{LiquidationLogger, ObservationEvent};
use services::heartbeat::HeartbeatService;
use services::observer::{ObserverService, Protocol};
use services::hunter::HunterService;
use solana_sdk::signature::read_keypair_file;
use solana_sdk::signer::Signer;
use std::sync::Arc;
use tokio::sync::mpsc;
use utils::utc_now;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    println!("Jawas Phase 2: Booting...");

    // ── 1. Load config from environment ─────────────────────────────────────
    let observer_rpc_url = std::env::var("OBSERVER_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .expect("OBSERVER_RPC_URL or RPC_URL must be set");

    let observer_ws_url = std::env::var("OBSERVER_WS_URL")
        .or_else(|_| std::env::var("WS_URL"))
        .expect("OBSERVER_WS_URL or WS_URL must be set");

    let hunter_rpc_url = std::env::var("HUNTER_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .expect("HUNTER_RPC_URL or RPC_URL must be set");

    let hunter_ws_url = std::env::var("HUNTER_WS_URL")
        .or_else(|_| std::env::var("WS_URL"))
        .expect("HUNTER_WS_URL or WS_URL must be set");

    let airtable_token = std::env::var("AIRTABLE_TOKEN")
        .expect("AIRTABLE_TOKEN must be set");

    let airtable_base_id = std::env::var("AIRTABLE_BASE_ID")
        .expect("AIRTABLE_BASE_ID must be set");

    let keypair_path = std::env::var("SOLANA_KEYPAIR_PATH").ok();
    
    let jito_url = std::env::var("JITO_URL")
        .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf/api/v1/bundles".to_string());

    let target_protocol = std::env::var("TARGET_PROTOCOL")
        .unwrap_or_else(|_| "KAMINO".to_string());

    let protocol = match target_protocol.to_uppercase().as_str() {
        "SOLEND" | "SAVE" => Protocol::Solend,
        _ => Protocol::Kamino,
    };

    // ── 2. Build adapters ────────────────────────────────────────────────────
    let observer_rpc = HeliusAdapter::new(&observer_rpc_url, &observer_ws_url);
    let hunter_rpc = HeliusAdapter::new(&hunter_rpc_url, &hunter_ws_url);
    let logger = AirtableAdapter::new(airtable_token, airtable_base_id, "Jawas-Watch".to_string());
    let oracle = SimplePriceOracle::new();
    let jito = JitoAdapter::new(&jito_url);
    let jupiter = JupiterAdapter::new(None);

    let max_repay_usd = std::env::var("MAX_REPAY_USD")
        .map(|v| v.parse::<f64>().unwrap_or(300.0))
        .unwrap_or(300.0);

    // ── 3. Load Keypair if Hunter mode is enabled ───────────────────────────
    let hunter_service = if let Some(path) = keypair_path {
        println!("  [Hunter] Loading keypair from {}...", path);
        let keypair = Arc::new(read_keypair_file(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read keypair: {}", e))?);
        println!("  [Hunter] Wallet: {}", keypair.pubkey());
        
        Some(HunterService::new(
            hunter_rpc.clone(),
            jito,
            jupiter,
            oracle.clone(),
            logger.clone(), // AirtableAdapter implements ConfigPort
            keypair,
            max_repay_usd,
        ))
    } else {
        println!("  [Hunter] No keypair provided. Running in WATCH mode only.");
        None
    };

    // ── 4. Health check — Solana RPC ─────────────────────────────────────────
    print!("  [RPC] Connecting to Solana (Observer)... ");
    match observer_rpc.get_version().await {
        Ok(version) => println!("OK (solana-core {})", version),
        Err(e) => {
            eprintln!("FAILED\n  → {}", e);
            std::process::exit(1);
        }
    }

    // ── 4. Health check — Airtable ───────────────────────────────────────────
    print!("  [Airtable] Sending boot ping... ");
    let ping_event = ObservationEvent {
        timestamp: utc_now(),
        signature: format!("Jawas {} is alive", protocol.name()),
        protocol: protocol.name().to_string(),
        market: "N/A".to_string(),
        liquidated_user: "health-check".to_string(),
        liquidator: "N/A".to_string(),
        repay_mint: "N/A".to_string(),
        withdraw_mint: "N/A".to_string(),
        repay_symbol: "N/A".to_string(),
        withdraw_symbol: "N/A".to_string(),
        repay_amount: 0.0,
        withdraw_amount: 0.0,
        repaid_usd: 0.0,
        withdrawn_usd: 0.0,
        profit_usd: 0.0,
        delay_ms: 0,
        competing_bots: 0,
        status: "WATCHED".to_string(),
    };

    match logger.log_observation(&ping_event).await {
        Ok(_) => println!("OK"),
        Err(e) => {
            eprintln!("FAILED\n  → {}", e);
            std::process::exit(1);
        }
    }

    // ── 5. Setup Communication ──────────────────────────────────────────────
    let (opp_tx, opp_rx) = mpsc::channel(100);

    // ── 6. Spawn Hunter ──────────────────────────────────────────────────────
    if let Some(hunter) = hunter_service {
        tokio::spawn(async move {
            if let Err(e) = hunter.run(opp_rx).await {
                eprintln!("[hunter] service exited with error: {}", e);
            }
        });
    }

    // ── 7. Spawn observer ────────────────────────────────────────────────────
    let logger_for_observer = logger.clone();
    let oracle_for_observer = oracle;
    let rpc_for_observer = observer_rpc;
    tokio::spawn(async move {
        loop {
            println!("[observer] Starting watch loop for {}...", protocol.name());
            let service = ObserverService::new(
                rpc_for_observer.clone(), 
                logger_for_observer.clone(), 
                oracle_for_observer.clone(), 
                protocol
            ).with_opportunity_channel(opp_tx.clone());
            
            if let Err(e) = service.watch().await {
                eprintln!("[observer] loop exited with error: {}. Restarting in 5s...", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            } else {
                println!("[observer] loop closed normally. Restarting in 5s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    });

    // ── 8. Spawn heartbeat ───────────────────────────────────────────────────
    let logger_for_heartbeat = logger.clone();
    tokio::spawn(async move {
        let heartbeat = HeartbeatService::new(logger_for_heartbeat);
        heartbeat.run(tokio::time::Duration::from_secs(15 * 60)).await;
    });

    // ── 9. Wait for termination ──────────────────────────────────────────────
    tokio::signal::ctrl_c().await?;
    println!("Jawas Phase 2: Shutdown requested. Bye!");

    Ok(())
}
