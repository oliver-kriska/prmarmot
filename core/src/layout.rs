//! Board display layout shared by every front end (the GPUI table and the
//! CLI): section order and labels, the Approved split, stack sub-groups in
//! dependency order, and the Snoozed group. Pure — no UI types, no I/O — so
//! the window and the terminal can never disagree about what a view shows.

use std::collections::HashSet;

use crate::board::{BoardRow, Category, Mode, ReviewState};

/// Which band a header introduces. Stack sub-headers sit inside a band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// Authored PRs awaiting merge that are already approved.
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

/// Lay out `rows` for `mode`. Snoozed rows (by PR id) move to a trailing
/// Snoozed group whose members are listed but only emitted as rows when
/// `show_snoozed` is set. Order within a section is stable.
pub fn layout(
    rows: &[BoardRow],
    mode: Mode,
    all_repos: bool,
    snoozed: &HashSet<String>,
    show_snoozed: bool,
) -> Vec<LayoutItem> {
    let mut display = Vec::with_capacity(rows.len() + 4);
    let mut order: Vec<usize> = (0..rows.len()).collect();
    let snoozed_order: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&ix| snoozed.contains(&rows[ix].id))
        .collect();
    order.retain(|&ix| !snoozed.contains(&rows[ix].id));
    order.sort_by_key(|&ix| {
        let row = &rows[ix];
        let section = match row.category {
            Category::Await if is_approved_section(mode, row) => 0,
            Category::Action | Category::Todo => 1,
            Category::Available => 2,
            Category::Await | Category::Done => 3,
            Category::Draft => 4,
        };
        let approved_action = mode == Mode::Authored
            && row.category == Category::Action
            && row.review_state == ReviewState::Approved;
        (section, !approved_action)
    });
    let mut i = 0;
    while i < order.len() {
        let row = &rows[order[i]];
        let cat = row.category;
        let approved = is_approved_section(mode, row);
        let start = i;
        while i < order.len()
            && rows[order[i]].category == cat
            && is_approved_section(mode, &rows[order[i]]) == approved
        {
            i += 1;
        }
        display.push(LayoutItem::Header {
            kind: if approved {
                SectionKind::Approved
            } else {
                SectionKind::Category(cat)
            },
            label: if approved {
                "Approved"
            } else {
                group_label(mode, cat, all_repos)
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

/// Authored PRs waiting to merge that already carry an approval get their own
/// band ahead of Needs action, without changing the core category.
pub fn is_approved_section(mode: Mode, row: &BoardRow) -> bool {
    mode == Mode::Authored
        && row.category == Category::Await
        && row.review_state == ReviewState::Approved
}

/// The section label for a category within a mode. Action/Await differ from
/// Todo/Done even though they share a sort rank.
pub fn group_label(mode: Mode, cat: Category, all_repos: bool) -> &'static str {
    match (mode, cat, all_repos) {
        (Mode::Authored, Category::Action, true) => "Needs attention",
        (Mode::Authored, Category::Await, true) => "In progress",
        (Mode::Authored, Category::Action, false) => "Needs action",
        (Mode::Authored, Category::Await, false) => "Awaiting review",
        (Mode::Review, Category::Todo, _) => "Requested from you",
        (Mode::Review, Category::Available, _) => "Available to review · no reviewer requested",
        (Mode::Review, Category::Done, _) => "Reviewed",
        (_, Category::Draft, _) => "Drafts",
        // Unreachable pairings (Action in Review etc.) — a calm fallback.
        _ => "Other",
    }
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
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: "2026-09-01T10:00:00Z".into(),
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
        let items = layout(&rows, Mode::Authored, false, &HashSet::new(), false);
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
        assert!(layout(&[], Mode::Authored, false, &HashSet::new(), true).is_empty());
    }

    #[test]
    fn review_sections_and_all_repository_labels() {
        let rows = vec![
            row(1, Category::Done),
            row(2, Category::Available),
            row(3, Category::Todo),
        ];
        let items = layout(&rows, Mode::Review, true, &HashSet::new(), false);
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
        assert_eq!(
            group_label(Mode::Authored, Category::Action, true),
            "Needs attention"
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
        let items = layout(&rows, Mode::Authored, true, &HashSet::new(), false);
        assert_eq!(
            outline(&rows, &items),
            [
                "Needs attention (4)",
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
        let collapsed = layout(&rows, Mode::Authored, false, &snoozed, false);
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
        let shown = layout(&rows, Mode::Authored, false, &snoozed, true);
        assert_eq!(
            outline(&rows, &shown),
            ["Awaiting review (1)", "2", "Snoozed (1)", "1"]
        );
    }
}
