use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current UTC time as an ISO 8601 string.
/// Uses only std — no chrono dependency.
pub fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = unix_to_utc(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

pub fn log_stdout(message: impl AsRef<str>) {
    println!("[{}] {}", utc_now(), message.as_ref());
}

pub fn log_stderr(message: impl AsRef<str>) {
    eprintln!("[{}] {}", utc_now(), message.as_ref());
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
