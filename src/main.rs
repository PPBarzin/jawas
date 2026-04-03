mod domain;
mod ports;
mod adapters;
mod services;

use adapters::{airtable::AirtableAdapter, helius::HeliusAdapter};
use ports::{
    logger::{LiquidationLogger, ObservationEvent},
    rpc::RpcClient,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    println!("Jawas Phase 1: Booting...");

    // ── 1. Load config from environment ─────────────────────────────────────
    let rpc_url = std::env::var("RPC_URL")
        .expect("RPC_URL must be set in .env or environment");

    let airtable_token = std::env::var("AIRTABLE_TOKEN")
        .expect("AIRTABLE_TOKEN must be set in .env or environment");

    let airtable_base_id = std::env::var("AIRTABLE_BASE_ID")
        .expect("AIRTABLE_BASE_ID must be set in .env or environment");

    let table_watch = std::env::var("AIRTABLE_TABLE_WATCH")
        .unwrap_or_else(|_| "Jawas-Watch".to_string());

    // ── 2. Build adapters ────────────────────────────────────────────────────
    let rpc = HeliusAdapter::new(&rpc_url);
    let logger = AirtableAdapter::new(airtable_token, airtable_base_id, table_watch);

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
    let now = utc_now();
    let ping_event = ObservationEvent {
        timestamp: now.clone(),
        borrower: "health-check".to_string(),
        collateral_token: "N/A".to_string(),
        collateral_amount: 0.0,
        debt_repaid_usdc: 0.0,
        profit_estimated_usd: 0.0,
        ltv_at_liquidation: 0.0,
        delay_ms: 0,
        competing_bots: 0,
        winner_tx: "Jawas is alive".to_string(),
    };

    match logger.log_observation(&ping_event).await {
        Ok(_) => println!("OK"),
        Err(e) => {
            eprintln!("FAILED\n  → {}", e);
            std::process::exit(1);
        }
    }

    println!("Jawas Phase 1: Ready. Watching Kamino...");

    // ── 5. Main watch loop (placeholder — observer will be wired here) ───────
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        println!("[heartbeat] watching...");
    }
}

/// Returns the current UTC time as an ISO 8601 string.
/// Uses only std — no chrono dependency in main.rs.
fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = unix_to_utc(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

fn unix_to_utc(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = s % 60; s /= 60;
    let min = s % 60; s /= 60;
    let hour = s % 24; s /= 24;
    let mut days = s;
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year { break; }
        days -= days_in_year;
        year += 1;
    }
    let months = [31u64, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 0u64;
    for &m in &months {
        if days < m { break; }
        days -= m;
        month += 1;
    }
    (year, month + 1, days + 1, hour, min, sec)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
