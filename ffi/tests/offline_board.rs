//! The whole boundary, end to end, without a network.
//!
//! This is the Rust twin of the Swift smoke test in `apple/Smoke`: the same
//! fixtures, the same assertions, so a failure tells you immediately whether
//! the problem is in the bridge or in the generated bindings.

use std::sync::{Arc, Mutex};

use prmarmot_ffi::{
    BoardClient, BoardScope, BoardSettings, ClientConfig, FfiError, FilterQualifier,
    GithubTransport, GraphqlRequest, Header, HttpResponse, Mode, ShareFormat, Sort, TokenSource,
};

/// 2026-07-26T12:00:00Z — a little after the newest fixture timestamp, and the
/// same instant `core/tests/golden/search.json` is pinned at.
const NOW: i64 = 1_785_067_200;

/// Answers every request with one recorded response, and remembers what it was
/// asked. A `URLSession` implementation is the same shape with the file read
/// replaced by a data task.
struct Offline {
    body: String,
    status: u16,
    seen: Mutex<Vec<GraphqlRequest>>,
}

impl Offline {
    fn fixture(name: &str) -> Arc<Self> {
        let path = format!(
            "{}/../core/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        Arc::new(Self {
            body: std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}")),
            status: 200,
            seen: Mutex::new(Vec::new()),
        })
    }

    /// The review queue asks for two aliases in one operation, which the
    /// committed fixture (a single legacy search) predates. Reshaping it here
    /// keeps one set of fixtures for the whole project rather than a second
    /// copy that can drift.
    fn review_fixture() -> Arc<Self> {
        let path = format!(
            "{}/../core/tests/fixtures/review_response.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let body = serde_json::json!({
            "data": {
                "requested": raw["data"]["search"],
                "available": {
                    "issueCount": 0,
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": []
                },
                "rateLimit": raw["data"]["rateLimit"],
            }
        });
        Arc::new(Self {
            body: body.to_string(),
            status: 200,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn refusing(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self {
            body: body.to_owned(),
            status,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<GraphqlRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl GithubTransport for Offline {
    async fn send(&self, request: GraphqlRequest) -> Result<HttpResponse, FfiError> {
        self.seen.lock().unwrap().push(request);
        Ok(HttpResponse {
            status: self.status,
            headers: vec![Header {
                name: "x-ratelimit-remaining".into(),
                value: "4999".into(),
            }],
            body: self.body.clone(),
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

fn client(transport: Arc<Offline>) -> Arc<BoardClient> {
    BoardClient::new(
        ClientConfig {
            host: "github.com".into(),
            viewer: "me".into(),
            user_agent: "prmarmot-ffi-test/0".into(),
        },
        transport,
        Arc::new(Token),
    )
}

fn settings() -> BoardSettings {
    BoardSettings {
        stale_after_days: 3,
        ..prmarmot_ffi::default_board_settings()
    }
}

#[test]
fn a_board_arrives_through_a_foreign_transport() {
    let transport = Offline::fixture("authored_response.json");
    let client = client(transport.clone());
    let board = futures::executor::block_on(client.fetch_board(
        Mode::Authored,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        settings(),
        NOW,
    ))
    .unwrap();

    // The same 18 rows `core/tests/golden/search.json` counts.
    assert_eq!(board.rows.len(), 18);
    assert_eq!(board.mode, Mode::Authored);
    let rate = board.rate_limit.expect("the fixture carries a rateLimit");
    assert!(rate.remaining > 0 && rate.limit >= rate.remaining);

    // Rows arrive derived, not raw: a Note, a category, and the wait computed
    // at the `now` that was passed in.
    let row = &board.rows[0];
    assert!(!row.note.is_empty(), "every row carries a Note");
    assert!(board.rows.iter().any(|row| row.stale));
    assert!(board
        .rows
        .iter()
        .any(|row| row.wait_label.is_some() && row.waiting_secs.is_some()));

    // One request, with the token and a real GraphQL body.
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://api.github.com/graphql");
    assert_eq!(requests[0].token, "ghu_offline");
    assert_eq!(requests[0].user_agent, "prmarmot-ffi-test/0");
    let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert!(body["query"].as_str().unwrap().contains("search"));
    assert_eq!(body["variables"]["who"], "me");
}

#[test]
fn the_review_queue_goes_through_the_same_door() {
    let board = futures::executor::block_on(client(Offline::review_fixture()).fetch_board(
        Mode::Review,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        settings(),
        NOW,
    ))
    .unwrap();
    assert_eq!(board.rows.len(), 12);
    assert!(
        board
            .rows
            .iter()
            .all(|row| row.author.as_deref() != Some("me")),
        "your own PRs never appear in your review queue"
    );
    assert!(board
        .rows
        .iter()
        .all(|row| row.queue == Some(prmarmot_ffi::Queue::Requested)));
}

#[test]
fn the_search_grammar_is_the_same_one_the_desktop_uses() {
    let transport = Offline::fixture("authored_response.json");
    let board = futures::executor::block_on(client(transport).fetch_board(
        Mode::Authored,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        settings(),
        NOW,
    ))
    .unwrap();

    // The counts below are the golden's, for the same fixture and instant.
    let matching =
        |query: &str| prmarmot_ffi::search(board.rows.clone(), query.to_owned(), NOW, 3).unwrap();
    assert_eq!(matching("label:bug").len(), 1);
    assert_eq!(matching("is:stale").len(), 4);
    assert_eq!(matching("repo:widgets").len(), 0);
    assert_eq!(matching("").len(), 18);

    // Chips round-trip: what the field shows plus what is left means the same.
    let chips = prmarmot_ffi::take_filter_chips("label:\"help wanted\" spare".into(), true);
    assert_eq!(chips.chips.len(), 1);
    assert_eq!(chips.chips[0].qualifier, FilterQualifier::Label);
    assert_eq!(chips.chips[0].value, "help wanted");
    assert_eq!(chips.chips[0].term, "label:\"help wanted\"");
    assert_eq!(chips.rest, "spare");

    let added =
        prmarmot_ffi::with_filter(chips.rest.clone(), FilterQualifier::Author, "alice".into());
    assert_eq!(added, "spare author:alice");
    // Adding it twice is not a second chip.
    assert_eq!(
        prmarmot_ffi::with_filter(added.clone(), FilterQualifier::Author, "ALICE".into()),
        added
    );
}

#[test]
fn a_board_lays_out_into_sections_and_shares_as_text() {
    let board = futures::executor::block_on(client(Offline::review_fixture()).fetch_board(
        Mode::Review,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        settings(),
        NOW,
    ))
    .unwrap();

    let items = prmarmot_ffi::layout(
        board.rows.clone(),
        Mode::Review,
        false,
        Vec::new(),
        false,
        Sort::Wait,
    );
    let headers: Vec<&prmarmot_ffi::BoardItem> = items
        .iter()
        .filter(|item| matches!(item, prmarmot_ffi::BoardItem::Header { .. }))
        .collect();
    assert!(!headers.is_empty(), "a review board has sections");
    let rows = items
        .iter()
        .filter(|item| matches!(item, prmarmot_ffi::BoardItem::Row { .. }))
        .count();
    assert_eq!(rows, board.rows.len(), "every row is placed exactly once");

    let payload = prmarmot_ffi::share_group(
        "Requested from you".into(),
        board.rows.clone(),
        Mode::Review,
        ShareFormat::Markdown,
    );
    // Core writes the heading, including the count and the one repository
    // every row is in. The iPad must not build that line itself.
    assert_eq!(
        payload.plain.lines().next().unwrap(),
        "**Requested from you (12)** · acme/widgets"
    );
    assert!(payload.html.is_none(), "Markdown has no rich flavour");

    let urls = prmarmot_ffi::share_group(
        "Requested from you".into(),
        board.rows.clone(),
        Mode::Review,
        ShareFormat::Urls,
    );
    assert_eq!(urls.plain.lines().count(), board.rows.len());
}

#[test]
fn the_changed_marker_survives_a_round_trip_through_bytes() {
    let transport = Offline::fixture("authored_response.json");
    let board = futures::executor::block_on(client(transport).fetch_board(
        Mode::Authored,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        settings(),
        NOW,
    ))
    .unwrap();

    let store = prmarmot_ffi::AttentionStore::new("github.com".into(), "me".into());
    for row in &board.rows {
        let result = store.observe(row.clone()).unwrap();
        assert_eq!(result.observed, prmarmot_ffi::Observed::Baseline);
        assert!(!result.changed, "a first sighting is never a change");
    }
    assert_eq!(store.count(), board.rows.len() as u32);

    // A new commit on the first PR is a change, and it is remembered.
    let mut moved = board.rows[0].clone();
    moved.head_oid = Some("deadbeef".into());
    let result = store.observe(moved.clone()).unwrap();
    assert_eq!(
        result.observed,
        prmarmot_ffi::Observed::Changed { semantic: true }
    );
    assert!(result.changed);
    assert!(result
        .changes
        .iter()
        .any(|change| change.contains("commits")));

    let bytes = store.to_bytes().unwrap();
    let restored =
        prmarmot_ffi::AttentionStore::from_bytes("github.com".into(), "me".into(), bytes.clone())
            .unwrap();
    assert!(restored.is_changed(moved.id.clone()));
    assert_eq!(restored.count(), store.count());
    assert!(restored.acknowledge(moved.id.clone()));
    assert!(!restored.is_changed(moved.id.clone()));

    // Another account's state is refused, never merged.
    let wrong =
        prmarmot_ffi::AttentionStore::from_bytes("github.com".into(), "someone-else".into(), bytes);
    assert!(matches!(wrong, Err(FfiError::Invalid { .. })));
}

#[test]
fn github_saying_no_reaches_swift_as_the_right_error() {
    let unauthorized = Offline::refusing(401, r#"{"message":"Bad credentials"}"#);
    let error = futures::executor::block_on(client(unauthorized).fetch_board(
        Mode::Authored,
        BoardScope::AllRepositories,
        settings(),
        NOW,
    ))
    .unwrap_err();
    assert_eq!(error, FfiError::NotAuthenticated);

    let server = Offline::refusing(502, "<html>502 Bad Gateway</html>");
    let error = futures::executor::block_on(client(server).fetch_board(
        Mode::Authored,
        BoardScope::AllRepositories,
        settings(),
        NOW,
    ))
    .unwrap_err();
    assert!(
        matches!(&error, FfiError::Network { message } if message.contains("having trouble (502)")),
        "{error:?}"
    );
}

#[test]
fn load_more_needs_a_board_to_continue() {
    let transport = Offline::fixture("authored_response.json");
    let client = client(transport);
    let error = futures::executor::block_on(client.load_more(
        Mode::Authored,
        BoardScope::AllRepositories,
        settings(),
        NOW,
    ))
    .unwrap_err();
    assert!(matches!(&error, FfiError::Invalid { .. }), "{error:?}");
    assert!(!client.has_more(Mode::Authored));
}

/// The cycle rule, from the Rust side.
///
/// `BoardClient` holds the transport and the token source strongly. If a
/// transport held its client strongly in return, neither would ever be freed:
/// UniFFI's foreign objects are reference-counted on both sides and nothing
/// sees the whole cycle. The Swift smoke test asserts the mirror of this with
/// a `weak` reference; here we assert that Rust itself leaks nothing, so a
/// leak in the app can only be the Swift half.
#[test]
fn dropping_the_client_releases_the_transport_and_the_token_source() {
    let transport = Offline::fixture("authored_response.json");
    let tokens = Arc::new(Token);
    let client = BoardClient::new(
        ClientConfig {
            host: "github.com".into(),
            viewer: "me".into(),
            user_agent: "prmarmot-ffi-test/0".into(),
        },
        transport.clone(),
        tokens.clone(),
    );

    // The client's own reference, plus the two held here.
    assert_eq!(Arc::strong_count(&transport), 2);
    assert_eq!(Arc::strong_count(&tokens), 2);

    // A finished fetch must not have parked a reference anywhere.
    let _ = futures::executor::block_on(client.fetch_board(
        Mode::Authored,
        BoardScope::AllRepositories,
        settings(),
        NOW,
    ))
    .unwrap();
    assert_eq!(Arc::strong_count(&transport), 2);

    drop(client);
    assert_eq!(Arc::strong_count(&transport), 1);
    assert_eq!(Arc::strong_count(&tokens), 1);
}

/// The device flow, end to end, against recorded GitHub responses.
///
/// The one thing that cannot be tested here is a real sign-in: it needs a
/// registered GitHub App. Everything up to that point — the form encoding, the
/// poll states, the clamped interval, the refresh — is exercised.
mod sign_in {
    use super::*;
    use prmarmot_ffi::{AuthTransport, DeviceFlow, DevicePoll, FormRequest};

    struct Recorded {
        answers: Mutex<Vec<String>>,
        seen: Mutex<Vec<FormRequest>>,
    }

    impl Recorded {
        fn new(answers: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.iter().map(|a| (*a).to_string()).collect()),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl AuthTransport for Recorded {
        async fn post_form(&self, request: FormRequest) -> Result<HttpResponse, FfiError> {
            self.seen.lock().unwrap().push(request);
            let mut answers = self.answers.lock().unwrap();
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: if answers.is_empty() {
                    "{}".into()
                } else {
                    answers.remove(0)
                },
            })
        }
    }

    fn flow(transport: Arc<Recorded>) -> Arc<DeviceFlow> {
        DeviceFlow::new(
            "github.com".into(),
            "Iv1.testclientid".into(),
            "prmarmot-ffi-test/0".into(),
            transport,
        )
    }

    #[test]
    fn a_code_is_requested_then_polled_until_a_token_arrives() {
        let transport = Recorded::new(&[
            r#"{"device_code":"dc","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","interval":5,"expires_in":900}"#,
            r#"{"error":"authorization_pending"}"#,
            r#"{"error":"slow_down","interval":10}"#,
            r#"{"access_token":"ghu_new","refresh_token":"ghr_new","expires_in":28800,"refresh_token_expires_in":15897600}"#,
        ]);
        let flow = flow(transport.clone());

        let code = futures::executor::block_on(flow.start()).unwrap();
        assert_eq!(code.user_code, "WDJB-MJHT");
        assert_eq!(code.interval_secs, 5);
        assert_eq!(code.expires_in_secs, 900);

        assert_eq!(
            futures::executor::block_on(flow.poll(code.device_code.clone(), 1_000)).unwrap(),
            DevicePoll::Pending
        );
        assert_eq!(
            futures::executor::block_on(flow.poll(code.device_code.clone(), 1_000)).unwrap(),
            DevicePoll::SlowDown { interval_secs: 10 }
        );
        let DevicePoll::Token { token } =
            futures::executor::block_on(flow.poll(code.device_code, 1_000)).unwrap()
        else {
            panic!("expected a token");
        };
        assert_eq!(token.access_token, "ghu_new");
        assert_eq!(token.expires_at, Some(1_000 + 28_800));

        // Swift never has to escape the grant-type URN itself.
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[0].url, "https://github.com/login/device/code");
        assert!(seen[0].body.contains("client_id=Iv1.testclientid"));
        assert_eq!(seen[1].url, "https://github.com/login/oauth/access_token");
        assert!(
            seen[1]
                .body
                .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"),
            "{}",
            seen[1].body
        );
        assert_eq!(seen[0].user_agent, "prmarmot-ffi-test/0");
    }

    #[test]
    fn declining_and_expiring_are_separate_answers() {
        let denied = flow(Recorded::new(&[r#"{"error":"access_denied"}"#]));
        assert_eq!(
            futures::executor::block_on(denied.poll("dc".into(), 0)).unwrap(),
            DevicePoll::Denied
        );
        let expired = flow(Recorded::new(&[r#"{"error":"expired_token"}"#]));
        assert_eq!(
            futures::executor::block_on(expired.poll("dc".into(), 0)).unwrap(),
            DevicePoll::Expired
        );
    }

    #[test]
    fn a_token_refreshes_without_a_client_secret() {
        let transport = Recorded::new(&[
            r#"{"access_token":"ghu_2","refresh_token":"ghr_2","expires_in":28800}"#,
        ]);
        let token =
            futures::executor::block_on(flow(transport.clone()).refresh("ghr_1".into(), 5_000))
                .unwrap();
        assert_eq!(token.access_token, "ghu_2");
        assert_eq!(token.expires_at, Some(5_000 + 28_800));

        let seen = transport.seen.lock().unwrap();
        assert!(seen[0].body.contains("refresh_token=ghr_1"));
        assert!(
            !seen[0].body.contains("client_secret"),
            "the device flow refreshes with the client id alone"
        );
    }

    #[test]
    fn the_placeholder_client_id_is_visible_before_a_request_is_made() {
        let unregistered = DeviceFlow::new(
            "github.com".into(),
            "REGISTER-THE-PRMARMOT-GITHUB-APP".into(),
            "prmarmot-ffi-test/0".into(),
            Recorded::new(&[]),
        );
        assert!(unregistered.client_id_is_placeholder());
        assert!(!flow(Recorded::new(&[])).client_id_is_placeholder());
        assert_eq!(
            flow(Recorded::new(&[])).verification_url(),
            "https://github.com/login/device"
        );
    }

    #[test]
    fn the_refresh_skew_belongs_to_core_and_is_not_retyped_in_swift() {
        let token = prmarmot_ffi::token_from_pat("ghp_x".into());
        assert!(!prmarmot_ffi::token_needs_refresh(token.clone(), 0));
        assert!(!prmarmot_ffi::token_can_refresh(token, 0));

        let expiring = prmarmot_ffi::Token {
            access_token: "ghu_x".into(),
            refresh_token: Some("ghr_x".into()),
            expires_at: Some(1_000),
            refresh_expires_at: Some(100_000),
            scope: None,
        };
        // Five minutes of skew: live at 600 s out, due at 200 s out.
        assert!(!prmarmot_ffi::token_needs_refresh(expiring.clone(), 400));
        assert!(prmarmot_ffi::token_needs_refresh(expiring.clone(), 800));
        assert!(prmarmot_ffi::token_can_refresh(expiring, 800));
    }
}

#[test]
fn a_board_survives_the_offline_cache() {
    let transport = Offline::fixture("authored_response.json");
    let board = futures::executor::block_on(client(transport).fetch_board(
        Mode::Authored,
        BoardScope::Repository {
            name: "acme/widgets".into(),
        },
        settings(),
        NOW,
    ))
    .unwrap();

    let bytes = prmarmot_ffi::encode_board(board.clone(), NOW).unwrap();
    let restored = prmarmot_ffi::decode_board(bytes).unwrap();
    assert_eq!(restored.board, board, "the cache is lossless");
    assert_eq!(restored.fetched_at_epoch, NOW);

    // Nonsense and a future version are refused rather than half-read.
    assert!(prmarmot_ffi::decode_board(b"not json".to_vec()).is_err());
    let future = br#"{"version":99,"fetched_at":0,"board":null}"#.to_vec();
    assert!(prmarmot_ffi::decode_board(future).is_err());
}
