use crate::ports::jito::JitoBundleStatus;
use crate::utils::utc_now;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const DEFAULT_INITIAL_TIP_LAMPORTS: u64 = 1_000_000;
const DEFAULT_MIN_TIP_LAMPORTS: u64 = 100_000;
const DEFAULT_MAX_TIP_LAMPORTS: u64 = 5_000_000;
const DEFAULT_MATCH_WINDOW_MS: u64 = 120_000;
const DEFAULT_BUNDLE_MONITOR_POLL_MS: u64 = 2_000;
const DEFAULT_BUNDLE_MONITOR_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_STATE_PATH: &str = ".jawas/kamino_adaptive_tip_state.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KaminoAdaptiveTipMode {
    Bootstrap,
    Convergence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KaminoAdaptiveTipState {
    pub mode: KaminoAdaptiveTipMode,
    pub current_tip_lamports: u64,
    pub last_winning_tip_lamports: Option<u64>,
    pub last_losing_tip_lamports: Option<u64>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KaminoAdaptiveTipOutcome {
    Win,
    Loss,
}

#[derive(Debug, Clone)]
pub struct KaminoAdaptiveTipConfig {
    pub initial_tip_lamports: u64,
    pub min_tip_lamports: u64,
    pub max_tip_lamports: u64,
    pub match_window_ms: u64,
    pub bundle_monitor_poll_ms: u64,
    pub bundle_monitor_timeout_ms: u64,
    pub state_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct KaminoAdaptiveTipPolicy {
    config: KaminoAdaptiveTipConfig,
    state: Arc<Mutex<KaminoAdaptiveTipState>>,
    current_tip: Arc<AtomicU64>,
    pending_first_shots: Arc<Mutex<HashMap<String, PendingFirstShot>>>,
    pending_bundles: Arc<Mutex<HashMap<String, PendingBundleAttempt>>>,
}

#[derive(Debug, Clone)]
struct PendingFirstShot {
    liquidator: String,
    tip_lamports: u64,
    observed_at_ms: u64,
    bundle_id: Option<String>,
    signal_signature: Option<String>,
    source: String,
}

#[derive(Debug, Clone)]
struct PendingBundleAttempt {
    bundle_id: String,
    signal_signature: String,
    obligation: String,
    liquidator: String,
    tip_lamports: u64,
    source: String,
    observed_at_ms: u64,
    last_status: Option<String>,
    landed_slot: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct KaminoBundleMonitorUpdate {
    pub stage: &'static str,
    pub reason: &'static str,
    pub bundle_id: String,
    pub signal_signature: String,
    pub obligation: String,
    pub liquidator: String,
    pub tip_lamports: u64,
    pub source: String,
    pub bundle_status: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct KaminoAdaptiveTipResolution {
    pub outcome: KaminoAdaptiveTipOutcome,
    pub bundle_id: Option<String>,
    pub signal_signature: Option<String>,
    pub obligation: String,
    pub expected_liquidator: String,
    pub actual_liquidator: String,
    pub attempted_tip_lamports: u64,
    pub source: String,
    pub bundle_status: Option<String>,
}

impl KaminoAdaptiveTipConfig {
    pub fn from_env() -> Self {
        let initial_tip_lamports = std::env::var("KAMINO_FIRST_SHOT_TIP_INITIAL_LAMPORTS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_INITIAL_TIP_LAMPORTS);
        let min_tip_lamports = std::env::var("KAMINO_FIRST_SHOT_TIP_MIN_LAMPORTS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MIN_TIP_LAMPORTS);
        let max_tip_lamports = std::env::var("KAMINO_FIRST_SHOT_TIP_MAX_LAMPORTS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_TIP_LAMPORTS);
        let match_window_ms = std::env::var("KAMINO_FIRST_SHOT_TIP_MATCH_WINDOW_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MATCH_WINDOW_MS);
        let bundle_monitor_poll_ms = std::env::var("KAMINO_BUNDLE_MONITOR_POLL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_BUNDLE_MONITOR_POLL_MS);
        let bundle_monitor_timeout_ms = std::env::var("KAMINO_BUNDLE_MONITOR_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_BUNDLE_MONITOR_TIMEOUT_MS);
        let state_path = std::env::var("KAMINO_FIRST_SHOT_TIP_STATE_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_PATH));

        Self {
            initial_tip_lamports,
            min_tip_lamports,
            max_tip_lamports,
            match_window_ms,
            bundle_monitor_poll_ms,
            bundle_monitor_timeout_ms,
            state_path,
        }
    }

    fn clamp_tip(&self, tip_lamports: u64) -> u64 {
        tip_lamports.clamp(self.min_tip_lamports, self.max_tip_lamports)
    }
}

impl KaminoAdaptiveTipPolicy {
    pub fn from_env() -> Self {
        let config = KaminoAdaptiveTipConfig::from_env();
        let state = load_tip_state(&config).unwrap_or_else(|| KaminoAdaptiveTipState {
            mode: KaminoAdaptiveTipMode::Bootstrap,
            current_tip_lamports: config.clamp_tip(config.initial_tip_lamports),
            last_winning_tip_lamports: None,
            last_losing_tip_lamports: None,
            updated_at: utc_now(),
        });
        let current_tip = Arc::new(AtomicU64::new(state.current_tip_lamports));

        Self {
            config,
            state: Arc::new(Mutex::new(state)),
            current_tip,
            pending_first_shots: Arc::new(Mutex::new(HashMap::new())),
            pending_bundles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn current_tip_lamports(&self) -> u64 {
        self.current_tip.load(Ordering::Relaxed)
    }

    pub fn bundle_monitor_poll_ms(&self) -> u64 {
        self.config.bundle_monitor_poll_ms
    }

    pub fn note_first_shot_bundle_sent(
        &self,
        obligation: impl Into<String>,
        liquidator: impl Into<String>,
        tip_lamports: u64,
        bundle_id: impl Into<String>,
        signal_signature: impl Into<String>,
        source: impl Into<String>,
        observed_at_ms: u64,
    ) {
        let obligation = obligation.into();
        let liquidator = liquidator.into();
        let bundle_id = bundle_id.into();
        let signal_signature = signal_signature.into();
        let source = source.into();

        let mut pending = self.pending_first_shots.lock().expect("pending tip state poisoned");
        pending.retain(|_, entry| {
            observed_at_ms.saturating_sub(entry.observed_at_ms) <= self.config.match_window_ms
        });
        pending.insert(
            obligation.clone(),
            PendingFirstShot {
                liquidator: liquidator.clone(),
                tip_lamports: self.config.clamp_tip(tip_lamports),
                observed_at_ms,
                bundle_id: Some(bundle_id.clone()),
                signal_signature: Some(signal_signature.clone()),
                source: source.clone(),
            },
        );

        let mut pending_bundles = self
            .pending_bundles
            .lock()
            .expect("pending bundle state poisoned");
        pending_bundles.retain(|_, entry| {
            observed_at_ms.saturating_sub(entry.observed_at_ms)
                <= self.config.bundle_monitor_timeout_ms
        });
        pending_bundles.insert(
            bundle_id.clone(),
            PendingBundleAttempt {
                bundle_id,
                signal_signature,
                obligation,
                liquidator,
                tip_lamports: self.config.clamp_tip(tip_lamports),
                source,
                observed_at_ms,
                last_status: None,
                landed_slot: None,
            },
        );
    }

    pub fn pending_bundle_ids(&self, observed_at_ms: u64) -> Vec<String> {
        let mut pending_bundles = self
            .pending_bundles
            .lock()
            .expect("pending bundle state poisoned");
        pending_bundles.retain(|_, entry| {
            observed_at_ms.saturating_sub(entry.observed_at_ms)
                <= self.config.bundle_monitor_timeout_ms
        });
        pending_bundles.keys().cloned().collect()
    }

    pub fn note_bundle_status(
        &self,
        bundle_id: &str,
        status: &JitoBundleStatus,
        observed_at_ms: u64,
    ) -> Option<KaminoBundleMonitorUpdate> {
        let mut pending_bundles = self
            .pending_bundles
            .lock()
            .expect("pending bundle state poisoned");
        let entry = pending_bundles.get_mut(bundle_id)?;
        let normalized_status = status.status.to_ascii_lowercase();
        let landed_slot = status.landed_slot;
        if entry.last_status.as_deref() == Some(normalized_status.as_str())
            && entry.landed_slot == landed_slot
        {
            return None;
        }

        entry.last_status = Some(normalized_status.clone());
        entry.landed_slot = landed_slot;

        let (stage, reason, terminal) = match normalized_status.as_str() {
            "landed" => ("bundle_landed", "jito_bundle_landed", true),
            "pending" => ("bundle_pending_status", "jito_bundle_pending", false),
            "failed" | "invalid" | "dropped" | "rejected" => {
                ("bundle_terminal_status", "jito_bundle_terminal", true)
            }
            _ => ("bundle_status_update", "jito_bundle_status_update", false),
        };

        let update = KaminoBundleMonitorUpdate {
            stage,
            reason,
            bundle_id: entry.bundle_id.clone(),
            signal_signature: entry.signal_signature.clone(),
            obligation: entry.obligation.clone(),
            liquidator: entry.liquidator.clone(),
            tip_lamports: entry.tip_lamports,
            source: entry.source.clone(),
            bundle_status: Some(status.status.clone()),
            detail: format!(
                "status={} landed_slot={:?} age_ms={}",
                status.status,
                status.landed_slot,
                observed_at_ms.saturating_sub(entry.observed_at_ms)
            ),
        };

        if terminal {
            pending_bundles.remove(bundle_id);
        }

        Some(update)
    }

    pub fn drain_bundle_timeouts(&self, observed_at_ms: u64) -> Vec<KaminoBundleMonitorUpdate> {
        let mut pending_bundles = self
            .pending_bundles
            .lock()
            .expect("pending bundle state poisoned");
        let expired_ids = pending_bundles
            .iter()
            .filter_map(|(bundle_id, entry)| {
                let age_ms = observed_at_ms.saturating_sub(entry.observed_at_ms);
                (age_ms > self.config.bundle_monitor_timeout_ms).then_some(bundle_id.clone())
            })
            .collect::<Vec<_>>();

        let mut updates = Vec::new();
        for bundle_id in expired_ids {
            if let Some(entry) = pending_bundles.remove(&bundle_id) {
                let last_status = entry.last_status.clone();
                updates.push(KaminoBundleMonitorUpdate {
                    stage: "bundle_monitor_timeout",
                    reason: "jito_bundle_timeout",
                    bundle_id: entry.bundle_id,
                    signal_signature: entry.signal_signature,
                    obligation: entry.obligation,
                    liquidator: entry.liquidator,
                    tip_lamports: entry.tip_lamports,
                    source: entry.source,
                    bundle_status: last_status.clone(),
                    detail: format!(
                        "status={} age_ms={} timeout_ms={}",
                        last_status.unwrap_or_else(|| "unknown".to_string()),
                        observed_at_ms.saturating_sub(entry.observed_at_ms),
                        self.config.bundle_monitor_timeout_ms
                    ),
                });
            }
        }
        updates
    }

    pub fn note_liquidation_result(
        &self,
        obligation: &str,
        actual_liquidator: &str,
        observed_at_ms: u64,
    ) -> Option<KaminoAdaptiveTipResolution> {
        let pending_attempt = {
            let mut pending = self.pending_first_shots.lock().expect("pending tip state poisoned");
            pending.retain(|_, entry| {
                observed_at_ms.saturating_sub(entry.observed_at_ms) <= self.config.match_window_ms
            });
            pending.remove(obligation)?
        };

        let outcome = if pending_attempt.liquidator == actual_liquidator {
            KaminoAdaptiveTipOutcome::Win
        } else {
            KaminoAdaptiveTipOutcome::Loss
        };
        let bundle_status = pending_attempt.bundle_id.as_ref().and_then(|bundle_id| {
            self.pending_bundles
                .lock()
                .expect("pending bundle state poisoned")
                .get(bundle_id)
                .and_then(|entry| entry.last_status.clone())
        });
        if let Some(bundle_id) = pending_attempt.bundle_id.as_ref() {
            self.pending_bundles
                .lock()
                .expect("pending bundle state poisoned")
                .remove(bundle_id);
        }
        self.apply_outcome(pending_attempt.tip_lamports, outcome);
        Some(KaminoAdaptiveTipResolution {
            outcome,
            bundle_id: pending_attempt.bundle_id,
            signal_signature: pending_attempt.signal_signature,
            obligation: obligation.to_string(),
            expected_liquidator: pending_attempt.liquidator,
            actual_liquidator: actual_liquidator.to_string(),
            attempted_tip_lamports: pending_attempt.tip_lamports,
            source: pending_attempt.source,
            bundle_status,
        })
    }

    pub fn state_snapshot(&self) -> KaminoAdaptiveTipState {
        self.state.lock().expect("tip state poisoned").clone()
    }

    fn apply_outcome(&self, attempted_tip_lamports: u64, outcome: KaminoAdaptiveTipOutcome) {
        let mut state = self.state.lock().expect("tip state poisoned");
        match outcome {
            KaminoAdaptiveTipOutcome::Win => {
                state.mode = KaminoAdaptiveTipMode::Convergence;
                state.last_winning_tip_lamports = Some(attempted_tip_lamports);
                let lower_bound =
                    state
                        .last_losing_tip_lamports
                        .unwrap_or(self.config.min_tip_lamports);
                state.current_tip_lamports = midpoint(
                    lower_bound.min(attempted_tip_lamports),
                    attempted_tip_lamports,
                    self.config.min_tip_lamports,
                    self.config.max_tip_lamports,
                );
            }
            KaminoAdaptiveTipOutcome::Loss => {
                state.last_losing_tip_lamports = Some(attempted_tip_lamports);
                state.current_tip_lamports =
                    if let Some(last_winning_tip_lamports) = state.last_winning_tip_lamports {
                        state.mode = KaminoAdaptiveTipMode::Convergence;
                        midpoint(
                            attempted_tip_lamports,
                            last_winning_tip_lamports.max(attempted_tip_lamports),
                            self.config.min_tip_lamports,
                            self.config.max_tip_lamports,
                        )
                    } else {
                        state.mode = KaminoAdaptiveTipMode::Bootstrap;
                        self.config
                            .clamp_tip(((attempted_tip_lamports as f64) * 1.5).round() as u64)
                    };
            }
        }
        state.updated_at = utc_now();
        self.current_tip
            .store(state.current_tip_lamports, Ordering::Relaxed);
        let _ = persist_tip_state(&self.config.state_path, &state);
    }
}

fn load_tip_state(config: &KaminoAdaptiveTipConfig) -> Option<KaminoAdaptiveTipState> {
    let raw = fs::read_to_string(&config.state_path).ok()?;
    let mut state = serde_json::from_str::<KaminoAdaptiveTipState>(&raw).ok()?;
    state.current_tip_lamports = config.clamp_tip(state.current_tip_lamports);
    state.last_winning_tip_lamports = state
        .last_winning_tip_lamports
        .map(|tip_lamports| config.clamp_tip(tip_lamports));
    state.last_losing_tip_lamports = state
        .last_losing_tip_lamports
        .map(|tip_lamports| config.clamp_tip(tip_lamports));
    Some(state)
}

fn persist_tip_state(path: &Path, state: &KaminoAdaptiveTipState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

fn midpoint(lower: u64, upper: u64, min_tip_lamports: u64, max_tip_lamports: u64) -> u64 {
    let midpoint = lower.saturating_add(upper.saturating_sub(lower) / 2);
    midpoint.clamp(min_tip_lamports, max_tip_lamports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_state_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("jawas-tip-state-{nanos}.json"))
    }

    fn make_policy(path: PathBuf) -> KaminoAdaptiveTipPolicy {
        std::env::set_var("KAMINO_FIRST_SHOT_TIP_STATE_PATH", path);
        std::env::set_var("KAMINO_FIRST_SHOT_TIP_INITIAL_LAMPORTS", "200000");
        std::env::set_var("KAMINO_FIRST_SHOT_TIP_MIN_LAMPORTS", "100000");
        std::env::set_var("KAMINO_FIRST_SHOT_TIP_MAX_LAMPORTS", "500000");
        KaminoAdaptiveTipPolicy::from_env()
    }

    #[test]
    fn loss_before_first_win_increases_tip_aggressively() {
        let path = unique_state_path();
        let policy = make_policy(path.clone());
        policy.note_first_shot_bundle_sent(
            "obl",
            "wallet",
            200_000,
            "bundle-1",
            "sig-1",
            "price_feed",
            10,
        );
        assert_eq!(
            policy
                .note_liquidation_result("obl", "other-wallet", 20)
                .map(|resolution| resolution.outcome),
            Some(KaminoAdaptiveTipOutcome::Loss)
        );
        assert_eq!(policy.current_tip_lamports(), 300_000);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn first_win_moves_to_midpoint_toward_lower_bound() {
        let path = unique_state_path();
        let policy = make_policy(path.clone());
        policy.note_first_shot_bundle_sent(
            "obl",
            "wallet",
            200_000,
            "bundle-1",
            "sig-1",
            "price_feed",
            10,
        );
        let _ = policy.note_liquidation_result("obl", "other-wallet", 20);
        policy.note_first_shot_bundle_sent(
            "obl",
            "wallet",
            300_000,
            "bundle-2",
            "sig-2",
            "price_feed",
            30,
        );
        assert_eq!(
            policy
                .note_liquidation_result("obl", "wallet", 40)
                .map(|resolution| resolution.outcome),
            Some(KaminoAdaptiveTipOutcome::Win)
        );
        assert_eq!(policy.current_tip_lamports(), 250_000);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn persisted_state_is_reloaded() {
        let path = unique_state_path();
        let policy = make_policy(path.clone());
        policy.note_first_shot_bundle_sent(
            "obl",
            "wallet",
            200_000,
            "bundle-1",
            "sig-1",
            "price_feed",
            10,
        );
        let _ = policy.note_liquidation_result("obl", "other-wallet", 20);
        let reloaded = make_policy(path.clone());
        assert_eq!(reloaded.current_tip_lamports(), 300_000);
        let _ = fs::remove_file(path);
    }
}
