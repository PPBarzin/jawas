use time::{format_description, OffsetDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeLogVerbosity {
    Low,
    Medium,
    High,
}

/// Returns the current UTC time as an ISO 8601 string.
pub fn utc_now() -> String {
    let format =
        format_description::parse_borrowed::<2>("[year]-[month]-[day]T[hour]:[minute]:[second]Z")
            .expect("static UTC timestamp format");
    OffsetDateTime::now_utc()
        .format(&format)
        .expect("formatting current UTC timestamp")
}

pub fn log_stdout(message: impl AsRef<str>) {
    crate::logging::log_info("runtime", message.as_ref());
}

pub fn log_stderr(message: impl AsRef<str>) {
    crate::logging::log_error("runtime", message.as_ref());
}

pub fn runtime_log_verbosity() -> RuntimeLogVerbosity {
    if let Ok(value) = std::env::var("JAWAS_LOG_VERBOSITY") {
        return parse_runtime_log_verbosity(&value).unwrap_or(RuntimeLogVerbosity::Low);
    }

    if let Ok(value) = std::env::var("HUNTER_VERBOSE") {
        let value = value.trim().to_ascii_lowercase();
        return if matches!(value.as_str(), "1" | "true" | "yes" | "on") {
            RuntimeLogVerbosity::High
        } else {
            RuntimeLogVerbosity::Low
        };
    }

    RuntimeLogVerbosity::Low
}

pub fn parse_runtime_log_verbosity(value: &str) -> Option<RuntimeLogVerbosity> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Some(RuntimeLogVerbosity::Low),
        "medium" => Some(RuntimeLogVerbosity::Medium),
        "high" => Some(RuntimeLogVerbosity::High),
        _ => None,
    }
}

pub fn runtime_log_enabled(required: RuntimeLogVerbosity) -> bool {
    runtime_log_verbosity() >= required
}

pub fn log_stdout_at(required: RuntimeLogVerbosity, message: impl AsRef<str>) {
    if runtime_log_enabled(required) {
        log_stdout(message);
    }
}

pub fn log_stderr_at(required: RuntimeLogVerbosity, message: impl AsRef<str>) {
    if runtime_log_enabled(required) {
        log_stderr(message);
    }
}
