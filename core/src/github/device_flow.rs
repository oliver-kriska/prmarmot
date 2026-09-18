//! GitHub's OAuth **Device Flow**, the sign-in path that needs no client
//! secret, no callback URL, and therefore no server of ours — on github.com
//! and on GitHub Enterprise Server alike.
//!
//! Everything here is a pure function over an [`AuthTransport`]: nothing
//! sleeps, nothing reads the clock (`now_epoch` is a parameter, same rule as
//! `pickup`). The caller drives the poll loop, so a terminal, the desktop app
//! and iOS can each use their own timer without core growing a runtime.
//!
//! Facts this encodes, from GitHub's device-flow documentation:
//! the `client_secret` is not needed to obtain a token **or to refresh one**
//! (the device-flow exemption); `user_code` is 8 characters plus a hyphen;
//! `interval` defaults to 5 s and `slow_down` adds 5 s to it; a device code
//! expires after 900 s; the verification URI is `https://HOST/login/device`.

use serde_json::Value;

use super::{normalize_host, AuthTransport, GhError};

/// `grant_type` for exchanging a device code for a token.
pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// GitHub's default poll interval, used when a response omits `interval`.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
/// `slow_down` adds this to the interval; also the floor we never poll below.
pub const SLOW_DOWN_STEP_SECS: u64 = 5;
/// Never poll slower than this even if a server asks for it.
pub const MAX_POLL_INTERVAL_SECS: u64 = 60;
/// GitHub expires a device code after 900 s; used when `expires_in` is absent.
pub const DEFAULT_EXPIRES_IN_SECS: u64 = 900;
/// Refresh an access token this long before it expires (guards clock skew and
/// a slow refresh round trip).
pub const REFRESH_SKEW_SECS: i64 = 300;

/// Scopes requested when the registration is a classic **OAuth App**. A
/// **GitHub App** ignores this field, because its permissions are fixed when it
/// is registered.
///
/// Both registrations work here and both token shapes are tested: an OAuth App
/// answers with an access token alone, a GitHub App adds `expires_in` and a
/// refresh token. The difference that decides which to register is reach — an
/// OAuth App token sees every repository its owner can see, while a GitHub App
/// user token sees only the accounts and orgs where the app has been installed.
/// PR Marmot registered an OAuth App (2026-09-17) for that reason.
pub const SCOPES: &str = "repo read:org";

/// PR Marmot's own registration on github.com: a classic OAuth App named
/// "PR Marmot", owned by oliver-kriska, with Device Flow enabled
/// (registered 2026-09-17). A client ID is public by design — the device
/// flow has no client secret, and GitHub shows this value to every user who
/// signs in. There is no secret for this app; never add one.
///
/// A GitHub Enterprise Server host needs its own registration, set as
/// `[auth] client_id` (or `PRMARMOT_CLIENT_ID`).
pub const GITHUB_COM_CLIENT_ID: &str = "Ov23liJnPBmrUZRLYilH";

/// The registration this build carries for `host`, if any. Only github.com
/// has a built-in one.
pub fn default_client_id(host: &str) -> Option<&'static str> {
    (super::normalize_host(host) == "github.com").then_some(GITHUB_COM_CLIENT_ID)
}

/// What a build carries when it has no registration to use: no client ID at
/// all, or a host (a GitHub Enterprise Server instance) the ID it has was not
/// registered with. Kept obvious on purpose: it appears verbatim in the error
/// message a user would see.
pub const PLACEHOLDER_CLIENT_ID: &str = "REGISTER-THE-PRMARMOT-GITHUB-APP";

/// True while the app has no real client ID, so callers can offer the token
/// path instead of a device flow that cannot possibly succeed.
pub fn is_placeholder_client_id(client_id: &str) -> bool {
    client_id.trim().is_empty() || client_id.trim() == PLACEHOLDER_CLIENT_ID
}

/// The code GitHub hands out at the start of a flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// The secret half, sent back when polling. Never shown to the user.
    pub device_code: String,
    /// The half the user types, e.g. `WDJB-MJHT`.
    pub user_code: String,
    /// Where the user types it, e.g. `https://github.com/login/device`.
    pub verification_uri: String,
    /// Seconds between polls, already clamped.
    pub interval_secs: u64,
    /// Seconds until `device_code` stops working.
    pub expires_in_secs: u64,
}

/// An access token plus, for GitHub App user tokens, the refresh half.
/// Times are Unix seconds so the type needs neither a clock nor chrono's
/// `serde` feature.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// `None` = never expires (a PAT, or an OAuth App token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl TokenSet {
    /// A pasted personal access token: no expiry we can see, no refresh.
    pub fn from_pat(token: impl Into<String>) -> Self {
        Self {
            access_token: token.into(),
            refresh_token: None,
            expires_at: None,
            refresh_expires_at: None,
            scope: None,
        }
    }

    /// True when the access token is gone or close enough that the next
    /// request would race its expiry.
    pub fn needs_refresh(&self, now_epoch: i64) -> bool {
        self.expires_at
            .is_some_and(|expires| expires - REFRESH_SKEW_SECS <= now_epoch)
    }

    /// True when a refresh could still succeed. A refresh token itself expires
    /// (6 months for GitHub App user tokens), after which only a new sign-in
    /// works.
    pub fn can_refresh(&self, now_epoch: i64) -> bool {
        self.refresh_token
            .as_ref()
            .is_some_and(|token| !token.is_empty())
            && self
                .refresh_expires_at
                .is_none_or(|expires| expires > now_epoch)
    }
}

/// One poll of the token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePoll {
    /// The user has not finished at GitHub yet; poll again after the interval.
    Pending,
    /// GitHub asked us to back off; poll again after the new interval.
    SlowDown { interval_secs: u64 },
    /// Signed in.
    Token(Box<TokenSet>),
    /// The device code aged out (900 s); start a new flow.
    Expired,
    /// The user pressed Cancel at GitHub.
    Denied,
}

/// A device-flow session for one `(host, client_id)` pair. GHES instances are
/// separate registrations, so the pair — not the host alone — identifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFlow {
    pub host: String,
    pub client_id: String,
}

impl DeviceFlow {
    pub fn new(host: impl AsRef<str>, client_id: impl Into<String>) -> Self {
        Self {
            host: normalize_host(host.as_ref()),
            client_id: client_id.into(),
        }
    }

    /// Where the user types the code.
    pub fn verification_url(&self) -> String {
        format!("https://{}/login/device", self.host)
    }

    /// Where a flow is started.
    pub fn code_url(&self) -> String {
        format!("https://{}/login/device/code", self.host)
    }

    /// Where a code is exchanged for a token, and where tokens are refreshed.
    pub fn token_url(&self) -> String {
        format!("https://{}/login/oauth/access_token", self.host)
    }

    fn check_client_id(&self) -> Result<(), GhError> {
        if is_placeholder_client_id(&self.client_id) {
            return Err(GhError::NotAuthenticated);
        }
        Ok(())
    }

    /// Ask GitHub for a code pair. One unauthenticated POST.
    pub fn start(&self, transport: &dyn AuthTransport) -> Result<DeviceCode, GhError> {
        self.check_client_id()?;
        let body = transport.post_form(
            &self.code_url(),
            &[("client_id", &self.client_id), ("scope", SCOPES)],
        )?;
        parse_device_code(&body, &self.host)
    }

    /// One poll. The caller sleeps for the interval between calls.
    pub fn poll(
        &self,
        transport: &dyn AuthTransport,
        device_code: &str,
        now_epoch: i64,
    ) -> Result<DevicePoll, GhError> {
        self.check_client_id()?;
        let body = transport.post_form(
            &self.token_url(),
            &[
                ("client_id", &self.client_id),
                ("device_code", device_code),
                ("grant_type", DEVICE_CODE_GRANT),
            ],
        )?;
        parse_token_response(&body, now_epoch)
    }

    /// Trade a refresh token for a fresh pair. **No client secret** — the
    /// device-flow exemption is what makes the whole design serverless.
    pub fn refresh(
        &self,
        transport: &dyn AuthTransport,
        refresh_token: &str,
        now_epoch: i64,
    ) -> Result<TokenSet, GhError> {
        self.check_client_id()?;
        let body = transport.post_form(
            &self.token_url(),
            &[
                ("client_id", &self.client_id),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )?;
        match parse_token_response(&body, now_epoch)? {
            DevicePoll::Token(token) => Ok(*token),
            // A refresh that is refused is a sign-in problem, not a poll state.
            _ => Err(GhError::NotAuthenticated),
        }
    }
}

/// Read a `/login/device/code` response. `verification_uri` is honoured when
/// GitHub sends one and derived from the host otherwise.
pub fn parse_device_code(body: &Value, host: &str) -> Result<DeviceCode, GhError> {
    if let Some(error) = oauth_error(body) {
        return Err(device_error(error, body));
    }
    let string = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let device_code = string("device_code")
        .ok_or_else(|| GhError::Parse("device-code response had no device_code".into()))?;
    let user_code = string("user_code")
        .ok_or_else(|| GhError::Parse("device-code response had no user_code".into()))?;
    Ok(DeviceCode {
        device_code,
        user_code,
        verification_uri: string("verification_uri")
            .unwrap_or_else(|| format!("https://{}/login/device", normalize_host(host))),
        interval_secs: clamp_interval(number(body, "interval").unwrap_or(0)),
        expires_in_secs: number(body, "expires_in").unwrap_or(DEFAULT_EXPIRES_IN_SECS),
    })
}

/// Read a `/login/oauth/access_token` response, for a poll or a refresh.
/// Exposed so the state machine can be tested against recorded bodies without
/// a transport at all.
pub fn parse_token_response(body: &Value, now_epoch: i64) -> Result<DevicePoll, GhError> {
    if let Some(error) = oauth_error(body) {
        return Ok(match error {
            "authorization_pending" => DevicePoll::Pending,
            "slow_down" => DevicePoll::SlowDown {
                interval_secs: clamp_interval(
                    number(body, "interval")
                        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS + SLOW_DOWN_STEP_SECS),
                ),
            },
            "expired_token" => DevicePoll::Expired,
            "access_denied" => DevicePoll::Denied,
            other => return Err(device_error(other, body)),
        });
    }
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| GhError::Parse("token response had no access_token".into()))?;
    Ok(DevicePoll::Token(Box::new(TokenSet {
        access_token: access_token.to_owned(),
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned),
        expires_at: number(body, "expires_in").map(|secs| now_epoch + secs as i64),
        refresh_expires_at: number(body, "refresh_token_expires_in")
            .map(|secs| now_epoch + secs as i64),
        scope: body
            .get("scope")
            .and_then(Value::as_str)
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned),
    })))
}

fn oauth_error(body: &Value) -> Option<&str> {
    body.get("error")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
}

/// GitHub returns `error_description` next to `error`; prefer it, because it
/// is the sentence a user can act on.
fn device_error(error: &str, body: &Value) -> GhError {
    let described = body
        .get("error_description")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or(error);
    match error {
        // The refresh token is dead (or was never valid): the only way out is
        // a fresh sign-in, which is exactly what `NotAuthenticated` tells the
        // user to do.
        "bad_refresh_token" | "invalid_grant" | "bad_verification_code" => {
            GhError::NotAuthenticated
        }
        "incorrect_client_credentials" | "unauthorized_client" | "unsupported_grant_type" => {
            GhError::GraphqlErrors(vec![format!(
                "{described} — check the client ID in [auth] client_id"
            )])
        }
        _ => GhError::GraphqlErrors(vec![described.to_owned()]),
    }
}

/// `interval` arrives as a JSON number from GitHub and as a string from a form
/// -encoded body; accept both, and never poll faster than the documented floor
/// or slower than a minute.
fn number(body: &Value, key: &str) -> Option<u64> {
    let value = body.get(key)?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn clamp_interval(secs: u64) -> u64 {
    secs.clamp(DEFAULT_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// One recorded POST: the URL and the form fields sent with it.
    type Posted = (String, Vec<(String, String)>);

    /// Replays recorded bodies in order and records what was posted.
    struct Recorded {
        replies: Mutex<Vec<Value>>,
        seen: Mutex<Vec<Posted>>,
    }

    impl Recorded {
        fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().rev().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl AuthTransport for Recorded {
        fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<Value, GhError> {
            self.seen.lock().unwrap().push((
                url.to_owned(),
                fields
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ));
            self.replies
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| GhError::Network("no recorded reply".into()))
        }
    }

    fn flow() -> DeviceFlow {
        DeviceFlow::new("github.com", "Iv1.testclientid")
    }

    #[test]
    fn urls_follow_the_host_and_never_go_through_the_api_subdomain() {
        let github = flow();
        assert_eq!(github.code_url(), "https://github.com/login/device/code");
        assert_eq!(
            github.token_url(),
            "https://github.com/login/oauth/access_token"
        );
        assert_eq!(github.verification_url(), "https://github.com/login/device");

        let ghes = DeviceFlow::new("https://ghe.acme.test/", "ghes-client");
        assert_eq!(ghes.code_url(), "https://ghe.acme.test/login/device/code");
        assert_eq!(
            ghes.verification_url(),
            "https://ghe.acme.test/login/device"
        );
    }

    #[test]
    fn start_posts_only_the_client_id_and_scope() {
        let transport = Recorded::new(vec![json!({
            "device_code": "3584d83530557fdd1f46af8289938c8ef79f9dc5",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5
        })]);
        let code = flow().start(&transport).unwrap();
        assert_eq!(code.user_code, "WDJB-MJHT");
        assert_eq!(code.interval_secs, 5);
        assert_eq!(code.expires_in_secs, 900);

        let seen = transport.seen.lock().unwrap();
        let (url, fields) = &seen[0];
        assert_eq!(url, "https://github.com/login/device/code");
        assert_eq!(fields[0], ("client_id".into(), "Iv1.testclientid".into()));
        assert!(
            !fields.iter().any(|(key, _)| key == "client_secret"),
            "the device flow must never send a client secret"
        );
    }

    #[test]
    fn a_missing_verification_uri_is_derived_from_the_host() {
        let body = json!({"device_code": "d", "user_code": "ABCD-EFGH"});
        let code = parse_device_code(&body, "ghe.acme.test").unwrap();
        assert_eq!(code.verification_uri, "https://ghe.acme.test/login/device");
        assert_eq!(code.expires_in_secs, DEFAULT_EXPIRES_IN_SECS);
        assert_eq!(code.interval_secs, DEFAULT_POLL_INTERVAL_SECS);
    }

    #[test]
    fn every_documented_poll_response_maps_to_a_state() {
        let now = 1_750_000_000;
        let poll = |body: Value| parse_token_response(&body, now).unwrap();

        assert_eq!(
            poll(json!({"error": "authorization_pending"})),
            DevicePoll::Pending
        );
        assert_eq!(
            poll(json!({"error": "slow_down", "interval": 10})),
            DevicePoll::SlowDown { interval_secs: 10 }
        );
        // A `slow_down` without an interval still backs off by the documented step.
        assert_eq!(
            poll(json!({"error": "slow_down"})),
            DevicePoll::SlowDown {
                interval_secs: DEFAULT_POLL_INTERVAL_SECS + SLOW_DOWN_STEP_SECS
            }
        );
        assert_eq!(poll(json!({"error": "expired_token"})), DevicePoll::Expired);
        assert_eq!(poll(json!({"error": "access_denied"})), DevicePoll::Denied);

        let DevicePoll::Token(token) = poll(json!({
            "access_token": "ghu_abc",
            "expires_in": 28800,
            "refresh_token": "ghr_def",
            "refresh_token_expires_in": 15_897_600,
            "token_type": "bearer",
            "scope": ""
        })) else {
            panic!("expected a token");
        };
        assert_eq!(token.access_token, "ghu_abc");
        assert_eq!(token.expires_at, Some(now + 28_800));
        assert_eq!(token.refresh_expires_at, Some(now + 15_897_600));
        assert_eq!(token.scope, None);
    }

    #[test]
    fn form_encoded_numbers_parse_too() {
        // GHES and proxies sometimes answer with strings rather than numbers.
        let body = json!({"error": "slow_down", "interval": "12"});
        assert_eq!(
            parse_token_response(&body, 0).unwrap(),
            DevicePoll::SlowDown { interval_secs: 12 }
        );
    }

    #[test]
    fn poll_intervals_are_clamped_at_both_ends() {
        let fast = json!({"device_code": "d", "user_code": "u", "interval": 1});
        assert_eq!(
            parse_device_code(&fast, "github.com")
                .unwrap()
                .interval_secs,
            5
        );
        let slow = json!({"device_code": "d", "user_code": "u", "interval": 6000});
        assert_eq!(
            parse_device_code(&slow, "github.com")
                .unwrap()
                .interval_secs,
            MAX_POLL_INTERVAL_SECS
        );
    }

    #[test]
    fn an_oauth_app_token_without_an_expiry_never_asks_for_a_refresh() {
        let DevicePoll::Token(token) =
            parse_token_response(&json!({"access_token": "gho_x", "scope": "repo"}), 0).unwrap()
        else {
            panic!("expected a token");
        };
        assert_eq!(token.expires_at, None);
        assert!(!token.needs_refresh(i64::MAX));
        assert!(!token.can_refresh(0));
        assert_eq!(token.scope.as_deref(), Some("repo"));
    }

    #[test]
    fn refresh_boundaries_use_the_skew_not_the_raw_expiry() {
        let token = TokenSet {
            access_token: "ghu_a".into(),
            refresh_token: Some("ghr_b".into()),
            expires_at: Some(1_000),
            refresh_expires_at: Some(10_000),
            scope: None,
        };
        assert!(!token.needs_refresh(1_000 - REFRESH_SKEW_SECS - 1));
        assert!(token.needs_refresh(1_000 - REFRESH_SKEW_SECS));
        assert!(token.needs_refresh(2_000));
        assert!(token.can_refresh(9_999));
        assert!(!token.can_refresh(10_000));

        let pat = TokenSet::from_pat("ghp_c");
        assert!(!pat.needs_refresh(i64::MAX));
        assert!(!pat.can_refresh(0));
    }

    #[test]
    fn refresh_sends_no_secret_and_returns_the_new_pair() {
        let transport = Recorded::new(vec![json!({
            "access_token": "ghu_new",
            "expires_in": 28800,
            "refresh_token": "ghr_new",
            "refresh_token_expires_in": 15_897_600
        })]);
        let token = flow().refresh(&transport, "ghr_old", 100).unwrap();
        assert_eq!(token.access_token, "ghu_new");
        assert_eq!(token.refresh_token.as_deref(), Some("ghr_new"));

        let seen = transport.seen.lock().unwrap();
        let (url, fields) = &seen[0];
        assert_eq!(url, "https://github.com/login/oauth/access_token");
        assert!(fields
            .iter()
            .any(|(key, value)| key == "grant_type" && value == "refresh_token"));
        assert!(!fields.iter().any(|(key, _)| key == "client_secret"));
    }

    #[test]
    fn a_refused_refresh_reads_as_signed_out() {
        let transport = Recorded::new(vec![json!({"error": "bad_refresh_token"})]);
        assert_eq!(
            flow().refresh(&transport, "ghr_old", 0).unwrap_err(),
            GhError::NotAuthenticated
        );
    }

    #[test]
    fn a_bad_client_id_is_named_in_the_error() {
        let body = json!({
            "error": "incorrect_client_credentials",
            "error_description": "The client_id passed is incorrect."
        });
        let GhError::GraphqlErrors(messages) = parse_token_response(&body, 0).unwrap_err() else {
            panic!("expected a described error");
        };
        assert!(messages[0].contains("client_id"), "{messages:?}");
    }

    #[test]
    fn the_placeholder_client_id_never_reaches_the_network() {
        let transport = Recorded::new(vec![]);
        let flow = DeviceFlow::new("github.com", PLACEHOLDER_CLIENT_ID);
        assert_eq!(
            flow.start(&transport).unwrap_err(),
            GhError::NotAuthenticated
        );
        assert!(transport.seen.lock().unwrap().is_empty());
        assert!(is_placeholder_client_id(""));
        assert!(!is_placeholder_client_id("Iv1.real"));
    }
}
