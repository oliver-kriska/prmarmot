//! The records and enums Swift sees, and their conversions to and from core.
//!
//! Field names mirror `cli/schema/board-v1.schema.json` (`prmarmot-cli/board@1`)
//! so that published schema doubles as the documentation for this boundary:
//! anything a coding agent can read out of `--json` has the same name here.
//! Three fields are derived rather than stored — `stale`, `waiting_secs` and
//! `wait_label` are computed from the `now` passed into the fetch, exactly as
//! the CLI computes them — and converting a [`PullRequest`] back into a core
//! row ignores them.

use chrono::{DateTime, Utc};
use prmarmot_core::board as core_board;
use prmarmot_core::cells as core_cells;
use prmarmot_core::layout as core_layout;
use prmarmot_core::pickup;
use prmarmot_core::share as core_share;
use prmarmot_core::size as core_size;

use crate::error::FfiError;

/// Which queue a board shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum Mode {
    /// PRs you authored — the outgoing queue.
    Authored,
    /// PRs waiting for your review — the incoming queue.
    Review,
}

impl From<Mode> for core_board::Mode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Authored => Self::Authored,
            Mode::Review => Self::Review,
        }
    }
}

/// How much of GitHub a board covers. Independent of [`Mode`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum BoardScope {
    AllRepositories,
    Repository { name: String },
}

impl From<BoardScope> for core_board::BoardScope {
    fn from(scope: BoardScope) -> Self {
        match scope {
            BoardScope::AllRepositories => Self::AllRepositories,
            BoardScope::Repository { name } => Self::Repository(name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum Category {
    Action,
    Await,
    Todo,
    Available,
    Done,
    Draft,
}

impl From<core_board::Category> for Category {
    fn from(category: core_board::Category) -> Self {
        match category {
            core_board::Category::Action => Self::Action,
            core_board::Category::Await => Self::Await,
            core_board::Category::Todo => Self::Todo,
            core_board::Category::Available => Self::Available,
            core_board::Category::Done => Self::Done,
            core_board::Category::Draft => Self::Draft,
        }
    }
}

impl From<Category> for core_board::Category {
    fn from(category: Category) -> Self {
        match category {
            Category::Action => Self::Action,
            Category::Await => Self::Await,
            Category::Todo => Self::Todo,
            Category::Available => Self::Available,
            Category::Done => Self::Done,
            Category::Draft => Self::Draft,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum Ci {
    Pass,
    Fail,
    None,
    Running,
}

impl From<core_board::Ci> for Ci {
    fn from(ci: core_board::Ci) -> Self {
        match ci {
            core_board::Ci::Pass => Self::Pass,
            core_board::Ci::Fail => Self::Fail,
            core_board::Ci::None => Self::None,
            core_board::Ci::Running => Self::Running,
        }
    }
}

impl From<Ci> for core_board::Ci {
    fn from(ci: Ci) -> Self {
        match ci {
            Ci::Pass => Self::Pass,
            Ci::Fail => Self::Fail,
            Ci::None => Self::None,
            Ci::Running => Self::Running,
        }
    }
}

/// The aggregate of completed human reviews on an authored PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum ReviewState {
    Changes,
    Approved,
    Commented,
    Waiting,
    None,
}

impl From<core_board::ReviewState> for ReviewState {
    fn from(state: core_board::ReviewState) -> Self {
        match state {
            core_board::ReviewState::Changes => Self::Changes,
            core_board::ReviewState::Approved => Self::Approved,
            core_board::ReviewState::Commented => Self::Commented,
            core_board::ReviewState::Waiting => Self::Waiting,
            core_board::ReviewState::None => Self::None,
        }
    }
}

impl From<ReviewState> for core_board::ReviewState {
    fn from(state: ReviewState) -> Self {
        match state {
            ReviewState::Changes => Self::Changes,
            ReviewState::Approved => Self::Approved,
            ReviewState::Commented => Self::Commented,
            ReviewState::Waiting => Self::Waiting,
            ReviewState::None => Self::None,
        }
    }
}

/// How a PR reached the review queue: GitHub asked you, or it is unclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum Queue {
    Requested,
    Available,
}

impl From<core_board::QueueProvenance> for Queue {
    fn from(queue: core_board::QueueProvenance) -> Self {
        match queue {
            core_board::QueueProvenance::Requested => Self::Requested,
            core_board::QueueProvenance::Available => Self::Available,
        }
    }
}

impl From<Queue> for core_board::QueueProvenance {
    fn from(queue: Queue) -> Self {
        match queue {
            Queue::Requested => Self::Requested,
            Queue::Available => Self::Available,
        }
    }
}

/// One thing keeping an authored PR out of the merge queue, most-blocking
/// first. Core owns the facts and, since the iPad, the wording and the tone
/// too — see [`NotePresentation`]. A front end owns only the colour.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum Blocker {
    NoReviewers { suggested: Vec<String> },
    MergeConflict,
    CiFailing,
    ChangesRequested,
    UnresolvedComments { count: u32 },
}

impl From<&core_board::Blocker> for Blocker {
    fn from(blocker: &core_board::Blocker) -> Self {
        match blocker {
            core_board::Blocker::NoReviewers { suggested } => Self::NoReviewers {
                suggested: suggested.clone(),
            },
            core_board::Blocker::MergeConflict => Self::MergeConflict,
            core_board::Blocker::CiFailing => Self::CiFailing,
            core_board::Blocker::ChangesRequested => Self::ChangesRequested,
            core_board::Blocker::UnresolvedComments(count) => Self::UnresolvedComments {
                count: *count as u32,
            },
        }
    }
}

impl From<&Blocker> for core_board::Blocker {
    fn from(blocker: &Blocker) -> Self {
        match blocker {
            Blocker::NoReviewers { suggested } => Self::NoReviewers {
                suggested: suggested.clone(),
            },
            Blocker::MergeConflict => Self::MergeConflict,
            Blocker::CiFailing => Self::CiFailing,
            Blocker::ChangesRequested => Self::ChangesRequested,
            Blocker::UnresolvedComments { count } => Self::UnresolvedComments(*count as usize),
        }
    }
}

/// One completed review, latest per author.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct Review {
    /// `None` when the reviewer's account is gone; the review still counts.
    pub login: Option<String>,
    pub state: String,
    pub submitted_at: Option<String>,
}

/// The linked ticket found by the configured issue-link rule.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct IssueRef {
    pub key: String,
    pub url: Option<String>,
}

/// A PR's place in a stack of dependent branches.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct StackRef {
    /// The number of the PR at the base of the stack.
    pub number: u64,
    /// How many PRs the stack has.
    pub size: u64,
    pub base_ref: String,
    /// 1-based position, when it is known.
    pub position: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum SizeBand {
    Small,
    Medium,
    Large,
}

impl From<core_size::SizeBand> for SizeBand {
    fn from(band: core_size::SizeBand) -> Self {
        match band {
            core_size::SizeBand::Small => Self::Small,
            core_size::SizeBand::Medium => Self::Medium,
            core_size::SizeBand::Large => Self::Large,
        }
    }
}

/// GitHub's change counts, plus the band they fall in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record,
)]
pub struct ChangeSize {
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    /// Derived from the counts; ignored when converting back.
    pub band: SizeBand,
}

impl From<core_size::ChangeSize> for ChangeSize {
    fn from(size: core_size::ChangeSize) -> Self {
        Self {
            additions: size.additions,
            deletions: size.deletions,
            changed_files: size.changed_files,
            band: size.band().into(),
        }
    }
}

impl From<ChangeSize> for core_size::ChangeSize {
    fn from(size: ChangeSize) -> Self {
        Self {
            additions: size.additions,
            deletions: size.deletions,
            changed_files: size.changed_files,
        }
    }
}

/// One row of a board.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct PullRequest {
    /// GitHub's node id. Stable across refreshes; the key for watches,
    /// snoozes and change snapshots.
    pub id: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: Option<String>,
    pub draft: bool,
    pub category: Category,
    pub queue: Option<Queue>,
    pub ci: Ci,
    pub conflict: bool,
    /// GitHub had not finished computing mergeability, so `conflict == false`
    /// says nothing yet.
    pub mergeable_unknown: bool,
    pub review_decision: Option<String>,
    pub review_state: ReviewState,
    pub requested_reviewers: Vec<String>,
    /// The team slugs among `requested_reviewers`.
    pub requested_teams: Vec<String>,
    pub reviews: Vec<Review>,
    pub my_review: Option<String>,
    pub unresolved_threads: u32,
    pub labels: Vec<String>,
    pub bug: bool,
    pub note: String,
    pub blockers: Vec<Blocker>,
    pub created_at: String,
    pub updated_at: Option<String>,
    /// When the PR started waiting for a reviewer, or `None` when it is not
    /// waiting on anyone.
    pub waiting_since: Option<String>,
    pub head_oid: Option<String>,
    pub reviewed_oid: Option<String>,
    pub reviewed_at: Option<String>,
    pub issue: Option<IssueRef>,
    pub stack: Option<StackRef>,
    pub size: Option<ChangeSize>,
    /// Derived at the `now` of the fetch: seconds this PR has been waiting.
    pub waiting_secs: Option<u64>,
    /// Derived: `waiting_secs` in the app's words ("3d", "<1h").
    pub wait_label: Option<String>,
    /// Derived: waiting at least `stale_after_days`.
    pub stale: bool,
}

impl PullRequest {
    pub(crate) fn from_row(
        row: &core_board::BoardRow,
        now: DateTime<Utc>,
        stale_days: u64,
    ) -> Self {
        let waiting_secs = pickup::waiting_secs(row, now);
        Self {
            id: row.id.clone(),
            repo: row.repo.clone(),
            number: row.number,
            url: row.url.clone(),
            title: row.title.clone(),
            author: row.author.clone(),
            draft: row.draft,
            category: row.category.into(),
            queue: row.queue_provenance.map(Into::into),
            ci: row.ci.into(),
            conflict: row.conflict,
            mergeable_unknown: row.mergeable_unknown,
            review_decision: row.review_decision.clone(),
            review_state: row.review_state.into(),
            requested_reviewers: row.requested.clone(),
            requested_teams: row.requested_teams.clone(),
            reviews: row
                .reviews
                .iter()
                .map(|review| Review {
                    login: review.login.clone(),
                    state: review.state.clone(),
                    submitted_at: review.submitted_at.clone(),
                })
                .collect(),
            my_review: row.my_review.clone(),
            unresolved_threads: row.unresolved as u32,
            labels: row.labels.clone(),
            bug: row.bug,
            note: row.note.clone(),
            blockers: row.blockers.iter().map(Blocker::from).collect(),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
            waiting_since: row.waiting_since.clone(),
            head_oid: row.head_oid.clone(),
            reviewed_oid: row.reviewed_oid.clone(),
            reviewed_at: row.reviewed_at.clone(),
            issue: row.issue.clone().map(|key| IssueRef {
                key,
                url: row.issue_url.clone(),
            }),
            stack: row.stack.as_ref().map(|stack| StackRef {
                number: stack.number,
                size: stack.size,
                base_ref: stack.base_ref_name.clone(),
                position: stack.position,
            }),
            size: row.size.map(Into::into),
            waiting_secs,
            wait_label: waiting_secs.map(pickup::wait_label),
            stale: pickup::is_stale(row, now, stale_days),
        }
    }

    /// Back to a core row. The derived fields are recomputed by whoever needs
    /// them, so they are dropped here rather than trusted.
    pub(crate) fn into_row(self) -> core_board::BoardRow {
        core_board::BoardRow {
            id: self.id,
            repo: self.repo,
            updated_at: self.updated_at,
            head_oid: self.head_oid,
            reviewed_oid: self.reviewed_oid,
            reviewed_at: self.reviewed_at,
            number: self.number,
            url: self.url,
            title: self.title,
            issue: self.issue.as_ref().map(|issue| issue.key.clone()),
            issue_url: self.issue.and_then(|issue| issue.url),
            author: self.author,
            stack: self.stack.map(|stack| core_board::StackInfo {
                number: stack.number,
                size: stack.size,
                base_ref_name: stack.base_ref,
                position: stack.position,
            }),
            queue_provenance: self.queue.map(Into::into),
            draft: self.draft,
            category: self.category.into(),
            bug: self.bug,
            labels: self.labels,
            ci: self.ci.into(),
            conflict: self.conflict,
            mergeable_unknown: self.mergeable_unknown,
            review_decision: self.review_decision,
            review_state: self.review_state.into(),
            requested: self.requested_reviewers,
            requested_teams: self.requested_teams,
            reviews: self
                .reviews
                .into_iter()
                .map(|review| core_board::ReviewSummary {
                    login: review.login,
                    state: review.state,
                    submitted_at: review.submitted_at,
                })
                .collect(),
            my_review: self.my_review,
            unresolved: self.unresolved_threads as usize,
            blockers: self
                .blockers
                .iter()
                .map(core_board::Blocker::from)
                .collect(),
            created_at: self.created_at,
            waiting_since: self.waiting_since,
            size: self.size.map(Into::into),
            note: self.note,
        }
    }
}

pub(crate) fn into_rows(rows: Vec<PullRequest>) -> Vec<core_board::BoardRow> {
    rows.into_iter().map(PullRequest::into_row).collect()
}

/// GitHub's API budget, as the footer shows it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record,
)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    pub cost: u32,
    /// Unix seconds when the budget refills.
    pub reset_epoch: i64,
}

/// What one fetch produced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct Board {
    pub mode: Mode,
    pub rows: Vec<PullRequest>,
    pub rate_limit: Option<RateLimit>,
    /// GitHub has more results than this board shows.
    pub truncated: bool,
    /// `load_more` would fetch another page.
    pub more_pages_available: bool,
    /// The five-page-per-queue cap has been reached; `load_more` is done.
    pub page_limit_reached: bool,
}

/// The configuration that changes what a board says. Mirrors the desktop
/// `config.toml` keys of the same names.
///
/// No field has a UniFFI default, on purpose. A default here would be a second
/// copy of a number that already has a home in `prmarmot-core`, and the two
/// would drift the first time one changed — `bots` in particular changes
/// categorization, which changes what counts as waiting, which changes
/// `is:stale`. Start from [`default_board_settings`] and change what you mean
/// to change.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BoardSettings {
    /// Review authors that never count as human review. An empty list means
    /// nobody is a bot, which is not the same as "use the defaults".
    pub bots: Vec<String>,
    /// Suggested reviewers for the "no reviewers — assign …" note.
    pub default_reviewers: Vec<String>,
    /// Per-owner or per-repository overrides, `owner` or `owner/name` keys.
    pub repo_reviewers: Vec<RepoReviewers>,
    /// A regular expression matched against the title, and a URL template with
    /// an `{id}` placeholder.
    pub issue_link: Option<IssueLink>,
    /// Across all repositories, show only PRs you authored instead of every PR
    /// involving you.
    pub authored_only: bool,
    /// A PR waiting this many days for a reviewer is stale.
    pub stale_after_days: u64,
}

/// The settings the desktop app starts from, straight out of
/// `prmarmot_core::board::BoardConfig::default()`. The one place these numbers
/// live is core; this hands them across unchanged.
#[uniffi::export]
pub fn default_board_settings() -> BoardSettings {
    BoardSettings::default()
}

impl Default for BoardSettings {
    fn default() -> Self {
        let core = core_board::BoardConfig::default();
        Self {
            bots: core.bots,
            default_reviewers: core.default_reviewers,
            repo_reviewers: Vec::new(),
            issue_link: None,
            authored_only: false,
            stale_after_days: core.stale_after_days,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RepoReviewers {
    /// `owner` or `owner/name`, matched case-insensitively.
    pub key: String,
    pub reviewers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct IssueLink {
    pub pattern: String,
    pub url_template: String,
}

impl BoardSettings {
    pub(crate) fn to_core(&self) -> Result<core_board::BoardConfig, FfiError> {
        let issue_link = match &self.issue_link {
            None => None,
            Some(link) => Some(
                core_board::IssueLinkRule::new(&link.pattern, &link.url_template).map_err(|e| {
                    FfiError::invalid(format!("issue_link pattern {:?}: {e}", link.pattern))
                })?,
            ),
        };
        Ok(core_board::BoardConfig {
            bots: self.bots.clone(),
            default_reviewers: self.default_reviewers.clone(),
            repo_reviewers: self
                .repo_reviewers
                .iter()
                .map(|entry| (entry.key.to_ascii_lowercase(), entry.reviewers.clone()))
                .collect(),
            issue_link,
            authored_only: self.authored_only,
            stale_after_days: self.stale_after_days,
        })
    }
}

/// Which band a section header introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SectionKind {
    Approved,
    Category { category: Category },
    Stack,
    Snoozed,
}

impl From<core_layout::SectionKind> for SectionKind {
    fn from(kind: core_layout::SectionKind) -> Self {
        match kind {
            core_layout::SectionKind::Approved => Self::Approved,
            core_layout::SectionKind::Category(category) => Self::Category {
                category: category.into(),
            },
            core_layout::SectionKind::Stack => Self::Stack,
            core_layout::SectionKind::Snoozed => Self::Snoozed,
        }
    }
}

impl From<SectionKind> for core_layout::SectionKind {
    fn from(kind: SectionKind) -> Self {
        match kind {
            SectionKind::Approved => Self::Approved,
            SectionKind::Category { category } => Self::Category(category.into()),
            SectionKind::Stack => Self::Stack,
            SectionKind::Snoozed => Self::Snoozed,
        }
    }
}

/// How rows are ordered inside a section. Section order never changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, uniffi::Enum)]
pub enum Sort {
    #[default]
    Wait,
    Smallest,
}

impl From<Sort> for core_layout::Sort {
    fn from(sort: Sort) -> Self {
        match sort {
            Sort::Wait => Self::Wait,
            Sort::Smallest => Self::Smallest,
        }
    }
}

/// One line of a laid-out board: a header, or a PR by its index in the rows
/// that were passed in.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum BoardItem {
    Header {
        kind: SectionKind,
        label: String,
        /// Row count for a top-level section; `None` for a stack sub-header.
        count: Option<u32>,
        /// Stack coverage, e.g. "2 of 3 layers shown".
        detail: Option<String>,
        /// Member rows in display order, kept even while the group is
        /// collapsed. Empty for a stack sub-header.
        members: Vec<u32>,
    },
    Row {
        index: u32,
    },
}

impl From<core_layout::LayoutItem> for BoardItem {
    fn from(item: core_layout::LayoutItem) -> Self {
        match item {
            core_layout::LayoutItem::Header {
                kind,
                label,
                count,
                detail,
                members,
            } => Self::Header {
                kind: kind.into(),
                label,
                count: count.map(|count| count as u32),
                detail,
                members: members.into_iter().map(|index| index as u32).collect(),
            },
            core_layout::LayoutItem::Row(index) => Self::Row {
                index: index as u32,
            },
        }
    }
}

/// How a shared group is written out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ShareFormat {
    List,
    Markdown,
    Table,
    Urls,
}

impl From<ShareFormat> for core_share::ShareFormat {
    fn from(format: ShareFormat) -> Self {
        match format {
            ShareFormat::List => Self::List,
            ShareFormat::Markdown => Self::Markdown,
            ShareFormat::Table => Self::Table,
            ShareFormat::Urls => Self::Urls,
        }
    }
}

/// What goes on the pasteboard: a rich flavour where there is one, and the
/// plain text every destination can read.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SharePayload {
    pub html: Option<String>,
    pub plain: String,
}

impl From<core_share::SharePayload> for SharePayload {
    fn from(payload: core_share::SharePayload) -> Self {
        Self {
            html: payload.html,
            plain: payload.plain,
        }
    }
}

/// An instant, as Unix seconds, because every front end already has one and
/// UniFFI's own timestamp type would add a conversion nobody needs.
pub(crate) fn instant(epoch_secs: i64) -> Result<DateTime<Utc>, FfiError> {
    DateTime::from_timestamp(epoch_secs, 0)
        .ok_or_else(|| FfiError::invalid(format!("{epoch_secs} is not a point in time")))
}

/// The four search qualifiers. A chip in the search field is one of these
/// plus a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FilterQualifier {
    Label,
    Author,
    Repo,
    /// Only `is:stale` is defined today.
    Is,
}

impl From<prmarmot_core::search::Qualifier> for FilterQualifier {
    fn from(qualifier: prmarmot_core::search::Qualifier) -> Self {
        use prmarmot_core::search::Qualifier;
        match qualifier {
            Qualifier::Label => Self::Label,
            Qualifier::Author => Self::Author,
            Qualifier::Repo => Self::Repo,
            Qualifier::Is => Self::Is,
        }
    }
}

impl From<FilterQualifier> for prmarmot_core::search::Qualifier {
    fn from(qualifier: FilterQualifier) -> Self {
        match qualifier {
            FilterQualifier::Label => Self::Label,
            FilterQualifier::Author => Self::Author,
            FilterQualifier::Repo => Self::Repo,
            FilterQualifier::Is => Self::Is,
        }
    }
}

/// One finished qualifier term, as the search field shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FilterChip {
    pub qualifier: FilterQualifier,
    pub value: String,
    /// The term exactly as it would be typed, e.g. `label:"help wanted"`.
    pub term: String,
}

impl From<&prmarmot_core::search::FilterChip> for FilterChip {
    fn from(chip: &prmarmot_core::search::FilterChip) -> Self {
        Self {
            qualifier: chip.qualifier.into(),
            value: chip.value.clone(),
            term: chip.term(),
        }
    }
}

/// The offline cache, as bytes.
///
/// The iPad keeps the last board so a relaunch shows something real while the
/// first fetch runs. It stores it the way it stores the attention state — as
/// opaque bytes this crate writes and reads — rather than as a hand-written
/// Swift mirror of every field, which would be a second definition of a
/// `PullRequest` and would go wrong the first time core gained a field.
///
/// The envelope carries a version, so an old cache from a previous release is
/// discarded rather than half-read.
const CACHE_VERSION: u32 = 1;
/// Bound every cache. A board is a few hundred kilobytes.
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedBoard {
    version: u32,
    /// Unix seconds when this board was fetched, for "synced 3m ago".
    fetched_at: i64,
    board: Board,
}

#[uniffi::export]
pub fn encode_board(board: Board, fetched_at_epoch: i64) -> Result<Vec<u8>, FfiError> {
    let bytes = serde_json::to_vec(&CachedBoard {
        version: CACHE_VERSION,
        fetched_at: fetched_at_epoch,
        board,
    })
    .map_err(|e| FfiError::invalid(format!("could not write the cached board: {e}")))?;
    if bytes.len() > MAX_CACHE_BYTES {
        return Err(FfiError::invalid(format!(
            "the board is {} bytes, past the {MAX_CACHE_BYTES}-byte cache bound",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// A board written by [`encode_board`], with the instant it was fetched.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RestoredBoard {
    pub board: Board,
    pub fetched_at_epoch: i64,
}

#[uniffi::export]
pub fn decode_board(bytes: Vec<u8>) -> Result<RestoredBoard, FfiError> {
    if bytes.len() > MAX_CACHE_BYTES {
        return Err(FfiError::invalid("the cached board is too large to read"));
    }
    let cached: CachedBoard = serde_json::from_slice(&bytes)
        .map_err(|e| FfiError::invalid(format!("could not read the cached board: {e}")))?;
    if cached.version != CACHE_VERSION {
        return Err(FfiError::invalid(format!(
            "the cached board is version {}, and this build writes version {CACHE_VERSION}",
            cached.version
        )));
    }
    Ok(RestoredBoard {
        board: cached.board,
        fetched_at_epoch: cached.fetched_at,
    })
}

/// How alarming a cell is, decided by core so both front ends decide it the
/// same way. Swift maps this to its palette and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum Tone {
    Danger,
    Warning,
    Success,
    Routine,
    Muted,
}

impl From<core_cells::Tone> for Tone {
    fn from(tone: core_cells::Tone) -> Self {
        match tone {
            core_cells::Tone::Danger => Self::Danger,
            core_cells::Tone::Warning => Self::Warning,
            core_cells::Tone::Success => Self::Success,
            core_cells::Tone::Routine => Self::Routine,
            core_cells::Tone::Muted => Self::Muted,
        }
    }
}

/// A Note cell decomposed for exception-first rendering: one emphasised
/// phrase, an optional remedy after an em dash, and the remaining blockers as
/// muted context so none of them hides in the hover text alone.
///
/// The caller shortens `context` when the row is narrow, never `primary`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct NotePresentation {
    pub tone: Tone,
    pub primary: String,
    pub remedy: Option<String>,
    pub context: Vec<String>,
    pub tooltip: String,
}

impl From<core_cells::NotePresentation> for NotePresentation {
    fn from(note: core_cells::NotePresentation) -> Self {
        Self {
            tone: note.tone.into(),
            primary: note.primary,
            remedy: note.remedy,
            context: note.context,
            tooltip: note.tooltip,
        }
    }
}

/// One reviewer's mark in the Review column. The glyph comes first so the
/// state survives truncation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct ReviewMark {
    pub glyph: String,
    pub tone: Tone,
    pub login: String,
}

/// What the Review column says about one row. A completed review supersedes a
/// pending request, and "nobody was asked" is said out loud because an empty
/// cell reads as "not loaded".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum ReviewCell {
    Reviewed {
        marks: Vec<ReviewMark>,
        summary: String,
        hover: String,
    },
    Requested {
        names: String,
        arrow: String,
        suffix: String,
        hover: String,
    },
    NotRequested {
        text: String,
    },
}

impl From<core_cells::ReviewCell> for ReviewCell {
    fn from(cell: core_cells::ReviewCell) -> Self {
        match cell {
            core_cells::ReviewCell::Reviewed {
                marks,
                summary,
                hover,
            } => Self::Reviewed {
                marks: marks
                    .into_iter()
                    .map(|mark| ReviewMark {
                        glyph: mark.glyph,
                        tone: mark.tone.into(),
                        login: mark.login,
                    })
                    .collect(),
                summary,
                hover,
            },
            core_cells::ReviewCell::Requested {
                names,
                arrow,
                suffix,
                hover,
            } => Self::Requested {
                names,
                arrow,
                suffix,
                hover,
            },
            core_cells::ReviewCell::NotRequested { text } => Self::NotRequested { text },
        }
    }
}

/// The CI column: the word and how loud it is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct CiCell {
    pub text: String,
    pub tone: Tone,
}
