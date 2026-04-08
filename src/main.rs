mod domain;
mod ports;
mod adapters;
mod services;
mod utils;

use adapters::{airtable::AirtableAdapter, helius::HeliusAdapter, oracle::SimplePriceOracle};
use ports::{
    logger::{LiquidationLogger, ObservationEvent},
    rpc::RpcClient,
};
use services::heartbeat::HeartbeatService;
use services::observer::{ObserverService, Protocol};
use utils::utc_now;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    println!("Jawas Phase 1: Booting...");

    // ── 1. Load config from environment ─────────────────────────────────────
    let rpc_url = std::env::var("RPC_URL")
        .expect("RPC_URL must be set in .env or environment");

    let ws_url = std::env::var("WS_URL")
        .expect("WS_URL must be set in .env or environment");

    let airtable_token = std::env::var("AIRTABLE_TOKEN")
        .expect("AIRTABLE_TOKEN must be set in .env or environment");

    let airtable_base_id = std::env::var("AIRTABLE_BASE_ID")
        .expect("AIRTABLE_BASE_ID must be set in .env or environment");

    let table_watch = std::env::var("AIRTABLE_TABLE_WATCH")
        .unwrap_or_else(|_| "Jawas-Watch".to_string());

    let target_protocol = std::env::var("TARGET_PROTOCOL")
        .unwrap_or_else(|_| "KAMINO".to_string());

    let protocol = match target_protocol.to_uppercase().as_str() {
        "SOLEND" | "SAVE" => Protocol::Solend,
        _ => Protocol::Kamino,
    };

    // ── 2. Build adapters ────────────────────────────────────────────────────
    let rpc = HeliusAdapter::new(&rpc_url, &ws_url);
    let logger = AirtableAdapter::new(airtable_token, airtable_base_id, table_watch);
    let oracle = SimplePriceOracle::new();

    // ── 3. Health check — Solana RPC ─────────────────────────────────────────
    print!("  [RPC] Connecting to Solana... ");
    match rpc.get_version().await {
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

    println!("Jawas Phase 1: Ready. Watching {}...", protocol.name());

    // ── 5. Spawn observer ────────────────────────────────────────────────────
    let logger_for_observer = logger.clone();
    let oracle_for_observer = oracle;
    let rpc_for_observer = rpc;
    tokio::spawn(async move {
        if let Err(e) = ObserverService::new(rpc_for_observer, logger_for_observer, oracle_for_observer, protocol).watch().await {
            eprintln!("[observer] exited with error: {}", e);
        }
    });

    // ── 6. Spawn heartbeat — every 15 minutes ────────────────────────────────
    let logger_for_heartbeat = logger.clone();
    tokio::spawn(async move {
        let heartbeat = HeartbeatService::new(logger_for_heartbeat);
        heartbeat.run(tokio::time::Duration::from_secs(15 * 60)).await;
    });

    // ── 7. Wait for termination ──────────────────────────────────────────────
    tokio::signal::ctrl_c().await?;
    println!("Jawas Phase 1: Shutdown requested. Bye!");

    Ok(())
}
