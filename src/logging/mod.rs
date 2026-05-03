pub fn log_info(source: &str, message: &str) {
    emit("INFO", source, message, None, None, None);
}

pub fn log_error(source: &str, message: &str) {
    emit("ERROR", source, message, None, None, None);
}

pub fn log_runtime(
    source: &str,
    event: &str,
    latency_ms: Option<u64>,
    decision: Option<&str>,
    result: Option<&str>,
    detail: Option<&str>,
) {
    emit(
        "INFO",
        source,
        event,
        latency_ms,
        decision,
        result.and_then(non_empty),
    );
    if let Some(detail) = detail.and_then(non_empty) {
        println!(
            "[{}] level=INFO source={} detail=\"{}\"",
            crate::utils::utc_now(),
            source,
            escape(detail)
        );
    }
}

fn emit(
    level: &str,
    source: &str,
    message: &str,
    latency_ms: Option<u64>,
    decision: Option<&str>,
    result: Option<&str>,
) {
    let mut fields = vec![
        format!("[{}]", crate::utils::utc_now()),
        format!("level={level}"),
        format!("source={source}"),
        format!("message=\"{}\"", escape(message)),
    ];

    if let Some(latency_ms) = latency_ms {
        fields.push(format!("latency_ms={latency_ms}"));
    }
    if let Some(decision) = decision.and_then(non_empty) {
        fields.push(format!("decision=\"{}\"", escape(decision)));
    }
    if let Some(result) = result.and_then(non_empty) {
        fields.push(format!("result=\"{}\"", escape(result)));
    }

    println!("{}", fields.join(" "));
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn non_empty(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
