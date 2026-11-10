//! `BoardClient` — the one stateful object Swift holds.
//!
//! It owns the foreign transport and token source, the host, and the last
//! fetch for each mode (which is how **Load more** knows where it got to;
//! GraphQL cursors never cross the boundary). Everything else in this crate is
//! a pure function.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use prmarmot_core::board as core_board;
use prmarmot_core::github::{gh_cli, viewer_login};
use prmarmot_core::search::RemoteFilter;

use crate::error::FfiError;
use crate::transport::{self, GithubTransport, TokenSource};
use crate::types::{
    Board, BoardScope, BoardSettings, FetchStep, FilterChip, Mode, PullRequest, RateLimit,
};

/// Told what a running fetch is doing, so the iPad can say "Loading review
/// requests (1 of 2)… 4 s" ([`crate::fetch_progress_text`]) and show rows
/// before the last request answers. Implemented in Swift. Called from the
/// fetch's own async task — in request order, never from the fetch thread —
/// and only while that fetch runs; hop to the main actor before touching UI.
/// Hold the store weakly (the cycle rule in `ffi/README.md`).
#[uniffi::export(with_foreign)]
pub trait FetchObserver: Send + Sync {
    /// A request is about to go out. `retry` is true when GitHub gave up on
    /// the full page and this asks again with fewer rows.
    fn step(&self, step: FetchStep, retry: bool);
    /// Rows a later step will add to or refine: the requested half of the
    /// all-repositories review queue, and every view's rows before
    /// [`FetchStep::Turns`]. Never the result — that is what the method
    /// returns. Show them only to a view with no rows yet, and count,
    /// observe, cache or notify nothing from them (the desktop's rule).
    /// Not called once `reset` has run since the fetch began.
    fn rows(&self, board: Board);
}

/// What became of a PR that is watched or snoozed but not on the board.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TrackedStatus {
    Open,
    Closed,
    Merged,
    /// Deleted, or in a repository this account can no longer see.
    Inaccessible,
}

impl From<core_board::TrackedPrStatus> for TrackedStatus {
    fn from(status: core_board::TrackedPrStatus) -> Self {
        match status {
            core_board::TrackedPrStatus::Open => Self::Open,
            core_board::TrackedPrStatus::Closed => Self::Closed,
            core_board::TrackedPrStatus::Merged => Self::Merged,
            core_board::TrackedPrStatus::Inaccessible => Self::Inaccessible,
        }
    }
}

/// One tracked PR as this refresh found it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TrackedPr {
    pub pr_id: String,
    pub status: TrackedStatus,
    /// The row, when the PR is still visible. Absent for an inaccessible one.
    pub row: Option<PullRequest>,
}

/// The repositories this account can see, and whether the walk hit its bound.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RepoList {
    pub repos: Vec<String>,
    /// True when there are more than the walk is willing to fetch, so the
    /// picker must not pretend it is the whole list.
    pub truncated: bool,
}

/// Who this client is and where it is talking.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ClientConfig {
    /// `github.com` or a GitHub Enterprise Server host. Accepts an
    /// `api.`-prefixed or scheme-prefixed spelling and normalizes it.
    #[uniffi(default = "github.com")]
    pub host: String,
    /// The signed-in login. Every board is relative to a person: "PRs you
    /// authored", "reviews GitHub asked *you* for".
    pub viewer: String,
    /// Sent as `User-Agent` on every request, e.g. `PRMarmot-iPad/1.0`.
    pub user_agent: String,
}

/// The board's data layer, as one object per signed-in account.
#[derive(uniffi::Object)]
pub struct BoardClient {
    transport: Arc<dyn GithubTransport>,
    tokens: Arc<dyn TokenSource>,
    config: ClientConfig,
    /// The last fetch per mode, so `load_more` can continue it.
    last: Mutex<Pages>,
}

/// What `load_more` continues, and which `reset` it belongs to.
#[derive(Default)]
struct Pages {
    /// Bumped by every `reset`. A fetch remembers its board only if no reset
    /// ran while it was on its way: one that started before is for a board
    /// the caller has left, and its cursor is for the scope it left. A Swift
    /// task cancelled at a scope switch does not stop the Rust future under
    /// it, which then lands after the reset (iPad review, 2026-09-21).
    generation: u64,
    /// Three entries at most, one per mode — bounded like every other cache
    /// in this project.
    fetches: BTreeMap<u8, core_board::BoardFetch>,
    /// Modes GitHub gave up on even at the small page, so the next fetch
    /// starts small although nothing was fetched to remember it by. Three
    /// entries at most.
    gave_up: BTreeSet<u8>,
}

/// On the way out of a fetch: whether GitHub gave up on it, for
/// [`BoardClient::settled`].
fn note_give_up(
    flag: &AtomicBool,
    error: prmarmot_core::github::GhError,
) -> prmarmot_core::github::GhError {
    if error.is_query_timeout() {
        flag.store(true, Ordering::Relaxed);
    }
    error
}

#[uniffi::export]
impl BoardClient {
    /// `transport` and `tokens` are yours; this object keeps a strong
    /// reference to both. Neither may keep a strong reference back — see the
    /// cycle rule in `ffi/README.md`.
    #[uniffi::constructor]
    pub fn new(
        config: ClientConfig,
        transport: Arc<dyn GithubTransport>,
        tokens: Arc<dyn TokenSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            transport,
            tokens,
            config,
            last: Mutex::new(Pages::default()),
        })
    }

    /// The login GitHub says this token belongs to. Worth calling once after
    /// sign-in rather than trusting what the user typed.
    pub async fn viewer_login(&self) -> Result<String, FfiError> {
        self.run(|core| viewer_login(core)).await
    }

    /// One complete board. `now_epoch` is Unix seconds and decides every
    /// "waiting 3d" and `is:stale` on the returned rows; core never reads a
    /// clock of its own.
    pub async fn fetch_board(
        &self,
        mode: Mode,
        scope: BoardScope,
        settings: BoardSettings,
        now_epoch: i64,
    ) -> Result<Board, FfiError> {
        let cfg = settings.to_core()?;
        let core_mode: core_board::Mode = mode.into();
        let core_scope: core_board::BoardScope = scope.into();
        let viewer = self.config.viewer.clone();
        let generation = self.generation();
        let small_pages = self.small_pages(core_mode);
        let gave_up = Arc::new(AtomicBool::new(false));
        let flag = gave_up.clone();
        let fetched = self
            .run(move |core| {
                core_board::fetch_view(
                    core,
                    core_mode,
                    &core_scope,
                    &viewer,
                    &cfg,
                    &RemoteFilter::default(),
                    core_board::Tracked::default(),
                    small_pages,
                )
                .map_err(|error| note_give_up(&flag, error))
            })
            .await;
        let fetched = self.settled(core_mode, generation, &gave_up, fetched)?;
        self.remember(core_mode, &fetched, generation);
        board(mode, fetched, now_epoch, settings.stale_after_days)
    }

    /// One more page for each queue that still has one, merged into the board
    /// this client last fetched for `mode`. Returns the whole board again, so
    /// a caller can replace its rows rather than splice them.
    ///
    /// Fails with [`FfiError::Invalid`] when nothing has been fetched yet:
    /// there is no page to come after.
    pub async fn load_more(
        &self,
        mode: Mode,
        scope: BoardScope,
        settings: BoardSettings,
        now_epoch: i64,
    ) -> Result<Board, FfiError> {
        let cfg = settings.to_core()?;
        let core_mode: core_board::Mode = mode.into();
        let core_scope: core_board::BoardScope = scope.into();
        let viewer = self.config.viewer.clone();
        let generation = self.generation();
        let current = self.recall(core_mode).ok_or_else(|| {
            FfiError::invalid("load_more needs a board to continue; fetch one first")
        })?;
        let fetched = self
            .run(move |core| {
                core_board::fetch_more_board_scoped(
                    core,
                    core_mode,
                    &core_scope,
                    &viewer,
                    &cfg,
                    &current,
                )
            })
            .await?;
        self.remember(core_mode, &fetched, generation);
        board(mode, fetched, now_epoch, settings.stale_after_days)
    }

    /// One board plus whatever watched or snoozed PRs are not on it, in a
    /// single GraphQL operation — the desktop's batched refresh. `tracked_ids`
    /// comes from `AttentionStore.trackedIds`.
    pub async fn fetch_board_tracking(
        &self,
        mode: Mode,
        scope: BoardScope,
        settings: BoardSettings,
        now_epoch: i64,
        tracked_ids: Vec<String>,
    ) -> Result<TrackedBoard, FfiError> {
        self.fetch_tracking(
            mode,
            scope.into(),
            Vec::new(),
            settings,
            now_epoch,
            tracked_ids,
            None,
        )
        .await
    }

    /// [`Self::fetch_board_tracking`], telling `observer` which request it
    /// waits for and handing it rows as soon as part of the board is known.
    /// Sends exactly the same requests.
    pub async fn fetch_board_tracking_with_progress(
        &self,
        mode: Mode,
        scope: BoardScope,
        settings: BoardSettings,
        now_epoch: i64,
        tracked_ids: Vec<String>,
        observer: Arc<dyn FetchObserver>,
    ) -> Result<TrackedBoard, FfiError> {
        self.fetch_tracking(
            mode,
            scope.into(),
            Vec::new(),
            settings,
            now_epoch,
            tracked_ids,
            Some(observer),
        )
        .await
    }

    /// All open for one repository, with the `label:` and `author:` chips in
    /// `filter` answered by GitHub so the rows, `total` and Load more cover
    /// the whole repository. Every other chip filters only the loaded rows —
    /// `local_only_terms` names them and `all_open_local_filter_notice` says
    /// so. Call it again when the committed chips change, never per keystroke;
    /// `load_more(.allOpen, …)` continues whichever search ran last.
    pub async fn fetch_all_open(
        &self,
        repository: String,
        filter: Vec<FilterChip>,
        settings: BoardSettings,
        now_epoch: i64,
        tracked_ids: Vec<String>,
    ) -> Result<TrackedBoard, FfiError> {
        self.fetch_tracking(
            Mode::AllOpen,
            core_board::BoardScope::Repository(repository),
            filter,
            settings,
            now_epoch,
            tracked_ids,
            None,
        )
        .await
    }

    /// [`Self::fetch_all_open`] with progress, as
    /// [`Self::fetch_board_tracking_with_progress`].
    pub async fn fetch_all_open_with_progress(
        &self,
        repository: String,
        filter: Vec<FilterChip>,
        settings: BoardSettings,
        now_epoch: i64,
        tracked_ids: Vec<String>,
        observer: Arc<dyn FetchObserver>,
    ) -> Result<TrackedBoard, FfiError> {
        self.fetch_tracking(
            Mode::AllOpen,
            core_board::BoardScope::Repository(repository),
            filter,
            settings,
            now_epoch,
            tracked_ids,
            Some(observer),
        )
        .await
    }

    /// Every repository this account is involved in, for the picker. A REST
    /// walk bounded at ten pages of a hundred, exactly as the desktop bounds
    /// it, and deliberately not a GraphQL point.
    pub async fn discover_repos(&self) -> Result<RepoList, FfiError> {
        let found = transport::run_rest(
            self.transport.clone(),
            self.tokens.clone(),
            &self.config.host,
            &self.config.user_agent,
            move |rest| {
                gh_cli::discover_repos_with(|page| {
                    rest.rest_get(
                        "user/repos",
                        &[
                            ("affiliation", "owner,collaborator,organization_member"),
                            ("per_page", "100"),
                            ("sort", "pushed"),
                            ("page", &page.to_string()),
                        ],
                    )
                })
            },
        )
        .await?;
        Ok(RepoList {
            repos: found.repos,
            truncated: found.truncated,
        })
    }

    /// Whether [`BoardClient::load_more`] would fetch anything for `mode`.
    /// False before the first fetch.
    pub fn has_more(&self, mode: Mode) -> bool {
        let core_mode: core_board::Mode = mode.into();
        self.last
            .lock()
            .expect("board cache")
            .fetches
            .get(&key(core_mode))
            .is_some_and(|fetch| fetch.pagination.can_load_more(core_mode))
    }

    /// Forget the pagination state, e.g. when the scope or the account
    /// changes. The next `fetch_board` starts from page one regardless; this
    /// only stops `load_more` continuing a board the user has left — including
    /// one a fetch still on its way would otherwise remember when it lands.
    pub fn reset(&self) {
        let mut pages = self.last.lock().expect("board cache");
        pages.generation = pages.generation.wrapping_add(1);
        pages.fetches.clear();
        pages.gave_up.clear();
    }
}

impl BoardClient {
    async fn run<T, F>(&self, work: F) -> Result<T, FfiError>
    where
        F: FnOnce(
                &dyn prmarmot_core::github::GithubTransport,
            ) -> Result<T, prmarmot_core::github::GhError>
            + Send
            + 'static,
        T: Send + 'static,
    {
        transport::run(
            self.transport.clone(),
            self.tokens.clone(),
            &self.config.host,
            &self.config.user_agent,
            work,
        )
        .await
    }

    /// The tracked fetches, with or without an observer: core's
    /// `fetch_view_with_progress`, which for All open is the same search and
    /// follow-up as `fetch_all_open` (the chips go only with All open).
    #[allow(clippy::too_many_arguments)]
    async fn fetch_tracking(
        &self,
        mode: Mode,
        scope: core_board::BoardScope,
        filter: Vec<FilterChip>,
        settings: BoardSettings,
        now_epoch: i64,
        tracked_ids: Vec<String>,
        observer: Option<Arc<dyn FetchObserver>>,
    ) -> Result<TrackedBoard, FfiError> {
        let cfg = settings.to_core()?;
        let core_mode: core_board::Mode = mode.into();
        let remote = RemoteFilter::from_chips(&core_chips(&filter));
        let viewer = self.config.viewer.clone();
        let generation = self.generation();
        let small_pages = self.small_pages(core_mode);
        let gave_up = Arc::new(AtomicBool::new(false));
        let flag = gave_up.clone();
        let stale_after_days = settings.stale_after_days;
        let on_progress = |event: core_board::FetchProgress| {
            let Some(observer) = &observer else {
                return;
            };
            match event {
                core_board::FetchProgress::Step { step, retry } => {
                    observer.step(step.into(), retry);
                }
                core_board::FetchProgress::Rows(fetched) => {
                    // Rows of a board the caller has left are not shown.
                    if self.generation() != generation {
                        return;
                    }
                    if let Ok(early) = board(mode, *fetched, now_epoch, stale_after_days) {
                        observer.rows(early);
                    }
                }
            }
        };
        let fetched = transport::run_with_progress(
            self.transport.clone(),
            self.tokens.clone(),
            &self.config.host,
            &self.config.user_agent,
            move |core, report| {
                core_board::fetch_view_with_progress(
                    core,
                    core_mode,
                    &scope,
                    &viewer,
                    &cfg,
                    &remote,
                    core_board::Tracked::rows(&tracked_ids),
                    small_pages,
                    report,
                )
                .map_err(|error| note_give_up(&flag, error))
            },
            on_progress,
        )
        .await;
        let fetched = self.settled(core_mode, generation, &gave_up, fetched)?;
        self.tracked_board(mode, fetched, now_epoch, &settings, generation)
    }

    /// Which `reset` a fetch starting now belongs to.
    fn generation(&self) -> u64 {
        self.last.lock().expect("board cache").generation
    }

    fn tracked_board(
        &self,
        mode: Mode,
        fetched: core_board::BoardFetch,
        now_epoch: i64,
        settings: &BoardSettings,
        generation: u64,
    ) -> Result<TrackedBoard, FfiError> {
        self.remember(mode.into(), &fetched, generation);
        let now = crate::types::instant(now_epoch)?;
        let tracked = fetched
            .tracked
            .iter()
            .map(|tracked| TrackedPr {
                pr_id: tracked.pr_id.clone(),
                status: tracked.status.into(),
                row: tracked
                    .row
                    .as_ref()
                    .map(|row| PullRequest::from_row(row, now, settings.stale_after_days)),
            })
            .collect();
        Ok(TrackedBoard {
            board: board(mode, fetched, now_epoch, settings.stale_after_days)?,
            tracked,
        })
    }

    /// Keep `fetched` for `load_more`, unless a reset ran since `generation`.
    fn remember(&self, mode: core_board::Mode, fetched: &core_board::BoardFetch, generation: u64) {
        let mut pages = self.last.lock().expect("board cache");
        if pages.generation == generation {
            pages.fetches.insert(key(mode), fetched.clone());
        }
    }

    /// Whether GitHub gave up on a full page for this mode since the last
    /// `reset`, so the next fetch asks for the smaller page straight away.
    fn small_pages(&self, mode: core_board::Mode) -> bool {
        let pages = self.last.lock().expect("board cache");
        pages.gave_up.contains(&key(mode))
            || pages
                .fetches
                .get(&key(mode))
                .is_some_and(|fetched| fetched.pagination.small_pages())
    }

    /// A fetch GitHub gave up on even at the small page is remembered for
    /// `mode` although it brought no board, so the next one starts small —
    /// what the desktop's `mark_small_pages` and the CLI's
    /// `small_pages::remember` do on the same error. A reset meanwhile means
    /// the caller has left that board, and nothing is kept.
    fn settled<T>(
        &self,
        mode: core_board::Mode,
        generation: u64,
        gave_up: &AtomicBool,
        result: Result<T, FfiError>,
    ) -> Result<T, FfiError> {
        if gave_up.load(Ordering::Relaxed) {
            let mut pages = self.last.lock().expect("board cache");
            if pages.generation == generation {
                pages.gave_up.insert(key(mode));
            }
        }
        result
    }

    fn recall(&self, mode: core_board::Mode) -> Option<core_board::BoardFetch> {
        self.last
            .lock()
            .expect("board cache")
            .fetches
            .get(&key(mode))
            .cloned()
    }
}

/// A board and the tracked PRs that came with it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TrackedBoard {
    pub board: Board,
    pub tracked: Vec<TrackedPr>,
}

fn key(mode: core_board::Mode) -> u8 {
    match mode {
        core_board::Mode::Authored => 0,
        core_board::Mode::Review => 1,
        core_board::Mode::AllOpen => 2,
    }
}

fn core_chips(chips: &[FilterChip]) -> Vec<prmarmot_core::search::FilterChip> {
    chips
        .iter()
        .map(|chip| prmarmot_core::search::FilterChip::new(chip.qualifier.into(), &chip.value))
        .collect()
}

fn board(
    mode: Mode,
    fetched: core_board::BoardFetch,
    now_epoch: i64,
    stale_after_days: u64,
) -> Result<Board, FfiError> {
    let now = crate::types::instant(now_epoch)?;
    let core_mode: core_board::Mode = mode.into();
    Ok(Board {
        mode,
        rows: fetched
            .rows
            .iter()
            .map(|row| PullRequest::from_row(row, now, stale_after_days))
            .collect(),
        rate_limit: fetched.rate.as_ref().map(|rate| RateLimit {
            limit: rate.limit,
            remaining: rate.remaining,
            cost: rate.cost,
            // `None` only when GitHub sent something that is not a timestamp;
            // 0 then reads as "unknown" rather than inventing a reset time.
            reset_epoch: rate.reset_epoch().unwrap_or_default() as i64,
        }),
        truncated: fetched.truncated,
        more_pages_available: fetched.pagination.can_load_more(core_mode),
        page_limit_reached: fetched.pagination.page_limit_reached(core_mode),
        access_notice: prmarmot_core::status::access_notice(&fetched.access),
        total: fetched.total,
    })
}
