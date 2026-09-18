use std::sync::{Arc, Mutex};

use prmarmot_ffi::{
    config_from_toml, config_to_toml, copy_items, default_app_config, detail_lines, AttentionStore,
    AuthConfig, BoardClient, BoardScope, BoardSettings, Ci, ClientConfig, FfiError,
    GithubTransport, GraphqlRequest, Header, HttpResponse, IssueLink, Mode, PullRequest,
    RestRequest, Review, SnoozeChoice, TokenSource, WatchStatus,
};

/// 2026-07-26T12:00:00Z, the instant the goldens are pinned at.
const NOW: i64 = 1_785_067_200;
const HOUR: i64 = 3_600;

/// The same recorded GitHub response the goldens use, so the rows under test
/// are real rows and not a hand-typed record that can drift from one.
struct Offline(String);

#[async_trait::async_trait]
impl GithubTransport for Offline {
    async fn send(&self, _request: GraphqlRequest) -> Result<HttpResponse, FfiError> {
        Ok(HttpResponse {
            status: 200,
            headers: vec![Header {
                name: "x-ratelimit-remaining".into(),
                value: "4999".into(),
            }],
            body: self.0.clone(),
        })
    }

    async fn get(&self, _request: RestRequest) -> Result<HttpResponse, FfiError> {
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: "[]".into(),
        })
    }
}

struct Token;

#[async_trait::async_trait]
impl TokenSource for Token {
    async fn token(&self) -> Result<String, FfiError> {
        Ok("ghu_offline".into())
    }
}

/// Every row of the authored fixture, derived at `NOW`.
fn rows() -> Vec<PullRequest> {
    static ROWS: Mutex<Option<Vec<PullRequest>>> = Mutex::new(None);
    let mut cached = ROWS.lock().unwrap();
    if let Some(rows) = cached.as_ref() {
        return rows.clone();
    }
    let body = std::fs::read_to_string(format!(
        "{}/../core/tests/fixtures/authored_response.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("the authored fixture");
    let client = BoardClient::new(
        ClientConfig {
            host: "github.com".into(),
            viewer: "me".into(),
            user_agent: "prmarmot-ffi-test/0".into(),
        },
        Arc::new(Offline(body)),
        Arc::new(Token),
    );
    let board = pollster(client.fetch_board(
        Mode::Authored,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        BoardSettings {
            stale_after_days: 3,
            ..prmarmot_ffi::default_board_settings()
        },
        NOW,
    ));
    let rows = board.expect("the fixture board").rows;
    *cached = Some(rows.clone());
    rows
}

/// The tests are synchronous and the boundary is async; this is the smallest
/// thing that bridges the two without pulling in a runtime (the guardrail).
fn pollster<T>(future: impl std::future::Future<Output = T>) -> T {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    static AWAKE: AtomicBool = AtomicBool::new(true);
    fn wake(_: *const ()) {
        AWAKE.store(true, Ordering::SeqCst);
    }
    const VTABLE: RawWakerVTable =
        RawWakerVTable::new(|p| RawWaker::new(p, &VTABLE), wake, wake, |_| {});
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::yield_now();
    }
}

/// A real fixture row wearing a predictable id, so a test can say which PR it
/// means without depending on the order of the recorded response.
fn row(number: u64) -> PullRequest {
    let mut row = rows()[(number as usize) % rows().len()].clone();
    row.id = format!("PR_{number}");
    row.number = number;
    row.url = format!("https://github.com/acme/widgets/pull/{number}");
    row
}

fn store() -> AttentionStore {
    AttentionStore::new("github.com".into(), "me".into())
}

#[test]
fn watching_a_pr_is_a_toggle_and_the_list_is_oldest_first() {
    let store = store();
    assert!(!store.is_watched("PR_1".into()));

    let first = store.toggle_watch(row(1));
    assert!(first.watching);
    assert!(first.evicted.is_none());
    assert!(store.is_watched("PR_1".into()));

    store.toggle_watch(row(2));
    let watches = store.watches();
    assert_eq!(watches.len(), 2);
    assert_eq!(watches[0].number, 1, "the oldest watch is first");
    assert_eq!(watches[1].pr_id, "PR_2");
    assert!(!watches[1].title.is_empty());
    assert_eq!(watches[0].status, WatchStatus::Open);

    let off = store.toggle_watch(row(1));
    assert!(!off.watching);
    assert!(!store.is_watched("PR_1".into()));
    assert_eq!(store.watches().len(), 1);
}

#[test]
fn the_fiftieth_watch_evicts_the_first() {
    let store = store();
    for number in 1..=50 {
        assert!(store.toggle_watch(row(number)).evicted.is_none());
    }
    assert_eq!(store.watches().len(), 50);

    let evicted = store.toggle_watch(row(51)).evicted;
    assert_eq!(evicted.map(|watch| watch.number), Some(1));
    assert_eq!(store.watches().len(), 50, "the bound holds");
    assert!(!store.is_watched("PR_1".into()));
    assert!(store.is_watched("PR_51".into()));
}

#[test]
fn a_watch_remembers_what_became_of_the_pr() {
    let store = store();
    store.toggle_watch(row(1));
    assert!(store.update_watch_status("PR_1".into(), WatchStatus::Merged));
    assert!(
        !store.update_watch_status("PR_1".into(), WatchStatus::Merged),
        "saying it twice is not news"
    );
    assert_eq!(store.watches()[0].status, WatchStatus::Merged);
}

#[test]
fn a_watch_can_be_dropped_from_the_list_without_its_row() {
    let store = store();
    store.toggle_watch(row(1));
    store.toggle_watch(row(2));
    assert!(store.unwatch("PR_1".into()));
    assert!(!store.is_watched("PR_1".into()));
    assert_eq!(store.watches().len(), 1, "only that one");
    assert!(!store.unwatch("PR_1".into()), "twice is not an unwatch");
}

#[test]
fn a_timed_snooze_wakes_when_its_hour_is_up_and_not_before() {
    let store = store();
    store.snooze(row(1), SnoozeChoice::OneHour, NOW);
    assert!(store.is_snoozed("PR_1".into()));
    assert_eq!(
        store.snooze_description("PR_1".into()),
        Some("Snoozed until 2026-07-26 13:00 UTC".into())
    );

    assert!(store.wake_due(vec![row(1)], NOW + HOUR - 1).is_empty());
    assert_eq!(
        store.wake_due(vec![row(1)], NOW + HOUR),
        vec!["PR_1".to_string()]
    );
    assert!(!store.is_snoozed("PR_1".into()));
}

#[test]
fn until_tomorrow_is_a_day_and_the_menu_words_are_the_desktops() {
    let store = store();
    store.snooze(row(1), SnoozeChoice::UntilTomorrow, NOW);
    assert_eq!(
        store.snooze_description("PR_1".into()),
        Some("Snoozed until 2026-07-27 12:00 UTC".into())
    );

    store.snooze(
        row(2),
        SnoozeChoice::WaitingPerson {
            login: "alice".into(),
        },
        NOW,
    );
    assert_eq!(
        store.snooze_description("PR_2".into()),
        Some("Waiting on alice".into())
    );

    store.snooze(row(3), SnoozeChoice::WaitingCi, NOW);
    assert_eq!(
        store.snooze_description("PR_3".into()),
        Some("Waiting for CI to finish".into())
    );

    store.snooze(row(4), SnoozeChoice::ReviewAgainWhenChanged, NOW);
    assert_eq!(
        store.snooze_description("PR_4".into()),
        Some("Review again when changed".into())
    );
}

#[test]
fn waiting_for_ci_wakes_on_a_terminal_state_and_not_on_a_running_one() {
    let store = store();
    let mut pending = row(3);
    pending.ci = Ci::Running;
    store.snooze(pending.clone(), SnoozeChoice::WaitingCi, NOW);

    let mut still_running = pending.clone();
    still_running.ci = Ci::Running;
    assert!(store.wake_due(vec![still_running], NOW + HOUR).is_empty());

    let mut failed = pending;
    failed.ci = Ci::Fail;
    assert_eq!(
        store.wake_due(vec![failed], NOW + HOUR),
        vec!["PR_3".to_string()],
        "a red build is exactly what was being waited for"
    );
}

#[test]
fn waiting_on_a_person_wakes_only_on_a_newer_review_from_them() {
    let store = store();
    let mut pr = row(1);
    pr.reviews = vec![Review {
        login: Some("alice".into()),
        state: "COMMENTED".into(),
        submitted_at: Some("2026-07-20T09:00:00Z".into()),
    }];
    store.snooze(
        pr.clone(),
        SnoozeChoice::WaitingPerson {
            login: "alice".into(),
        },
        NOW,
    );

    // Somebody else reviewing is not what was asked for.
    let mut other = pr.clone();
    other.reviews.push(Review {
        login: Some("bob".into()),
        state: "APPROVED".into(),
        submitted_at: Some("2026-07-27T09:00:00Z".into()),
    });
    assert!(store.wake_due(vec![other], NOW + HOUR).is_empty());

    let mut answered = pr;
    answered.reviews.push(Review {
        login: Some("alice".into()),
        state: "APPROVED".into(),
        submitted_at: Some("2026-07-27T09:00:00Z".into()),
    });
    assert_eq!(
        store.wake_due(vec![answered], NOW + HOUR),
        vec!["PR_1".to_string()]
    );
}

#[test]
fn there_is_no_waiting_on_yourself() {
    let store = store();
    let mut pr = row(1);
    pr.author = Some("alice".into());
    assert_eq!(store.waiting_on(pr.clone()), Some("alice".into()));
    pr.author = Some("ME".into());
    assert_eq!(store.waiting_on(pr.clone()), None, "logins ignore case");
    pr.author = None;
    assert_eq!(store.waiting_on(pr), None);
}

#[test]
fn a_snooze_can_be_cancelled_by_hand() {
    let store = store();
    store.snooze(row(1), SnoozeChoice::UntilTomorrow, NOW);
    assert!(store.cancel_snooze("PR_1".into()));
    assert!(!store.is_snoozed("PR_1".into()));
    assert!(!store.cancel_snooze("PR_1".into()), "twice is not a cancel");
}

#[test]
fn watches_and_snoozes_survive_a_write_and_a_read() {
    let store = store();
    store.toggle_watch(row(1));
    store.snooze(row(2), SnoozeChoice::OneHour, NOW);
    store.observe(row(3)).unwrap();
    let bytes = store.to_bytes().unwrap();

    let again =
        AttentionStore::from_bytes("github.com".into(), "me".into(), bytes.clone()).unwrap();
    assert!(again.is_watched("PR_1".into()));
    assert!(again.is_snoozed("PR_2".into()));
    assert_eq!(again.count(), 1);
    assert!(again.storage_error().is_none());

    // Another account's file is refused, not merged: a "changed" marker from
    // someone else's view of a PR is not a fact about yours.
    assert!(AttentionStore::from_bytes("github.com".into(), "someone-else".into(), bytes).is_err());
}

#[test]
fn tracked_ids_covers_watches_and_snoozes_and_says_how_many_there_are() {
    let store = store();
    for number in 1..=4 {
        store.toggle_watch(row(number));
    }
    store.snooze(row(9), SnoozeChoice::OneHour, NOW);

    let all = store.tracked_ids(10, 0);
    assert_eq!(all.total, 5);
    assert_eq!(all.ids.len(), 5);

    // More tracked than one refresh will fetch: a window that moves, so a long
    // list is covered over several refreshes instead of never.
    let first = store.tracked_ids(2, 0);
    assert_eq!(first.total, 5);
    assert_eq!(first.ids, vec!["PR_1".to_string(), "PR_2".to_string()]);
    let second = store.tracked_ids(2, 2);
    assert_eq!(second.ids, vec!["PR_3".to_string(), "PR_4".to_string()]);
}

#[test]
fn a_config_file_round_trips_through_the_boundary() {
    let mut config = default_app_config();
    config.repo = Some("acme/widgets".into());
    config.scope = Some("repo".into());
    config.pinned_repos = vec!["acme/widgets".into()];
    config.refresh_secs = Some(600);
    config.theme = Some("dark".into());
    config.view = Some("review".into());
    config.default_reviewers = vec!["alice".into()];
    config.stale_after_days = Some(5);
    config.notifications = false;
    config.issue_link = Some(IssueLink {
        pattern: "PROJ-[0-9]+".into(),
        url_template: "https://tracker.example.test/issues/{id}".into(),
    });
    config.auth = Some(AuthConfig {
        host: Some("git.acme.test".into()),
        client_id: Some("Iv1.example".into()),
        mode: Some("device".into()),
        store: None,
    });

    let text = config_to_toml(config.clone()).unwrap();
    assert!(text.contains("repo = \"acme/widgets\""));
    assert!(text.contains("[auth]"));

    let again = config_from_toml(text).unwrap();
    assert_eq!(again, config);
}

#[test]
fn a_desktop_config_imports_with_the_keys_this_front_end_does_not_use_intact() {
    let desktop = r#"
repo = "acme/widgets"
refresh_secs = 300
automatic_update_checks = true

[window]
width = 1440.0
height = 900.0
"#;
    let imported = config_from_toml(desktop.into()).unwrap();
    assert_eq!(imported.repo.as_deref(), Some("acme/widgets"));
    let window = imported
        .window
        .clone()
        .expect("the window size survived the import");
    assert_eq!(window.width, 1440.0);

    // Exporting it again must not have quietly deleted the desktop's keys.
    let exported = config_to_toml(imported).unwrap();
    assert!(
        exported.contains("[window]"),
        "re-export dropped [window]:\n{exported}"
    );
    assert!(exported.contains("width = 1440.0"));
}

#[test]
fn nonsense_in_a_config_file_is_an_error_and_not_a_default() {
    assert!(config_from_toml("refresh_secs = \"whenever\"".into()).is_err());
    assert!(config_from_toml("= = =".into()).is_err());
}

#[test]
fn an_empty_config_writes_only_what_it_has() {
    let text = config_to_toml(default_app_config()).unwrap();
    assert!(!text.contains("repo ="));
    assert!(!text.contains("[auth]"));
    assert!(text.contains("notifications = true"));
}

#[test]
fn a_config_from_the_boundary_matches_the_defaults_core_hands_over() {
    let config = default_app_config();
    assert_eq!(
        config.refresh_secs, None,
        "unset means the desktop's default"
    );
    assert!(config.notifications);
    assert!(config.dock_badge);
    assert_eq!(
        config,
        config_from_toml(config_to_toml(config.clone()).unwrap()).unwrap()
    );
}

#[test]
fn observing_a_row_reports_what_changed_and_the_marker_clears_on_acknowledgement() {
    let store = store();
    let first = store.observe(row(1)).unwrap();
    assert!(
        !first.changed,
        "a first sighting is a baseline, not a change"
    );

    let mut updated = row(1);
    updated.head_oid = Some("def456".into());
    let second = store.observe(updated).unwrap();
    assert!(second.changed);
    assert!(
        second
            .changes
            .iter()
            .any(|phrase| phrase.contains("commit")),
        "expected a phrase about commits, got {:?}",
        second.changes
    );

    assert!(store.acknowledge("PR_1".into()));
    assert!(!store.is_changed("PR_1".into()));
    assert!(store.changes("PR_1".into()).is_empty());
}

#[test]
fn the_backoff_the_footer_counts_down_is_clamped_at_both_ends() {
    // The PRFlow lesson, reachable from Swift: never sooner than a minute,
    // never later than fifteen, whatever GitHub says the reset is.
    assert_eq!(prmarmot_ffi::backoff_secs(Some(1_000), 990), 60);
    assert_eq!(prmarmot_ffi::backoff_secs(Some(1_300), 1_000), 300);
    assert_eq!(prmarmot_ffi::backoff_secs(Some(9_999_999), 1_000), 900);
    assert_eq!(prmarmot_ffi::backoff_secs(None, 1_000), 60);
}

#[test]
fn the_details_panel_and_its_copy_menu_come_from_core_not_from_swift() {
    let row = row(1);
    let lines = detail_lines(row.clone(), Mode::Authored, NOW, 0).unwrap();
    // The first four lines are fixed; the rest depend on the row.
    assert_eq!(lines[0], prmarmot_ffi::strip_note_glyphs(row.note.clone()));
    assert!(lines[1].starts_with("Author: "));
    assert!(lines[1].contains(" · CI: "));
    assert!(lines[2].starts_with("Requested reviewers: "));
    assert!(lines[3].starts_with("Reviews: "));
    assert_eq!(
        lines.last().map(String::as_str),
        Some("Details reflect the loaded snapshot; refresh restarts pagination.")
    );

    let items = copy_items(row.clone(), Mode::Authored, NOW, 0).unwrap();
    assert_eq!(
        items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Copy PR URL",
            "Copy PR number",
            "Copy PR reference",
            "Copy title",
            "Copy all details"
        ]
    );
    assert_eq!(items[0].text, row.url);
    assert_eq!(items[2].text, format!("{}#{}", row.repo, row.number));
    assert!(items[4].text.ends_with(&lines.join("\n")));
}

#[test]
fn the_header_sentence_is_cores_and_names_the_icon_badge_not_a_dock() {
    let summary = prmarmot_ffi::header_summary(prmarmot_ffi::HeaderCounts {
        loaded: 56,
        truncated: true,
        mode: Mode::Review,
        all_repos: true,
        need_you: 3,
        badge: 5,
        badge_complete: true,
        tracked_loaded: 50,
        tracked_total: 64,
    });
    assert_eq!(
        summary.line,
        "56 loaded · partial results · 3 need you · 50 of 64 watched/snoozed"
    );
    assert!(summary.explanation.contains("The app icon badge shows 5"));
    assert!(!summary.explanation.contains("Dock"));
}

#[test]
fn the_ipad_reads_the_same_clock_words_as_the_desktop() {
    assert_eq!(prmarmot_ffi::relative_time(0), "just now");
    assert_eq!(prmarmot_ffi::relative_time(8_100), "2h 15m ago");
    assert_eq!(prmarmot_ffi::human_duration(600), "10m");
    assert_eq!(
        prmarmot_ffi::queue_sync_text(Mode::Authored, true, true, Some(300)),
        "Updating involving PRs… · synced 5m ago"
    );
    assert_eq!(
        prmarmot_ffi::changed_marker_text(vec!["New commits".into()]),
        "Changed since you last selected it: New commits. Select the PR to clear."
    );
}

/// The authored fixture with #105, its one conflicting PR, reported as
/// `mergeable`, fetched as the iPad fetches it.
fn rows_with_105_as(mergeable: &str) -> Vec<PullRequest> {
    let body = std::fs::read_to_string(format!(
        "{}/../core/tests/fixtures/authored_response.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("the authored fixture");
    assert_eq!(
        body.matches("\"CONFLICTING\"").count(),
        1,
        "#105 is the one conflict"
    );
    let body = body.replace("\"CONFLICTING\"", &format!("\"{mergeable}\""));
    let client = BoardClient::new(
        ClientConfig {
            host: "github.com".into(),
            viewer: "me".into(),
            user_agent: "prmarmot-ffi-test/0".into(),
        },
        Arc::new(Offline(body)),
        Arc::new(Token),
    );
    pollster(client.fetch_board(
        Mode::Authored,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        settings(),
        NOW,
    ))
    .expect("the fixture board")
    .rows
}

fn settings() -> BoardSettings {
    BoardSettings {
        stale_after_days: 3,
        ..prmarmot_ffi::default_board_settings()
    }
}

fn number(rows: &[PullRequest], number: u64) -> PullRequest {
    rows.iter()
        .find(|row| row.number == number)
        .cloned()
        .expect("the row")
}

#[test]
fn a_known_conflict_stays_while_github_is_still_deciding() {
    let conflicting = number(&rows(), 105);
    assert!(conflicting.conflict);
    let store = store();
    store.observe(conflicting.clone()).unwrap();

    let unknown = rows_with_105_as("UNKNOWN");
    let deciding = number(&unknown, 105);
    assert!(deciding.mergeable_unknown && !deciding.conflict);
    assert_ne!(
        deciding.note, conflicting.note,
        "GitHub's answer alone drops the conflict"
    );

    let kept = store
        .keep_known_conflicts(unknown.clone(), Mode::Authored, settings(), NOW)
        .unwrap();
    let still = number(&kept, 105);
    assert!(still.conflict);
    assert_eq!(still.note, conflicting.note);
    assert_eq!(still.category, conflicting.category);
    // Nothing else is touched.
    let others = |rows: &[PullRequest]| -> Vec<PullRequest> {
        rows.iter()
            .filter(|row| row.number != 105)
            .cloned()
            .collect()
    };
    assert_eq!(others(&kept), others(&unknown));
}

#[test]
fn github_deciding_or_no_history_leaves_the_row_as_github_says() {
    let store = store();
    // Never seen: an unknown is only an unknown.
    let unknown = rows_with_105_as("UNKNOWN");
    let kept = store
        .keep_known_conflicts(unknown.clone(), Mode::Authored, settings(), NOW)
        .unwrap();
    assert!(!number(&kept, 105).conflict);

    // Seen conflicting, then GitHub decides it merges: the conflict is gone.
    store.observe(number(&rows(), 105)).unwrap();
    let mergeable = rows_with_105_as("MERGEABLE");
    let kept = store
        .keep_known_conflicts(mergeable.clone(), Mode::Authored, settings(), NOW)
        .unwrap();
    assert_eq!(kept, mergeable);
}
