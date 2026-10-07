//! Fetching a view: the one GraphQL operation per refresh (two, in order, for
//! the Review queue across all repositories), the retry at
//! [`SMALL_PAGE_SIZE`] rows when GitHub gives up, followed PRs, and Load more
//! folded into the rows already loaded.

use super::*;
use crate::github::query::{check_graphql_errors, parse_rate, MAX_TURN_IDS, TURNS_QUERY};

/// One request, sent again unchanged when GitHub gave up on it at
/// [`SMALL_PAGE_SIZE`] rows. Measured 2026-10-06: a request that takes 4–5 s
/// ten times runs into GitHub's cut-off the eleventh, so the small page's
/// size is not what failed, and the same request is worth one more try. A
/// full page is not resent: the small page is its retry. One retry, never
/// more — a request GitHub gives up on still costs points and counts toward
/// its secondary limit.
fn send_with_ids(
    transport: &dyn GithubTransport,
    query: &str,
    variables: &[(&str, &str)],
    ids: &[String],
    first: u8,
) -> Result<serde_json::Value, GhError> {
    match transport.graphql_with_ids(query, variables, ids) {
        Err(error) if error.is_query_timeout() && first == SMALL_PAGE_SIZE => {
            transport.graphql_with_ids(query, variables, ids)
        }
        sent => sent,
    }
}

/// One bounded `nodes(ids:)` request for specific PRs, in any repository: what
/// became of PRs that left a view, or one PR followed on its own.
pub fn fetch_tracked(
    transport: &dyn GithubTransport,
    ids: &[String],
    me: &str,
    cfg: &BoardConfig,
) -> Result<TrackedFetch, GhError> {
    if ids.is_empty() {
        return Ok(TrackedFetch {
            tracked: Vec::new(),
            rate: None,
            access: AccessGaps::default(),
        });
    }
    let query = with_tracked_nodes(TRACKED_ONLY_QUERY)?;
    let mut body = transport.graphql_with_ids(&query, &[("who", me)], ids)?;
    let access = tolerate_access_errors(&mut body);
    let rate = parse_tracked_response(&body)?;
    Ok(TrackedFetch {
        tracked: derive_tracked(&body, ids, "", me, cfg),
        rate,
        access,
    })
}

/// What became of specific PRs, state only: whether each is open, closed,
/// merged, or no longer visible. One request of about a point for up to 100
/// ids, for callers that need no rows (the CLI's removed PRs).
pub fn fetch_tracked_status(
    transport: &dyn GithubTransport,
    ids: &[String],
) -> Result<TrackedFetch, GhError> {
    if ids.is_empty() {
        return Ok(TrackedFetch {
            tracked: Vec::new(),
            rate: None,
            access: AccessGaps::default(),
        });
    }
    let mut body = transport.graphql_with_ids(TRACKED_STATUS_QUERY, &[], ids)?;
    let access = tolerate_access_errors(&mut body);
    let rate = parse_tracked_response(&body)?;
    Ok(TrackedFetch {
        tracked: tracked_statuses(&body, "/data/tracked", ids),
        rate,
        access,
    })
}

/// Each requested id's state from a state-only `nodes(ids:)` list at
/// `pointer`, in request order; a node GitHub did not return is inaccessible.
fn tracked_statuses(body: &serde_json::Value, pointer: &str, ids: &[String]) -> Vec<TrackedPr> {
    let nodes = body.pointer(pointer).and_then(serde_json::Value::as_array);
    ids.iter()
        .enumerate()
        .map(|(index, requested_id)| {
            let node = nodes
                .and_then(|nodes| nodes.get(index))
                .filter(|node| node.get("state").is_some());
            let status = match node {
                None => TrackedPrStatus::Inaccessible,
                Some(node)
                    if node.get("merged").and_then(serde_json::Value::as_bool) == Some(true) =>
                {
                    TrackedPrStatus::Merged
                }
                Some(node)
                    if node.get("state").and_then(serde_json::Value::as_str) == Some("CLOSED") =>
                {
                    TrackedPrStatus::Closed
                }
                Some(_) => TrackedPrStatus::Open,
            };
            TrackedPr {
                pr_id: requested_id.clone(),
                status,
                row: None,
            }
        })
        .collect()
}

/// The node id of `owner/name#number`, for [`fetch_tracked`].
pub fn resolve_pull_request_id(
    transport: &dyn GithubTransport,
    repo: &str,
    number: u64,
) -> Result<String, GhError> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(GhError::RepositoryNotFound(repo.to_owned()));
    };
    let body = transport.graphql(
        &pull_request_id_query(number),
        &[("owner", owner), ("name", name)],
    )?;
    parse_pull_request_id(&body, repo, number)
}

/// Page one of any view, plus followed PRs — the one call the desktop, the
/// CLI and the iPad make for a refresh. All open sends `filter`'s labels and
/// authors with its search (see [`fetch_all_open`]); My PRs and the review
/// queue ignore it. `small_pages` is the previous fetch's
/// [`BoardPagination::small_pages`] for the same view (false for the first):
/// once GitHub has given up on a full page, the view keeps asking for
/// [`SMALL_PAGE_SIZE`] rows.
#[allow(clippy::too_many_arguments)]
pub fn fetch_view(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    filter: &RemoteFilter,
    tracked: Tracked<'_>,
    small_pages: bool,
) -> Result<BoardFetch, GhError> {
    let unfiltered = RemoteFilter::default();
    let filter = if mode == Mode::AllOpen {
        filter
    } else {
        &unfiltered
    };
    let mut fetched = fetch_scoped(
        transport,
        mode,
        scope,
        me,
        cfg,
        tracked,
        filter,
        small_pages,
    )?;
    settle_turns(transport, mode, me, cfg, &mut fetched);
    Ok(fetched)
}

/// The ownership rule's second request ([`TURNS_QUERY`]): whose turn the
/// unresolved threads are on, for the rows it can move
/// ([`wants_turns`]), at most [`MAX_TURN_IDS`] of them, nearest the top
/// first. A refusal leaves the rows on the plain rule — the board is
/// already there, and this only refines it — so nothing here fails a
/// refresh; the budget it reports is the latest.
fn settle_turns(
    transport: &dyn GithubTransport,
    mode: Mode,
    me: &str,
    cfg: &BoardConfig,
    fetched: &mut BoardFetch,
) {
    let ids: Vec<String> = fetched
        .rows
        .iter()
        .filter(|row| wants_turns(row, mode, me))
        .map(|row| row.id.clone())
        .take(MAX_TURN_IDS)
        .collect();
    if ids.is_empty() {
        return;
    }
    let Ok(body) = transport.graphql_with_ids(TURNS_QUERY, &[], &ids) else {
        return;
    };
    if check_graphql_errors(&body).is_err() {
        return;
    }
    if let Some(rate) = parse_rate(&body) {
        fetched.rate = Some(rate);
    }
    let Some(nodes) = body
        .pointer("/data/turns")
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for node in nodes.iter().filter(|node| !node.is_null()) {
        let Some(id) = node.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let turns = turns_of(node, me);
        if let Some(row) = fetched.rows.iter_mut().find(|row| row.id == id) {
            apply_turns(row, turns, mode, me, cfg);
        }
    }
}

/// Count a PR's unresolved threads by who spoke last: yours when your
/// comment is the latest, returned when you opened the thread and someone
/// else answered last.
fn turns_of(node: &serde_json::Value, me: &str) -> Turns {
    let author = |thread: &serde_json::Value, edge: &str| -> Option<String> {
        thread
            .pointer(&format!("/{edge}/nodes/0/author/login"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let mut turns = Turns::default();
    for thread in node
        .pointer("/reviewThreads/nodes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if thread
            .get("isResolved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
        {
            continue;
        }
        let latest = author(thread, "latest");
        let opened = author(thread, "opened");
        if latest.as_deref() == Some(me) {
            turns.replied += 1;
        } else if opened.as_deref() == Some(me) && latest.is_some() {
            turns.returned += 1;
        }
    }
    turns
}

/// All open for one repository, with `filter`'s labels and authors matched by
/// GitHub rather than among the loaded rows, so the count, the first page, and
/// Load more all cover the whole repository. Same one-operation contract and
/// bounds as the other views: [`PAGE_SIZE`] rows a page (or
/// [`SMALL_PAGE_SIZE`], see [`fetch_view`]), user-invoked
/// Load more, at most [`MAX_PAGES_PER_ALIAS`] pages.
pub fn fetch_all_open(
    transport: &dyn GithubTransport,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    filter: &RemoteFilter,
    tracked: Tracked<'_>,
    small_pages: bool,
) -> Result<BoardFetch, GhError> {
    let mut fetched = fetch_scoped(
        transport,
        Mode::AllOpen,
        &BoardScope::Repository(repo.to_owned()),
        me,
        cfg,
        tracked,
        filter,
        small_pages,
    )?;
    settle_turns(transport, Mode::AllOpen, me, cfg, &mut fetched);
    Ok(fetched)
}

/// One page-one operation, and when GitHub gives up on a full page (see
/// [`GhError::is_query_timeout`]) the same operation once more with
/// [`SMALL_PAGE_SIZE`] rows, itself sent twice if need be
/// ([`send_with_ids`]). Still one request per refresh when GitHub answers
/// (two for the Review queue across all repositories); more when it did not,
/// and then the view remembers.
#[allow(clippy::too_many_arguments)]
fn fetch_scoped(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    tracked: Tracked<'_>,
    filter: &RemoteFilter,
    small_pages: bool,
) -> Result<BoardFetch, GhError> {
    if !small_pages {
        match fetch_scoped_sized(transport, mode, scope, me, cfg, tracked, filter, PAGE_SIZE) {
            Err(error) if error.is_query_timeout() => {}
            fetched => return fetched,
        }
    }
    let full = tracked.rows.len().min(SMALL_PAGE_TRACKED_ROWS);
    let status: Vec<String> = tracked
        .status
        .iter()
        .chain(&tracked.rows[full..])
        .cloned()
        .collect();
    let tracked = Tracked {
        rows: &tracked.rows[..full],
        status: &status,
    };
    let mut fetched = fetch_scoped_sized(
        transport,
        mode,
        scope,
        me,
        cfg,
        tracked,
        filter,
        SMALL_PAGE_SIZE,
    )?;
    fetched.pagination.small_pages = true;
    Ok(fetched)
}

#[allow(clippy::too_many_arguments)]
fn fetch_scoped_sized(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    tracked: Tracked<'_>,
    filter: &RemoteFilter,
    first: u8,
) -> Result<BoardFetch, GhError> {
    if mode == Mode::AllOpen && scope.is_all() {
        return Err(GhError::NeedsRepository);
    }
    let repo = scope.fallback_repo();
    let scope_repository = scope.repository().and_then(|repo| repo.split_once('/'));
    let initial_operation = |base: &str| -> Result<String, GhError> {
        let mut query = with_page_size(base, first);
        if scope_repository.is_some() {
            query = with_scope_repository(&query)?;
        }
        if !tracked.rows.is_empty() {
            query = with_tracked_nodes(&query)?;
        }
        with_tracked_status_nodes(&query, tracked.status)
    };
    let with_scope_variables = |mut variables: Vec<(&'static str, String)>| {
        if let Some((owner, name)) = scope_repository {
            variables.push(("scopeOwner", owner.to_owned()));
            variables.push(("scopeName", name.to_owned()));
        }
        variables
    };
    let request = |query: &str, variables: &[(&'static str, String)]| {
        let variables: Vec<(&str, &str)> = variables
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let mut body = send_with_ids(transport, query, &variables, tracked.rows, first)?;
        if let Some(error) = scope_repository_error(&body, repo) {
            return Err(error);
        }
        let access = tolerate_access_errors(&mut body);
        Ok((body, access))
    };
    match mode {
        Mode::Authored => {
            let search = scope_search_string(scope, mode, me, cfg);
            let (body, access) = request(
                &initial_operation(PR_SEARCH_QUERY)?,
                &with_scope_variables(vec![("q", search), ("who", me.to_owned())]),
            )?;
            let truncated = body
                .pointer("/data/search/pageInfo/hasNextPage")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let (prs, rate) = parse_search_response(&body)?;
            let pagination = BoardPagination {
                authored: AliasCursor::from_page(page_info(&body, "search")),
                ..Default::default()
            };
            Ok(BoardFetch {
                rows: if scope.is_all() {
                    derive_involving_rows(&prs, repo, me, cfg)
                } else {
                    derive_rows(&prs, mode, repo, me, cfg)
                },
                rate,
                truncated,
                pagination,
                tracked: derive_followed(&body, tracked, repo, me, cfg),
                access,
                total: issue_count(&body, "search"),
            })
        }
        Mode::AllOpen => {
            let search = all_open_search_string(repo, &filter.qualifiers());
            let (body, access) = request(
                &initial_operation(&with_requested_ids(PR_SEARCH_QUERY)?)?,
                &with_scope_variables(vec![
                    ("q", search.clone()),
                    // The review queue's own search, unfiltered: a review
                    // asked of you stays one whatever the chips say.
                    ("requested", search_string(Mode::Review, repo, me)),
                    ("who", me.to_owned()),
                ]),
            )?;
            let (prs, rate) = parse_search_response(&body)?;
            let page = page_info(&body, "search");
            let requested = requested_ids(&body);
            Ok(BoardFetch {
                rows: derive_all_open_rows(&prs, repo, me, cfg, &requested),
                rate,
                truncated: page.has_next_page,
                pagination: BoardPagination {
                    authored: AliasCursor::from_page(page),
                    search: Some(search),
                    requested_ids: requested,
                    ..Default::default()
                },
                tracked: derive_followed(&body, tracked, repo, me, cfg),
                access,
                total: issue_count(&body, "search"),
            })
        }
        Mode::Review => {
            let requested_search = scope_search_string(scope, mode, me, cfg);
            let available_search = scope_available_search_string(scope, me);
            let (body, access, parsed) = if scope.is_all() {
                // Across all repositories the two searches go as two
                // requests, in order: with every field a PR carries, both in
                // one request ran at 85–100 % of GitHub's cut-off and failed
                // one run in three, each alone at about half of it (measured
                // 2026-10-06; Oliver, 2026-10-07). Followed PRs ride on the
                // first. A half GitHub gives up on fails the refresh, as one
                // request did: the view keeps what it had, with the reason.
                let (body, access) = request(
                    &initial_operation(REVIEW_REQUESTED_QUERY)?,
                    &[("requested", requested_search), ("who", me.to_owned())],
                )?;
                let (requested, requested_page, rate) = parse_alias_response(&body, "requested")?;
                let mut second = send_with_ids(
                    transport,
                    &with_page_size(REVIEW_AVAILABLE_QUERY, first),
                    &[("available", &available_search), ("who", me)],
                    &[],
                    first,
                )?;
                let access = access.plus(tolerate_access_errors(&mut second));
                let (available, available_page, later_rate) =
                    parse_alias_response(&second, "available")?;
                let parsed = ReviewSearchResult {
                    requested,
                    available,
                    // The later request's budget is the current one.
                    rate: later_rate.or(rate),
                    truncated: requested_page.has_next_page || available_page.has_next_page,
                    requested_page,
                    available_page,
                };
                (body, access, parsed)
            } else {
                let (body, access) = request(
                    &initial_operation(REVIEW_SEARCH_QUERY)?,
                    &with_scope_variables(vec![
                        ("requested", requested_search),
                        ("available", available_search),
                        ("who", me.to_owned()),
                    ]),
                )?;
                let parsed = parse_review_response(&body)?;
                (body, access, parsed)
            };
            let mut seen = HashSet::new();
            let mut rows = Vec::new();

            for pr in parsed.requested.iter().filter(|pr| !is_own_pr(pr, me)) {
                if seen.insert(pr_identity(pr, repo)) {
                    rows.push(derive_review_row(
                        pr,
                        repo,
                        me,
                        cfg,
                        QueueProvenance::Requested,
                    ));
                }
            }
            for pr in parsed
                .available
                .iter()
                .filter(|pr| !is_own_pr(pr, me) && pr.review_requests.total_count == 0)
            {
                if seen.insert(pr_identity(pr, repo)) {
                    rows.push(derive_review_row(
                        pr,
                        repo,
                        me,
                        cfg,
                        QueueProvenance::Available,
                    ));
                }
            }
            let overflow = rows.len() > MAX_EXPANDED_BOARD_ROWS;
            rows.truncate(MAX_EXPANDED_BOARD_ROWS);
            rows.sort_by_key(|r| (r.category.rank(), r.number));
            Ok(BoardFetch {
                rows,
                rate: parsed.rate,
                truncated: parsed.truncated || overflow,
                pagination: BoardPagination {
                    requested: AliasCursor::from_page(parsed.requested_page),
                    available: AliasCursor::from_page(parsed.available_page),
                    ..Default::default()
                },
                tracked: derive_followed(&body, tracked, repo, me, cfg),
                access,
                total: None,
            })
        }
    }
}

/// The full rows of `tracked.rows`, then the states of `tracked.status`.
fn derive_followed(
    body: &serde_json::Value,
    tracked: Tracked<'_>,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) -> Vec<TrackedPr> {
    let mut followed = derive_tracked(body, tracked.rows, repo, me, cfg);
    followed.extend(tracked_statuses(
        body,
        "/data/trackedStatus",
        tracked.status,
    ));
    followed
}

fn derive_tracked(
    body: &serde_json::Value,
    requested_ids: &[String],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) -> Vec<TrackedPr> {
    let nodes = body
        .pointer("/data/tracked")
        .and_then(serde_json::Value::as_array);
    requested_ids
        .iter()
        .enumerate()
        .map(|(index, requested_id)| {
            let raw = nodes
                .and_then(|nodes| nodes.get(index))
                .filter(|value| !value.is_null())
                .and_then(|value| serde_json::from_value::<RawPr>(value.clone()).ok());
            match raw {
                None => TrackedPr {
                    pr_id: requested_id.clone(),
                    status: TrackedPrStatus::Inaccessible,
                    row: None,
                },
                Some(raw) => {
                    let status = if raw.merged {
                        TrackedPrStatus::Merged
                    } else if raw.state == Some(PrState::Closed) {
                        TrackedPrStatus::Closed
                    } else {
                        TrackedPrStatus::Open
                    };
                    TrackedPr {
                        pr_id: pr_identity(&raw, repo),
                        status,
                        // Someone else's PR reads as it does in Involving me.
                        row: Some(derive_involving_row(&raw, repo, me, cfg)),
                    }
                }
            }
        })
        .collect()
}

/// Fetch one user-requested page for each alias that still has a usable cursor.
/// Exhausted aliases are omitted from the GraphQL operation. The returned value
/// is independent, so callers can retain all prior rows/cursors on any error.
pub fn fetch_more_board(
    transport: &dyn GithubTransport,
    mode: Mode,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    current: &BoardFetch,
) -> Result<BoardFetch, GhError> {
    fetch_more_board_scoped(
        transport,
        mode,
        &BoardScope::Repository(repo.to_owned()),
        me,
        cfg,
        current,
    )
}

pub fn fetch_more_board_scoped(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    current: &BoardFetch,
) -> Result<BoardFetch, GhError> {
    let first = current.pagination.page_size();
    let mut fetched = match fetch_more_sized(transport, mode, scope, me, cfg, current, first) {
        // Same fallback as page one: the cursor stays valid at any page size.
        Err(error) if error.is_query_timeout() && !current.pagination.small_pages => {
            let mut fetched =
                fetch_more_sized(transport, mode, scope, me, cfg, current, SMALL_PAGE_SIZE)?;
            fetched.pagination.small_pages = true;
            fetched
        }
        fetched => fetched?,
    };
    // The rows already settled keep their turns; only the page's new rows ask.
    settle_turns(transport, mode, me, cfg, &mut fetched);
    Ok(fetched)
}

fn fetch_more_sized(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    current: &BoardFetch,
    first: u8,
) -> Result<BoardFetch, GhError> {
    let repo = scope.fallback_repo();
    let mut next = current.clone();
    match mode {
        Mode::Authored => {
            if !next.pagination.authored.can_load() {
                return Ok(next);
            }
            let search = scope_search_string(scope, mode, me, cfg);
            let cursor = next.pagination.authored.end_cursor.clone().unwrap();
            let mut body = send_with_ids(
                transport,
                &with_page_size(PR_SEARCH_PAGE_QUERY, first),
                &[("q", &search), ("after", &cursor), ("who", me)],
                &[],
                first,
            )?;
            next.access = next.access.plus(tolerate_access_errors(&mut body));
            let (prs, rate) = parse_search_response(&body)?;
            next.pagination.authored.update(page_info(&body, "search"));
            next.total = issue_count(&body, "search").or(next.total);
            if scope.is_all() {
                merge_involving(&mut next.rows, &prs, repo, me, cfg);
            } else {
                merge_authored(&mut next.rows, &prs, repo, me, cfg);
            }
            next.rate = rate;
        }
        Mode::AllOpen => {
            let Some(search) = next.pagination.search.clone() else {
                return Ok(next);
            };
            if !next.pagination.authored.can_load() {
                return Ok(next);
            }
            let cursor = next.pagination.authored.end_cursor.clone().unwrap();
            let mut body = send_with_ids(
                transport,
                &with_page_size(PR_SEARCH_PAGE_QUERY, first),
                &[("q", &search), ("after", &cursor), ("who", me)],
                &[],
                first,
            )?;
            next.access = next.access.plus(tolerate_access_errors(&mut body));
            let (prs, rate) = parse_search_response(&body)?;
            next.pagination.authored.update(page_info(&body, "search"));
            next.total = issue_count(&body, "search").or(next.total);
            let requested = std::mem::take(&mut next.pagination.requested_ids);
            merge_all_open(&mut next.rows, &prs, repo, me, cfg, &requested);
            next.pagination.requested_ids = requested;
            next.rate = rate;
        }
        Mode::Review => {
            let requested = next.pagination.requested.can_load();
            let available = next.pagination.available.can_load();
            if !requested && !available {
                return Ok(next);
            }
            let requested_search = scope_search_string(scope, mode, me, cfg);
            let available_search = scope_available_search_string(scope, me);
            let mut requested_prs = Vec::new();
            let mut available_prs = Vec::new();
            if requested && available {
                let rc = next.pagination.requested.end_cursor.clone().unwrap();
                let ac = next.pagination.available.end_cursor.clone().unwrap();
                let mut body = send_with_ids(
                    transport,
                    &with_page_size(REVIEW_BOTH_PAGE_QUERY, first),
                    &[
                        ("requested", &requested_search),
                        ("requestedAfter", &rc),
                        ("available", &available_search),
                        ("availableAfter", &ac),
                        ("who", me),
                    ],
                    &[],
                    first,
                )?;
                next.access = next.access.plus(tolerate_access_errors(&mut body));
                let parsed = parse_review_response(&body)?;
                requested_prs = parsed.requested;
                available_prs = parsed.available;
                next.pagination.requested.update(parsed.requested_page);
                next.pagination.available.update(parsed.available_page);
                next.rate = parsed.rate;
            } else {
                let (query, alias, search, cursor) = if requested {
                    (
                        REVIEW_REQUESTED_PAGE_QUERY,
                        "requested",
                        &requested_search,
                        next.pagination.requested.end_cursor.clone().unwrap(),
                    )
                } else {
                    (
                        REVIEW_AVAILABLE_PAGE_QUERY,
                        "available",
                        &available_search,
                        next.pagination.available.end_cursor.clone().unwrap(),
                    )
                };
                let mut body = send_with_ids(
                    transport,
                    &with_page_size(query, first),
                    &[(alias, search), ("after", &cursor), ("who", me)],
                    &[],
                    first,
                )?;
                next.access = next.access.plus(tolerate_access_errors(&mut body));
                let (prs, page, rate) = parse_alias_response(&body, alias)?;
                if requested {
                    requested_prs = prs;
                    next.pagination.requested.update(page);
                } else {
                    available_prs = prs;
                    next.pagination.available.update(page);
                }
                next.rate = rate;
            }
            merge_review(
                &mut next.rows,
                &requested_prs,
                &available_prs,
                repo,
                me,
                cfg,
            );
        }
    }
    next.truncated = next.pagination.truncated(mode);
    Ok(next)
}

fn scope_search_string(scope: &BoardScope, mode: Mode, me: &str, cfg: &BoardConfig) -> String {
    match scope {
        BoardScope::AllRepositories if mode == Mode::Authored && cfg.authored_only => {
            global_authored_search_string(me)
        }
        BoardScope::AllRepositories => global_search_string(mode, me),
        BoardScope::Repository(repo) => crate::github::query::search_string(mode, repo, me),
    }
}

fn scope_available_search_string(scope: &BoardScope, me: &str) -> String {
    match scope {
        BoardScope::AllRepositories => global_available_search_string(me),
        BoardScope::Repository(repo) => available_search_string(repo, me),
    }
}

/// Fold a Load more page into the rows already loaded: `insert` puts the
/// page's rows into the map by identity (replacing or keeping what is there,
/// as the view needs), then the board is sorted by `order` and cut to `limit`.
fn merge_by_identity(
    rows: &mut Vec<BoardRow>,
    insert: impl FnOnce(&mut HashMap<String, BoardRow>),
    order: impl FnMut(&BoardRow, &BoardRow) -> std::cmp::Ordering,
    limit: usize,
) {
    let mut by_id: HashMap<String, BoardRow> = rows.drain(..).map(|r| (r.id.clone(), r)).collect();
    insert(&mut by_id);
    *rows = by_id.into_values().collect();
    rows.sort_by(order);
    rows.truncate(limit);
}

pub(super) fn merge_authored(
    rows: &mut Vec<BoardRow>,
    prs: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) {
    merge_by_identity(
        rows,
        |by_id| {
            for pr in prs {
                by_id.insert(
                    pr_identity(pr, repo),
                    derive_row(pr, Mode::Authored, repo, me, cfg),
                );
            }
        },
        |a, b| {
            (a.category.rank(), std::cmp::Reverse(a.number), &a.id).cmp(&(
                b.category.rank(),
                std::cmp::Reverse(b.number),
                &b.id,
            ))
        },
        MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize,
    );
}

fn merge_involving(
    rows: &mut Vec<BoardRow>,
    prs: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) {
    merge_by_identity(
        rows,
        |by_id| {
            for pr in prs {
                by_id.insert(
                    pr_identity(pr, repo),
                    derive_involving_row(pr, repo, me, cfg),
                );
            }
        },
        |a, b| {
            (a.category.rank(), std::cmp::Reverse(&a.updated_at), &a.id).cmp(&(
                b.category.rank(),
                std::cmp::Reverse(&b.updated_at),
                &b.id,
            ))
        },
        MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize,
    );
}

fn merge_all_open(
    rows: &mut Vec<BoardRow>,
    prs: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    requested: &[String],
) {
    merge_by_identity(
        rows,
        |by_id| {
            for pr in prs {
                by_id.insert(
                    pr_identity(pr, repo),
                    derive_all_open_row(pr, repo, me, cfg, requested),
                );
            }
        },
        most_recently_updated_first,
        MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize,
    );
}

pub(super) fn merge_review(
    rows: &mut Vec<BoardRow>,
    requested: &[RawPr],
    available: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) {
    merge_by_identity(
        rows,
        |by_id| {
            for pr in available
                .iter()
                .filter(|pr| !is_own_pr(pr, me) && pr.review_requests.total_count == 0)
            {
                by_id.entry(pr_identity(pr, repo)).or_insert_with(|| {
                    derive_review_row(pr, repo, me, cfg, QueueProvenance::Available)
                });
            }
            // Requested provenance wins even when the broad alias produced it earlier.
            for pr in requested.iter().filter(|pr| !is_own_pr(pr, me)) {
                by_id.insert(
                    pr_identity(pr, repo),
                    derive_review_row(pr, repo, me, cfg, QueueProvenance::Requested),
                );
            }
        },
        |a, b| (a.category.rank(), a.number, &a.id).cmp(&(b.category.rank(), b.number, &b.id)),
        MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize * 2,
    );
}
