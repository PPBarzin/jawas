use anyhow::{Context, Result};
use jawas::infrastructure::helius::HeliusAdapter;
use jawas::ports::rpc::RpcClient;

const DEFAULT_TX_SIGNATURE: &str =
    "5b2W6vU2E7dZuZYjxXL1YHagCB83sVErdd14dKQ3BzBs4FPiuZwC82npCqbBqokbwC7UFzbdHMs8anYpeqsRHmcp";
const DEFAULT_OBLIGATION: &str = "4X58VJW7MRGzZjctKCi5Kg3vFKAVT8UEXVgc2S3drej9";

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let mut args = std::env::args().skip(1);
    let obligation = args
        .next()
        .unwrap_or_else(|| DEFAULT_OBLIGATION.to_string());
    let signature = args
        .next()
        .unwrap_or_else(|| DEFAULT_TX_SIGNATURE.to_string());

    let rpc_url = std::env::var("OBSERVER_RPC_URL")
        .or_else(|_| std::env::var("RPC_URL"))
        .context("OBSERVER_RPC_URL or RPC_URL not set")?;
    let ws_url = std::env::var("OBSERVER_WS_URL")
        .or_else(|_| std::env::var("WS_URL"))
        .context("OBSERVER_WS_URL or WS_URL not set")?;

    let rpc = HeliusAdapter::with_tx_commitment(&rpc_url, &ws_url, "confirmed");

    println!("Helius investigation");
    println!("  rpc_url           : {rpc_url}");
    println!("  obligation        : {obligation}");
    println!("  signature         : {signature}");

    match rpc.get_version().await {
        Ok(version) => println!("  version           : {version}"),
        Err(error) => println!("  version_error     : {error:#}"),
    }

    match rpc.get_signature_status(&signature).await {
        Ok(Some(status)) => {
            println!("Signature status:");
            println!("  slot              : {:?}", status.slot);
            println!("  confirmation      : {:?}", status.confirmation_status);
            println!("  has_error         : {}", status.has_error);
        }
        Ok(None) => {
            println!("Signature status:");
            println!("  result            : not found");
        }
        Err(error) => {
            println!("Signature status:");
            println!("  error             : {error:#}");
        }
    }

    match rpc.get_transaction(&signature).await {
        Ok(tx) => {
            println!("Transaction:");
            println!("  account_keys      : {}", tx.account_keys.len());
            println!("  instruction_count : {}", tx.instruction_accounts.len());
            println!("  block_time        : {:?}", tx.block_time);

            for (idx, key) in tx.account_keys.iter().enumerate() {
                if key == &obligation {
                    println!("  obligation_index  : {idx}");
                }
            }

            for (idx, accounts) in tx.instruction_accounts.iter().enumerate() {
                let rendered = accounts
                    .iter()
                    .map(|account_idx| {
                        let key = tx
                            .account_keys
                            .get(*account_idx)
                            .cloned()
                            .unwrap_or_else(|| "<out-of-range>".to_string());
                        format!("{account_idx}:{key}")
                    })
                    .collect::<Vec<_>>();
                let program_idx = tx
                    .instruction_programs
                    .get(idx)
                    .copied()
                    .unwrap_or_default();
                let program = tx
                    .account_keys
                    .get(program_idx)
                    .cloned()
                    .unwrap_or_else(|| "<unknown-program>".to_string());
                println!("  ix#{idx} program  : {program_idx}:{program}");
                println!("  ix#{idx} accounts : {}", rendered.join(", "));
            }
        }
        Err(error) => {
            println!("Transaction:");
            println!("  error             : {error:#}");
        }
    }

    match rpc.get_account_info(&obligation).await {
        Ok(bytes) => {
            println!("Account info:");
            println!("  found             : true");
            println!("  data_len          : {}", bytes.len());
        }
        Err(error) => {
            println!("Account info:");
            println!("  found             : false");
            println!("  error             : {error:#}");
        }
    }

    Ok(())
}
