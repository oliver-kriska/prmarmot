//! `watch`: poll one view at the refresh cadence and print what changed, one
//! event per line. Changes are measured poll-to-poll in memory, so the stream
//! never consumes or moves PR Marmot's own "changed since you looked" marks.
//! `--pr … --until` turns the follow into a wait that ends with an exit code.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use prmarmot_core::attention::{
    semantic_notice, NoticeKind, Observation, SnapshotNamespace, SnapshotStore,
};
use prmarmot_core::board::{
    carry_forward_conflicts, fetch_tracked, resolve_pull_request_id, BoardConfig, BoardFetch,
    BoardPagination, BoardRow, BoardScope, Mode, TrackedPrStatus,
};
use prmarmot_core::github::rate_limit::{
    backoff_secs, should_back_off, RateLimitInfo, MIN_REFRESH_SECS,
};
use prmarmot_core::github::{GhError, GithubTransport};
use prmarmot_local::attention_state::AttentionState;
use serde_json::{json, Value};

use crate::args::PrRef;
use crate::render::{mode_key, pr_json, scope_json, scope_label, view_title};
use crate::term::{Paint, Tone};
use crate::until::{self, Condition, Outcome};
use crate::view::{self, BoardView, Filters};

pub const EVENT_SCHEMA: &str = "prmarmot-cli/event@1";
/// Removed PRs resolved per poll; the GraphQL `nodes(ids:)` batch is bounded.
const MAX_RESOLVED_REMOVALS: usize = 50;

/// Which PRs produce events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventFilter {
    pub watched_only: bool,
    pub include_snoozed: bool,
}

impl EventFilter {
    fn includes(&self, watched: bool, snoozed: bool) -> bool {
        (!self.watched_only || watched) && (self.include_snoozed || !snoozed)
    }
}

/// What the previous poll knew about a PR, enough to describe it once gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub id: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    included: bool,
}

impl Seen {
    fn of(row: &BoardRow, included: bool) -> Self {
        Self {
            id: row.id.clone(),
            repo: row.repo.clone(),
            number: row.number,
            url: row.url.clone(),
            title: row.title.clone(),
            included,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Ready {
        count: usize,
    },
    Changed {
        kind: NoticeKind,
        title: String,
        body: String,
        changes: Vec<String>,
        pr: Value,
    },
    Added {
        pr: Value,
    },
    Removed {
        seen: Seen,
        status: Option<TrackedPrStatus>,
    },
    RateLimited {
        retry_in_secs: u64,
        rate: Option<RateLimitInfo>,
    },
    Error {
        message: String,
        retry_in_secs: u64,
    },
    /// The last event of a `--until` or `--timeout` watch.
    Until {
        outcome: Outcome,
        pr: Value,
    },
}

impl Event {
    /// Events that count toward `--events`.
    pub fn is_change(&self) -> bool {
        matches!(
            self,
            Event::Changed { .. } | Event::Added { .. } | Event::Removed { .. }
        )
    }
}

/// Poll-to-poll change detection. Bounded: the snapshot store is FIFO-capped
/// and `previous` is replaced by each poll's (page-bounded) board.
pub struct WatchEngine {
    store: SnapshotStore,
    previous: HashMap<String, Seen>,
    primed: bool,
    filter: EventFilter,
}

/// One poll's events, plus the PRs that left the view (whose fate the caller
/// may resolve before turning them into events).
pub struct Observed {
    pub events: Vec<Event>,
    pub removed: Vec<Seen>,
}

impl WatchEngine {
    pub fn new(namespace: SnapshotNamespace, filter: EventFilter) -> Self {
        Self {
            store: SnapshotStore::new(namespace),
            previous: HashMap::new(),
            primed: false,
            filter,
        }
    }

    pub fn is_primed(&self) -> bool {
        self.primed
    }

    /// The merge-conflict state this stream observed last for a PR.
    pub fn last_conflict(&self, pr_id: &str) -> Option<bool> {
        self.store.last_conflict(pr_id)
    }

    pub fn observe(&mut self, view: &BoardView) -> Observed {
        let mut events = Vec::new();
        let mut current = HashMap::with_capacity(view.rows.len());
        for (row, marks) in view.rows.iter().zip(&view.marks) {
            let included = self.filter.includes(marks.watched, marks.snoozed.is_some());
            let before = self.store.snapshot(&row.id).map(|s| s.latest.clone());
            let result = self
                .store
                .observe(row.id.clone(), Observation::from_row(row));
            let changes = self
                .store
                .snapshot(&row.id)
                .map(|s| s.change_summary())
                .unwrap_or_default();
            self.store.acknowledge(&row.id);
            if self.primed && included {
                if !self.previous.contains_key(&row.id) {
                    events.push(Event::Added {
                        pr: pr_json(row, marks),
                    });
                } else if let Ok(result) = result {
                    if result.kind
                        == (prmarmot_core::attention::ObservationKind::Changed {
                            semantic_transition: true,
                        })
                    {
                        if let Some(notice) = semantic_notice(before.as_ref(), row) {
                            events.push(Event::Changed {
                                kind: notice.kind,
                                title: notice.title,
                                body: notice.body,
                                changes,
                                pr: pr_json(row, marks),
                            });
                        }
                    }
                }
            }
            current.insert(row.id.clone(), Seen::of(row, included));
        }
        let mut removed: Vec<Seen> = if self.primed {
            self.previous
                .drain()
                .filter(|(id, seen)| seen.included && !current.contains_key(id))
                .map(|(_, seen)| seen)
                .collect()
        } else {
            Vec::new()
        };
        removed.sort_by(|a, b| (&a.repo, a.number).cmp(&(&b.repo, b.number)));
        if !self.primed {
            events.push(Event::Ready {
                count: view.rows.len(),
            });
            self.primed = true;
        }
        self.previous = current;
        Observed { events, removed }
    }
}

// ---- Output ------------------------------------------------------------------

pub struct Context<'a> {
    pub mode: Mode,
    pub scope: &'a BoardScope,
    pub authored_only: bool,
    /// `watch --pr`: the one pull request followed instead of a view.
    pub pr: Option<&'a PrRef>,
    pub viewer: &'a str,
    pub interval: Duration,
    pub until: &'a [Condition],
    pub timeout: Option<Duration>,
}

impl Context<'_> {
    fn title(&self) -> String {
        match self.pr {
            Some(pr) => format!("{}#{}", pr.repo, pr.number),
            None => view_title(self.mode, self.scope, self.authored_only).to_owned(),
        }
    }

    fn scope_json(&self) -> Value {
        match self.pr {
            Some(pr) => json!({ "type": "pull_request", "repo": pr.repo, "number": pr.number }),
            None => scope_json(self.scope),
        }
    }
}

fn status_key(status: Option<TrackedPrStatus>) -> &'static str {
    match status {
        Some(TrackedPrStatus::Open) => "open",
        Some(TrackedPrStatus::Closed) => "closed",
        Some(TrackedPrStatus::Merged) => "merged",
        Some(TrackedPrStatus::Inaccessible) => "inaccessible",
        None => "unknown",
    }
}

fn seen_json(seen: &Seen) -> Value {
    json!({
        "id": seen.id,
        "repo": seen.repo,
        "number": seen.number,
        "url": seen.url,
        "title": seen.title,
    })
}

fn until_keys(conditions: &[Condition]) -> Vec<&'static str> {
    conditions.iter().map(|condition| condition.key()).collect()
}

fn rate_value(rate: Option<&RateLimitInfo>) -> Value {
    rate.map(|rate| {
        json!({
            "limit": rate.limit,
            "remaining": rate.remaining,
            "cost": rate.cost,
            "reset_at": rate.reset_at,
        })
    })
    .unwrap_or(Value::Null)
}

pub fn event_json(
    event: &Event,
    ctx: &Context,
    at: DateTime<Utc>,
    rate: Option<&RateLimitInfo>,
) -> Value {
    let mut value = json!({
        "schema": EVENT_SCHEMA,
        "at": at.to_rfc3339(),
        "mode": mode_key(ctx.mode),
    });
    let fields = match event {
        Event::Ready { count } => {
            let mut ready = json!({
                "type": "ready",
                "view": ctx.title(),
                "scope": ctx.scope_json(),
                "viewer": ctx.viewer,
                "count": count,
                "interval_secs": ctx.interval.as_secs(),
                "rate_limit": rate_value(rate),
            });
            if ctx.pr.is_some() {
                ready["until"] = json!(until_keys(ctx.until));
                ready["timeout_secs"] = json!(ctx.timeout.map(|timeout| timeout.as_secs()));
            }
            ready
        }
        Event::Changed {
            kind,
            title,
            body,
            changes,
            pr,
        } => json!({
            "type": "changed",
            "kind": kind.key(),
            "title": title,
            "body": body,
            "changes": changes,
            "pr": pr,
        }),
        Event::Added { pr } => json!({ "type": "added", "pr": pr }),
        Event::Removed { seen, status } => json!({
            "type": "removed",
            "status": status_key(*status),
            "pr": seen_json(seen),
        }),
        Event::RateLimited {
            retry_in_secs,
            rate,
        } => json!({
            "type": "rate_limited",
            "retry_in_secs": retry_in_secs,
            "rate_limit": rate_value(rate.as_ref()),
        }),
        Event::Error {
            message,
            retry_in_secs,
        } => json!({
            "type": "error",
            "message": message,
            "retry_in_secs": retry_in_secs,
        }),
        Event::Until { outcome, pr } => {
            let (key, condition, reasons) = match outcome {
                Outcome::Met(condition) => ("met", Some(condition.key()), Vec::new()),
                Outcome::Merged(unseen) => (
                    "met",
                    Some(Condition::Merged.key()),
                    unseen
                        .iter()
                        .map(|condition| {
                            json!({
                                "condition": condition.key(),
                                "reason": "not_seen",
                                "text": "merged before this was seen",
                            })
                        })
                        .collect(),
                ),
                Outcome::Failed(blocked) => (
                    "unmet",
                    None,
                    blocked
                        .iter()
                        .map(|(condition, reason)| {
                            json!({
                                "condition": condition.key(),
                                "reason": reason.key(),
                                "text": reason.text(),
                            })
                        })
                        .collect(),
                ),
                Outcome::TimedOut(_) => ("timeout", None, Vec::new()),
            };
            json!({
                "type": "until",
                "outcome": key,
                "condition": condition,
                "reasons": reasons,
                "until": until_keys(ctx.until),
                "timeout_secs": ctx.timeout.map(|timeout| timeout.as_secs()),
                "pr": pr,
            })
        }
    };
    if let (Value::Object(base), Value::Object(extra)) = (&mut value, fields) {
        base.extend(extra);
    }
    value
}

fn human_duration(secs: u64) -> String {
    if secs >= 60 && secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn pr_word(pr: &Value) -> String {
    format!(
        "{}#{}",
        pr["repo"].as_str().unwrap_or("?"),
        pr["number"].as_u64().unwrap_or(0)
    )
}

fn notice_tone(kind: NoticeKind) -> Tone {
    match kind {
        NoticeKind::MergeConflict | NoticeKind::ChangesRequested => Tone::Danger,
        NoticeKind::ReviewAgain => Tone::Warning,
        NoticeKind::CiPassed => Tone::Success,
        NoticeKind::Changed => Tone::Accent,
    }
}

pub fn event_text(
    event: &Event,
    ctx: &Context,
    at: DateTime<Utc>,
    rate: Option<&RateLimitInfo>,
    paint: Paint,
) -> String {
    let time = paint.tone(
        Tone::Muted,
        &at.with_timezone(&Local).format("%H:%M").to_string(),
    );
    let indent = "       ";
    match event {
        Event::Ready { count } => {
            let budget = rate
                .map(|r| format!(" · API {}/{}", r.remaining, r.limit))
                .unwrap_or_default();
            let every = human_duration(ctx.interval.as_secs());
            let text = match ctx.pr {
                Some(_) => {
                    let mut goal = String::new();
                    if !ctx.until.is_empty() {
                        goal.push_str(&format!(" · until {}", until_keys(ctx.until).join(" or ")));
                    }
                    if let Some(timeout) = ctx.timeout {
                        goal.push_str(&format!(" · for up to {}", human_span(timeout)));
                    }
                    format!("Watching {}{goal} · every {every}{budget}", ctx.title())
                }
                None => format!(
                    "Watching {} · {} · {count} PR{} · every {every}{budget}",
                    ctx.title(),
                    scope_label(ctx.scope),
                    if *count == 1 { "" } else { "s" },
                ),
            };
            format!("{time}  {}", paint.tone(Tone::Muted, &text))
        }
        Event::Changed {
            kind,
            title,
            body,
            changes,
            pr,
        } => {
            let mut line = format!(
                "{time}  {} {}  {body}",
                paint.tone(notice_tone(*kind), "●"),
                paint.bold(title)
            );
            let detail = if changes.is_empty() {
                String::new()
            } else {
                format!("{} · ", changes.join("; "))
            };
            line.push_str(&format!(
                "\n{indent}{}",
                paint.tone(
                    Tone::Muted,
                    &format!("{detail}{}", pr["url"].as_str().unwrap_or(""))
                )
            ));
            line
        }
        Event::Added { pr } => format!(
            "{time}  {} {}  {} · {}\n{indent}{}",
            paint.tone(Tone::Accent, "+"),
            paint.bold(&format!("New in {}", ctx.title())),
            pr_word(pr),
            pr["title"].as_str().unwrap_or(""),
            paint.tone(Tone::Muted, pr["url"].as_str().unwrap_or(""))
        ),
        Event::Removed { seen, status } => {
            let (label, tone) = match status {
                Some(TrackedPrStatus::Merged) => ("Merged".to_owned(), Tone::Success),
                Some(TrackedPrStatus::Closed) => ("Closed".to_owned(), Tone::Muted),
                Some(TrackedPrStatus::Open) => {
                    (format!("Left {} (still open)", ctx.title()), Tone::Muted)
                }
                Some(TrackedPrStatus::Inaccessible) if ctx.pr.is_some() => {
                    ("No longer accessible".to_owned(), Tone::Muted)
                }
                _ => (format!("Left {}", ctx.title()), Tone::Muted),
            };
            format!(
                "{time}  {} {}  {}#{} · {}\n{indent}{}",
                paint.tone(tone, "−"),
                paint.bold(&label),
                seen.repo,
                seen.number,
                seen.title,
                paint.tone(Tone::Muted, &seen.url)
            )
        }
        Event::RateLimited { retry_in_secs, .. } => format!(
            "{time}  {} {}",
            paint.tone(Tone::Warning, "!"),
            paint.tone(
                Tone::Warning,
                &format!(
                    "GitHub API budget low — next check in {}",
                    human_duration(*retry_in_secs)
                )
            )
        ),
        Event::Error {
            message,
            retry_in_secs,
        } => format!(
            "{time}  {} {}",
            paint.tone(Tone::Danger, "!"),
            paint.tone(
                Tone::Danger,
                &format!("{message} — retrying in {}", human_duration(*retry_in_secs))
            )
        ),
        Event::Until { outcome, pr } => {
            let url = pr["url"].as_str().unwrap_or("");
            let (glyph, tone, label, detail) = match outcome {
                Outcome::Met(condition) => (
                    "✓",
                    Tone::Success,
                    format!("Reached {}", condition.key()),
                    url.to_owned(),
                ),
                Outcome::Merged(unseen) => (
                    "✓",
                    Tone::Success,
                    "Merged".to_owned(),
                    if unseen.is_empty() {
                        url.to_owned()
                    } else {
                        format!(
                            "merged before {} was seen · {url}",
                            until_keys(unseen).join(" or ")
                        )
                    },
                ),
                Outcome::Failed(blocked) => {
                    let reasons = match blocked.as_slice() {
                        [(_, reason)] => reason.text().to_owned(),
                        _ => blocked
                            .iter()
                            .map(|(condition, reason)| {
                                format!("{}: {}", condition.key(), reason.text())
                            })
                            .collect::<Vec<_>>()
                            .join("; "),
                    };
                    (
                        "✗",
                        Tone::Danger,
                        format!("Can no longer reach {}", until_keys(ctx.until).join(" or ")),
                        format!("{reasons} · {url}"),
                    )
                }
                Outcome::TimedOut(timeout) => (
                    "!",
                    Tone::Warning,
                    format!("Gave up after {}", human_span(*timeout)),
                    if ctx.until.is_empty() {
                        url.to_owned()
                    } else {
                        format!("still not {} · {url}", until_keys(ctx.until).join(" or "))
                    },
                ),
            };
            format!(
                "{time}  {} {}  {} · {}\n{indent}{}",
                paint.tone(tone, glyph),
                paint.bold(&label),
                pr_word(pr),
                pr["title"].as_str().unwrap_or(""),
                paint.tone(Tone::Muted, &detail)
            )
        }
    }
}

/// `--timeout` spans: `90s`, `30m`, `1h 30m`.
fn human_span(span: Duration) -> String {
    let secs = span.as_secs();
    if secs >= 3600 {
        match (secs / 3600, secs % 3600) {
            (hours, 0) => format!("{hours}h"),
            (hours, rest) => format!("{hours}h {}", human_duration(rest)),
        }
    } else {
        human_duration(secs)
    }
}

// ---- Loop ----------------------------------------------------------------------

pub enum Output {
    Text(Paint),
    Json,
}

pub struct Session<'a> {
    pub transport: &'a dyn GithubTransport,
    pub host: String,
    pub viewer: String,
    pub mode: Mode,
    pub scope: BoardScope,
    pub board: BoardConfig,
    pub interval: Duration,
    pub filter: EventFilter,
    pub max_events: Option<u64>,
    pub output: Output,
    /// Follow this one pull request instead of the view.
    pub pr: Option<PrRef>,
    /// With `pr`: stop once any of these holds (or none of them can).
    pub until: Vec<Condition>,
    /// With `pr`: stop waiting after this long.
    pub timeout: Option<Duration>,
}

/// Why the loop stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// `--events N` reached, the followed PR closed, or the reader closed the pipe.
    Done,
    /// The very first fetch failed; there is no baseline to watch from.
    Failed(GhError),
    /// A `--until` condition holds.
    Met,
    /// No `--until` condition can hold any more.
    Unmet,
    /// `--timeout` passed first.
    TimedOut,
}

impl Stop {
    fn after(outcome: &Outcome) -> Self {
        match outcome {
            Outcome::Met(_) | Outcome::Merged(_) => Stop::Met,
            Outcome::Failed(_) => Stop::Unmet,
            Outcome::TimedOut(_) => Stop::TimedOut,
        }
    }
}

/// Waiting between polls, and the time a `--timeout` is measured against.
pub trait Clock {
    fn sleep(&mut self, wait: Duration);
    /// Time since the watch started.
    fn elapsed(&self) -> Duration;
}

pub struct SystemClock(Instant);

impl SystemClock {
    pub fn start() -> Self {
        Self(Instant::now())
    }
}

impl Clock for SystemClock {
    fn sleep(&mut self, wait: Duration) {
        std::thread::sleep(wait);
    }

    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Ask GitHub what became of PRs that left the view. One extra request, only
/// on polls where something disappeared; failures just leave the fate unknown.
fn resolve_removals(session: &Session, removed: &[Seen]) -> HashMap<String, TrackedPrStatus> {
    let ids: Vec<String> = removed
        .iter()
        .take(MAX_RESOLVED_REMOVALS)
        .map(|seen| seen.id.clone())
        .collect();
    fetch_tracked(session.transport, &ids, &session.viewer, &session.board)
        .map(|fetch| {
            fetch
                .tracked
                .into_iter()
                .map(|tracked| (tracked.pr_id, tracked.status))
                .collect()
        })
        .unwrap_or_default()
}

/// What became of the followed PR once it is no longer open.
type Gone = (TrackedPrStatus, Seen);

/// A `--pr` poll: the PR as a one-row board while it is open; after that, no
/// rows and its fate. One `nodes(ids:)` request, no board search.
fn fetch_pull_request(
    session: &Session,
    pr: &PrRef,
    id: &str,
) -> Result<(BoardFetch, Option<Gone>), GhError> {
    // The row carries the viewer's own standing review (`my_review`), which
    // `--until approved` counts on someone else's PR.
    let fetched = fetch_tracked(
        session.transport,
        &[id.to_owned()],
        &session.viewer,
        &session.board,
    )?;
    let unknown = || Seen {
        id: id.to_owned(),
        repo: pr.repo.clone(),
        number: pr.number,
        url: format!("https://{}/{}/pull/{}", session.host, pr.repo, pr.number),
        title: String::new(),
        included: true,
    };
    let (rows, gone) = match fetched.tracked.into_iter().next() {
        Some(tracked) if tracked.status == TrackedPrStatus::Open && tracked.row.is_some() => {
            (tracked.row.into_iter().collect(), None)
        }
        Some(tracked) => {
            let seen = tracked
                .row
                .as_ref()
                .map_or_else(unknown, |row| Seen::of(row, true));
            (Vec::new(), Some((tracked.status, seen)))
        }
        None => (Vec::new(), Some((TrackedPrStatus::Inaccessible, unknown()))),
    };
    let board = BoardFetch {
        rows,
        rate: fetched.rate,
        truncated: false,
        pagination: BoardPagination::default(),
        tracked: Vec::new(),
    };
    Ok((board, gone))
}

/// GitHub reports mergeability as UNKNOWN while it recomputes (after every
/// base-branch push). Keep the conflict seen last, from this stream's previous
/// poll or else the app's snapshots, so recomputation is not a change.
fn keep_known_conflicts(
    rows: &mut [BoardRow],
    engine: &WatchEngine,
    attention: &AttentionState,
    mode: Mode,
    viewer: &str,
    board: &BoardConfig,
) {
    carry_forward_conflicts(
        rows,
        |id| {
            engine
                .last_conflict(id)
                .or_else(|| attention.snapshots.last_conflict(id))
        },
        mode,
        viewer,
        board,
    );
}

/// Print one event; false once the reader has gone away.
fn emit(
    out: &mut dyn Write,
    output: &Output,
    event: &Event,
    ctx: &Context,
    at: DateTime<Utc>,
    rate: Option<&RateLimitInfo>,
) -> bool {
    let line = match output {
        Output::Json => event_json(event, ctx, at, rate).to_string(),
        Output::Text(paint) => event_text(event, ctx, at, rate, *paint),
    };
    writeln!(out, "{line}").and_then(|_| out.flush()).is_ok()
}

pub fn run(session: Session, out: &mut dyn Write, clock: &mut dyn Clock) -> Stop {
    let ctx = Context {
        mode: session.mode,
        scope: &session.scope,
        authored_only: session.board.authored_only,
        pr: session.pr.as_ref(),
        viewer: &session.viewer,
        interval: session.interval,
        until: &session.until,
        timeout: session.timeout,
    };
    let mut engine = WatchEngine::new(
        SnapshotNamespace::new(session.host.clone(), session.viewer.clone()),
        session.filter,
    );
    // `--pr`: resolve the node id once; each poll is then one small request.
    let followed = match &session.pr {
        Some(pr) => match resolve_pull_request_id(session.transport, &pr.repo, pr.number) {
            Ok(id) => Some((pr, id)),
            Err(error) => return Stop::Failed(error),
        },
        None => None,
    };
    // The followed PR as last seen, for the event that ends a wait.
    let mut last_pr = followed.as_ref().map_or(Value::Null, |(pr, id)| {
        json!({
            "id": id,
            "repo": pr.repo,
            "number": pr.number,
            "url": format!("https://{}/{}/pull/{}", session.host, pr.repo, pr.number),
            "title": "",
        })
    });
    let mut counted = 0u64;
    // Set when the next poll lands on the `--timeout` deadline.
    let mut last_look = false;
    loop {
        let now = Utc::now();
        // Re-read each poll: watches and snoozes change in the app meanwhile.
        let attention = view::attention(&session.host, &session.viewer);
        let fetched = match &followed {
            Some((pr, id)) => fetch_pull_request(&session, pr, id),
            None => view::fetch(
                session.transport,
                session.mode,
                &session.scope,
                &session.viewer,
                &session.board,
                1,
            )
            .map(|fetch| (fetch, None)),
        };
        let mut finish = None;
        let (events, rate, wait) = match fetched {
            Ok((mut fetch, gone)) => {
                keep_known_conflicts(
                    &mut fetch.rows,
                    &engine,
                    &attention,
                    session.mode,
                    &session.viewer,
                    &session.board,
                );
                let rate = fetch.rate.clone();
                let mut board = view::build(
                    fetch,
                    &attention,
                    session.mode,
                    session.scope.clone(),
                    session.viewer.clone(),
                    Filters {
                        stale_after_days: session.board.stale_after_days,
                        ..Filters::default()
                    },
                    now,
                );
                board.authored_only = session.board.authored_only;
                let Observed {
                    mut events,
                    removed,
                } = engine.observe(&board);
                if let Some((status, fallback)) = gone {
                    // The followed PR merged, closed, or vanished: say so and stop.
                    let seen = removed.into_iter().next().unwrap_or(fallback);
                    let pr = seen_json(&seen);
                    events.push(Event::Removed {
                        seen,
                        status: Some(status),
                    });
                    finish = Some(match until::settle_gone(&session.until, status) {
                        Some(outcome) => {
                            let stop = Stop::after(&outcome);
                            events.push(Event::Until { outcome, pr });
                            stop
                        }
                        None => Stop::Done,
                    });
                } else if followed.is_some() {
                    if let (Some(row), Some(marks)) = (board.rows.first(), board.marks.first()) {
                        last_pr = pr_json(row, marks);
                        if let Some(outcome) =
                            until::settle(&session.until, |c| until::judge_open(c, row))
                        {
                            finish = Some(Stop::after(&outcome));
                            events.push(Event::Until {
                                outcome,
                                pr: last_pr.clone(),
                            });
                        }
                    }
                } else if !removed.is_empty() {
                    let fates = resolve_removals(&session, &removed);
                    events.extend(removed.into_iter().map(|seen| Event::Removed {
                        status: fates.get(&seen.id).copied(),
                        seen,
                    }));
                }
                let mut wait = session.interval;
                if let Some(budget) = rate
                    .as_ref()
                    .filter(|r| finish.is_none() && should_back_off(r))
                {
                    let secs = backoff_secs(budget.reset_epoch(), now.timestamp().max(0) as u64)
                        .max(session.interval.as_secs());
                    wait = Duration::from_secs(secs);
                    events.push(Event::RateLimited {
                        retry_in_secs: secs,
                        rate: Some(budget.clone()),
                    });
                }
                (events, rate, wait)
            }
            Err(error @ (GhError::NotInstalled | GhError::NotAuthenticated)) => {
                return Stop::Failed(error);
            }
            Err(error) if !engine.is_primed() => return Stop::Failed(error),
            Err(GhError::RateLimited { reset_epoch }) => {
                let secs = backoff_secs(reset_epoch, now.timestamp().max(0) as u64);
                (
                    vec![Event::RateLimited {
                        retry_in_secs: secs,
                        rate: None,
                    }],
                    None,
                    Duration::from_secs(secs),
                )
            }
            Err(error) => (
                vec![Event::Error {
                    message: error.to_string(),
                    retry_in_secs: session.interval.as_secs(),
                }],
                None,
                session.interval,
            ),
        };
        for event in &events {
            if !emit(out, &session.output, event, &ctx, now, rate.as_ref()) {
                return Stop::Done;
            }
            if event.is_change() {
                counted += 1;
                if session.max_events.is_some_and(|max| counted >= max) {
                    return Stop::Done;
                }
            }
        }
        if let Some(stop) = finish {
            return stop;
        }
        let mut wait = wait;
        if let Some(timeout) = session.timeout {
            let remaining = timeout.saturating_sub(clock.elapsed());
            let mut give_up = last_look || remaining.is_zero();
            if !give_up && wait >= remaining {
                // One last look on the deadline if the poll floor allows it
                // (never inside a rate-limit backoff); else just wait it out.
                if wait == session.interval && remaining >= Duration::from_secs(MIN_REFRESH_SECS) {
                    last_look = true;
                    wait = remaining;
                } else {
                    clock.sleep(remaining);
                    give_up = true;
                }
            }
            if give_up {
                let event = Event::Until {
                    outcome: Outcome::TimedOut(timeout),
                    pr: last_pr,
                };
                emit(out, &session.output, &event, &ctx, Utc::now(), None);
                return Stop::TimedOut;
            }
        }
        clock.sleep(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::build;
    use crate::view::tests::{fetch_of, row};
    use prmarmot_core::attention::Observation;
    use prmarmot_core::board::{Category, Ci};

    fn namespace() -> SnapshotNamespace {
        SnapshotNamespace::new("github.com", "me")
    }

    fn board(rows: Vec<BoardRow>, attention: &AttentionState) -> BoardView {
        build(
            fetch_of(rows),
            attention,
            Mode::Authored,
            BoardScope::AllRepositories,
            "me".into(),
            Filters::default(),
            Utc::now(),
        )
    }

    fn kinds(events: &[Event]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event {
                Event::Ready { .. } => "ready",
                Event::Changed { .. } => "changed",
                Event::Added { .. } => "added",
                Event::Removed { .. } => "removed",
                Event::RateLimited { .. } => "rate_limited",
                Event::Error { .. } => "error",
                Event::Until { .. } => "until",
            })
            .collect()
    }

    #[test]
    fn first_poll_is_a_baseline_then_semantic_changes_become_events() {
        let attention = AttentionState::empty(namespace());
        let mut engine = WatchEngine::new(namespace(), EventFilter::default());
        let mut pr = row(1, Category::Await);
        pr.ci = Ci::Running;
        let first = engine.observe(&board(
            vec![pr.clone(), row(2, Category::Await)],
            &attention,
        ));
        assert_eq!(first.events, vec![Event::Ready { count: 2 }]);

        // Nothing semantic moved: silence.
        let mut touched = pr.clone();
        touched.updated_at = Some("2026-09-12T00:00:00Z".into());
        let quiet = engine.observe(&board(
            vec![touched.clone(), row(2, Category::Await)],
            &attention,
        ));
        assert!(quiet.events.is_empty() && quiet.removed.is_empty());

        touched.ci = Ci::Pass;
        let changed = engine.observe(&board(vec![touched, row(2, Category::Await)], &attention));
        match changed.events.as_slice() {
            [Event::Changed {
                kind,
                title,
                changes,
                pr,
                ..
            }] => {
                assert_eq!(*kind, NoticeKind::CiPassed);
                assert_eq!(title, "Ready for you — CI passed");
                assert_eq!(changes, &vec!["CI running → passing".to_string()]);
                assert_eq!(pr["number"], 1);
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn a_conflict_github_is_still_recomputing_is_not_a_change() {
        let attention = AttentionState::empty(namespace());
        let mut engine = WatchEngine::new(namespace(), EventFilter::default());
        let mut conflicted = row(1, Category::Action);
        conflicted.conflict = true;
        engine.observe(&board(vec![conflicted.clone()], &attention));

        let mut recomputing = conflicted.clone();
        recomputing.conflict = false;
        recomputing.mergeable_unknown = true;
        let mut rows = vec![recomputing];
        let config = BoardConfig::default();
        keep_known_conflicts(
            &mut rows,
            &engine,
            &attention,
            Mode::Authored,
            "me",
            &config,
        );
        assert!(rows[0].conflict);
        let quiet = engine.observe(&board(rows, &attention));
        assert!(quiet.events.is_empty(), "{:?}", quiet.events);
        let settled = engine.observe(&board(vec![conflicted], &attention));
        assert!(settled.events.is_empty(), "{:?}", settled.events);
    }

    #[test]
    fn a_fresh_stream_takes_the_last_known_conflict_from_the_app() {
        let mut attention = AttentionState::empty(namespace());
        let mut conflicted = row(1, Category::Action);
        conflicted.conflict = true;
        attention
            .snapshots
            .observe(&conflicted.id, Observation::from_row(&conflicted))
            .unwrap();
        let engine = WatchEngine::new(namespace(), EventFilter::default());

        let mut recomputing = conflicted;
        recomputing.conflict = false;
        recomputing.mergeable_unknown = true;
        let mut rows = vec![recomputing];
        let config = BoardConfig::default();
        keep_known_conflicts(
            &mut rows,
            &engine,
            &attention,
            Mode::Authored,
            "me",
            &config,
        );
        assert!(rows[0].conflict);
        assert!(rows[0].note.contains("merge conflict"), "{}", rows[0].note);
    }

    #[test]
    fn changes_are_poll_to_poll_not_since_the_apps_acknowledgement() {
        let attention = AttentionState::empty(namespace());
        let mut engine = WatchEngine::new(namespace(), EventFilter::default());
        let mut pr = row(1, Category::Await);
        pr.ci = Ci::Running;
        engine.observe(&board(vec![pr.clone()], &attention));
        pr.ci = Ci::Fail;
        let events = engine.observe(&board(vec![pr.clone()], &attention)).events;
        assert_eq!(kinds(&events), ["changed"]);
        pr.ci = Ci::Pass;
        match &engine.observe(&board(vec![pr], &attention)).events[..] {
            [Event::Changed { changes, .. }] => {
                assert_eq!(changes, &vec!["CI failing → passing".to_string()])
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn arrivals_and_departures_are_reported_after_the_baseline() {
        let attention = AttentionState::empty(namespace());
        let mut engine = WatchEngine::new(namespace(), EventFilter::default());
        engine.observe(&board(
            vec![row(1, Category::Await), row(2, Category::Await)],
            &attention,
        ));
        let next = engine.observe(&board(
            vec![row(2, Category::Await), row(3, Category::Action)],
            &attention,
        ));
        assert_eq!(kinds(&next.events), ["added"]);
        assert_eq!(next.removed.len(), 1);
        assert_eq!(next.removed[0].number, 1);
        assert_eq!(
            next.removed[0].url,
            "https://github.com/acme/widgets/pull/1"
        );
    }

    #[test]
    fn filters_limit_events_to_watched_and_unsnoozed_prs() {
        let mut attention = AttentionState::empty(namespace());
        let watched = row(1, Category::Await);
        attention.toggle_watch(&watched);
        let mut engine = WatchEngine::new(
            namespace(),
            EventFilter {
                watched_only: true,
                include_snoozed: false,
            },
        );
        engine.observe(&board(
            vec![watched.clone(), row(2, Category::Await)],
            &attention,
        ));
        let mut w = watched.clone();
        w.conflict = true;
        let mut other = row(2, Category::Await);
        other.conflict = true;
        let next = engine.observe(&board(vec![w, other, row(3, Category::Await)], &attention));
        assert_eq!(kinds(&next.events), ["changed"]);
        let next = engine.observe(&board(vec![], &attention));
        assert_eq!(next.removed.len(), 1, "only the watched PR's departure");
    }

    #[test]
    fn json_events_carry_schema_type_and_facts() {
        let scope = BoardScope::Repository("acme/widgets".into());
        let ctx = Context {
            mode: Mode::Review,
            scope: &scope,
            authored_only: false,
            pr: None,
            viewer: "me",
            interval: Duration::from_secs(300),
            until: &[],
            timeout: None,
        };
        let at = Utc::now();
        let ready = event_json(&Event::Ready { count: 3 }, &ctx, at, None);
        assert_eq!(ready["schema"], EVENT_SCHEMA);
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["mode"], "review");
        assert_eq!(ready["interval_secs"], 300);
        let removed = event_json(
            &Event::Removed {
                seen: Seen {
                    id: "PR_1".into(),
                    repo: "acme/widgets".into(),
                    number: 1,
                    url: "https://github.com/acme/widgets/pull/1".into(),
                    title: "One".into(),
                    included: true,
                },
                status: Some(TrackedPrStatus::Merged),
            },
            &ctx,
            at,
            None,
        );
        assert_eq!(removed["type"], "removed");
        assert_eq!(removed["status"], "merged");
        assert_eq!(removed["pr"]["number"], 1);
        let text = event_text(
            &Event::RateLimited {
                retry_in_secs: 900,
                rate: None,
            },
            &ctx,
            at,
            None,
            Paint::new(false),
        );
        assert!(
            text.ends_with("! GitHub API budget low — next check in 15m"),
            "{text}"
        );
    }

    #[test]
    fn a_wait_names_its_goal_up_front_and_its_outcome_last() {
        let scope = BoardScope::AllRepositories;
        let pr = PrRef {
            repo: "acme/widgets".into(),
            number: 7,
        };
        let until = [Condition::Approved, Condition::CiPass];
        let ctx = Context {
            mode: Mode::Authored,
            scope: &scope,
            authored_only: false,
            pr: Some(&pr),
            viewer: "me",
            interval: Duration::from_secs(60),
            until: &until,
            timeout: Some(Duration::from_secs(5400)),
        };
        let at = Utc::now();
        let plain = Paint::new(false);
        let ready = event_json(&Event::Ready { count: 1 }, &ctx, at, None);
        assert_eq!(ready["until"], json!(["approved", "ci-pass"]));
        assert_eq!(ready["timeout_secs"], 5400);
        let text = event_text(&Event::Ready { count: 1 }, &ctx, at, None, plain);
        assert!(
            text.ends_with(
                "Watching acme/widgets#7 · until approved or ci-pass · for up to 1h 30m · every 1m"
            ),
            "{text}"
        );

        let pr_value = json!({
            "repo": "acme/widgets",
            "number": 7,
            "url": "https://github.com/acme/widgets/pull/7",
            "title": "Change 7",
        });
        let unmet = Event::Until {
            outcome: Outcome::Failed(vec![
                (Condition::Approved, until::Blocked::ChangesRequested),
                (Condition::CiPass, until::Blocked::CiFailed),
            ]),
            pr: pr_value.clone(),
        };
        let value = event_json(&unmet, &ctx, at, None);
        assert_eq!(value["type"], "until");
        assert_eq!(value["outcome"], "unmet");
        assert_eq!(value["condition"], Value::Null);
        assert_eq!(value["reasons"][0]["condition"], "approved");
        assert_eq!(value["reasons"][0]["reason"], "changes_requested");
        assert_eq!(value["reasons"][1]["text"], "CI failed");
        assert_eq!(value["pr"]["number"], 7);
        let text = event_text(&unmet, &ctx, at, None, plain);
        assert!(
            text.contains("✗ Can no longer reach approved or ci-pass  acme/widgets#7 · Change 7"),
            "{text}"
        );
        assert!(
            text.contains("approved: changes requested; ci-pass: CI failed · https://"),
            "{text}"
        );

        let met = Event::Until {
            outcome: Outcome::Met(Condition::CiPass),
            pr: pr_value.clone(),
        };
        assert_eq!(event_json(&met, &ctx, at, None)["condition"], "ci-pass");
        assert!(
            event_text(&met, &ctx, at, None, plain).contains("✓ Reached ci-pass  acme/widgets#7")
        );
        let merged = Event::Until {
            outcome: Outcome::Merged(vec![Condition::Approved, Condition::CiPass]),
            pr: pr_value.clone(),
        };
        let value = event_json(&merged, &ctx, at, None);
        assert_eq!(value["outcome"], "met");
        assert_eq!(value["condition"], "merged");
        assert_eq!(value["reasons"][1]["condition"], "ci-pass");
        assert_eq!(value["reasons"][1]["reason"], "not_seen");
        let text = event_text(&merged, &ctx, at, None, plain);
        assert!(
            text.contains("✓ Merged  acme/widgets#7 · Change 7"),
            "{text}"
        );
        assert!(
            text.contains("merged before approved or ci-pass was seen · https://"),
            "{text}"
        );
        let late = Event::Until {
            outcome: Outcome::TimedOut(Duration::from_secs(5400)),
            pr: pr_value,
        };
        assert_eq!(event_json(&late, &ctx, at, None)["outcome"], "timeout");
        let text = event_text(&late, &ctx, at, None, plain);
        assert!(
            text.contains("! Gave up after 1h 30m  acme/widgets#7"),
            "{text}"
        );
        assert!(
            text.contains("still not approved or ci-pass · https://"),
            "{text}"
        );
    }

    #[test]
    fn every_event_matches_the_published_schema() {
        use crate::schema_check::{assert_conforms, violations, Schema};
        use crate::until::Blocked;
        use crate::view::Marks;
        use prmarmot_core::board::{Blocker, StackInfo};

        let mut row = row(7, Category::Action);
        row.blockers = vec![
            Blocker::MergeConflict,
            Blocker::CiFailing,
            Blocker::ChangesRequested,
            Blocker::UnresolvedComments(2),
            Blocker::NoReviewers {
                suggested: vec!["sam".into()],
            },
        ];
        row.my_review = Some("APPROVED".into());
        row.review_decision = Some("CHANGES_REQUESTED".into());
        row.issue = Some("DEMO-7".into());
        row.issue_url = Some("https://example.com/issues/DEMO-7".into());
        row.stack = Some(StackInfo {
            number: 3,
            size: 2,
            base_ref_name: "main".into(),
            position: None,
        });
        row.waiting_since = Some("2026-09-10T10:00:00Z".into());
        let marks = Marks {
            watched: true,
            snoozed: Some("until tomorrow".into()),
            changed: true,
            changes: vec!["CI passed".into()],
            stale: true,
        };
        let pr = pr_json(&row, &marks);
        let seen = Seen {
            id: row.id.clone(),
            repo: row.repo.clone(),
            number: row.number,
            url: row.url.clone(),
            title: row.title.clone(),
            included: true,
        };
        let rate = RateLimitInfo {
            limit: 5000,
            cost: 1,
            remaining: 4999,
            reset_at: "2026-09-16T12:00:00Z".into(),
        };
        let events = [
            Event::Ready { count: 1 },
            Event::Changed {
                kind: NoticeKind::ReviewAgain,
                title: "Review again".into(),
                body: "New commits since your review".into(),
                changes: vec!["new commits".into()],
                pr: pr.clone(),
            },
            Event::Added { pr: pr.clone() },
            Event::Removed {
                seen: seen.clone(),
                status: Some(TrackedPrStatus::Merged),
            },
            Event::Removed {
                seen: seen.clone(),
                status: None,
            },
            Event::RateLimited {
                retry_in_secs: 900,
                rate: Some(rate.clone()),
            },
            Event::RateLimited {
                retry_in_secs: 60,
                rate: None,
            },
            Event::Error {
                message: "network unreachable".into(),
                retry_in_secs: 60,
            },
            Event::Until {
                outcome: Outcome::Met(Condition::Approved),
                pr: pr.clone(),
            },
            Event::Until {
                outcome: Outcome::Merged(vec![Condition::CiPass, Condition::Mergeable]),
                pr: pr.clone(),
            },
            Event::Until {
                outcome: Outcome::Failed(vec![
                    (Condition::CiPass, Blocked::CiFailed),
                    (Condition::Approved, Blocked::ChangesRequested),
                    (Condition::Mergeable, Blocked::Inaccessible),
                    (Condition::Merged, Blocked::Closed),
                ]),
                pr: seen_json(&seen),
            },
            Event::Until {
                outcome: Outcome::TimedOut(Duration::from_secs(60)),
                pr: Value::Null,
            },
        ];
        // Adding an event type breaks this list until it is checked here too.
        let mut covered = kinds(&events);
        covered.dedup();
        assert_eq!(
            covered,
            [
                "ready",
                "changed",
                "added",
                "removed",
                "rate_limited",
                "error",
                "until"
            ]
        );

        let followed = PrRef {
            repo: "acme/widgets".into(),
            number: 7,
        };
        let until = [Condition::Approved, Condition::CiPass];
        for (mode, scope, pr_ref) in [
            (Mode::Authored, BoardScope::AllRepositories, None),
            (
                Mode::Review,
                BoardScope::Repository("acme/widgets".into()),
                None,
            ),
            (Mode::Authored, BoardScope::AllRepositories, Some(&followed)),
        ] {
            let ctx = Context {
                mode,
                scope: &scope,
                authored_only: false,
                pr: pr_ref,
                viewer: "me",
                interval: Duration::from_secs(60),
                until: &until,
                timeout: pr_ref.map(|_| Duration::from_secs(5400)),
            };
            for event in &events {
                for rate in [None, Some(&rate)] {
                    assert_conforms(Schema::Event, &event_json(event, &ctx, Utc::now(), rate));
                }
            }
        }

        // The PR inside an event is held to the board schema's rules.
        let ctx = Context {
            mode: Mode::Authored,
            scope: &BoardScope::AllRepositories,
            authored_only: false,
            pr: None,
            viewer: "me",
            interval: Duration::from_secs(60),
            until: &[],
            timeout: None,
        };
        let mut wrong = pr.clone();
        wrong["ci"] = json!("green");
        let errors = violations(
            Schema::Event,
            &event_json(&Event::Added { pr: wrong }, &ctx, Utc::now(), None),
        );
        assert!(!errors.is_empty());
    }

    // ---- The poll loop, end to end against a scripted transport ----------------

    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Canned GraphQL responses served in order; records every query sent.
    struct Script {
        responses: Mutex<VecDeque<Result<Value, GhError>>>,
        queries: Mutex<Vec<String>>,
    }

    impl Script {
        fn new(responses: Vec<Result<Value, GhError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                queries: Mutex::new(Vec::new()),
            }
        }

        fn serve(&self, query: &str) -> Result<Value, GhError> {
            self.queries.lock().unwrap().push(query.to_owned());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("the loop sent more requests than scripted")
        }

        fn queries(&self) -> Vec<String> {
            self.queries.lock().unwrap().clone()
        }
    }

    impl GithubTransport for Script {
        fn graphql(&self, query: &str, _variables: &[(&str, &str)]) -> Result<Value, GhError> {
            self.serve(query)
        }

        fn graphql_with_ids(
            &self,
            query: &str,
            _variables: &[(&str, &str)],
            _ids: &[String],
        ) -> Result<Value, GhError> {
            self.serve(query)
        }
    }

    fn node(number: u64, state: &str, ci: &str, mergeable: &str) -> Value {
        json!({
            "id": format!("PR_{number}"),
            "url": format!("https://github.com/acme/widgets/pull/{number}"),
            "repository": {"nameWithOwner": "acme/widgets"},
            "state": state,
            "merged": state == "MERGED",
            "number": number,
            "title": format!("Change {number}"),
            "isDraft": false,
            "mergeable": mergeable,
            "createdAt": "2026-09-01T00:00:00Z",
            "updatedAt": "2026-09-01T00:00:00Z",
            "author": {"login": "me"},
            "reviewRequests": {"totalCount": 1, "nodes": [
                {"requestedReviewer": {"__typename": "User", "login": "bob"}}
            ]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": ci}}}]}
        })
    }

    fn search(nodes: Vec<Value>) -> Result<Value, GhError> {
        Ok(json!({"data": {
            "search": {"pageInfo": {"hasNextPage": false}, "nodes": nodes},
            "rateLimit": null
        }}))
    }

    fn tracked(node: Value) -> Result<Value, GhError> {
        Ok(json!({"data": {"tracked": [node], "rateLimit": null}}))
    }

    fn session(transport: &Script, pr: Option<PrRef>, max_events: Option<u64>) -> Session<'_> {
        Session {
            transport,
            // No attention state exists for this host, so none is read.
            host: "loop-test.invalid".into(),
            viewer: "me".into(),
            mode: Mode::Authored,
            scope: BoardScope::AllRepositories,
            board: BoardConfig::default(),
            interval: Duration::from_secs(60),
            filter: EventFilter::default(),
            max_events,
            output: Output::Json,
            pr,
            until: Vec::new(),
            timeout: None,
        }
    }

    /// Time that passes only by sleeping.
    #[derive(Default)]
    struct FakeClock {
        now: Duration,
        sleeps: Vec<Duration>,
    }

    impl Clock for FakeClock {
        fn sleep(&mut self, wait: Duration) {
            self.sleeps.push(wait);
            self.now += wait;
        }

        fn elapsed(&self) -> Duration {
            self.now
        }
    }

    /// Run the loop; returns how it stopped, the JSON events, and each sleep.
    fn run_script(session: Session) -> (Stop, Vec<Value>, Vec<Duration>) {
        let mut out = Vec::new();
        let mut clock = FakeClock::default();
        let stop = run(session, &mut out, &mut clock);
        let events = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (stop, events, clock.sleeps)
    }

    fn types(events: &[Value]) -> Vec<&str> {
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn the_loop_reports_changes_resolves_removals_and_stops_after_n_events() {
        let script = Script::new(vec![
            search(vec![
                node(1, "OPEN", "PENDING", "MERGEABLE"),
                node(2, "OPEN", "SUCCESS", "CONFLICTING"),
            ]),
            // #1's CI passes; GitHub is recomputing #2's mergeability.
            search(vec![
                node(1, "OPEN", "SUCCESS", "MERGEABLE"),
                node(2, "OPEN", "SUCCESS", "UNKNOWN"),
            ]),
            Err(GhError::Network("offline".into())),
            // #1 left the view; the loop asks what became of it.
            search(vec![node(2, "OPEN", "SUCCESS", "CONFLICTING")]),
            tracked(node(1, "MERGED", "SUCCESS", "MERGEABLE")),
        ]);
        let (stop, events, sleeps) = run_script(session(&script, None, Some(2)));

        assert_eq!(stop, Stop::Done);
        assert_eq!(types(&events), ["ready", "changed", "error", "removed"]);
        assert_eq!(events[0]["count"], 2);
        assert_eq!(events[1]["kind"], "ci_passed");
        assert_eq!(events[1]["pr"]["number"], 1);
        assert_eq!(events[3]["status"], "merged");
        assert_eq!(events[3]["pr"]["number"], 1);
        assert_eq!(sleeps, [Duration::from_secs(60); 3]);

        let queries = script.queries();
        assert_eq!(queries.len(), 5);
        assert!(queries[..4].iter().all(|query| query.contains("search(")));
        // Resolving a removal is a small nodes() request, not another search.
        assert!(queries[4].contains("tracked: nodes(ids:$tracked)"));
        assert!(!queries[4].contains("search("));
    }

    #[test]
    fn a_first_poll_failure_stops_the_loop_with_the_error() {
        let script = Script::new(vec![Err(GhError::Network("offline".into()))]);
        let (stop, events, sleeps) = run_script(session(&script, None, None));
        assert_eq!(stop, Stop::Failed(GhError::Network("offline".into())));
        assert!(events.is_empty() && sleeps.is_empty());
    }

    #[test]
    fn following_one_pr_ends_when_it_merges() {
        let script = Script::new(vec![
            Ok(json!({"data": {"repository": {"pullRequest": {"id": "PR_7"}}}})),
            tracked(node(7, "OPEN", "PENDING", "MERGEABLE")),
            tracked(node(7, "OPEN", "SUCCESS", "MERGEABLE")),
            tracked(node(7, "MERGED", "SUCCESS", "MERGEABLE")),
        ]);
        let pr = PrRef {
            repo: "acme/widgets".into(),
            number: 7,
        };
        let (stop, events, sleeps) = run_script(session(&script, Some(pr), None));

        assert_eq!(stop, Stop::Done);
        assert_eq!(types(&events), ["ready", "changed", "removed"]);
        assert_eq!(events[0]["view"], "acme/widgets#7");
        assert_eq!(events[0]["scope"]["type"], "pull_request");
        assert_eq!(events[0]["count"], 1);
        assert_eq!(events[1]["kind"], "ci_passed");
        assert_eq!(events[2]["status"], "merged");
        assert_eq!(events[2]["pr"]["title"], "Change 7");
        assert_eq!(sleeps.len(), 2);

        let queries = script.queries();
        assert!(queries[0].contains("pullRequest(number:7)"));
        assert!(queries[1..].iter().all(|query| !query.contains("search(")));
    }

    #[test]
    fn following_a_pr_that_already_closed_reports_it_at_once() {
        let script = Script::new(vec![
            Ok(json!({"data": {"repository": {"pullRequest": {"id": "PR_7"}}}})),
            tracked(node(7, "CLOSED", "SUCCESS", "MERGEABLE")),
        ]);
        let pr = PrRef {
            repo: "acme/widgets".into(),
            number: 7,
        };
        let (stop, events, sleeps) = run_script(session(&script, Some(pr), None));
        assert_eq!(stop, Stop::Done);
        assert_eq!(types(&events), ["ready", "removed"]);
        assert_eq!(events[0]["count"], 0);
        assert_eq!(events[1]["status"], "closed");
        assert!(sleeps.is_empty());
    }

    // ---- `--until` and `--timeout` -------------------------------------------------

    fn resolved(number: u64) -> Result<Value, GhError> {
        Ok(json!({"data": {"repository": {"pullRequest": {"id": format!("PR_{number}")}}}}))
    }

    fn reviewed(mut node: Value, decision: &str) -> Value {
        node["reviewDecision"] = json!(decision);
        node
    }

    /// `watch --pr acme/widgets#7 --until … [--timeout …]` against `responses`.
    fn wait_for(
        responses: Vec<Result<Value, GhError>>,
        until: &[Condition],
        timeout: Option<u64>,
    ) -> (Stop, Vec<Value>, Vec<Duration>) {
        let script = Script::new(responses);
        let pr = PrRef {
            repo: "acme/widgets".into(),
            number: 7,
        };
        run_script(Session {
            until: until.to_vec(),
            timeout: timeout.map(Duration::from_secs),
            ..session(&script, Some(pr), None)
        })
    }

    #[test]
    fn a_wait_for_ci_ends_when_the_checks_pass() {
        let (stop, events, sleeps) = wait_for(
            vec![
                resolved(7),
                tracked(node(7, "OPEN", "PENDING", "MERGEABLE")),
                tracked(node(7, "OPEN", "PENDING", "MERGEABLE")),
                tracked(node(7, "OPEN", "SUCCESS", "MERGEABLE")),
            ],
            &[Condition::CiPass],
            None,
        );
        assert_eq!(stop, Stop::Met);
        assert_eq!(types(&events), ["ready", "changed", "until"]);
        assert_eq!(events[0]["until"], json!(["ci-pass"]));
        assert_eq!(events[0]["timeout_secs"], Value::Null);
        assert_eq!(events[2]["outcome"], "met");
        assert_eq!(events[2]["condition"], "ci-pass");
        assert_eq!(events[2]["pr"]["ci"], "pass");
        assert_eq!(events[2]["pr"]["title"], "Change 7");
        assert_eq!(sleeps, [Duration::from_secs(60); 2]);
    }

    #[test]
    fn a_condition_that_already_holds_ends_on_the_first_poll() {
        let (stop, events, sleeps) = wait_for(
            vec![
                resolved(7),
                tracked(reviewed(
                    node(7, "OPEN", "SUCCESS", "MERGEABLE"),
                    "APPROVED",
                )),
            ],
            &[Condition::Merged, Condition::Mergeable],
            Some(600),
        );
        assert_eq!(stop, Stop::Met);
        assert_eq!(types(&events), ["ready", "until"]);
        assert_eq!(events[1]["condition"], "mergeable");
        assert_eq!(events[1]["until"], json!(["merged", "mergeable"]));
        assert_eq!(events[1]["timeout_secs"], 600);
        assert!(sleeps.is_empty());
    }

    #[test]
    fn mergeable_waits_for_github_to_finish_computing_mergeability() {
        let approved =
            |ci, mergeable| tracked(reviewed(node(7, "OPEN", ci, mergeable), "APPROVED"));
        let (stop, events, sleeps) = wait_for(
            vec![
                resolved(7),
                approved("PENDING", "MERGEABLE"),
                approved("SUCCESS", "UNKNOWN"),
                approved("SUCCESS", "MERGEABLE"),
            ],
            &[Condition::Mergeable],
            None,
        );
        assert_eq!(stop, Stop::Met);
        assert_eq!(types(&events).last(), Some(&"until"));
        assert_eq!(sleeps.len(), 2);
    }

    #[test]
    fn requested_changes_end_a_wait_for_approval_as_unmet() {
        let (stop, events, sleeps) = wait_for(
            vec![
                resolved(7),
                tracked(reviewed(
                    node(7, "OPEN", "SUCCESS", "MERGEABLE"),
                    "REVIEW_REQUIRED",
                )),
                tracked(reviewed(
                    node(7, "OPEN", "SUCCESS", "MERGEABLE"),
                    "CHANGES_REQUESTED",
                )),
            ],
            &[Condition::Approved],
            None,
        );
        assert_eq!(stop, Stop::Unmet);
        assert_eq!(types(&events).first(), Some(&"ready"));
        let last = events.last().unwrap();
        assert_eq!(last["type"], "until");
        assert_eq!(last["outcome"], "unmet");
        assert_eq!(
            last["reasons"],
            json!([{
                "condition": "approved",
                "reason": "changes_requested",
                "text": "changes requested",
            }])
        );
        assert_eq!(sleeps.len(), 1);
    }

    #[test]
    fn failing_ci_does_not_end_an_any_of_wait_that_can_still_be_met() {
        // Red CI blocks ci-pass, but the any-of wait goes on for merged.
        let (stop, events, _) = wait_for(
            vec![
                resolved(7),
                tracked(node(7, "OPEN", "FAILURE", "MERGEABLE")),
                tracked(node(7, "MERGED", "FAILURE", "MERGEABLE")),
            ],
            &[Condition::CiPass, Condition::Merged],
            None,
        );
        assert_eq!(stop, Stop::Met);
        assert_eq!(types(&events), ["ready", "removed", "until"]);
        assert_eq!(events[1]["status"], "merged");
        assert_eq!(events[2]["condition"], "merged");
    }

    #[test]
    fn a_pr_that_closes_or_disappears_fails_the_wait() {
        let (stop, events, _) = wait_for(
            vec![
                resolved(7),
                tracked(node(7, "OPEN", "PENDING", "MERGEABLE")),
                tracked(node(7, "CLOSED", "PENDING", "MERGEABLE")),
            ],
            &[Condition::Merged],
            None,
        );
        assert_eq!(stop, Stop::Unmet);
        assert_eq!(types(&events), ["ready", "removed", "until"]);
        assert_eq!(events[2]["reasons"][0]["reason"], "closed");
        assert_eq!(events[2]["pr"]["number"], 7);

        let (stop, events, _) = wait_for(
            vec![
                resolved(7),
                tracked(node(7, "OPEN", "PENDING", "MERGEABLE")),
                tracked(Value::Null),
            ],
            &[Condition::CiPass],
            None,
        );
        assert_eq!(stop, Stop::Unmet);
        assert_eq!(events[1]["status"], "inaccessible");
        assert_eq!(events[2]["reasons"][0]["reason"], "inaccessible");
    }

    #[test]
    fn a_merge_ends_any_wait_as_met_and_says_what_was_never_seen() {
        let (stop, events, sleeps) = wait_for(
            vec![
                resolved(7),
                tracked(node(7, "OPEN", "NONE", "MERGEABLE")),
                tracked(node(7, "MERGED", "NONE", "MERGEABLE")),
            ],
            &[Condition::CiPass, Condition::Approved],
            None,
        );
        assert_eq!(stop, Stop::Met);
        assert_eq!(types(&events), ["ready", "removed", "until"]);
        let until = &events[2];
        assert_eq!(until["outcome"], "met");
        assert_eq!(until["condition"], "merged");
        assert_eq!(until["reasons"][0]["condition"], "ci-pass");
        assert_eq!(until["reasons"][0]["reason"], "not_seen");
        assert_eq!(until["reasons"][1]["condition"], "approved");
        assert_eq!(until["pr"]["title"], "Change 7");
        assert_eq!(sleeps.len(), 1);

        // Already merged when the wait starts, and merged was asked for.
        let (stop, events, _) = wait_for(
            vec![
                resolved(7),
                tracked(node(7, "MERGED", "SUCCESS", "MERGEABLE")),
            ],
            &[Condition::Merged],
            None,
        );
        assert_eq!(stop, Stop::Met);
        assert_eq!(events[2]["condition"], "merged");
        assert_eq!(events[2]["reasons"], json!([]));
    }

    #[test]
    fn a_wait_times_out_after_a_last_look_on_the_deadline() {
        let pending = || tracked(node(7, "OPEN", "PENDING", "MERGEABLE"));
        let (stop, events, sleeps) = wait_for(
            vec![resolved(7), pending(), pending(), pending(), pending()],
            &[Condition::CiPass],
            Some(150),
        );
        assert_eq!(stop, Stop::TimedOut);
        assert_eq!(types(&events), ["ready", "until"]);
        assert_eq!(events[1]["outcome"], "timeout");
        assert_eq!(events[1]["timeout_secs"], 150);
        assert_eq!(events[1]["pr"]["ci"], "running");
        // Polls at 0s, 60s, 120s, and the deadline 30s later.
        let secs = Duration::from_secs;
        assert_eq!(sleeps, [secs(60), secs(60), secs(30)]);
    }

    #[test]
    fn a_deadline_inside_the_poll_floor_is_waited_out_without_polling() {
        let pending = || tracked(node(7, "OPEN", "PENDING", "MERGEABLE"));
        // --timeout alone bounds a plain follow.
        let (stop, events, sleeps) =
            wait_for(vec![resolved(7), pending(), pending()], &[], Some(80));
        assert_eq!(stop, Stop::TimedOut);
        assert_eq!(types(&events), ["ready", "until"]);
        assert_eq!(events[1]["until"], json!([]));
        assert_eq!(sleeps, [Duration::from_secs(60), Duration::from_secs(20)]);

        // A rate-limit backoff past the deadline is never cut short for a look.
        let low = Ok(json!({"data": {
            "tracked": [node(7, "OPEN", "PENDING", "MERGEABLE")],
            "rateLimit": {"limit": 5000, "remaining": 10, "cost": 1, "resetAt": "2100-01-01T00:00:00Z"}
        }}));
        let (stop, events, sleeps) =
            wait_for(vec![resolved(7), low], &[Condition::CiPass], Some(300));
        assert_eq!(stop, Stop::TimedOut);
        assert_eq!(types(&events), ["ready", "rate_limited", "until"]);
        assert_eq!(sleeps, [Duration::from_secs(300)]);
    }

    /// `node` authored by someone else, with the viewer's own latest review.
    fn by_alice(mut node: Value, my_review: Option<&str>) -> Value {
        node["author"] = json!({"login": "alice"});
        if let Some(state) = my_review {
            node["latestReview"] = json!({"nodes": [{
                "state": state,
                "submittedAt": "2026-09-02T00:00:00Z",
                "commit": {"oid": "head-7"},
            }]});
            node["reviews"] = json!({"nodes": [{
                "author": {"login": "me"},
                "state": state,
                "submittedAt": "2026-09-02T00:00:00Z",
            }]});
        }
        node
    }

    #[test]
    fn without_a_review_decision_your_own_review_counts() {
        let open = || node(7, "OPEN", "SUCCESS", "MERGEABLE");
        let (stop, events, sleeps) = wait_for(
            vec![
                resolved(7),
                tracked(by_alice(open(), None)),
                tracked(by_alice(open(), Some("COMMENTED"))),
                tracked(by_alice(open(), Some("APPROVED"))),
            ],
            &[Condition::Approved],
            None,
        );
        assert_eq!(stop, Stop::Met);
        let last = events.last().unwrap();
        assert_eq!(last["condition"], "approved");
        assert_eq!(last["pr"]["author"], "alice");
        assert_eq!(last["pr"]["review_decision"], Value::Null);
        assert_eq!(last["pr"]["my_review"], "APPROVED");
        assert_eq!(sleeps.len(), 2);

        let (stop, events, _) = wait_for(
            vec![
                resolved(7),
                tracked(by_alice(open(), Some("CHANGES_REQUESTED"))),
            ],
            &[Condition::Approved],
            None,
        );
        assert_eq!(stop, Stop::Unmet);
        assert_eq!(events[1]["reasons"][0]["reason"], "changes_requested");

        // A comment after your approval does not take it back.
        let mut commented = by_alice(open(), Some("APPROVED"));
        commented["reviews"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "author": {"login": "me"},
                "state": "COMMENTED",
                "submittedAt": "2026-09-03T00:00:00Z",
            }));
        let (stop, events, _) = wait_for(
            vec![resolved(7), tracked(commented)],
            &[Condition::Approved],
            None,
        );
        assert_eq!(stop, Stop::Met);
        assert_eq!(events[1]["pr"]["my_review"], "APPROVED");
    }

    #[test]
    fn mergeable_counts_approvals_from_others_on_your_pr_and_yours_on_theirs() {
        let green = || node(7, "OPEN", "SUCCESS", "MERGEABLE");
        // Your PR, approved by bob; no branch protection, so no decision.
        let mut mine = green();
        mine["reviews"] = json!({"nodes": [{
            "author": {"login": "bob"},
            "state": "APPROVED",
            "submittedAt": "2026-09-02T00:00:00Z",
        }]});
        // Someone else's PR you approved, with and without a decision.
        let decided = reviewed(by_alice(green(), Some("APPROVED")), "APPROVED");
        let undecided = by_alice(green(), Some("APPROVED"));
        for (case, pr) in [
            ("mine", mine),
            ("decided", decided),
            ("undecided", undecided),
        ] {
            let (stop, events, sleeps) = wait_for(
                vec![resolved(7), tracked(pr)],
                &[Condition::Mergeable],
                None,
            );
            assert_eq!(stop, Stop::Met, "{case}: {events:?}");
            assert_eq!(events[1]["condition"], "mergeable", "{case}");
            assert_eq!(events[1]["pr"]["category"], "await", "{case}");
            assert!(sleeps.is_empty(), "{case}");
        }
    }

    #[test]
    fn a_plain_follow_still_ends_quietly_when_the_pr_merges() {
        let (stop, events, _) = wait_for(
            vec![
                resolved(7),
                tracked(node(7, "MERGED", "SUCCESS", "MERGEABLE")),
            ],
            &[],
            Some(600),
        );
        assert_eq!(stop, Stop::Done);
        assert_eq!(types(&events), ["ready", "removed"]);
    }
}
