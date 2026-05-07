use crate::application::hunter::{load_wallet_tokens, WalletToken};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, Deserialize)]
pub struct HunterTraceEntry {
    pub timestamp: String,
    pub protocol: String,
    pub stage: String,
    pub signature: String,
    pub obligation: Option<String>,
    pub repay_mint: Option<String>,
    pub repay_symbol: Option<String>,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub ws_received_at_ms: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub bundle_id: Option<String>,
    pub shortlist_hit: Option<bool>,
    pub shortlist_state: Option<String>,
    pub shortlist_age_ms: Option<u64>,
    pub prepared_context_used: Option<bool>,
    pub candidate_score: Option<f64>,
    pub refresh_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetectionMetric {
    pub first_ts_ms: u64,
    pub count: u64,
    pub won_lock: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignalMetricEntry {
    pub protocol: String,
    pub obligation: String,
    pub repay_mint: Option<String>,
    pub winner_source: String,
    pub fire_outcome: String,
    pub detections: HashMap<String, DetectionMetric>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LtvSummary {
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
}

#[derive(Debug, Clone)]
pub struct HunterReport {
    pub trace_count: usize,
    pub metrics_count: usize,
    pub trace_window: Option<(String, String)>,
    pub metrics_window: Option<(u64, u64)>,
    pub stage_counts: Vec<(String, usize)>,
    pub reason_counts: Vec<(String, usize)>,
    pub error_counts: Vec<(String, usize)>,
    pub healthy_source_counts: Vec<(String, usize)>,
    pub healthy_ltv_by_source: Vec<(String, LtvSummary)>,
    pub winner_source_counts: Vec<(String, usize)>,
    pub fire_outcome_counts: Vec<(String, usize)>,
    pub fire_outcome_by_winner: Vec<(String, Vec<(String, usize)>)>,
    pub lead_time_by_winner: Vec<(String, LeadTimeSummary)>,
    pub missing_wallet_mints: Vec<MissingMintStat>,
    pub shortlist_hit_counts: Vec<(String, usize)>,
    pub prepared_context_counts: Vec<(String, usize)>,
    pub shortlist_state_counts: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeadTimeSummary {
    pub count: usize,
    pub avg_ms: f64,
    pub min_ms: u64,
    pub max_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingMintStat {
    pub mint: String,
    pub count: usize,
    pub symbols: Vec<String>,
}

pub fn load_trace_entries(path: &str) -> anyhow::Result<Vec<HunterTraceEntry>> {
    let content = std::fs::read_to_string(path)?;
    let mut entries = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(trimmed)?);
    }
    Ok(entries)
}

pub fn load_signal_metrics(path: &str) -> anyhow::Result<Vec<SignalMetricEntry>> {
    let content = std::fs::read_to_string(path)?;
    let mut entries = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(trimmed)?);
    }
    Ok(entries)
}

pub fn analyze_hunter_data(
    traces: &[HunterTraceEntry],
    metrics: &[SignalMetricEntry],
    wallet_tokens: &[WalletToken],
) -> HunterReport {
    let trace_window = traces.first().and_then(|first| {
        traces
            .last()
            .map(|last| (first.timestamp.clone(), last.timestamp.clone()))
    });

    let metrics_window = metric_time_bounds(metrics);

    let stage_counts = sorted_counts(
        traces
            .iter()
            .map(|entry| entry.stage.clone())
            .collect::<Vec<_>>(),
    );
    let reason_counts = sorted_counts(
        traces
            .iter()
            .filter_map(|entry| entry.reason.clone())
            .collect::<Vec<_>>(),
    );

    let error_counts = sorted_counts(
        traces
            .iter()
            .filter(|entry| entry.stage == "error")
            .map(|entry| classify_error(entry.detail.as_deref()))
            .collect::<Vec<_>>(),
    );

    let healthy_traces = traces
        .iter()
        .filter(|entry| entry.reason.as_deref() == Some("source_obligation_healthy"))
        .collect::<Vec<_>>();

    let healthy_source_counts = sorted_counts(
        healthy_traces
            .iter()
            .map(|entry| {
                parse_source_from_detail(entry.detail.as_deref())
                    .unwrap_or_else(|| "unknown".to_string())
            })
            .collect::<Vec<_>>(),
    );

    let mut healthy_ltv_map: HashMap<String, Vec<f64>> = HashMap::new();
    for entry in &healthy_traces {
        if let Some(ltv) = extract_ltv_from_detail(entry.detail.as_deref()) {
            let source = parse_source_from_detail(entry.detail.as_deref())
                .unwrap_or_else(|| "unknown".to_string());
            healthy_ltv_map.entry(source).or_default().push(ltv);
        }
    }
    let mut healthy_ltv_by_source = healthy_ltv_map
        .into_iter()
        .filter_map(|(source, ltvs)| summarize_ltv(&ltvs).map(|summary| (source, summary)))
        .collect::<Vec<_>>();
    healthy_ltv_by_source.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(&b.0)));

    let winner_source_counts = sorted_counts(
        metrics
            .iter()
            .map(|entry| entry.winner_source.clone())
            .collect::<Vec<_>>(),
    );
    let fire_outcome_counts = sorted_counts(
        metrics
            .iter()
            .map(|entry| entry.fire_outcome.clone())
            .collect::<Vec<_>>(),
    );

    let mut fire_outcome_by_winner_map: HashMap<String, Vec<String>> = HashMap::new();
    for entry in metrics {
        fire_outcome_by_winner_map
            .entry(entry.winner_source.clone())
            .or_default()
            .push(entry.fire_outcome.clone());
    }
    let mut fire_outcome_by_winner = fire_outcome_by_winner_map
        .into_iter()
        .map(|(winner, outcomes)| (winner, sorted_counts(outcomes)))
        .collect::<Vec<_>>();
    fire_outcome_by_winner.sort_by(|a, b| a.0.cmp(&b.0));

    let mut lead_times: HashMap<String, Vec<u64>> = HashMap::new();
    for entry in metrics {
        if let Some(lead_ms) = compute_winner_lead_ms(entry) {
            lead_times
                .entry(entry.winner_source.clone())
                .or_default()
                .push(lead_ms);
        }
    }
    let mut lead_time_by_winner = lead_times
        .into_iter()
        .filter_map(|(source, values)| {
            summarize_lead_times(&values).map(|summary| (source, summary))
        })
        .collect::<Vec<_>>();
    lead_time_by_winner.sort_by(|a, b| a.0.cmp(&b.0));

    let wallet_mints = wallet_tokens
        .iter()
        .map(|token| token.mint.clone())
        .collect::<BTreeSet<_>>();
    let mut missing_wallet_mint_counts: HashMap<String, usize> = HashMap::new();
    let mut missing_wallet_symbols: HashMap<String, BTreeSet<String>> = HashMap::new();
    for entry in traces {
        if entry.reason.as_deref() != Some("token_not_whitelisted") {
            continue;
        }
        let Some(mint) = entry.repay_mint.as_ref() else {
            continue;
        };
        if wallet_mints.contains(mint) {
            continue;
        }
        *missing_wallet_mint_counts.entry(mint.clone()).or_insert(0) += 1;
        if let Some(symbol) = entry.repay_symbol.as_ref() {
            missing_wallet_symbols
                .entry(mint.clone())
                .or_default()
                .insert(symbol.clone());
        }
    }
    let mut missing_wallet_mints = missing_wallet_mint_counts
        .into_iter()
        .map(|(mint, count)| MissingMintStat {
            symbols: missing_wallet_symbols
                .remove(&mint)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            mint,
            count,
        })
        .collect::<Vec<_>>();
    missing_wallet_mints.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.mint.cmp(&b.mint)));

    let shortlist_hit_counts = sorted_counts(
        traces
            .iter()
            .filter_map(|entry| {
                entry
                    .shortlist_hit
                    .map(|value| if value { "true" } else { "false" }.to_string())
            })
            .collect::<Vec<_>>(),
    );
    let prepared_context_counts = sorted_counts(
        traces
            .iter()
            .filter_map(|entry| {
                entry
                    .prepared_context_used
                    .map(|value| if value { "true" } else { "false" }.to_string())
            })
            .collect::<Vec<_>>(),
    );
    let shortlist_state_counts = sorted_counts(
        traces
            .iter()
            .filter_map(|entry| entry.shortlist_state.clone())
            .collect::<Vec<_>>(),
    );

    HunterReport {
        trace_count: traces.len(),
        metrics_count: metrics.len(),
        trace_window,
        metrics_window,
        stage_counts,
        reason_counts,
        error_counts,
        healthy_source_counts,
        healthy_ltv_by_source,
        winner_source_counts,
        fire_outcome_counts,
        fire_outcome_by_winner,
        lead_time_by_winner,
        missing_wallet_mints,
        shortlist_hit_counts,
        prepared_context_counts,
        shortlist_state_counts,
    }
}

pub fn render_report_markdown(
    report: &HunterReport,
    trace_path: &str,
    metrics_path: &str,
    wallet_path: &str,
) -> String {
    let mut out = String::new();
    out.push_str("# Hunter Analysis Report\n\n");
    out.push_str("## Inputs\n\n");
    out.push_str(&format!("- trace file: `{trace_path}`\n"));
    out.push_str(&format!("- metrics file: `{metrics_path}`\n"));
    out.push_str(&format!("- wallet file: `{wallet_path}`\n\n"));

    out.push_str("## Coverage\n\n");
    out.push_str(&format!("- trace entries: `{}`\n", report.trace_count));
    out.push_str(&format!(
        "- signal metric entries: `{}`\n",
        report.metrics_count
    ));
    if let Some((start, end)) = &report.trace_window {
        out.push_str(&format!("- trace window: `{start}` -> `{end}`\n"));
    }
    if let Some((start, end)) = report.metrics_window {
        out.push_str(&format!(
            "- metrics first-seen window: `{start}` -> `{end}` ms\n"
        ));
    }
    out.push('\n');

    out.push_str("## Funnel\n\n");
    for (stage, count) in &report.stage_counts {
        out.push_str(&format!("- `{stage}`: `{count}`\n"));
    }
    out.push('\n');

    out.push_str("## Skip Reasons\n\n");
    for (reason, count) in &report.reason_counts {
        out.push_str(&format!("- `{reason}`: `{count}`\n"));
    }
    out.push('\n');

    out.push_str("## Error Hotspots\n\n");
    for (error, count) in &report.error_counts {
        out.push_str(&format!("- `{error}`: `{count}`\n"));
    }
    out.push('\n');

    out.push_str("## Healthy Signal Profile\n\n");
    for (source, count) in &report.healthy_source_counts {
        out.push_str(&format!("- `{source}` healthy skips: `{count}`\n"));
    }
    if !report.healthy_ltv_by_source.is_empty() {
        out.push('\n');
        for (source, summary) in &report.healthy_ltv_by_source {
            out.push_str(&format!(
                "- `{source}` LTV from healthy traces: `count={}` `avg={:.6}` `min={:.6}` `max={:.6}`\n",
                summary.count, summary.avg, summary.min, summary.max
            ));
        }
    }
    out.push('\n');

    out.push_str("## Source Race\n\n");
    for (winner, count) in &report.winner_source_counts {
        out.push_str(&format!("- `{winner}` winner lock count: `{count}`\n"));
    }
    if !report.lead_time_by_winner.is_empty() {
        out.push('\n');
        for (winner, lead) in &report.lead_time_by_winner {
            out.push_str(&format!(
                "- `{winner}` lead over next source: `count={}` `avg_ms={:.2}` `min_ms={}` `max_ms={}`\n",
                lead.count, lead.avg_ms, lead.min_ms, lead.max_ms
            ));
        }
    }
    out.push('\n');

    out.push_str("## Fire Outcomes\n\n");
    for (outcome, count) in &report.fire_outcome_counts {
        out.push_str(&format!("- `{outcome}`: `{count}`\n"));
    }
    out.push('\n');
    for (winner, outcomes) in &report.fire_outcome_by_winner {
        out.push_str(&format!("- `{winner}` outcomes:\n"));
        for (outcome, count) in outcomes {
            out.push_str(&format!("  - `{outcome}`: `{count}`\n"));
        }
    }
    out.push('\n');

    out.push_str("## Shortlist Signals\n\n");
    for (value, count) in &report.shortlist_hit_counts {
        out.push_str(&format!("- `shortlist_hit={value}`: `{count}`\n"));
    }
    for (value, count) in &report.prepared_context_counts {
        out.push_str(&format!("- `prepared_context_used={value}`: `{count}`\n"));
    }
    for (state, count) in &report.shortlist_state_counts {
        out.push_str(&format!("- `shortlist_state={state}`: `{count}`\n"));
    }
    out.push('\n');

    out.push_str("## Wallet Gaps\n\n");
    if report.missing_wallet_mints.is_empty() {
        out.push_str("- no `token_not_whitelisted` gap found in the trace window\n");
    } else {
        for gap in &report.missing_wallet_mints {
            if gap.symbols.is_empty() {
                out.push_str(&format!(
                    "- `{}` seen `{}` times outside wallet coverage\n",
                    gap.mint, gap.count
                ));
            } else {
                out.push_str(&format!(
                    "- `{}` (`{}`) seen `{}` times outside wallet coverage\n",
                    gap.mint,
                    gap.symbols.join(", "),
                    gap.count
                ));
            }
        }
    }
    out.push('\n');

    out.push_str("## Notes\n\n");
    out.push_str("- `bundle_sent` measures acceptance by the Jito endpoint, not an observed on-chain liquidation win.\n");
    out.push_str("- Observed success still has to be correlated with Airtable or another on-chain verification source.\n");
    out.push_str("- This report is meant to be regenerated over time to compare the same funnel across different observation windows.\n");
    out
}

pub fn report_file_date() -> String {
    crate::utils::utc_now()
        .split('T')
        .next()
        .unwrap_or("1970-01-01")
        .to_string()
}

pub fn load_wallet_or_default(path: &str) -> Vec<WalletToken> {
    load_wallet_tokens(path)
}

fn metric_time_bounds(metrics: &[SignalMetricEntry]) -> Option<(u64, u64)> {
    let mut times = metrics.iter().flat_map(|entry| {
        entry
            .detections
            .values()
            .map(|detection| detection.first_ts_ms)
    });
    let first = times.next()?;
    let mut min = first;
    let mut max = first;
    for ts in times {
        min = min.min(ts);
        max = max.max(ts);
    }
    Some((min, max))
}

fn sorted_counts(values: Vec<String>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    let mut rows = counts.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

fn classify_error(detail: Option<&str>) -> String {
    let detail = detail.unwrap_or("unknown");
    if detail.contains("no KLEND liquidate instruction found") {
        return "no KLEND liquidate instruction found".to_string();
    }
    if detail.contains("expired blockhash") {
        return "expired blockhash".to_string();
    }
    if detail.to_ascii_lowercase().contains("rate limit")
        || detail.to_ascii_lowercase().contains("network congested")
    {
        return "jito rate limit / congestion".to_string();
    }
    detail
        .split('|')
        .next()
        .unwrap_or(detail)
        .trim()
        .to_string()
}

pub fn parse_source_from_detail(detail: Option<&str>) -> Option<String> {
    let detail = detail?;
    detail.split_whitespace().find_map(|part| {
        part.strip_prefix("source=")
            .map(|source| source.trim().to_string())
    })
}

pub fn extract_ltv_from_detail(detail: Option<&str>) -> Option<f64> {
    let detail = detail?;
    let marker = "LTV:";
    let (_, rest) = detail.split_once(marker)?;
    rest.trim().split_whitespace().next()?.parse::<f64>().ok()
}

fn summarize_ltv(values: &[f64]) -> Option<LtvSummary> {
    if values.is_empty() {
        return None;
    }
    let mut min = values[0];
    let mut max = values[0];
    let mut sum = 0.0;
    for value in values {
        min = min.min(*value);
        max = max.max(*value);
        sum += *value;
    }
    Some(LtvSummary {
        count: values.len(),
        min,
        max,
        avg: sum / values.len() as f64,
    })
}

pub fn compute_winner_lead_ms(entry: &SignalMetricEntry) -> Option<u64> {
    let winner = entry.detections.get(&entry.winner_source)?;
    let mut losing_times = entry
        .detections
        .iter()
        .filter(|(source, _)| *source != &entry.winner_source)
        .map(|(_, detection)| detection.first_ts_ms)
        .collect::<Vec<_>>();
    if losing_times.is_empty() {
        return None;
    }
    losing_times.sort_unstable();
    Some(losing_times[0].saturating_sub(winner.first_ts_ms))
}

fn summarize_lead_times(values: &[u64]) -> Option<LeadTimeSummary> {
    if values.is_empty() {
        return None;
    }
    let mut min = values[0];
    let mut max = values[0];
    let mut sum = 0u128;
    for value in values {
        min = min.min(*value);
        max = max.max(*value);
        sum += *value as u128;
    }
    Some(LeadTimeSummary {
        count: values.len(),
        avg_ms: sum as f64 / values.len() as f64,
        min_ms: min,
        max_ms: max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_source_from_detail_extracts_source_tag() {
        let detail = "source=helius Program log: Instruction: Liquidate";
        assert_eq!(
            parse_source_from_detail(Some(detail)).as_deref(),
            Some("helius")
        );
    }

    #[test]
    fn extract_ltv_from_detail_reads_decimal_value() {
        let detail = "source=helius Program log: Obligation is healthy and cannot be liquidated, LTV: 0.859154365162309905";
        let ltv = extract_ltv_from_detail(Some(detail)).unwrap();
        assert!((ltv - 0.8591543651623099).abs() < 1e-12);
    }

    #[test]
    fn compute_winner_lead_ms_uses_fastest_loser() {
        let entry = SignalMetricEntry {
            protocol: "kamino".to_string(),
            obligation: "obl".to_string(),
            repay_mint: Some("mint".to_string()),
            winner_source: "helius".to_string(),
            fire_outcome: "bundle_sent".to_string(),
            detections: HashMap::from([
                (
                    "helius".to_string(),
                    DetectionMetric {
                        first_ts_ms: 100,
                        count: 1,
                        won_lock: true,
                    },
                ),
                (
                    "quicknode".to_string(),
                    DetectionMetric {
                        first_ts_ms: 145,
                        count: 1,
                        won_lock: false,
                    },
                ),
                (
                    "hermes".to_string(),
                    DetectionMetric {
                        first_ts_ms: 180,
                        count: 1,
                        won_lock: false,
                    },
                ),
            ]),
        };

        assert_eq!(compute_winner_lead_ms(&entry), Some(45));
    }
}
