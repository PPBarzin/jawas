use anyhow::{bail, Context, Result};
use borsh::BorshDeserialize;
use jawas::config::wallet::load_wallet_tokens;
use jawas::domain::kamino::Obligation;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};
use std::{collections::HashSet, fs, path::PathBuf, str::FromStr};

#[derive(Debug, Clone)]
struct Cli {
    csv_path: PathBuf,
    wallet_toml_path: String,
    rpc_url: Option<String>,
    limit: usize,
    borrow_symbol: Option<String>,
    single_borrow_only: bool,
    all_borrows_covered: bool,
    check_rpc: bool,
    liquidatable_only: bool,
    found_only: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    public_key: String,
    current_ltv: f64,
    unhealthy_ltv: f64,
    dist_to_liq: f64,
    collateral: f64,
    debt: f64,
    deposit_tokens: Vec<String>,
    borrow_tokens: Vec<String>,
}

#[derive(Debug, Clone)]
struct FilteredCandidate {
    candidate: Candidate,
    matched_wallet_symbols: Vec<String>,
    all_borrows_covered: bool,
    onchain_exists: Option<bool>,
    onchain_liquidatable: Option<bool>,
    onchain_error: Option<String>,
}

fn decode_anchor_account<T: BorshDeserialize>(data: &[u8]) -> Result<T> {
    if data.len() < 8 {
        bail!("account too small to contain Anchor discriminator");
    }
    let mut cursor = &data[8..];
    T::deserialize(&mut cursor).map_err(|error| anyhow::anyhow!("borsh decode failed: {error}"))
}

fn parse_cli() -> Result<Cli> {
    let mut args = std::env::args().skip(1);
    let csv_path = match args.next() {
        Some(value) if !value.starts_with("--") => PathBuf::from(value),
        _ => PathBuf::from("2026-05-08T07-46_export.csv"),
    };

    let mut wallet_toml_path =
        std::env::var("WALLET_TOML_PATH").unwrap_or_else(|_| "wallet.toml".to_string());
    let mut rpc_url = std::env::var("HUNTER_RPC_URL")
        .ok()
        .or_else(|| std::env::var("OBSERVER_RPC_URL").ok())
        .or_else(|| std::env::var("RPC_URL").ok());
    let mut limit = 12_usize;
    let mut borrow_symbol = None;
    let mut single_borrow_only = false;
    let mut all_borrows_covered = false;
    let mut check_rpc = false;
    let mut liquidatable_only = false;
    let mut found_only = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--wallet-toml" => {
                wallet_toml_path = args.next().context("missing value for --wallet-toml")?;
            }
            "--rpc-url" => {
                rpc_url = Some(args.next().context("missing value for --rpc-url")?);
            }
            "--limit" => {
                let value = args.next().context("missing value for --limit")?;
                limit = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --limit value '{value}'"))?;
            }
            "--borrow-symbol" => {
                let value = args.next().context("missing value for --borrow-symbol")?;
                borrow_symbol = Some(normalize_symbol(&value));
            }
            "--single-borrow-only" => single_borrow_only = true,
            "--all-borrows-covered" => all_borrows_covered = true,
            "--check-rpc" => check_rpc = true,
            "--liquidatable-only" => {
                check_rpc = true;
                liquidatable_only = true;
            }
            "--found-only" => {
                check_rpc = true;
                found_only = true;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => bail!("unknown argument '{arg}'"),
        }
    }

    Ok(Cli {
        csv_path,
        wallet_toml_path,
        rpc_url,
        limit,
        borrow_symbol,
        single_borrow_only,
        all_borrows_covered,
        check_rpc,
        liquidatable_only,
        found_only,
    })
}

fn print_usage() {
    eprintln!(
        "Usage: cargo run --bin select_kamino_candidates -- [CSV_PATH] [options]

Options:
  --wallet-toml <path>        wallet.toml path (default: WALLET_TOML_PATH or wallet.toml)
  --rpc-url <url>             RPC URL used for on-chain checks
  --limit <n>                 number of candidates to print (default: 12)
  --borrow-symbol <SYMBOL>    keep only candidates borrowing this symbol
  --single-borrow-only        keep only obligations with a single borrow token
  --all-borrows-covered       keep only obligations where every borrow token is covered
  --check-rpc                 annotate candidates with on-chain existence/liquidatable state
  --liquidatable-only         keep only candidates that still decode as liquidatable on-chain
  --found-only                keep only candidates whose account is found by RPC"
    );
}

fn normalize_symbol(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_uppercase())
        .collect()
}

fn split_symbols(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(normalize_symbol)
        .collect()
}

fn extract_pubkey(value: &str) -> String {
    value
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn parse_f64(value: &str) -> Result<f64> {
    value
        .trim()
        .replace('_', "")
        .parse::<f64>()
        .with_context(|| format!("invalid float value '{value}'"))
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes && matches!(chars.peek(), Some('"')) {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    fields.push(current.trim().to_string());
    fields
}

fn load_candidates(path: &PathBuf) -> Result<Vec<Candidate>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read CSV at {}", path.display()))?;
    let mut lines = content.lines();
    let header_line = lines.next().context("CSV is empty")?;
    let headers = parse_csv_line(header_line)
        .into_iter()
        .map(|header| header.trim_start_matches('\u{feff}').to_string())
        .collect::<Vec<_>>();

    let index_of = |name: &str| -> Result<usize> {
        headers
            .iter()
            .position(|header| header == name)
            .with_context(|| format!("missing CSV column '{name}'"))
    };

    let public_key_idx = index_of("Public Key")?;
    let current_ltv_idx = index_of("Current LTV")?;
    let unhealthy_ltv_idx = index_of("Unhealthy LTV")?;
    let dist_to_liq_idx = index_of("Dist To Liq")?;
    let collateral_idx = index_of("Collateral")?;
    let debt_idx = index_of("Debt")?;
    let deposit_tokens_idx = index_of("Deposit Tokens")?;
    let borrow_tokens_idx = index_of("Borrow Tokens")?;

    let mut candidates = Vec::new();
    for (line_no, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_csv_line(line);
        if fields.len() != headers.len() {
            bail!(
                "CSV row {} has {} fields but header has {}",
                line_no + 2,
                fields.len(),
                headers.len()
            );
        }
        candidates.push(Candidate {
            public_key: extract_pubkey(&fields[public_key_idx]),
            current_ltv: parse_f64(&fields[current_ltv_idx])?,
            unhealthy_ltv: parse_f64(&fields[unhealthy_ltv_idx])?,
            dist_to_liq: parse_f64(&fields[dist_to_liq_idx])?,
            collateral: parse_f64(&fields[collateral_idx])?,
            debt: parse_f64(&fields[debt_idx])?,
            deposit_tokens: split_symbols(&fields[deposit_tokens_idx]),
            borrow_tokens: split_symbols(&fields[borrow_tokens_idx]),
        });
    }

    Ok(candidates)
}

fn filter_candidates(cli: &Cli, candidates: Vec<Candidate>) -> Result<Vec<FilteredCandidate>> {
    let wallet_tokens = load_wallet_tokens(&cli.wallet_toml_path)?;
    let wallet_symbols = wallet_tokens
        .into_iter()
        .filter(|token| token.max_repay_native > 0)
        .map(|token| normalize_symbol(&token.symbol))
        .collect::<HashSet<_>>();

    let mut filtered = candidates
        .into_iter()
        .filter_map(|candidate| {
            let mut matched = candidate
                .borrow_tokens
                .iter()
                .filter(|symbol| wallet_symbols.contains(*symbol))
                .cloned()
                .collect::<Vec<_>>();

            matched.sort();
            matched.dedup();

            if matched.is_empty() {
                return None;
            }

            if let Some(symbol) = &cli.borrow_symbol {
                if !candidate
                    .borrow_tokens
                    .iter()
                    .any(|borrow| borrow == symbol)
                {
                    return None;
                }
            }

            if cli.single_borrow_only && candidate.borrow_tokens.len() != 1 {
                return None;
            }

            let all_borrows_covered = candidate
                .borrow_tokens
                .iter()
                .all(|symbol| wallet_symbols.contains(symbol));
            if cli.all_borrows_covered && !all_borrows_covered {
                return None;
            }

            Some(FilteredCandidate {
                candidate,
                matched_wallet_symbols: matched,
                all_borrows_covered,
                onchain_exists: None,
                onchain_liquidatable: None,
                onchain_error: None,
            })
        })
        .collect::<Vec<_>>();

    filtered.sort_by(|left, right| {
        right
            .candidate
            .borrow_tokens
            .len()
            .cmp(&left.candidate.borrow_tokens.len())
            .reverse()
            .then_with(|| {
                right
                    .all_borrows_covered
                    .cmp(&left.all_borrows_covered)
                    .reverse()
            })
            .then_with(|| {
                right
                    .matched_wallet_symbols
                    .len()
                    .cmp(&left.matched_wallet_symbols.len())
                    .reverse()
            })
            .then_with(|| {
                right
                    .candidate
                    .debt
                    .partial_cmp(&left.candidate.debt)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    Ok(filtered)
}

fn resolve_rpc_url(cli: &Cli) -> Result<String> {
    cli.rpc_url.clone().context(
        "missing RPC URL; set HUNTER_RPC_URL, OBSERVER_RPC_URL, RPC_URL, or pass --rpc-url",
    )
}

fn annotate_onchain(cli: &Cli, filtered: Vec<FilteredCandidate>) -> Result<Vec<FilteredCandidate>> {
    if !cli.check_rpc {
        return Ok(filtered);
    }

    let rpc_url = resolve_rpc_url(cli)?;
    let rpc = RpcClient::new_with_timeout_and_commitment(
        rpc_url,
        std::time::Duration::from_secs(10),
        CommitmentConfig::confirmed(),
    );

    let mut annotated = Vec::with_capacity(filtered.len());
    for mut entry in filtered {
        let pubkey = match Pubkey::from_str(&entry.candidate.public_key) {
            Ok(pubkey) => pubkey,
            Err(error) => {
                entry.onchain_exists = Some(false);
                entry.onchain_error = Some(format!("invalid pubkey: {error}"));
                if !cli.liquidatable_only {
                    annotated.push(entry);
                }
                continue;
            }
        };

        match rpc.get_account(&pubkey) {
            Ok(account) => match decode_anchor_account::<Obligation>(&account.data) {
                Ok(obligation) => {
                    let liquidatable = obligation.is_liquidatable();
                    entry.onchain_exists = Some(true);
                    entry.onchain_liquidatable = Some(liquidatable);
                    if (!cli.liquidatable_only || liquidatable) && (!cli.found_only || true) {
                        annotated.push(entry);
                    }
                }
                Err(error) => {
                    entry.onchain_exists = Some(true);
                    entry.onchain_error = Some(format!("decode failed: {error}"));
                    if !cli.liquidatable_only && cli.found_only {
                        annotated.push(entry);
                    } else if !cli.liquidatable_only && !cli.found_only {
                        annotated.push(entry);
                    }
                }
            },
            Err(error) => {
                entry.onchain_exists = Some(false);
                entry.onchain_error = Some(error.to_string());
                if !cli.liquidatable_only && !cli.found_only {
                    annotated.push(entry);
                }
            }
        }
    }

    Ok(annotated)
}

fn print_wallet_summary(path: &str) -> Result<()> {
    let wallet_tokens = load_wallet_tokens(path)?;
    println!("Wallet coverage:");
    println!("  wallet_toml       : {path}");
    for token in wallet_tokens
        .iter()
        .filter(|token| token.max_repay_native > 0)
    {
        println!(
            "  {}               mint={} max_repay_native={}",
            normalize_symbol(&token.symbol),
            token.mint,
            token.max_repay_native
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = parse_cli()?;
    print_wallet_summary(&cli.wallet_toml_path)?;
    let candidates = load_candidates(&cli.csv_path)?;
    let filtered = annotate_onchain(&cli, filter_candidates(&cli, candidates)?)?;

    println!("Scan:");
    println!("  csv               : {}", cli.csv_path.display());
    println!(
        "  borrow_symbol     : {}",
        cli.borrow_symbol.as_deref().unwrap_or("ANY")
    );
    println!("  single_borrow     : {}", cli.single_borrow_only);
    println!("  all_covered       : {}", cli.all_borrows_covered);
    println!("  check_rpc         : {}", cli.check_rpc);
    println!("  liquidatable_only : {}", cli.liquidatable_only);
    println!("  found_only        : {}", cli.found_only);
    println!("  matches           : {}", filtered.len());

    for candidate in filtered.iter().take(cli.limit) {
        println!();
        println!("{}", candidate.candidate.public_key);
        println!(
            "  debt/collateral   : {:.2} / {:.2}",
            candidate.candidate.debt, candidate.candidate.collateral
        );
        println!(
            "  ltv/dist          : {:.4} / {:.4}",
            candidate.candidate.current_ltv, candidate.candidate.dist_to_liq
        );
        println!(
            "  unhealthy_ltv     : {:.4}",
            candidate.candidate.unhealthy_ltv
        );
        println!(
            "  borrow_tokens     : {}",
            candidate.candidate.borrow_tokens.join(",")
        );
        println!(
            "  deposit_tokens    : {}",
            candidate.candidate.deposit_tokens.join(",")
        );
        println!(
            "  wallet_match      : {}",
            candidate.matched_wallet_symbols.join(",")
        );
        println!("  all_borrows_covered: {}", candidate.all_borrows_covered);
        if cli.check_rpc {
            println!(
                "  onchain_exists    : {}",
                candidate.onchain_exists.unwrap_or(false)
            );
            println!(
                "  onchain_liquidatable: {}",
                candidate.onchain_liquidatable.unwrap_or(false)
            );
            if let Some(error) = &candidate.onchain_error {
                println!("  onchain_error     : {error}");
            }
        }
        println!(
            "  next_probe        : cargo run --bin liquidate_one -- {} --mode simulate",
            candidate.candidate.public_key
        );
    }

    Ok(())
}
