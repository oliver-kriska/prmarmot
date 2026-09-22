//! Board display layout shared by every front end (the GPUI table and the
//! CLI): section order and labels, the Approved split, stack sub-groups in
//! dependency order, and the Snoozed group. Pure — no UI types, no I/O — so
//! the window and the terminal can never disagree about what a view shows.

use std::collections::HashSet;

use crate::board::{BoardRow, Category, Mode, ReviewState};
use crate::size::ChangeSize;

/// Which band a header introduces. Stack sub-headers sit inside a band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// PRs awaiting merge that are already approved: your own in My PRs, and
    /// anyone's in All open.
    Approved,
    Category(Category),
    Stack,
    Snoozed,
}

impl SectionKind {
    /// Stable machine key for JSON output.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Category(category) => category.as_str(),
            Self::Stack => "stack",
            Self::Snoozed => "snoozed",
        }
    }
}

/// How rows are ordered inside a section. Section order never changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// Where a section is about getting picked up, the longest wait first.
    #[default]
    Wait,
    /// Review queue only: Requested from you and Available to review list the
    /// smallest size band first, then fewer changed lines, then the longest
    /// wait; rows without a size follow. Other sections sort as for `Wait`.
    Smallest,
}

/// One line of a board view: a header, or a PR as an index into the input rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutItem {
    Header {
        kind: SectionKind,
        label: String,
        /// Row count for top-level sections; `None` for stack sub-headers.
        count: Option<usize>,
        /// Stack coverage ("3 layers", "2 of 3 layers shown"); front ends add
        /// their own wording for the Snoozed group.
        detail: Option<String>,
        /// Top-level sections: member rows in display order, kept even while
        /// the group is collapsed. Empty for stack sub-headers.
        members: Vec<usize>,
    },
    Row(usize),
}

/// [`layout_ordered`] in the default section order.
pub fn layout(
    rows: &[BoardRow],
    mode: Mode,
    all_repos: bool,
    snoozed: &HashSet<String>,
    show_snoozed: bool,
    sort: Sort,
) -> Vec<LayoutItem> {
    layout_ordered(
        rows,
        mode,
        all_repos,
        snoozed,
        show_snoozed,
        sort,
        &SectionOrder::default(),
    )
}

/// Lay out `rows` for `mode`, its sections in `sections` order. Snoozed rows
/// (by PR id) move to a trailing Snoozed group whose members are listed but
/// only emitted as rows when `show_snoozed` is set. Requested from you,
/// Available to review, and Awaiting review list the longest pickup wait
/// first, unless `sort` says otherwise; every other order within a section is
/// the incoming one.
pub fn layout_ordered<'a>(
    rows: &'a [BoardRow],
    mode: Mode,
    all_repos: bool,
    snoozed: &HashSet<String>,
    show_snoozed: bool,
    sort: Sort,
    sections: &SectionOrder,
) -> Vec<LayoutItem> {
    let mut display = Vec::with_capacity(rows.len() + 4);
    let mut order: Vec<usize> = (0..rows.len()).collect();
    let snoozed_order: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&ix| snoozed.contains(&rows[ix].id))
        .collect();
    order.retain(|&ix| !snoozed.contains(&rows[ix].id));
    let key = |row: &'a BoardRow| {
        let kind = section_kind(mode, row);
        let section = sections.rank(kind);
        let approved_action = matches!(mode, Mode::Authored | Mode::AllOpen)
            && row.category == Category::Action
            && row.review_state == ReviewState::Approved;
        // Where a section is about getting picked up, the longest wait leads
        // and rows that are not waiting follow in their incoming order.
        let picked_up_by_wait = matches!(
            kind,
            SectionKind::Category(Category::Todo | Category::Available | Category::Await)
        );
        let wait = row.waiting_since.as_deref().filter(|_| picked_up_by_wait);
        let by_size = sort == Sort::Smallest
            && mode == Mode::Review
            && matches!(row.category, Category::Todo | Category::Available);
        // Without the size sort every row ties here; with it, rows without a
        // size go last.
        let size = row.size.filter(|_| by_size);
        let size = (
            by_size && size.is_none(),
            size.map(ChangeSize::band),
            size.map(ChangeSize::lines),
        );
        (section, !approved_action, size, wait.is_none(), wait)
    };
    order.sort_by(|&a, &b| key(&rows[a]).cmp(&key(&rows[b])));
    let mut i = 0;
    while i < order.len() {
        let kind = section_kind(mode, &rows[order[i]]);
        let start = i;
        while i < order.len() && section_kind(mode, &rows[order[i]]) == kind {
            i += 1;
        }
        display.push(LayoutItem::Header {
            kind,
            label: match kind {
                SectionKind::Approved => "Approved",
                SectionKind::Category(cat) => group_label(mode, cat, all_repos),
                SectionKind::Stack | SectionKind::Snoozed => "Other",
            }
            .into(),
            count: Some(i - start),
            detail: None,
            members: Vec::new(),
        });
        let header_ix = display.len() - 1;
        let mut emitted = HashSet::new();
        for &j in &order[start..i] {
            if let Some(stack) = &rows[j].stack {
                if !emitted.insert((&rows[j].repo, stack.number)) {
                    continue;
                }
                let mut members: Vec<usize> = order[start..i]
                    .iter()
                    .copied()
                    .filter(|&k| {
                        rows[k].repo == rows[j].repo
                            && rows[k]
                                .stack
                                .as_ref()
                                .is_some_and(|s| s.number == stack.number)
                    })
                    .collect();
                members.sort_by_key(|&k| {
                    rows[k]
                        .stack
                        .as_ref()
                        .and_then(|s| s.position)
                        .unwrap_or(u64::MAX)
                });
                display.push(LayoutItem::Header {
                    kind: SectionKind::Stack,
                    label: if all_repos {
                        format!("{} · Stack #{}", rows[j].repo, stack.number)
                    } else {
                        format!("Stack #{}", stack.number)
                    },
                    count: None,
                    detail: Some(if members.len() as u64 == stack.size {
                        format!("{} layers", stack.size)
                    } else {
                        format!("{} of {} layers shown", members.len(), stack.size)
                    }),
                    members: Vec::new(),
                });
                display.extend(members.into_iter().map(LayoutItem::Row));
            } else {
                display.push(LayoutItem::Row(j));
            }
        }
        let emitted: Vec<usize> = display[header_ix + 1..]
            .iter()
            .filter_map(|item| match item {
                LayoutItem::Row(ix) => Some(*ix),
                LayoutItem::Header { .. } => None,
            })
            .collect();
        if let LayoutItem::Header { members, .. } = &mut display[header_ix] {
            *members = emitted;
        }
    }
    if !snoozed_order.is_empty() {
        display.push(LayoutItem::Header {
            kind: SectionKind::Snoozed,
            label: "Snoozed".into(),
            count: Some(snoozed_order.len()),
            detail: None,
            members: snoozed_order.clone(),
        });
        if show_snoozed {
            display.extend(snoozed_order.into_iter().map(LayoutItem::Row));
        }
    }
    display
}

/// Which section a row sits in: its category, except that an approved PR
/// waiting to merge gets the Approved band, and one that nobody was asked to
/// review is Available to review.
fn section_kind(mode: Mode, row: &BoardRow) -> SectionKind {
    if is_approved_section(mode, row) {
        SectionKind::Approved
    } else if is_available_section(mode, row) {
        SectionKind::Category(Category::Available)
    } else {
        SectionKind::Category(row.category)
    }
}

/// The sections a person can put in their own order, in the default one:
/// what is done leads (Approved), then what needs you, then what waits, then
/// drafts. All open holds both a review asked of you and PRs that need action,
/// and the review comes first. Stack sub-headers stay inside their section
/// and Snoozed always trails, so neither is here. Awaiting review and
/// Reviewed never share a view.
pub const ORDERABLE_SECTIONS: [SectionKind; 7] = [
    SectionKind::Approved,
    SectionKind::Category(Category::Todo),
    SectionKind::Category(Category::Action),
    SectionKind::Category(Category::Available),
    SectionKind::Category(Category::Await),
    SectionKind::Category(Category::Done),
    SectionKind::Category(Category::Draft),
];

/// The order sections come in, one order for every view: each view shows the
/// sections it has in this order. Always all of [`ORDERABLE_SECTIONS`], each
/// once; the default is theirs. Written as the JSON section keys
/// (`section_order` in config.toml), so a person, the Settings list and an
/// agent all name a section the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionOrder(Vec<SectionKind>);

impl Default for SectionOrder {
    fn default() -> Self {
        Self(ORDERABLE_SECTIONS.to_vec())
    }
}

impl SectionOrder {
    /// The order `keys` asks for: the sections it names first, in its order,
    /// then the rest in their default order, so an order written before a
    /// section existed still holds every section. Keys are trimmed and match
    /// in any case. Returns, one sentence each, what it ignored: an unknown or
    /// repeated key, or `snoozed`, which always comes last.
    pub fn from_keys<S: AsRef<str>>(keys: &[S]) -> (Self, Vec<String>) {
        let mut chosen: Vec<SectionKind> = Vec::with_capacity(ORDERABLE_SECTIONS.len());
        let mut ignored = Vec::new();
        for key in keys {
            let key = key.as_ref().trim();
            match ORDERABLE_SECTIONS
                .iter()
                .find(|kind| kind.key().eq_ignore_ascii_case(key))
            {
                Some(kind) if chosen.contains(kind) => {
                    ignored.push(format!("ignoring repeated section_order entry {key:?}"));
                }
                Some(kind) => chosen.push(*kind),
                None if key.eq_ignore_ascii_case(SectionKind::Snoozed.key()) => ignored.push(
                    "ignoring section_order entry \"snoozed\": Snoozed always comes last".into(),
                ),
                None => ignored.push(format!(
                    "ignoring section_order entry {key:?}: use {}",
                    ORDERABLE_SECTIONS.map(|kind| kind.key()).join(", ")
                )),
            }
        }
        chosen.extend(
            ORDERABLE_SECTIONS
                .iter()
                .filter(|kind| !chosen.contains(kind))
                .copied()
                .collect::<Vec<_>>(),
        );
        (Self(chosen), ignored)
    }

    /// The sections, first to last.
    pub fn kinds(&self) -> &[SectionKind] {
        &self.0
    }

    /// The JSON keys, first to last: what `section_order` in config.toml holds.
    pub fn keys(&self) -> Vec<&'static str> {
        self.0.iter().map(SectionKind::key).collect()
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// `kind` one place earlier (`earlier`) or later, the Settings list's up
    /// and down buttons. At the top or the bottom, the order stays as it is.
    pub fn moved(&self, kind: SectionKind, earlier: bool) -> Self {
        let mut kinds = self.0.clone();
        if let Some(ix) = kinds.iter().position(|k| *k == kind) {
            let to = if earlier {
                ix.checked_sub(1)
            } else {
                Some(ix + 1).filter(|&to| to < kinds.len())
            };
            if let Some(to) = to {
                kinds.swap(ix, to);
            }
        }
        Self(kinds)
    }

    /// Where a section sorts: its place in the order. Stack sub-headers and
    /// Snoozed are never ranked against the rest, so they come after.
    fn rank(&self, kind: SectionKind) -> usize {
        self.0
            .iter()
            .position(|k| *k == kind)
            .unwrap_or(self.0.len())
    }
}

/// A section's name in the Settings order list, the same in every view it
/// appears in (a header adds detail, e.g. "Available to review · no reviewer
/// requested").
pub fn section_name(kind: SectionKind) -> &'static str {
    match kind {
        SectionKind::Approved => "Approved",
        SectionKind::Category(Category::Todo) => "Requested from you",
        SectionKind::Category(Category::Action) => "Needs action",
        SectionKind::Category(Category::Available) => "Available to review",
        SectionKind::Category(Category::Await) => "Awaiting review",
        SectionKind::Category(Category::Done) => "Reviewed",
        SectionKind::Category(Category::Draft) => "Drafts",
        SectionKind::Stack => "Stack",
        SectionKind::Snoozed => "Snoozed",
    }
}

/// Which views show a section, for the line under its name in the Settings
/// order list: one order covers them all, so the list says where each goes.
pub fn section_views(kind: SectionKind) -> &'static str {
    match kind {
        SectionKind::Approved | SectionKind::Category(Category::Action | Category::Await) => {
            "My PRs, Involving me, All open"
        }
        SectionKind::Category(Category::Todo) => "Review queue, All open",
        SectionKind::Category(Category::Available) => "Review queue, Involving me, All open",
        SectionKind::Category(Category::Done) => "Review queue",
        _ => "Every view",
    }
}

/// PRs waiting to merge that already carry an approval get their own band
/// ahead of Needs action, without changing the core category: in My PRs,
/// Involving me and All open alike.
pub fn is_approved_section(mode: Mode, row: &BoardRow) -> bool {
    matches!(mode, Mode::Authored | Mode::AllOpen)
        && row.category == Category::Await
        && row.review_state == ReviewState::Approved
}

/// A PR waiting with no reviewer requested and no review at all sits under
/// Available to review in Involving me and All open, the Review queue's name
/// for exactly that, rather than under Awaiting review, where it waits on
/// nobody. Only someone else's PR gets here: your own with no reviewer is
/// Needs action (`board::classify_authored`). The core category is unchanged.
///
/// `review_state` leaves your own review out, and GitHub drops your request
/// once you review, so a PR only you reviewed also reads as `None`: your
/// review (`my_review`) keeps it under Awaiting review, as `pickup` does.
pub fn is_available_section(mode: Mode, row: &BoardRow) -> bool {
    matches!(mode, Mode::Authored | Mode::AllOpen)
        && row.category == Category::Await
        && row.review_state == ReviewState::None
        && !matches!(
            row.my_review.as_deref(),
            Some("APPROVED" | "COMMENTED" | "CHANGES_REQUESTED")
        )
}

/// The section label for a category within a mode. One name per section in
/// every view (Oliver, 2026-09-22: "we should be consistent with naming");
/// where other people's PRs sit beside yours, the hover sentence
/// ([`section_explanation`]) says what that means for them. `all_repos` is
/// kept for the callers that pass it alongside.
pub fn group_label(mode: Mode, cat: Category, _all_repos: bool) -> &'static str {
    match (mode, cat) {
        (Mode::Authored | Mode::AllOpen, Category::Action) => "Needs action",
        (Mode::Authored | Mode::AllOpen, Category::Await) => "Awaiting review",
        (Mode::Review | Mode::AllOpen, Category::Todo) => "Requested from you",
        (_, Category::Available) => "Available to review · no reviewer requested",
        (Mode::Review, Category::Done) => "Reviewed",
        (_, Category::Draft) => "Drafts",
        // Unreachable pairings (Action in Review etc.) — a calm fallback.
        _ => "Other",
    }
}

/// What puts a PR in a section, in one sentence: the hover text on a section
/// header, and the iPad's long-press. It is paired with [`group_label`] by
/// the same inputs and follows the derivation in `board.rs`: your own PR
/// with no reviewer is blocked, someone else's is not, and an approved PR
/// that is blocked stays with the blocked ones. `None` for a stack
/// sub-header, which explains itself with its own hover.
pub fn section_explanation(mode: Mode, kind: SectionKind, all_repos: bool) -> Option<String> {
    // The views that hold other people's PRs next to yours.
    let mixed = mode == Mode::AllOpen || (mode == Mode::Authored && all_repos);
    Some(match (mode, kind) {
        (_, SectionKind::Approved) => format!(
            "Approved by at least one reviewer, with no changes requested and nothing blocking \
             it. An approved PR that is blocked stays at the top of {}.",
            group_label(mode, Category::Action, all_repos)
        ),
        (Mode::Authored | Mode::AllOpen, SectionKind::Category(Category::Action)) if mixed => {
            "Something blocks it: a merge conflict, failing CI, requested changes or \
             unresolved comments, or no reviewer yet on one of yours. Someone else's PR here \
             is theirs to fix."
                .into()
        }
        (Mode::Authored, SectionKind::Category(Category::Action)) => {
            "Your move: a merge conflict, failing CI, requested changes or unresolved comments, \
             or no reviewer requested yet."
                .into()
        }
        (Mode::Authored | Mode::AllOpen, SectionKind::Category(Category::Await)) if mixed => {
            "Reviewers are asked or have commented, nothing blocks it, and nobody has approved \
             it yet: its author is waiting on them."
                .into()
        }
        (Mode::Authored, SectionKind::Category(Category::Await)) => {
            "Reviewers are asked or have commented, nothing blocks it, and nobody has approved \
             it yet: you are waiting on them."
                .into()
        }
        (Mode::Review | Mode::AllOpen, SectionKind::Category(Category::Todo)) => {
            "Your review is requested, by name or through one of your teams, and you have not \
             reviewed it yet."
                .into()
        }
        (Mode::Authored | Mode::AllOpen, SectionKind::Category(Category::Available)) => {
            "Someone else's PR with nothing blocking it, no reviewer requested and no review \
             yet. Optional: nobody assigned it to you."
                .into()
        }
        (Mode::Review, SectionKind::Category(Category::Available)) => {
            "Someone else's PR with no reviewer requested and no review from you. Optional: \
             nobody assigned it to you."
                .into()
        }
        (Mode::Review, SectionKind::Category(Category::Done)) => {
            "You already reviewed it. The Note says when new commits came in since.".into()
        }
        (_, SectionKind::Category(Category::Draft)) => {
            "Marked as a draft on GitHub, so not ready for review yet.".into()
        }
        (_, SectionKind::Snoozed) => {
            "You snoozed these. Each comes back when its snooze ends, and counts toward nothing \
             until then."
                .into()
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Ci, StackInfo};

    fn row(number: u64, category: Category) -> BoardRow {
        BoardRow {
            id: format!("PR_{number}"),
            repo: "acme/widgets".into(),
            updated_at: None,
            head_oid: None,
            reviewed_oid: None,
            reviewed_at: None,
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: format!("Change {number}"),
            issue: None,
            issue_url: None,
            author: Some("alice".into()),
            stack: None,
            queue_provenance: None,
            draft: category == Category::Draft,
            category,
            bug: false,
            labels: Vec::new(),
            ci: Ci::Pass,
            conflict: false,
            mergeable_unknown: false,
            review_decision: None,
            review_state: ReviewState::Waiting,
            requested: Vec::new(),
            requested_teams: Vec::new(),
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: "2026-09-01T10:00:00Z".into(),
            waiting_since: None,
            size: None,
            note: String::new(),
        }
    }

    fn stacked(mut pr: BoardRow, repo: &str, number: u64, size: u64, position: u64) -> BoardRow {
        pr.repo = repo.into();
        pr.stack = Some(StackInfo {
            number,
            size,
            base_ref_name: "main".into(),
            position: Some(position),
        });
        pr
    }

    /// Headers as "label (count)" / "  label · detail", rows as PR numbers.
    fn outline(rows: &[BoardRow], items: &[LayoutItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                LayoutItem::Header {
                    label,
                    count: Some(count),
                    ..
                } => format!("{label} ({count})"),
                LayoutItem::Header { label, detail, .. } => {
                    format!("  {label} · {}", detail.as_deref().unwrap_or(""))
                }
                LayoutItem::Row(ix) => rows[*ix].number.to_string(),
            })
            .collect()
    }

    #[test]
    fn all_open_groups_everyones_prs_by_state_with_your_reviews_first_among_the_work() {
        let waiting = |mut r: BoardRow, since: &str| {
            r.waiting_since = Some(since.into());
            r
        };
        let approved = |mut r: BoardRow| {
            r.review_state = ReviewState::Approved;
            r
        };
        let unasked = |mut r: BoardRow| {
            r.review_state = ReviewState::None;
            r
        };
        // As derive_rows hands them over: most recently updated first.
        let rows = vec![
            unasked(row(10, Category::Await)),
            row(9, Category::Draft),
            row(8, Category::Action),
            waiting(row(7, Category::Todo), "2026-09-20T10:00:00Z"),
            waiting(row(6, Category::Await), "2026-09-01T10:00:00Z"),
            waiting(row(5, Category::Await), "2026-09-15T10:00:00Z"),
            approved(row(4, Category::Action)),
            approved(row(3, Category::Await)),
            waiting(row(2, Category::Todo), "2026-09-10T10:00:00Z"),
        ];
        let items = layout(
            &rows,
            Mode::AllOpen,
            false,
            &HashSet::new(),
            false,
            Sort::Wait,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Approved (1)",
                "3",
                "Requested from you (2)",
                "2",
                "7",
                "Needs action (2)",
                "4",
                "8",
                "Available to review · no reviewer requested (1)",
                "10",
                "Awaiting review (2)",
                "6",
                "5",
                "Drafts (1)",
                "9",
            ],
            "My PRs' names, order and waits; an approved PR that needs action leads its section, \
             and one nobody was asked to review is available rather than awaiting"
        );
        let keys: Vec<&str> = items
            .iter()
            .filter_map(|item| match item {
                LayoutItem::Header {
                    kind,
                    count: Some(_),
                    ..
                } => Some(kind.key()),
                _ => None,
            })
            .collect();
        assert_eq!(
            keys,
            ["approved", "todo", "action", "available", "await", "draft"]
        );
    }

    #[test]
    fn every_section_says_what_puts_a_pr_there() {
        let approved = |mut r: BoardRow| {
            r.review_state = ReviewState::Approved;
            r
        };
        let rows = [
            approved(row(1, Category::Await)),
            row(2, Category::Action),
            row(3, Category::Await),
            row(4, Category::Todo),
            row(5, Category::Available),
            row(6, Category::Done),
            row(7, Category::Draft),
            row(8, Category::Await),
            BoardRow {
                review_state: ReviewState::None,
                ..row(9, Category::Await)
            },
        ];
        let snoozed: HashSet<String> = ["PR_8".to_string()].into();
        for (mode, all_repos) in [
            (Mode::Authored, false),
            (Mode::Authored, true),
            (Mode::Review, false),
            (Mode::AllOpen, false),
        ] {
            // Only the categories this view's derivation produces.
            let produced: Vec<BoardRow> = rows
                .iter()
                .filter(|row| match mode {
                    Mode::Authored => matches!(
                        row.category,
                        Category::Action | Category::Await | Category::Draft
                    ),
                    Mode::Review => matches!(
                        row.category,
                        Category::Todo | Category::Available | Category::Done | Category::Draft
                    ),
                    Mode::AllOpen => !matches!(row.category, Category::Available | Category::Done),
                })
                .cloned()
                .collect();
            for item in layout(&produced, mode, all_repos, &snoozed, true, Sort::Wait) {
                let LayoutItem::Header {
                    kind,
                    label,
                    count: Some(_),
                    ..
                } = item
                else {
                    continue;
                };
                let text = section_explanation(mode, kind, all_repos)
                    .unwrap_or_else(|| panic!("{mode:?} {label}: no explanation"));
                assert!(text.ends_with('.'), "{label}: {text}");
                assert!(!text.contains('\u{fe0f}'), "no emoji: {text}");
            }
        }
        // The approved band points at the section a blocked approval stays in.
        for (mode, all_repos) in [
            (Mode::Authored, false),
            (Mode::Authored, true),
            (Mode::AllOpen, false),
        ] {
            assert!(section_explanation(mode, SectionKind::Approved, all_repos)
                .unwrap()
                .ends_with("stays at the top of Needs action."));
        }
        // Where other people's PRs sit beside yours, their blockers are theirs.
        for (mode, all_repos) in [(Mode::Authored, true), (Mode::AllOpen, false)] {
            let text =
                section_explanation(mode, SectionKind::Category(Category::Action), all_repos)
                    .unwrap();
            assert!(text.contains("theirs to fix"), "{text}");
        }
        assert!(section_explanation(
            Mode::Authored,
            SectionKind::Category(Category::Action),
            false
        )
        .unwrap()
        .starts_with("Your move"));
        assert_eq!(
            section_explanation(Mode::AllOpen, SectionKind::Stack, false),
            None
        );
    }

    #[test]
    fn authored_sections_follow_the_apps_order_with_approvals_first() {
        let mut approved_await = row(1, Category::Await);
        approved_await.review_state = ReviewState::Approved;
        let mut approved_action = row(2, Category::Action);
        approved_action.review_state = ReviewState::Approved;
        let rows = vec![
            row(5, Category::Draft),
            row(3, Category::Await),
            row(4, Category::Action),
            approved_action,
            approved_await,
        ];
        let items = layout(
            &rows,
            Mode::Authored,
            false,
            &HashSet::new(),
            false,
            Sort::Wait,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Approved (1)",
                "1",
                "Needs action (2)",
                "2",
                "4",
                "Awaiting review (1)",
                "3",
                "Drafts (1)",
                "5"
            ]
        );
        match &items[2] {
            LayoutItem::Header { kind, members, .. } => {
                assert_eq!(*kind, SectionKind::Category(Category::Action));
                assert_eq!(kind.key(), "action");
                assert_eq!(members, &vec![3, 2]);
            }
            other => panic!("expected a header, got {other:?}"),
        }
        assert!(layout(
            &[],
            Mode::Authored,
            false,
            &HashSet::new(),
            true,
            Sort::Wait
        )
        .is_empty());
    }

    fn waiting(mut pr: BoardRow, since: Option<&str>) -> BoardRow {
        pr.waiting_since = since.map(str::to_owned);
        pr
    }

    #[test]
    fn pickup_sections_put_the_longest_wait_first() {
        let rows = vec![
            waiting(row(1, Category::Todo), Some("2026-09-03T10:00:00Z")),
            waiting(row(2, Category::Todo), None),
            waiting(row(3, Category::Todo), Some("2026-09-01T10:00:00Z")),
            waiting(row(4, Category::Available), Some("2026-09-02T10:00:00Z")),
            waiting(row(5, Category::Available), Some("2026-08-30T10:00:00Z")),
            row(6, Category::Done),
            row(7, Category::Done),
        ];
        let items = layout(
            &rows,
            Mode::Review,
            false,
            &HashSet::new(),
            false,
            Sort::Wait,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Requested from you (3)",
                "3",
                "1",
                "2",
                "Available to review · no reviewer requested (2)",
                "5",
                "4",
                "Reviewed (2)",
                "6",
                "7"
            ]
        );

        // Your PRs: only the waiting band reorders; a stack keeps its layers
        // together where its longest-waiting member lands.
        let mut commented = row(10, Category::Await);
        commented.review_state = ReviewState::Commented;
        let rows = vec![
            waiting(row(11, Category::Action), Some("2026-09-03T10:00:00Z")),
            waiting(row(12, Category::Action), Some("2026-09-01T10:00:00Z")),
            commented,
            waiting(row(13, Category::Await), Some("2026-09-02T10:00:00Z")),
            waiting(
                stacked(row(14, Category::Await), "acme/widgets", 9, 2, 1),
                Some("2026-09-01T09:00:00Z"),
            ),
            waiting(
                stacked(row(15, Category::Await), "acme/widgets", 9, 2, 2),
                Some("2026-09-05T09:00:00Z"),
            ),
        ];
        let items = layout(
            &rows,
            Mode::Authored,
            false,
            &HashSet::new(),
            false,
            Sort::Wait,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Needs action (2)",
                "11",
                "12",
                "Awaiting review (4)",
                "  Stack #9 · 2 layers",
                "14",
                "15",
                "13",
                "10"
            ]
        );
    }

    #[test]
    fn smallest_first_orders_the_pickup_sections_by_size_then_wait() {
        let sized = |pr: BoardRow, lines: u64, files: u64| BoardRow {
            size: Some(ChangeSize {
                additions: lines,
                deletions: 0,
                changed_files: files,
            }),
            ..pr
        };
        let rows = vec![
            sized(row(1, Category::Todo), 500, 2),
            row(2, Category::Todo),
            sized(row(3, Category::Todo), 90, 40),
            sized(row(4, Category::Todo), 30, 1),
            sized(
                waiting(row(5, Category::Todo), Some("2026-09-03T10:00:00Z")),
                150,
                2,
            ),
            sized(
                waiting(row(6, Category::Todo), Some("2026-09-01T10:00:00Z")),
                150,
                2,
            ),
            sized(row(7, Category::Todo), 120, 3),
            sized(row(8, Category::Available), 800, 3),
            sized(row(9, Category::Available), 10, 1),
            sized(row(10, Category::Done), 800, 3),
            sized(row(11, Category::Done), 10, 1),
        ];
        let items = layout(
            &rows,
            Mode::Review,
            false,
            &HashSet::new(),
            false,
            Sort::Smallest,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Requested from you (7)",
                "4",
                "7",
                "6",
                "5",
                "3",
                "1",
                "2",
                "Available to review · no reviewer requested (2)",
                "9",
                "8",
                "Reviewed (2)",
                "10",
                "11"
            ]
        );

        // Your PRs keep the wait order.
        let rows = vec![
            sized(
                waiting(row(1, Category::Await), Some("2026-09-03T10:00:00Z")),
                900,
                2,
            ),
            sized(
                waiting(row(2, Category::Await), Some("2026-09-01T10:00:00Z")),
                5,
                2,
            ),
        ];
        let items = layout(
            &rows,
            Mode::Authored,
            false,
            &HashSet::new(),
            false,
            Sort::Smallest,
        );
        assert_eq!(outline(&rows, &items), ["Awaiting review (2)", "2", "1"]);
    }

    #[test]
    fn review_sections_and_all_repository_labels() {
        let rows = vec![
            row(1, Category::Done),
            row(2, Category::Available),
            row(3, Category::Todo),
        ];
        let items = layout(
            &rows,
            Mode::Review,
            true,
            &HashSet::new(),
            false,
            Sort::Wait,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Requested from you (1)",
                "3",
                "Available to review · no reviewer requested (1)",
                "2",
                "Reviewed (1)",
                "1"
            ]
        );
        // One name per section, whichever view holds it.
        for category in [Category::Action, Category::Await, Category::Draft] {
            let mine = group_label(Mode::Authored, category, false);
            assert_eq!(group_label(Mode::Authored, category, true), mine);
            assert_eq!(group_label(Mode::AllOpen, category, false), mine);
        }
        assert_eq!(
            group_label(Mode::AllOpen, Category::Available, false),
            group_label(Mode::Review, Category::Available, false)
        );
    }

    #[test]
    fn stacks_group_by_repository_in_layer_order_and_report_coverage() {
        let rows = vec![
            stacked(row(12, Category::Action), "acme/widgets", 9, 3, 3),
            row(20, Category::Action),
            stacked(row(11, Category::Action), "acme/widgets", 9, 3, 1),
            stacked(row(30, Category::Action), "acme/gears", 9, 1, 1),
        ];
        let items = layout(
            &rows,
            Mode::Authored,
            true,
            &HashSet::new(),
            false,
            Sort::Wait,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Needs action (4)",
                "  acme/widgets · Stack #9 · 2 of 3 layers shown",
                "11",
                "12",
                "20",
                "  acme/gears · Stack #9 · 1 layers",
                "30"
            ]
        );
        match &items[0] {
            LayoutItem::Header { members, .. } => assert_eq!(members, &vec![2, 0, 1, 3]),
            other => panic!("expected a header, got {other:?}"),
        }
    }

    #[test]
    fn snoozed_rows_trail_in_their_own_group_listed_even_when_collapsed() {
        let rows = vec![row(1, Category::Action), row(2, Category::Await)];
        let snoozed = HashSet::from(["PR_1".to_string()]);
        let collapsed = layout(&rows, Mode::Authored, false, &snoozed, false, Sort::Wait);
        assert_eq!(
            outline(&rows, &collapsed),
            ["Awaiting review (1)", "2", "Snoozed (1)"]
        );
        match collapsed.last() {
            Some(LayoutItem::Header { kind, members, .. }) => {
                assert_eq!(*kind, SectionKind::Snoozed);
                assert_eq!(members, &vec![0]);
            }
            other => panic!("expected the Snoozed header, got {other:?}"),
        }
        let shown = layout(&rows, Mode::Authored, false, &snoozed, true, Sort::Wait);
        assert_eq!(
            outline(&rows, &shown),
            ["Awaiting review (1)", "2", "Snoozed (1)", "1"]
        );
    }

    #[test]
    fn a_section_order_names_what_it_wants_first_and_keeps_every_section() {
        assert!(SectionOrder::default().is_default());
        assert_eq!(
            SectionOrder::default().keys(),
            [
                "approved",
                "todo",
                "action",
                "available",
                "await",
                "done",
                "draft"
            ]
        );
        // Listed sections lead in their order; the rest keep the default one.
        let (order, ignored) = SectionOrder::from_keys(&[" Available ", "AWAIT"]);
        assert!(ignored.is_empty(), "{ignored:?}");
        assert_eq!(
            order.keys(),
            [
                "available",
                "await",
                "approved",
                "todo",
                "action",
                "done",
                "draft"
            ]
        );
        // What it cannot use is named, one sentence each, and changes nothing.
        let (order, ignored) =
            SectionOrder::from_keys(&["draft", "in-progress", "draft", "snoozed"]);
        assert_eq!(
            order.keys(),
            [
                "draft",
                "approved",
                "todo",
                "action",
                "available",
                "await",
                "done"
            ]
        );
        assert_eq!(
            ignored,
            [
                "ignoring section_order entry \"in-progress\": use approved, todo, action, \
                 available, await, done, draft",
                "ignoring repeated section_order entry \"draft\"",
                "ignoring section_order entry \"snoozed\": Snoozed always comes last",
            ]
        );
        let empty: [&str; 0] = [];
        assert_eq!(SectionOrder::from_keys(&empty).0, SectionOrder::default());
        // What `keys` writes reads back as the same order.
        assert_eq!(SectionOrder::from_keys(&order.keys()).0, order);
    }

    #[test]
    fn moving_a_section_swaps_it_with_its_neighbour_and_stops_at_the_ends() {
        let available = SectionKind::Category(Category::Available);
        let order = SectionOrder::default().moved(available, true);
        assert_eq!(
            order.keys(),
            [
                "approved",
                "todo",
                "available",
                "action",
                "await",
                "done",
                "draft"
            ]
        );
        assert_eq!(order.moved(available, false), SectionOrder::default());
        let first = SectionOrder::default();
        assert_eq!(first.moved(SectionKind::Approved, true), first);
        let last = SectionKind::Category(Category::Draft);
        assert_eq!(first.moved(last, false), first);
        // Not a section one can order: nothing moves.
        assert_eq!(first.moved(SectionKind::Snoozed, true), first);
    }

    #[test]
    fn every_view_shows_its_sections_in_the_persons_order_with_snoozed_last() {
        let unasked = |mut r: BoardRow| {
            r.review_state = ReviewState::None;
            r
        };
        let approved = |mut r: BoardRow| {
            r.review_state = ReviewState::Approved;
            r
        };
        let rows = vec![
            row(1, Category::Action),
            row(2, Category::Await),
            unasked(row(3, Category::Await)),
            approved(row(4, Category::Await)),
            row(5, Category::Todo),
            row(6, Category::Draft),
            row(7, Category::Action),
        ];
        let snoozed: HashSet<String> = ["PR_7".to_string()].into();
        // A lead who assigns reviewers: what nobody was asked about comes first.
        let (order, _) = SectionOrder::from_keys(&["available", "draft", "await"]);
        let items = layout_ordered(
            &rows,
            Mode::AllOpen,
            false,
            &snoozed,
            true,
            Sort::Wait,
            &order,
        );
        assert_eq!(
            outline(&rows, &items),
            [
                "Available to review · no reviewer requested (1)",
                "3",
                "Drafts (1)",
                "6",
                "Awaiting review (1)",
                "2",
                "Approved (1)",
                "4",
                "Requested from you (1)",
                "5",
                "Needs action (1)",
                "1",
                "Snoozed (1)",
                "7",
            ]
        );
        // The Review queue takes its own sections from the same order.
        let review = vec![
            row(1, Category::Todo),
            row(2, Category::Available),
            row(3, Category::Done),
            row(4, Category::Draft),
        ];
        let (order, _) = SectionOrder::from_keys(&["done", "draft"]);
        let items = layout_ordered(
            &review,
            Mode::Review,
            false,
            &HashSet::new(),
            false,
            Sort::Wait,
            &order,
        );
        assert_eq!(
            outline(&review, &items),
            [
                "Reviewed (1)",
                "3",
                "Drafts (1)",
                "4",
                "Requested from you (1)",
                "1",
                "Available to review · no reviewer requested (1)",
                "2",
            ]
        );
        // The default order is the layout every view had before it could change.
        assert_eq!(
            layout_ordered(
                &rows,
                Mode::AllOpen,
                false,
                &snoozed,
                true,
                Sort::Wait,
                &SectionOrder::default()
            ),
            layout(&rows, Mode::AllOpen, false, &snoozed, true, Sort::Wait)
        );
    }

    #[test]
    fn every_orderable_section_has_a_name_and_says_where_it_shows() {
        for kind in ORDERABLE_SECTIONS {
            assert!(!section_name(kind).is_empty(), "{kind:?}");
            assert!(!section_views(kind).is_empty(), "{kind:?}");
        }
        assert_eq!(
            section_views(SectionKind::Category(Category::Done)),
            "Review queue"
        );
        // The Settings name is the header's, before any " · " detail.
        for (mode, kind) in [
            (Mode::Authored, SectionKind::Category(Category::Action)),
            (Mode::Review, SectionKind::Category(Category::Available)),
            (Mode::AllOpen, SectionKind::Category(Category::Todo)),
        ] {
            let SectionKind::Category(category) = kind else {
                unreachable!()
            };
            let header = group_label(mode, category, false);
            assert!(header.starts_with(section_name(kind)), "{header}");
        }
    }
}
