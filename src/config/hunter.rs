use crate::config::env_flag;
use crate::ports::rpc::RpcCommitment;

#[derive(Debug, Clone, Copy)]
pub struct HunterTxFetchConfig {
    pub attempts: usize,
    pub retry_delay_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct HunterRuntimeConfig {
    pub signal_commitment: RpcCommitment,
    pub tx_fetch: HunterTxFetchConfig,
    pub non_whitelist_cooldown_ms: u128,
    pub ws_idle_timeout_secs: u64,
    pub signal_lock_ms: u64,
    pub shortlist_enabled: bool,
    pub shortlist_max_obligations: usize,
    pub shortlist_refresh_secs: u64,
    pub shortlist_refresh_debounce_ms: u64,
    pub shortlist_cooling_down_ms: u64,
    pub shortlist_candidate_history_limit: usize,
    pub hermes_armed_stale_ms: u64,
    pub hermes_cooling_down_ms: u64,
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KaminoSignalSourceConfig {
    pub primary_rpc_enabled: bool,
    pub secondary_rpc_enabled: bool,
    pub price_feed_enabled: bool,
}

impl HunterTxFetchConfig {
    pub fn from_env(prefix: &str) -> Self {
        let attempts = std::env::var(format!("{prefix}_GET_TX_ATTEMPTS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_GET_TX_ATTEMPTS").ok())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(3);
        let retry_delay_ms = std::env::var(format!("{prefix}_GET_TX_RETRY_DELAY_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_GET_TX_RETRY_DELAY_MS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(40);
        let timeout_ms = std::env::var(format!("{prefix}_GET_TX_TIMEOUT_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_GET_TX_TIMEOUT_MS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(800);

        Self {
            attempts,
            retry_delay_ms,
            timeout_ms,
        }
    }
}

impl HunterRuntimeConfig {
    /// Runtime tuning is intentionally reloaded on every hunter loop restart.
    /// This keeps emergency env tweaks effective without a full process restart.
    pub fn from_env(prefix: &str) -> Self {
        let signal_commitment = match std::env::var(format!("{prefix}_SIGNAL_COMMITMENT"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SIGNAL_COMMITMENT").ok())
            .unwrap_or_else(|| "confirmed".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "processed" => RpcCommitment::Processed,
            _ => RpcCommitment::Confirmed,
        };

        let non_whitelist_cooldown_ms =
            std::env::var(format!("{prefix}_NON_WHITELIST_COOLDOWN_MS"))
                .ok()
                .or_else(|| std::env::var("HUNTER_NON_WHITELIST_COOLDOWN_MS").ok())
                .and_then(|v| v.parse::<u128>().ok())
                .unwrap_or(30_000);

        let ws_idle_timeout_secs = std::env::var(format!("{prefix}_WS_IDLE_TIMEOUT_SECS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_WS_IDLE_TIMEOUT_SECS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(180);

        let signal_lock_ms = std::env::var(format!("{prefix}_SIGNAL_LOCK_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SIGNAL_LOCK_MS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1_500);

        let verbose = std::env::var(format!("{prefix}_VERBOSE"))
            .ok()
            .or_else(|| std::env::var("HUNTER_VERBOSE").ok())
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);

        let shortlist_enabled = std::env::var(format!("{prefix}_SHORTLIST_ENABLED"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SHORTLIST_ENABLED").ok())
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true);

        let shortlist_max_obligations =
            std::env::var(format!("{prefix}_SHORTLIST_MAX_OBLIGATIONS"))
                .ok()
                .or_else(|| std::env::var("HUNTER_SHORTLIST_MAX_OBLIGATIONS").ok())
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| v.clamp(1, 10))
                .unwrap_or(10);

        let shortlist_refresh_secs = std::env::var(format!("{prefix}_SHORTLIST_REFRESH_SECS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SHORTLIST_REFRESH_SECS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20);

        let shortlist_refresh_debounce_ms =
            std::env::var(format!("{prefix}_SHORTLIST_REFRESH_DEBOUNCE_MS"))
                .ok()
                .or_else(|| std::env::var("HUNTER_SHORTLIST_REFRESH_DEBOUNCE_MS").ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1_500);

        let shortlist_cooling_down_ms = std::env::var(format!("{prefix}_SHORTLIST_COOLDOWN_MS"))
            .ok()
            .or_else(|| std::env::var("HUNTER_SHORTLIST_COOLDOWN_MS").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20_000);

        let shortlist_candidate_history_limit =
            std::env::var(format!("{prefix}_SHORTLIST_CANDIDATE_HISTORY_LIMIT"))
                .ok()
                .or_else(|| std::env::var("HUNTER_SHORTLIST_CANDIDATE_HISTORY_LIMIT").ok())
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| v.max(shortlist_max_obligations))
                .unwrap_or(64);

        let hermes_armed_stale_ms = std::env::var("HERMES_ARMED_STALE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(shortlist_refresh_secs.saturating_mul(1_000));

        let hermes_cooling_down_ms = std::env::var("HERMES_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20_000);

        Self {
            signal_commitment,
            tx_fetch: HunterTxFetchConfig::from_env(prefix),
            non_whitelist_cooldown_ms,
            ws_idle_timeout_secs,
            signal_lock_ms,
            shortlist_enabled,
            shortlist_max_obligations,
            shortlist_refresh_secs,
            shortlist_refresh_debounce_ms,
            shortlist_cooling_down_ms,
            shortlist_candidate_history_limit,
            hermes_armed_stale_ms,
            hermes_cooling_down_ms,
            verbose,
        }
    }
}

pub fn read_kamino_signal_source_config(
    has_signal_secondary_rpc: bool,
) -> KaminoSignalSourceConfig {
    let primary_rpc_enabled = env_flag_aliases(
        &[
            "ENABLE_HUNTER_SIGNAL_PRIMARY",
            "ENABLE_HUNTER_SIGNAL_QUICKNODE",
        ],
        true,
    );
    let secondary_rpc_requested = env_flag_aliases(
        &[
            "ENABLE_HUNTER_SIGNAL_SECONDARY",
            "ENABLE_HUNTER_SIGNAL_HELIUS",
        ],
        true,
    );
    let price_feed_enabled = env_flag_aliases(
        &[
            "ENABLE_HUNTER_SIGNAL_PRICE_FEED",
            "ENABLE_HUNTER_SIGNAL_HERMES",
        ],
        false,
    );

    KaminoSignalSourceConfig {
        primary_rpc_enabled,
        secondary_rpc_enabled: secondary_rpc_requested && has_signal_secondary_rpc,
        price_feed_enabled,
    }
}

pub fn env_flag_aliases(names: &[&str], default: bool) -> bool {
    for name in names {
        if std::env::var(name).is_ok() {
            return env_flag(name, default);
        }
    }

    default
}

#[cfg(test)]
mod tests {
    use super::read_kamino_signal_source_config;
    use std::sync::{Mutex, OnceLock};

    fn env_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env mutex poisoned")
    }

    #[test]
    fn source_toggles_are_read_independently() {
        let _guard = env_test_guard();
        unsafe {
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRIMARY", "false");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_SECONDARY", "true");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED", "true");
        }
        let cfg = read_kamino_signal_source_config(true);
        assert!(!cfg.primary_rpc_enabled);
        assert!(cfg.secondary_rpc_enabled);
        assert!(cfg.price_feed_enabled);
        unsafe {
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRIMARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_SECONDARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED");
        }
    }

    #[test]
    fn primary_only_mode_disables_secondary_and_price_feed_effectively() {
        let _guard = env_test_guard();
        unsafe {
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRIMARY", "true");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_SECONDARY", "false");
            std::env::set_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED", "false");
        }
        let cfg = read_kamino_signal_source_config(true);
        assert!(cfg.primary_rpc_enabled);
        assert!(!cfg.secondary_rpc_enabled);
        assert!(!cfg.price_feed_enabled);
        unsafe {
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRIMARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_SECONDARY");
            std::env::remove_var("ENABLE_HUNTER_SIGNAL_PRICE_FEED");
        }
    }
}
