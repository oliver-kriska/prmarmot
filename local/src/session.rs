//! Turning resolved `[auth]` settings into a transport both front ends can use.
//!
//! One place decides whether a run talks to GitHub through the `gh` CLI or
//! directly over HTTPS, so the desktop app, the CLI and any future front end
//! cannot drift on precedence. Feature-gated (`http`) because the direct
//! transport lives behind the same feature in `prmarmot-core`.

use std::sync::Arc;

use prmarmot_core::github::gh_cli::{self, GhCliTransport, RepoDiscovery};
use prmarmot_core::github::http::HttpTransport;
use prmarmot_core::github::{
    normalize_host, viewer_login, AuthTransport, GhError, GithubTransport, RestTransport,
    StaticToken, TokenSource,
};

use crate::auth::{token_store, StoredTokenSource, TokenKind, TokenStore};
use crate::config::{AuthMode, AuthSettings};

/// Which door this run actually went through, after `auto` resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    /// `gh api graphql`, with `gh` owning the token.
    GhCli,
    /// Direct HTTPS with a device-flow token stored on this machine.
    Device,
    /// Direct HTTPS with a personal access token.
    Token,
}

impl Connection {
    pub fn word(self) -> &'static str {
        match self {
            Self::GhCli => "gh",
            Self::Device => "device",
            Self::Token => "token",
        }
    }

    /// True when the token came from `PRMARMOT_TOKEN` or the token store
    /// rather than from `gh`, i.e. `gh` need not be installed at all.
    pub fn is_direct(self) -> bool {
        !matches!(self, Self::GhCli)
    }
}

/// A ready-to-use GitHub connection.
pub struct Session {
    transport: Arc<dyn GithubTransport>,
    rest: Option<Arc<HttpTransport>>,
    pub connection: Connection,
    pub host: String,
    /// The login remembered at sign-in, when there is one; `login()` falls
    /// back to asking GitHub.
    pub stored_login: Option<String>,
    /// True when a pasted fine-grained token is carrying this session. Its
    /// reach is one owner's repositories, so the board can be missing an
    /// organization's private repositories with nothing to say so.
    pub fine_grained_token: bool,
}

impl Session {
    pub fn transport(&self) -> &dyn GithubTransport {
        self.transport.as_ref()
    }

    /// The same transport as an `Arc`, for front ends that keep it alive
    /// across refreshes (the desktop app's `AppState`).
    pub fn transport_arc(&self) -> Arc<dyn GithubTransport> {
        self.transport.clone()
    }

    /// The signed-in account. The `gh` path keeps using `gh api user` so it
    /// spends REST budget rather than a GraphQL point, exactly as before.
    pub fn login(&self) -> Result<String, GhError> {
        match self.connection {
            Connection::GhCli => gh_cli::current_login(),
            _ => viewer_login(self.transport()),
        }
    }

    /// Every repository affiliation visible to this account, bounded exactly
    /// as the `gh` path bounds it.
    pub fn list_repos(&self) -> Result<RepoDiscovery, GhError> {
        match &self.rest {
            None => gh_cli::list_repos(),
            Some(rest) => gh_cli::discover_repos_with(|page| {
                rest.rest_get(
                    "user/repos",
                    &[
                        ("affiliation", "owner,collaborator,organization_member"),
                        ("per_page", "100"),
                        ("sort", "pushed"),
                        ("page", &page.to_string()),
                    ],
                )
            }),
        }
    }
}

/// The user agent every direct request carries. GitHub answers a request
/// without one with a silent 403.
pub fn user_agent(front_end: &str, version: &str) -> String {
    format!("{front_end}/{version}")
}

/// What the GitHub CLI can do for one host, checked once when a session opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhLogin {
    /// `gh` has a login for the host, and its requests go to that host.
    SignedIn,
    /// `gh` is installed but can't carry this host: no login there, or its
    /// requests go elsewhere (see [`gh_login`]).
    SignedOut,
    /// There is no `gh`.
    Missing,
    /// `gh` is there but didn't answer (a timeout, a crash). `auto` still
    /// goes through it, as it did before this check existed, so that `gh`'s
    /// own requests say what is wrong.
    Unknown,
}

/// Probe the GitHub CLI for `host`, without a network call.
///
/// The `gh` transport doesn't pass `--hostname`, so its requests go to
/// `GH_HOST`, or github.com without one. A login for any other host can't
/// carry this run, so it reads as signed out rather than sending the board
/// query to the wrong server.
pub fn gh_login(host: &str) -> GhLogin {
    let host = normalize_host(host);
    let gh_host = normalize_host(&std::env::var("GH_HOST").unwrap_or_default());
    match gh_cli::has_login(&host) {
        Ok(true) if gh_host == host => GhLogin::SignedIn,
        Ok(_) => GhLogin::SignedOut,
        Err(GhError::NotInstalled) => GhLogin::Missing,
        Err(_) => GhLogin::Unknown,
    }
}

/// Which sign-in a run uses, for status displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// A token supplied for this run (`PRMARMOT_TOKEN`), never stored.
    Supplied,
    /// The GitHub CLI's login.
    GhCli,
    /// A token stored by signing in here.
    Stored,
}

/// The sign-in [`connect`] picks, in the same order, so `auth status` and
/// Settings can't disagree with a refresh. `stored` is the kind of token
/// stored for the host, and `supplied` says a `PRMARMOT_TOKEN` is set.
///
/// `auto` takes a token someone chose on purpose first: one supplied for this
/// run, then a personal access token they pasted (**Use a token**, `auth login
/// --with-token`). Then the GitHub CLI login, then a device-flow token, then
/// nothing. `gh` comes before the device-flow token because that token belongs
/// to PR Marmot's own OAuth app, which organizations' OAuth-app restrictions
/// apply to. The GitHub CLI is a privileged OAuth app they don't apply to, so
/// preferring the device-flow token would silently hide repositories in
/// restricting organizations. A pasted token is the user's own choice of
/// credentials, and on a CI runner, where `gh` is often signed in through
/// `GH_TOKEN`, it must not be quietly replaced. `[auth] mode = "token"` or
/// `PRMARMOT_AUTH=token` still picks a stored device-flow token over `gh`.
pub fn sign_in_used(
    mode: AuthMode,
    gh: GhLogin,
    stored: Option<TokenKind>,
    supplied: bool,
) -> Option<SignIn> {
    match mode {
        AuthMode::Gh => Some(SignIn::GhCli),
        AuthMode::Token if supplied => Some(SignIn::Supplied),
        AuthMode::Token => stored.map(|_| SignIn::Stored),
        AuthMode::Device => (stored == Some(TokenKind::Device)).then_some(SignIn::Stored),
        AuthMode::Auto if supplied => Some(SignIn::Supplied),
        AuthMode::Auto if stored == Some(TokenKind::Token) => Some(SignIn::Stored),
        AuthMode::Auto if matches!(gh, GhLogin::SignedIn | GhLogin::Unknown) => Some(SignIn::GhCli),
        AuthMode::Auto => stored.map(|_| SignIn::Stored),
    }
}

/// Open a connection for these settings, resolving `auto` in the order
/// [`sign_in_used`] gives, or failing with the error the front ends show as
/// the sign-in screen.
pub fn connect(settings: &AuthSettings, user_agent: &str) -> Result<Session, GhError> {
    let store = token_store(settings.store);
    connect_with(settings, user_agent, store.as_ref(), || {
        gh_login(&settings.host)
    })
}

/// [`connect`] with the token store and the `gh` probe passed in, so the
/// order is testable without a keychain or a `gh` binary. The probe runs
/// only for `auto`.
fn connect_with(
    settings: &AuthSettings,
    user_agent: &str,
    store: &dyn TokenStore,
    gh: impl FnOnce() -> GhLogin,
) -> Result<Session, GhError> {
    match settings.mode {
        AuthMode::Gh => Ok(gh_session(settings)),
        AuthMode::Token => direct_session(settings, store, user_agent, TokenNeed::Any),
        AuthMode::Device => direct_session(settings, store, user_agent, TokenNeed::Device),
        AuthMode::Auto => {
            // Only what can change the answer is looked at: the store (a
            // keychain read) unless a token was supplied, and `gh` unless a
            // token someone chose already won.
            let supplied = settings.inline_token.is_some();
            let stored = if supplied {
                None
            } else {
                store
                    .load(&settings.host)
                    .map_err(GhError::Network)?
                    .map(|auth| auth.kind)
            };
            let gh = if supplied || stored == Some(TokenKind::Token) {
                GhLogin::SignedOut
            } else {
                gh()
            };
            match sign_in_used(AuthMode::Auto, gh, stored, supplied) {
                Some(SignIn::GhCli) => Ok(gh_session(settings)),
                Some(SignIn::Supplied | SignIn::Stored) => {
                    direct_session(settings, store, user_agent, TokenNeed::Any)
                }
                None => Err(match gh {
                    GhLogin::Missing => GhError::NotInstalled,
                    _ => GhError::NotAuthenticated,
                }),
            }
        }
    }
}

enum TokenNeed {
    /// `--auth device`: only a device-flow token will do.
    Device,
    Any,
}

fn gh_session(settings: &AuthSettings) -> Session {
    Session {
        transport: Arc::new(GhCliTransport::new()),
        rest: None,
        connection: Connection::GhCli,
        host: settings.host.clone(),
        stored_login: None,
        fine_grained_token: false,
    }
}

fn direct_session(
    settings: &AuthSettings,
    store: &dyn TokenStore,
    user_agent: &str,
    need: TokenNeed,
) -> Result<Session, GhError> {
    // An inline token is a one-run override and is never written to the store.
    if let Some(token) = settings.inline_token.clone() {
        if matches!(need, TokenNeed::Any) {
            let source: Arc<dyn TokenSource> = Arc::new(StaticToken(token));
            return Ok(http_session(
                settings,
                source,
                user_agent,
                Connection::Token,
                None,
            ));
        }
    }
    let stored = store.load(&settings.host).map_err(GhError::Network)?;
    let Some(stored) = stored else {
        return Err(GhError::NotAuthenticated);
    };
    if matches!(need, TokenNeed::Device) && stored.kind != TokenKind::Device {
        return Err(GhError::NotAuthenticated);
    }
    let connection = match stored.kind {
        TokenKind::Device => Connection::Device,
        TokenKind::Token => Connection::Token,
    };
    let login = stored.login.clone();
    // The refresh endpoint takes no token, so it gets its own bare client.
    let refresher: Arc<dyn AuthTransport> =
        Arc::new(HttpTransport::unauthenticated(&settings.host).with_user_agent(user_agent));
    let source: Arc<dyn TokenSource> = Arc::new(StoredTokenSource::new(
        token_store(settings.store),
        &settings.host,
        Some(refresher),
    ));
    Ok(http_session(
        settings, source, user_agent, connection, login,
    ))
}

fn http_session(
    settings: &AuthSettings,
    source: Arc<dyn TokenSource>,
    user_agent: &str,
    connection: Connection,
    stored_login: Option<String>,
) -> Session {
    // Only a pasted token is asked about: a device-flow token is never
    // fine-grained, and reading it here would mean a refresh before the first
    // request. A store that cannot answer leaves the question open, and the
    // notice stays off rather than guessing.
    let fine_grained = matches!(connection, Connection::Token)
        && source
            .token()
            .is_ok_and(|token| crate::auth::is_fine_grained(&token));
    let transport =
        Arc::new(HttpTransport::new(&settings.host, source).with_user_agent(user_agent));
    Session {
        transport: transport.clone(),
        rest: Some(transport),
        connection,
        host: settings.host.clone(),
        stored_login,
        fine_grained_token: fine_grained,
    }
}

/// The unauthenticated client the device flow signs in with.
pub fn auth_transport(host: &str, user_agent: &str) -> Arc<dyn AuthTransport> {
    Arc::new(HttpTransport::unauthenticated(host).with_user_agent(user_agent))
}

/// A one-off authenticated client, for checking a token before storing it.
pub fn probe_transport(host: &str, token: &str, user_agent: &str) -> impl GithubTransport {
    HttpTransport::new(host, Arc::new(StaticToken(token.to_owned()))).with_user_agent(user_agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{StoreKind, StoredAuth};
    use prmarmot_core::github::device_flow::TokenSet;

    /// `Session` holds trait objects and has no `Debug`; this keeps the
    /// assertions readable without deriving one just for tests.
    fn error_of(result: Result<Session, GhError>) -> Option<GhError> {
        result.err()
    }

    fn settings(mode: AuthMode, inline: Option<&str>) -> AuthSettings {
        AuthSettings {
            host: "github.com".into(),
            client_id: "Iv1.abc".into(),
            mode,
            store: StoreKind::File,
            inline_token: inline.map(str::to_owned),
        }
    }

    struct Empty;
    impl TokenStore for Empty {
        fn load(&self, _host: &str) -> Result<Option<StoredAuth>, String> {
            Ok(None)
        }
        fn save(&self, _auth: &StoredAuth) -> Result<(), String> {
            Ok(())
        }
        fn delete(&self, _host: &str) -> Result<(), String> {
            Ok(())
        }
        fn describe(&self) -> String {
            "nothing".into()
        }
    }

    struct Holds(StoredAuth);
    impl TokenStore for Holds {
        fn load(&self, _host: &str) -> Result<Option<StoredAuth>, String> {
            Ok(Some(self.0.clone()))
        }
        fn save(&self, _auth: &StoredAuth) -> Result<(), String> {
            Ok(())
        }
        fn delete(&self, _host: &str) -> Result<(), String> {
            Ok(())
        }
        fn describe(&self) -> String {
            "a test store".into()
        }
    }

    #[test]
    fn an_inline_token_connects_directly_without_a_store() {
        let session = direct_session(
            &settings(AuthMode::Token, Some("ghp_inline")),
            &Empty,
            "prmarmot-test/0",
            TokenNeed::Any,
        )
        .unwrap();
        assert_eq!(session.connection, Connection::Token);
        assert!(session.connection.is_direct());
        assert!(
            !session.fine_grained_token,
            "a classic token reaches every organization"
        );
    }

    #[test]
    fn a_pasted_fine_grained_token_is_marked_as_one() {
        let session = direct_session(
            &settings(AuthMode::Token, Some("github_pat_11INLINE")),
            &Empty,
            "prmarmot-test/0",
            TokenNeed::Any,
        )
        .unwrap();
        assert!(session.fine_grained_token);
        // `gh` carries no token of ours, so the question never arises.
        assert!(!gh_session(&settings(AuthMode::Gh, None)).fine_grained_token);
    }

    #[test]
    fn device_mode_refuses_a_stored_personal_access_token() {
        let store = Holds(StoredAuth::pat("github.com", "ghp_x"));
        assert_eq!(
            error_of(direct_session(
                &settings(AuthMode::Device, None),
                &store,
                "prmarmot-test/0",
                TokenNeed::Device,
            )),
            Some(GhError::NotAuthenticated)
        );

        let ok = direct_session(
            &settings(AuthMode::Token, None),
            &store,
            "prmarmot-test/0",
            TokenNeed::Any,
        )
        .unwrap();
        assert_eq!(ok.connection, Connection::Token);
    }

    #[test]
    fn a_stored_device_token_reports_the_device_connection() {
        let store = Holds(
            StoredAuth::device("github.com", "Iv1.abc", TokenSet::from_pat("ghu_x"))
                .with_login("octo"),
        );
        let session = direct_session(
            &settings(AuthMode::Device, None),
            &store,
            "prmarmot-test/0",
            TokenNeed::Device,
        )
        .unwrap();
        assert_eq!(session.connection, Connection::Device);
        assert_eq!(session.stored_login.as_deref(), Some("octo"));
    }

    #[test]
    fn nothing_stored_and_no_inline_token_reads_as_signed_out() {
        assert_eq!(
            error_of(direct_session(
                &settings(AuthMode::Token, None),
                &Empty,
                "prmarmot-test/0",
                TokenNeed::Any,
            )),
            Some(GhError::NotAuthenticated)
        );
    }

    #[test]
    fn a_gh_session_is_not_a_direct_connection() {
        let session = gh_session(&settings(AuthMode::Gh, None));
        assert_eq!(session.connection, Connection::GhCli);
        assert!(!session.connection.is_direct());
        assert_eq!(session.connection.word(), "gh");
    }

    /// A personal access token someone pasted.
    fn stored_pat() -> Holds {
        Holds(StoredAuth::pat("github.com", "ghp_x").with_login("octo"))
    }

    /// A token from signing in with GitHub (the device flow).
    fn stored_device() -> Holds {
        Holds(
            StoredAuth::device("github.com", "Iv1.abc", TokenSet::from_pat("ghu_x"))
                .with_login("octo"),
        )
    }

    fn auto(inline: Option<&str>, store: &dyn TokenStore, gh: GhLogin) -> Result<Session, GhError> {
        connect_with(
            &settings(AuthMode::Auto, inline),
            "prmarmot-test/0",
            store,
            || gh,
        )
    }

    /// `auto` with a `gh` that records whether it was asked.
    fn auto_asking(inline: Option<&str>, store: &dyn TokenStore) -> (Session, bool) {
        let asked = std::cell::Cell::new(false);
        let session = connect_with(
            &settings(AuthMode::Auto, inline),
            "prmarmot-test/0",
            store,
            || {
                asked.set(true);
                GhLogin::SignedIn
            },
        )
        .unwrap();
        (session, asked.get())
    }

    #[test]
    fn auto_takes_the_github_cli_login_over_a_device_flow_token() {
        let session = auto(None, &stored_device(), GhLogin::SignedIn).unwrap();
        assert_eq!(session.connection, Connection::GhCli);
    }

    #[test]
    fn a_token_supplied_for_the_run_beats_the_github_cli_login() {
        // A CI runner: `gh` is signed in through GH_TOKEN, and the job passed
        // PRMARMOT_TOKEN. The job's token is used, and neither `gh` nor the
        // store is consulted.
        let (session, asked) = auto_asking(Some("ghp_job"), &stored_pat());
        assert_eq!(session.connection, Connection::Token);
        assert_eq!(
            session.stored_login, None,
            "the supplied token, not the stored one"
        );
        assert!(!asked);
    }

    #[test]
    fn a_pasted_token_beats_the_github_cli_login() {
        let (session, asked) = auto_asking(None, &stored_pat());
        assert_eq!(session.connection, Connection::Token);
        assert_eq!(session.stored_login.as_deref(), Some("octo"));
        assert!(!asked);
    }

    #[test]
    fn a_gh_that_does_not_answer_is_still_tried_first() {
        let session = auto(None, &stored_device(), GhLogin::Unknown).unwrap();
        assert_eq!(session.connection, Connection::GhCli);
    }

    #[test]
    fn auto_uses_the_stored_token_when_gh_is_missing_or_signed_out() {
        for gh in [GhLogin::Missing, GhLogin::SignedOut] {
            let session = auto(None, &stored_device(), gh).unwrap();
            assert_eq!(session.connection, Connection::Device, "{gh:?}");
            assert_eq!(session.stored_login.as_deref(), Some("octo"));
        }
    }

    #[test]
    fn auto_with_neither_is_the_sign_in_screen() {
        assert_eq!(
            error_of(auto(None, &Empty, GhLogin::Missing)),
            Some(GhError::NotInstalled)
        );
        assert_eq!(
            error_of(auto(None, &Empty, GhLogin::SignedOut)),
            Some(GhError::NotAuthenticated)
        );
    }

    #[test]
    fn status_names_the_same_sign_in_that_connect_uses() {
        use GhLogin::{Missing, SignedIn, SignedOut, Unknown};
        let (pat, device) = (stored_pat(), stored_device());
        for gh in [SignedIn, Unknown, SignedOut, Missing] {
            for stored in [None, Some(TokenKind::Device), Some(TokenKind::Token)] {
                for supplied in [false, true] {
                    let named = sign_in_used(AuthMode::Auto, gh, stored, supplied);
                    let store: &dyn TokenStore = match stored {
                        None => &Empty,
                        Some(TokenKind::Device) => &device,
                        Some(TokenKind::Token) => &pat,
                    };
                    let connected = auto(supplied.then_some("ghp_job"), store, gh)
                        .ok()
                        .map(|session| (session.connection, session.stored_login.is_some()));
                    let expected = match named {
                        Some(SignIn::Supplied) => Some((Connection::Token, false)),
                        Some(SignIn::GhCli) => Some((Connection::GhCli, false)),
                        Some(SignIn::Stored) if stored == Some(TokenKind::Device) => {
                            Some((Connection::Device, true))
                        }
                        Some(SignIn::Stored) => Some((Connection::Token, true)),
                        None => None,
                    };
                    assert_eq!(
                        connected, expected,
                        "{gh:?} {stored:?} supplied={supplied}: status {named:?}"
                    );
                }
            }
        }
        use SignIn::{GhCli, Stored, Supplied};
        let (device, pat) = (Some(TokenKind::Device), Some(TokenKind::Token));
        // The order in `auto`.
        assert_eq!(
            sign_in_used(AuthMode::Auto, SignedIn, pat, true),
            Some(Supplied)
        );
        assert_eq!(
            sign_in_used(AuthMode::Auto, SignedIn, pat, false),
            Some(Stored)
        );
        assert_eq!(
            sign_in_used(AuthMode::Auto, SignedIn, device, false),
            Some(GhCli)
        );
        assert_eq!(
            sign_in_used(AuthMode::Auto, Missing, device, false),
            Some(Stored)
        );
        assert_eq!(sign_in_used(AuthMode::Auto, Missing, None, false), None);
        // The explicit modes.
        assert_eq!(
            sign_in_used(AuthMode::Token, SignedIn, device, false),
            Some(Stored)
        );
        assert_eq!(
            sign_in_used(AuthMode::Token, SignedIn, device, true),
            Some(Supplied)
        );
        assert_eq!(sign_in_used(AuthMode::Device, SignedIn, pat, true), None);
        assert_eq!(
            sign_in_used(AuthMode::Device, SignedIn, device, true),
            Some(Stored)
        );
        assert_eq!(sign_in_used(AuthMode::Gh, Missing, pat, true), Some(GhCli));
    }

    #[test]
    fn explicit_modes_never_ask_gh() {
        let asked = std::cell::Cell::new(false);
        let probe = || {
            asked.set(true);
            GhLogin::SignedIn
        };
        let token = connect_with(
            &settings(AuthMode::Token, None),
            "prmarmot-test/0",
            &stored_pat(),
            probe,
        )
        .unwrap();
        assert_eq!(token.connection, Connection::Token);
        let gh = connect_with(
            &settings(AuthMode::Gh, None),
            "prmarmot-test/0",
            &Empty,
            || {
                asked.set(true);
                GhLogin::Missing
            },
        )
        .unwrap();
        assert_eq!(gh.connection, Connection::GhCli);
        assert!(!asked.get(), "an explicit mode probed gh");
    }

    #[test]
    fn user_agents_name_the_front_end() {
        assert_eq!(user_agent("prmarmot-cli", "0.8.1"), "prmarmot-cli/0.8.1");
    }
}
