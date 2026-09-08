//! App state entity: the board data plus refresh/sync/rate-limit status.
//! All GitHub work happens on the background executor; results hop back to
//! the UI thread via `this.update`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local};
use gpui::Context;
use prboard_core::board::{fetch_board, BoardConfig, BoardRow, Mode};
use prboard_core::github::rate_limit::{backoff_secs, should_back_off, RateLimitInfo};
use prboard_core::github::{GhError, GithubTransport};

pub struct AppState {
    pub repo: String,
    pub me: String,
    pub mode: Mode,
    pub config: BoardConfig,
    pub transport: Arc<dyn GithubTransport>,
    pub rows: Vec<BoardRow>,
    pub last_synced: Option<DateTime<Local>>,
    pub syncing: bool,
    pub error: Option<String>,
    pub rate: Option<RateLimitInfo>,
    pub truncated: bool,
    /// Bumped on every successful fetch; observers use it to detect new rows
    /// without diffing (and to gate their reactions — the PRFlow observer-loop
    /// lesson).
    pub generation: u64,
    /// Do not fetch before this epoch second (set after a rate-limit hit,
    /// always clamped 60..900s ahead).
    backoff_until: Option<u64>,
    /// Bumped on every repo/mode switch; an in-flight fetch from a previous
    /// epoch must not write its (now wrong-view) results.
    epoch: u64,
    /// Last-seen rows per queue, so switching restores the previous view
    /// instantly and refreshes in the background instead of flashing empty
    /// (critique #1). Bounded by construction — one entry per `Mode` — but the
    /// cap is explicit per the PRFlow bounded-everything guardrail.
    cache: HashMap<Mode, CachedQueue>,
}

/// A queue's last-known rows and when they were fetched. Rate-limit budget is
/// deliberately NOT cached: it is account-global, not per-queue, so restoring a
/// stale budget could wrongly trip the back-off on a switch.
struct CachedQueue {
    rows: Vec<BoardRow>,
    last_synced: Option<DateTime<Local>>,
    truncated: bool,
}

/// One cache entry per queue; there are exactly two queues today. Kept explicit
/// so the bound survives a third view (roadmap "All open PRs").
const MAX_CACHED_QUEUES: usize = 2;

impl AppState {
    pub fn new(
        repo: String,
        me: String,
        mode: Mode,
        config: BoardConfig,
        transport: Arc<dyn GithubTransport>,
    ) -> Self {
        Self {
            repo,
            me,
            mode,
            config,
            transport,
            rows: Vec::new(),
            last_synced: None,
            syncing: false,
            error: None,
            rate: None,
            truncated: false,
            generation: 0,
            backoff_until: None,
            epoch: 0,
            cache: HashMap::new(),
        }
    }

    /// Snapshot the active queue's rows before we leave it, so returning to it
    /// restores instantly. Evicts the oldest-arbitrary entry if somehow over
    /// the (currently unreachable) bound.
    fn stash_current(&mut self) {
        if self.cache.len() >= MAX_CACHED_QUEUES && !self.cache.contains_key(&self.mode) {
            if let Some(&victim) = self.cache.keys().find(|k| **k != self.mode) {
                self.cache.remove(&victim);
            }
        }
        self.cache.insert(
            self.mode,
            CachedQueue {
                rows: self.rows.clone(),
                last_synced: self.last_synced,
                truncated: self.truncated,
            },
        );
    }

    /// Repo-picker switch: drop the old repo's rows immediately (stale rows
    /// from another repo are worse than an empty table) and fetch the new one.
    /// Every cached queue belonged to the old repo, so the cache is dropped.
    pub fn switch_repo(&mut self, repo: String, cx: &mut Context<Self>) {
        if repo == self.repo {
            return;
        }
        self.repo = repo;
        self.cache.clear();
        self.reset_and_refetch(cx);
    }

    /// Select one of the two prototype dashboards. The previous queue's rows
    /// are stashed and the target queue is restored from cache immediately (if
    /// seen before) so the table never flashes empty; a background refresh then
    /// updates it in place (critique #1).
    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.stash_current();
        self.mode = mode;
        self.epoch += 1; // any in-flight fetch is now for the wrong view
        self.syncing = false; // don't let it dedup the refresh we start now
        self.error = None;
        match self.cache.get(&mode) {
            Some(cached) => {
                self.rows = cached.rows.clone();
                self.last_synced = cached.last_synced;
                self.truncated = cached.truncated;
            }
            None => {
                self.rows.clear();
                self.last_synced = None;
                self.truncated = false;
            }
        }
        self.generation += 1; // observers push the restored (or empty) rows
        self.refresh(cx); // background-update the queue we just switched to
        cx.notify();
    }

    fn reset_and_refetch(&mut self, cx: &mut Context<Self>) {
        self.rows.clear();
        self.last_synced = None;
        self.error = None;
        self.truncated = false;
        self.generation += 1; // observers push the (empty) rows to the table
        self.epoch += 1; // any in-flight fetch is now for the wrong view
        self.syncing = false; // don't let it dedup the fetch we start now
        self.refresh(cx);
        cx.notify();
    }

    /// Seconds until the next allowed fetch while backing off after a
    /// rate-limit hit, or `None` if a fetch could run now. Lets the UI show a
    /// "paused — retrying in Xm" state instead of a permanent "Loading…" when a
    /// switch to an unseen queue lands inside a back-off window.
    pub fn backoff_remaining(&self) -> Option<u64> {
        let now = Local::now().timestamp().max(0) as u64;
        self.backoff_until.filter(|&u| u > now).map(|u| u - now)
    }

    /// Kick off one fetch unless one is in flight or we are backing off.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        let now = Local::now().timestamp().max(0) as u64;
        if let Some(until) = self.backoff_until {
            if now < until {
                return; // quietly wait out the backoff window
            }
            self.backoff_until = None;
        }
        // Preserve the reserve for the user's own gh/git usage.
        if let Some(rate) = &self.rate {
            if should_back_off(rate) {
                let reset = rate.reset_epoch();
                if reset.is_some_and(|r| now < r) {
                    self.backoff_until = Some(now + backoff_secs(reset, now));
                    self.error = Some(format!(
                        "GitHub budget low ({} left) — pausing refresh",
                        rate.remaining
                    ));
                    cx.notify();
                    return;
                }
            }
        }

        self.syncing = true;
        self.error = None;
        cx.notify();

        let transport = self.transport.clone();
        let repo = self.repo.clone();
        let me = self.me.clone();
        let mode = self.mode;
        let config = self.config.clone();
        let epoch = self.epoch;

        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    // The `gh` subprocess blocks; that is fine on the
                    // background pool for a call made every few minutes.
                    fetch_board(transport.as_ref(), mode, &repo, &me, &config)
                })
                .await;

            let _ = this.update(cx, |state, cx| {
                if state.epoch != epoch {
                    return; // view switched mid-fetch; a newer fetch owns state
                }
                state.syncing = false;
                match fetched {
                    Ok(board) => {
                        state.rows = board.rows;
                        state.rate = board.rate;
                        state.truncated = board.truncated;
                        state.last_synced = Some(Local::now());
                        state.generation += 1;
                    }
                    Err(GhError::RateLimited { reset_epoch }) => {
                        let now = Local::now().timestamp().max(0) as u64;
                        let wait = backoff_secs(reset_epoch, now);
                        state.backoff_until = Some(now + wait);
                        state.error = Some(format!(
                            "GitHub rate limited — retrying in {}m",
                            wait.div_ceil(60)
                        ));
                    }
                    Err(e) => {
                        state.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// "just now" / "3m ago" / "2h 15m ago" — static text, recomputed on notify.
pub fn relative(since: DateTime<Local>) -> String {
    let secs = (Local::now() - since).num_seconds().max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60),
    }
}

/// Refresh interval: `PRBOARD_REFRESH_SECS` > config file > default, always
/// clamped to the hard floor.
pub fn refresh_interval(config_secs: Option<u64>) -> Duration {
    use prboard_core::github::rate_limit::{DEFAULT_REFRESH_SECS, MIN_REFRESH_SECS};
    let secs = std::env::var("PRBOARD_REFRESH_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .or(config_secs)
        .unwrap_or(DEFAULT_REFRESH_SECS)
        .max(MIN_REFRESH_SECS);
    Duration::from_secs(secs)
}
