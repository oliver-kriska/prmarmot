//! `watch`: poll one view at the refresh cadence and print what changed, one
//! event per line. Changes are measured poll-to-poll in memory, so the stream
//! never consumes or moves PR Marmot's own "changed since you looked" marks.

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use prmarmot_core::attention::{
    semantic_notice, NoticeKind, Observation, SnapshotNamespace, SnapshotStore,
};
use prmarmot_core::board::{
    carry_forward_conflicts, fetch_tracked, resolve_pull_request_id, BoardConfig, BoardFetch,
    BoardPagination, BoardRow, BoardScope, Mode, TrackedPrStatus,
};
use prmarmot_core::github::rate_limit::{backoff_secs, should_back_off, RateLimitInfo};
use prmarmot_core::github::{GhError, GithubTransport};
use prmarmot_local::attention_state::AttentionState;
use serde_json::{json, Value};

use crate::args::PrRef;
use crate::render::{mode_key, pr_json, scope_json, scope_label, view_title};
use crate::term::{Paint, Tone};
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
        Event::Ready { count } => json!({
            "type": "ready",
            "view": ctx.title(),
            "scope": ctx.scope_json(),
            "viewer": ctx.viewer,
            "count": count,
            "interval_secs": ctx.interval.as_secs(),
            "rate_limit": rate_value(rate),
        }),
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
            "pr": {
                "id": seen.id,
                "repo": seen.repo,
                "number": seen.number,
                "url": seen.url,
                "title": seen.title,
            },
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
                Some(_) => format!("Watching {} · every {every}{budget}", ctx.title()),
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
}

/// Why the loop stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// `--events N` reached, or the reader closed the pipe.
    Done,
    /// The very first fetch failed; there is no baseline to watch from.
    Failed(GhError),
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

pub fn run(session: Session, out: &mut dyn Write, sleep: &mut dyn FnMut(Duration)) -> Stop {
    let ctx = Context {
        mode: session.mode,
        scope: &session.scope,
        authored_only: session.board.authored_only,
        pr: session.pr.as_ref(),
        viewer: &session.viewer,
        interval: session.interval,
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
    let mut counted = 0u64;
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
        let mut finished = false;
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
                    Filters::default(),
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
                    events.push(Event::Removed {
                        seen,
                        status: Some(status),
                    });
                    finished = true;
                } else if !removed.is_empty() {
                    let fates = resolve_removals(&session, &removed);
                    events.extend(removed.into_iter().map(|seen| Event::Removed {
                        status: fates.get(&seen.id).copied(),
                        seen,
                    }));
                }
                let mut wait = session.interval;
                if let Some(budget) = rate.as_ref().filter(|r| should_back_off(r)) {
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
            let line = match &session.output {
                Output::Json => event_json(event, &ctx, now, rate.as_ref()).to_string(),
                Output::Text(paint) => event_text(event, &ctx, now, rate.as_ref(), *paint),
            };
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                return Stop::Done;
            }
            if event.is_change() {
                counted += 1;
                if session.max_events.is_some_and(|max| counted >= max) {
                    return Stop::Done;
                }
            }
        }
        if finished {
            return Stop::Done;
        }
        sleep(wait);
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
        }
    }

    /// Run the loop; returns how it stopped, the JSON events, and each sleep.
    fn run_script(session: Session) -> (Stop, Vec<Value>, Vec<Duration>) {
        let mut out = Vec::new();
        let mut sleeps = Vec::new();
        let stop = run(session, &mut out, &mut |wait| sleeps.push(wait));
        let events = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (stop, events, sleeps)
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
}
