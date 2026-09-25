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

    /// [`reserve_pause_until`] for this budget.
    pub fn pause_until(&self, now_epoch: u64) -> Option<u64> {
        reserve_pause_until(self.remaining, self.reset_epoch(), now_epoch)
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

/// How long to wait after GitHub refused a request as rate limited, in
/// seconds, in GitHub's documented order: `retry-after` when it was sent,
/// otherwise the budget's reset, otherwise the floor — clamped by
/// [`backoff_secs`]. Retrying a secondary limit before `retry-after` is up can
/// make GitHub extend the block; waiting for the primary reset instead would
/// stall the board for up to fifteen minutes with budget left.
pub fn rate_limited_wait_secs(
    reset_epoch: Option<u64>,
    retry_after_secs: Option<u64>,
    now_epoch: u64,
) -> u64 {
    let until = match retry_after_secs {
        Some(secs) => Some(now_epoch.saturating_add(secs)),
        None => reset_epoch,
    };
    backoff_secs(until, now_epoch)
}

/// True when the next refresh should be skipped to preserve the reserve.
pub fn should_back_off(rate: &RateLimitInfo) -> bool {
    rate.remaining < RATE_LIMIT_RESERVE
}

/// Whether the last known budget says to stop fetching, and until when.
///
/// `Some(epoch second)` when fewer than [`RATE_LIMIT_RESERVE`] points remain
/// and the budget has not reset yet: pause until then, clamped by
/// [`backoff_secs`] to between one and fifteen minutes from now. `None` when
/// there is budget to spend, or when the reset is unknown or already past —
/// the stale number no longer describes the budget, so the next fetch goes
/// ahead and brings a fresh one. The desktop and the iPad both call this.
pub fn reserve_pause_until(
    remaining: u32,
    reset_epoch: Option<u64>,
    now_epoch: u64,
) -> Option<u64> {
    (remaining < RATE_LIMIT_RESERVE && reset_epoch.is_some_and(|reset| now_epoch < reset))
        .then(|| now_epoch.saturating_add(backoff_secs(reset_epoch, now_epoch)))
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
    fn the_reserve_pauses_only_until_a_reset_still_ahead() {
        let now = 1_000_000;
        let low = RATE_LIMIT_RESERVE - 1;
        // Enough budget: never pause, whatever the reset says.
        assert_eq!(
            reserve_pause_until(RATE_LIMIT_RESERVE, Some(now + 300), now),
            None
        );
        assert_eq!(reserve_pause_until(5000, Some(now + 300), now), None);
        // Low budget, reset ahead: wait for it, one to fifteen minutes.
        assert_eq!(
            reserve_pause_until(low, Some(now + 300), now),
            Some(now + 300)
        );
        assert_eq!(reserve_pause_until(0, Some(now + 1), now), Some(now + 60));
        assert_eq!(reserve_pause_until(0, Some(now + 59), now), Some(now + 60));
        assert_eq!(
            reserve_pause_until(0, Some(now + 900), now),
            Some(now + 900)
        );
        assert_eq!(
            reserve_pause_until(0, Some(now + 901), now),
            Some(now + 900)
        );
        assert_eq!(reserve_pause_until(0, Some(u64::MAX), now), Some(now + 900));
        // Low budget, but the number is stale or unknown: fetch a fresh one.
        assert_eq!(reserve_pause_until(0, Some(now), now), None);
        assert_eq!(reserve_pause_until(0, Some(now - 1), now), None);
        assert_eq!(reserve_pause_until(0, None, now), None);
        // A clock at the end of time cannot overflow.
        assert_eq!(
            reserve_pause_until(0, Some(u64::MAX), u64::MAX - 1),
            Some(u64::MAX)
        );
    }

    #[test]
    fn a_refused_request_waits_for_the_later_of_reset_and_retry_after() {
        let now = 1_000_000;
        // A secondary limit: retry-after and no reset.
        assert_eq!(rate_limited_wait_secs(None, Some(300), now), 300);
        // The primary budget, spent: its reset, clamped.
        assert_eq!(rate_limited_wait_secs(Some(now + 600), None, now), 600);
        assert_eq!(rate_limited_wait_secs(Some(now + 90_000), None, now), 900);
        // Both: retry-after wins, in GitHub's documented order.
        assert_eq!(rate_limited_wait_secs(Some(now + 120), Some(300), now), 300);
        assert_eq!(rate_limited_wait_secs(Some(now + 2_700), Some(60), now), 60);
        // Neither, or a tiny one: the one-minute floor.
        assert_eq!(rate_limited_wait_secs(None, None, now), 60);
        assert_eq!(rate_limited_wait_secs(None, Some(5), now), 60);
        // Absurd values cannot overflow and still cap at fifteen minutes.
        assert_eq!(rate_limited_wait_secs(None, Some(u64::MAX), now), 900);
    }

    #[test]
    fn a_budget_pauses_by_the_same_rule() {
        let rate = RateLimitInfo {
            limit: 5000,
            cost: 3,
            remaining: 10,
            reset_at: "2026-07-24T18:00:00Z".into(),
        };
        let reset = rate.reset_epoch().unwrap();
        assert_eq!(rate.pause_until(reset - 120), Some(reset));
        assert_eq!(rate.pause_until(reset), None);
        let unparsable = RateLimitInfo {
            reset_at: "soon".into(),
            ..rate
        };
        assert_eq!(unparsable.pause_until(reset - 120), None);
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
