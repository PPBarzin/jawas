use jawas::domain::hunter_analysis::{
    analyze_hunter_data, load_signal_metrics, load_trace_entries, load_wallet_or_default,
    render_report_markdown, report_file_date,
};

fn env_or_default(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn main() -> anyhow::Result<()> {
    dotenv::dotenv().ok();

    let trace_path = env_or_default(
        "HUNTER_TRACE_PATH",
        "docs/specifications/hunter_trace.jsonl",
    );
    let metrics_path = env_or_default(
        "HUNTER_SIGNAL_METRICS_PATH",
        "docs/specifications/hunter_signal_metrics.jsonl",
    );
    let wallet_path = std::env::var("HUNTER_REPORT_WALLET_PATH")
        .ok()
        .or_else(|| std::env::var("WALLET_TOML_PATH").ok())
        .unwrap_or_else(|| "wallet.toml".to_string());

    let traces = load_trace_entries(&trace_path)?;
    let metrics = load_signal_metrics(&metrics_path)?;
    let wallet_tokens = load_wallet_or_default(&wallet_path);

    let report = analyze_hunter_data(&traces, &metrics, &wallet_tokens);
    let markdown = render_report_markdown(&report, &trace_path, &metrics_path, &wallet_path);

    let output_dir = std::path::Path::new("docs/analysis");
    std::fs::create_dir_all(output_dir)?;
    let output_path = output_dir.join(format!("{}-hunter-analysis.md", report_file_date()));
    std::fs::write(&output_path, markdown)?;

    println!("{}", output_path.display());
    Ok(())
}
