use time::{OffsetDateTime, format_description};

/// Returns the current UTC time as an ISO 8601 string.
pub fn utc_now() -> String {
    let format = format_description::parse_borrowed::<2>(
        "[year]-[month]-[day]T[hour]:[minute]:[second]Z",
    )
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
