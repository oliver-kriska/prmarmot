//! Rate-limit accounting. GraphQL budget: 5,000 points/hr; our query costs
//! ~3 points per repo refresh. The dashboard must never starve the user's own
//! `gh`/git usage of the shared budget.

use serde::Deserialize;

/// The `rateLimit{}` field of a GraphQL response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitInfo {
    pub limit: u32,
    pub cost: u32,
    pub remaining: u32,
    /// ISO-8601, e.g. "2026-07-24T18:00:00Z".
    pub reset_at: String,
}

impl RateLimitInfo {
    pub fn reset_epoch(&self) -> Option<u64> {
        chrono::DateTime::parse_from_rfc3339(&self.reset_at)
            .ok()
            .map(|t| t.timestamp().max(0) as u64)
    }
}

/// Skip refreshes when fewer points than this remain — the rest of the hourly
/// budget belongs to the user's own tooling on the same token.
pub const RATE_LIMIT_RESERVE: u32 = 50;

/// Auto-refresh floor (seconds). The "5 s" idea is wrong as a floor — see the
/// data-layer research doc §3.4 for the math. Default interval is 300 s.
pub const MIN_REFRESH_SECS: u64 = 30;
pub const DEFAULT_REFRESH_SECS: u64 = 300;

/// How long to pause after hitting the limit. Clamped so we always retry
/// within 15 min and never spin faster than once a minute — a far-future or
/// garbage reset time must not freeze the refresh loop (PRFlow lesson).
pub fn backoff_secs(reset_epoch: Option<u64>, now_epoch: u64) -> u64 {
    reset_epoch
        .map(|reset| reset.saturating_sub(now_epoch))
        .unwrap_or(60)
        .clamp(60, 900)
}

/// True when the next refresh should be skipped to preserve the reserve.
pub fn should_back_off(rate: &RateLimitInfo) -> bool {
    rate.remaining < RATE_LIMIT_RESERVE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_clamped() {
        assert_eq!(backoff_secs(Some(1000), 990), 60); // 10s away → floor 60
        assert_eq!(backoff_secs(Some(1300), 1000), 300); // 5 min away → as-is
        assert_eq!(backoff_secs(Some(1_000_000), 1000), 900); // far future → cap 900
        assert_eq!(backoff_secs(Some(500), 1000), 60); // already past → floor
        assert_eq!(backoff_secs(None, 1000), 60); // unknown reset → floor
    }

    #[test]
    fn reserve_threshold() {
        let mk = |remaining| RateLimitInfo {
            limit: 5000,
            cost: 3,
            remaining,
            reset_at: "2026-07-24T18:00:00Z".into(),
        };
        assert!(!should_back_off(&mk(5000)));
        assert!(!should_back_off(&mk(RATE_LIMIT_RESERVE)));
        assert!(should_back_off(&mk(RATE_LIMIT_RESERVE - 1)));
    }

    #[test]
    fn reset_epoch_parses_iso() {
        let r = RateLimitInfo {
            limit: 5000,
            cost: 3,
            remaining: 4997,
            reset_at: "2026-07-24T18:00:00Z".into(),
        };
        assert!(r.reset_epoch().is_some());
    }
}
