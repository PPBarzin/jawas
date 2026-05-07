use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortlistState {
    Warm,
    Armed,
    CoolingDown,
    Dropped,
}

impl ShortlistState {
    pub fn as_str(self) -> &'static str {
        match self {
            ShortlistState::Warm => "warm",
            ShortlistState::Armed => "armed",
            ShortlistState::CoolingDown => "cooling_down",
            ShortlistState::Dropped => "dropped",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedExecutionContext {
    pub obligation_pubkey: String,
    pub repay_mint: String,
    pub repay_symbol: String,
    pub wallet_eligible: bool,
    pub repay_reserve: String,
    pub withdraw_reserve: String,
    pub withdraw_mint: String,
    pub inclusion_reason: String,
}

#[derive(Debug, Clone)]
pub struct ShortlistCandidate {
    pub context: PreparedExecutionContext,
    pub observed_count: u32,
    pub last_observed_at_ms: u64,
    pub last_refreshed_at_ms: Option<u64>,
    pub distance_to_liq: Option<f64>,
    pub cooldown_until_ms: Option<u64>,
    pub last_refresh_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShortlistEntry {
    pub context: PreparedExecutionContext,
    pub state: ShortlistState,
    pub observed_count: u32,
    pub distance_to_liq: f64,
    pub last_observed_at_ms: u64,
    pub last_refreshed_at_ms: u64,
    pub shortlist_age_ms: u64,
    pub refresh_reason: String,
}

impl ShortlistCandidate {
    pub fn new(context: PreparedExecutionContext, observed_at_ms: u64) -> Self {
        Self {
            context,
            observed_count: 1,
            last_observed_at_ms: observed_at_ms,
            last_refreshed_at_ms: None,
            distance_to_liq: None,
            cooldown_until_ms: None,
            last_refresh_reason: None,
        }
    }

    pub fn record_observation(&mut self, observed_at_ms: u64) {
        self.observed_count = self.observed_count.saturating_add(1);
        self.last_observed_at_ms = observed_at_ms;
    }

    pub fn update_refresh(
        &mut self,
        distance_to_liq: f64,
        refreshed_at_ms: u64,
        refresh_reason: &str,
    ) {
        self.distance_to_liq = Some(distance_to_liq);
        self.last_refreshed_at_ms = Some(refreshed_at_ms);
        self.last_refresh_reason = Some(refresh_reason.to_string());
    }

    pub fn cooldown(&mut self, cooldown_until_ms: u64) {
        self.cooldown_until_ms = Some(cooldown_until_ms);
    }

    pub fn clear_cooldown_if_expired(&mut self, now_ms: u64) {
        if self.cooldown_until_ms.is_some_and(|until| until <= now_ms) {
            self.cooldown_until_ms = None;
        }
    }
}

pub fn enforce_candidate_history_limit(
    candidates: &mut HashMap<String, ShortlistCandidate>,
    max_candidates: usize,
) {
    if candidates.len() <= max_candidates {
        return;
    }

    let mut ordered = candidates
        .iter()
        .map(|(obligation, candidate)| (obligation.clone(), candidate.last_observed_at_ms))
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| b.1.cmp(&a.1));

    let keep = ordered
        .into_iter()
        .take(max_candidates)
        .map(|(obligation, _)| obligation)
        .collect::<std::collections::HashSet<_>>();

    candidates.retain(|obligation, _| keep.contains(obligation));
}

pub fn select_shortlist(
    candidates: &HashMap<String, ShortlistCandidate>,
    max_entries: usize,
    now_ms: u64,
) -> HashMap<String, ShortlistEntry> {
    let mut eligible = candidates
        .values()
        .filter_map(|candidate| {
            let distance_to_liq = candidate.distance_to_liq?;
            let last_refreshed_at_ms = candidate.last_refreshed_at_ms?;

            if !candidate.context.wallet_eligible {
                return None;
            }

            let state = if candidate
                .cooldown_until_ms
                .is_some_and(|cooldown_until_ms| cooldown_until_ms > now_ms)
            {
                ShortlistState::CoolingDown
            } else {
                ShortlistState::Armed
            };

            Some(ShortlistEntry {
                context: candidate.context.clone(),
                state,
                observed_count: candidate.observed_count,
                distance_to_liq,
                last_observed_at_ms: candidate.last_observed_at_ms,
                last_refreshed_at_ms,
                shortlist_age_ms: now_ms.saturating_sub(last_refreshed_at_ms),
                refresh_reason: candidate
                    .last_refresh_reason
                    .clone()
                    .unwrap_or_else(|| "unspecified".to_string()),
            })
        })
        .filter(|entry| entry.state != ShortlistState::CoolingDown)
        .collect::<Vec<_>>();

    eligible.sort_by(|a, b| {
        a.distance_to_liq
            .partial_cmp(&b.distance_to_liq)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.observed_count.cmp(&a.observed_count))
            .then_with(|| b.last_observed_at_ms.cmp(&a.last_observed_at_ms))
            .then_with(|| a.context.obligation_pubkey.cmp(&b.context.obligation_pubkey))
    });

    eligible
        .into_iter()
        .take(max_entries)
        .map(|entry| (entry.context.obligation_pubkey.clone(), entry))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        PreparedExecutionContext, ShortlistCandidate, ShortlistState,
        enforce_candidate_history_limit, select_shortlist,
    };
    use std::collections::HashMap;

    fn context(obligation: &str, repay_mint: &str, wallet_eligible: bool) -> PreparedExecutionContext {
        PreparedExecutionContext {
            obligation_pubkey: obligation.to_string(),
            repay_mint: repay_mint.to_string(),
            repay_symbol: "USDC".to_string(),
            wallet_eligible,
            repay_reserve: "repay-reserve".to_string(),
            withdraw_reserve: "withdraw-reserve".to_string(),
            withdraw_mint: "withdraw-mint".to_string(),
            inclusion_reason: "observed_liquidation".to_string(),
        }
    }

    #[test]
    fn shortlist_keeps_only_wallet_eligible_candidates() {
        let mut candidates = HashMap::new();
        let mut allowed = ShortlistCandidate::new(context("allowed", "mint-a", true), 100);
        allowed.update_refresh(0.01, 110, "safety");
        candidates.insert("allowed".to_string(), allowed);

        let mut blocked = ShortlistCandidate::new(context("blocked", "mint-b", false), 100);
        blocked.update_refresh(0.001, 110, "safety");
        candidates.insert("blocked".to_string(), blocked);

        let shortlist = select_shortlist(&candidates, 10, 120);

        assert_eq!(shortlist.len(), 1);
        assert!(shortlist.contains_key("allowed"));
    }

    #[test]
    fn shortlist_respects_max_entries_and_distance_order() {
        let mut candidates = HashMap::new();
        for (obligation, distance) in [("one", 0.03), ("two", 0.02), ("three", 0.01)] {
            let mut candidate = ShortlistCandidate::new(context(obligation, obligation, true), 100);
            candidate.update_refresh(distance, 110, "safety");
            candidates.insert(obligation.to_string(), candidate);
        }

        let shortlist = select_shortlist(&candidates, 2, 120);
        assert_eq!(shortlist.len(), 2);
        assert!(shortlist.contains_key("two"));
        assert!(shortlist.contains_key("three"));
        assert!(!shortlist.contains_key("one"));
    }

    #[test]
    fn cooling_down_candidates_are_excluded_from_active_shortlist() {
        let mut candidates = HashMap::new();
        let mut candidate = ShortlistCandidate::new(context("cooling", "mint", true), 100);
        candidate.update_refresh(0.01, 110, "liquidation");
        candidate.cooldown(200);
        candidates.insert("cooling".to_string(), candidate);

        let shortlist = select_shortlist(&candidates, 10, 150);
        assert!(shortlist.is_empty());
    }

    #[test]
    fn enforce_candidate_history_limit_keeps_most_recent_candidates() {
        let mut candidates = HashMap::new();
        for (obligation, observed_at_ms) in [("old", 10), ("mid", 20), ("new", 30)] {
            candidates.insert(
                obligation.to_string(),
                ShortlistCandidate::new(context(obligation, obligation, true), observed_at_ms),
            );
        }

        enforce_candidate_history_limit(&mut candidates, 2);

        assert_eq!(candidates.len(), 2);
        assert!(candidates.contains_key("mid"));
        assert!(candidates.contains_key("new"));
        assert!(!candidates.contains_key("old"));
    }

    #[test]
    fn shortlist_entries_are_marked_armed_when_active() {
        let mut candidates = HashMap::new();
        let mut candidate = ShortlistCandidate::new(context("armed", "mint", true), 100);
        candidate.update_refresh(0.01, 110, "safety");
        candidates.insert("armed".to_string(), candidate);

        let shortlist = select_shortlist(&candidates, 10, 120);
        assert_eq!(
            shortlist.get("armed").map(|entry| entry.state),
            Some(ShortlistState::Armed)
        );
    }
}
