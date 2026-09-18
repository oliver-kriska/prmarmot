//! `BoardClient` — the one stateful object Swift holds.
//!
//! It owns the foreign transport and token source, the host, and the last
//! fetch for each mode (which is how **Load more** knows where it got to;
//! GraphQL cursors never cross the boundary). Everything else in this crate is
//! a pure function.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use prmarmot_core::board as core_board;
use prmarmot_core::github::{gh_cli, viewer_login};

use crate::error::FfiError;
use crate::transport::{self, GithubTransport, TokenSource};
use crate::types::{Board, BoardScope, BoardSettings, Mode, PullRequest, RateLimit};

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
    /// The last fetch per mode, so `load_more` can continue it. Two entries at
    /// most — bounded like every other cache in this project.
    last: Mutex<BTreeMap<u8, core_board::BoardFetch>>,
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
            last: Mutex::new(BTreeMap::new()),
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
        let fetched = self
            .run(move |core| {
                core_board::fetch_board_scoped(core, core_mode, &core_scope, &viewer, &cfg)
            })
            .await?;
        self.remember(core_mode, &fetched);
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
        self.remember(core_mode, &fetched);
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
        let cfg = settings.to_core()?;
        let core_mode: core_board::Mode = mode.into();
        let core_scope: core_board::BoardScope = scope.into();
        let viewer = self.config.viewer.clone();
        let fetched = self
            .run(move |core| {
                core_board::fetch_board_scoped_with_tracked(
                    core,
                    core_mode,
                    &core_scope,
                    &viewer,
                    &cfg,
                    &tracked_ids,
                )
            })
            .await?;
        self.remember(core_mode, &fetched);
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
            .get(&key(core_mode))
            .is_some_and(|fetch| fetch.pagination.can_load_more(core_mode))
    }

    /// Forget the pagination state, e.g. when the scope or the account
    /// changes. The next `fetch_board` starts from page one regardless; this
    /// only stops `load_more` continuing a board the user has left.
    pub fn reset(&self) {
        self.last.lock().expect("board cache").clear();
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

    fn remember(&self, mode: core_board::Mode, fetched: &core_board::BoardFetch) {
        self.last
            .lock()
            .expect("board cache")
            .insert(key(mode), fetched.clone());
    }

    fn recall(&self, mode: core_board::Mode) -> Option<core_board::BoardFetch> {
        self.last
            .lock()
            .expect("board cache")
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
    }
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
    })
}
