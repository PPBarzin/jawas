use crate::application::heartbeat::HeartbeatService;
use crate::application::hunter::{
    load_wallet_tokens, HunterService, WalletToken, DEFAULT_KAMINO_REPLAY_SIGNATURE,
};
use crate::application::observer::{ObserverService, Protocol};
use crate::config::AppConfig;
use crate::infrastructure::{
    airtable::AirtableAdapter,
    helius::HeliusAdapter,
    jito::JitoAdapter,
    jupiter::JupiterAdapter,
    oracle::SimplePriceOracle,
};
use crate::logging::{log_error, log_info, log_runtime};
use crate::ports::logger::{LiquidationLogger, ObservationEvent};
use crate::ports::rpc::RpcClient;
use solana_sdk::signature::read_keypair_file;
use solana_sdk::signer::Signer;
use std::sync::Arc;

pub async fn run() -> anyhow::Result<()> {
    let config = AppConfig::from_env()?;

    log_info("app", "booting Jawas research runtime");

    let observer_rpc = HeliusAdapter::with_tx_commitment(
        &config.observer.rpc_url,
        &config.observer.ws_url,
        &config.observer.tx_commitment,
    );
    let hunter_rpc = HeliusAdapter::with_tx_commitment(
        &config.hunter.rpc_url,
        &config.hunter.ws_url,
        &config.hunter.tx_commitment,
    );

    let logger = AirtableAdapter::new(
        config.airtable.token.clone(),
        config.airtable.base_id.clone(),
        config.airtable.watch_table.clone(),
    );
    let oracle = SimplePriceOracle::new();
    let jito = JitoAdapter::new(&config.hunter.jito_url);
    let jupiter = JupiterAdapter::new(None);

    let protocol = config.runtime.protocol();
    let hunter_service = build_hunter_service(
        &config,
        observer_rpc.clone(),
        hunter_rpc.clone(),
        logger.clone(),
        oracle.clone(),
        jito,
        jupiter,
    )?;

    verify_observer_rpc(&observer_rpc).await?;
    send_boot_ping(&logger, protocol).await?;

    if let Some(hunter) = hunter_service {
        let wallet_tokens = load_wallet_tokens(&config.hunter.wallet_toml_path);
        if config.hunter.replay_enabled || config.hunter.replay_signature.is_some() {
            run_hunter_replay(&hunter, protocol, wallet_tokens, config.hunter.replay_signature.clone())
                .await?;
            log_info("app", "replay completed");
            return Ok(());
        }

        spawn_hunter(protocol, hunter, wallet_tokens);
    } else {
        log_info("hunter", "disabled via configuration");
    }

    if config.runtime.enable_observer {
        spawn_observer(protocol, observer_rpc, logger.clone(), oracle);
    } else {
        log_info("observer", "disabled via configuration");
    }

    spawn_heartbeat(logger);

    tokio::signal::ctrl_c().await?;
    log_info("app", "shutdown requested");
    Ok(())
}

type JawasHunter = HunterService<
    HeliusAdapter,
    JitoAdapter,
    JupiterAdapter,
    SimplePriceOracle,
    AirtableAdapter,
>;

fn build_hunter_service(
    config: &AppConfig,
    observer_rpc: HeliusAdapter,
    hunter_rpc: HeliusAdapter,
    logger: AirtableAdapter,
    oracle: SimplePriceOracle,
    jito: JitoAdapter,
    jupiter: JupiterAdapter,
) -> anyhow::Result<Option<JawasHunter>>
{
    if !config.runtime.enable_hunter {
        return Ok(None);
    }

    let keypair_path = config
        .hunter
        .keypair_path
        .clone()
        .ok_or_else(|| anyhow::anyhow!("ENABLE_HUNTER=true but SOLANA_KEYPAIR_PATH is not set"))?;

    log_runtime("hunter", "loading keypair", None, Some("startup"), None, Some(&keypair_path));
    let keypair = Arc::new(
        read_keypair_file(&keypair_path)
            .map_err(|e| anyhow::anyhow!("failed to read keypair: {}", e))?,
    );
    log_runtime(
        "hunter",
        "wallet loaded",
        None,
        Some("startup"),
        Some(&keypair.pubkey().to_string()),
        None,
    );

    Ok(Some(HunterService::new(
        hunter_rpc,
        Some(observer_rpc),
        jito,
        jupiter,
        oracle,
        logger,
        keypair,
        config.hunter.max_repay_usd,
    )))
}

async fn verify_observer_rpc(observer_rpc: &HeliusAdapter) -> anyhow::Result<()> {
    match observer_rpc.get_version().await {
        Ok(version) => {
            log_runtime(
                "rpc",
                "observer RPC reachable",
                None,
                Some("healthcheck"),
                Some(&format!("solana-core {}", version)),
                None,
            );
            Ok(())
        }
        Err(error) => {
            log_error("rpc", &format!("observer RPC healthcheck failed: {}", error));
            Err(error)
        }
    }
}

async fn send_boot_ping(
    logger: &AirtableAdapter,
    protocol: Protocol,
) -> anyhow::Result<()> {
    let ping_event = ObservationEvent {
        timestamp: crate::utils::utc_now(),
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

    logger.log_observation(&ping_event).await.map(|_| {
        log_runtime("airtable", "boot ping sent", None, Some("healthcheck"), Some("ok"), None);
    })
}

async fn run_hunter_replay(
    hunter: &JawasHunter,
    protocol: Protocol,
    wallet_tokens: Vec<WalletToken>,
    replay_signature: Option<String>,
) -> anyhow::Result<()> {
    let signature = replay_signature.unwrap_or_else(|| match protocol {
        Protocol::Kamino => DEFAULT_KAMINO_REPLAY_SIGNATURE.to_string(),
        Protocol::Solend => String::new(),
    });

    if signature.is_empty() {
        return Err(anyhow::anyhow!(
            "HUNTER_REPLAY_SIGNATURE is required for Solend replay"
        ));
    }

    match protocol {
        Protocol::Kamino => hunter.replay_kamino(wallet_tokens, signature).await,
        Protocol::Solend => hunter.replay_solend(wallet_tokens, signature).await,
    }
}

fn spawn_hunter(
    protocol: Protocol,
    hunter: JawasHunter,
    wallet_tokens: Vec<WalletToken>,
) {
    match protocol {
        Protocol::Kamino => {
            tokio::spawn(async move {
                loop {
                    if let Err(error) = hunter.run_kamino(wallet_tokens.clone()).await {
                        log_error("hunter-kamino", &format!("loop exited: {}. Restarting in 2s", error));
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    }
                }
            });
        }
        Protocol::Solend => {
            tokio::spawn(async move {
                loop {
                    if let Err(error) = hunter.run_solend(wallet_tokens.clone()).await {
                        log_error("hunter-solend", &format!("loop exited: {}. Restarting in 2s", error));
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    }
                }
            });
        }
    }
}

fn spawn_observer(
    protocol: Protocol,
    rpc: HeliusAdapter,
    logger: AirtableAdapter,
    oracle: SimplePriceOracle,
) {
    tokio::spawn(async move {
        loop {
            log_runtime(
                "observer",
                "starting watch loop",
                None,
                Some(protocol.name()),
                Some("running"),
                None,
            );
            let service = ObserverService::new(rpc.clone(), logger.clone(), oracle.clone(), protocol);
            if let Err(error) = service.watch().await {
                log_error("observer", &format!("loop exited: {}. Restarting in 5s", error));
            } else {
                log_info("observer", "loop closed normally. Restarting in 5s");
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    });
}

fn spawn_heartbeat(logger: AirtableAdapter) {
    tokio::spawn(async move {
        let heartbeat = HeartbeatService::new(logger);
        heartbeat.run(tokio::time::Duration::from_secs(15 * 60)).await;
    });
}
