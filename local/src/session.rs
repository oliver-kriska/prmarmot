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
    viewer_login, AuthTransport, GhError, GithubTransport, RestTransport, StaticToken, TokenSource,
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

/// Open a connection for these settings, resolving `auto`.
///
/// `auto` prefers a token this machine stored (that is what signing in means)
/// and falls back to `gh`, so an existing install keeps working untouched.
pub fn connect(settings: &AuthSettings, user_agent: &str) -> Result<Session, GhError> {
    let store = token_store(settings.store);
    match settings.mode {
        AuthMode::Gh => Ok(gh_session(settings)),
        AuthMode::Token => direct_session(settings, store.as_ref(), user_agent, TokenNeed::Any),
        AuthMode::Device => direct_session(settings, store.as_ref(), user_agent, TokenNeed::Device),
        AuthMode::Auto => {
            if settings.inline_token.is_some()
                || store
                    .load(&settings.host)
                    .map_err(GhError::Network)?
                    .is_some()
            {
                direct_session(settings, store.as_ref(), user_agent, TokenNeed::Any)
            } else {
                Ok(gh_session(settings))
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
    let transport =
        Arc::new(HttpTransport::new(&settings.host, source).with_user_agent(user_agent));
    Session {
        transport: transport.clone(),
        rest: Some(transport),
        connection,
        host: settings.host.clone(),
        stored_login,
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
    fn gh_stays_the_fallback_and_is_not_a_direct_connection() {
        let session = gh_session(&settings(AuthMode::Gh, None));
        assert_eq!(session.connection, Connection::GhCli);
        assert!(!session.connection.is_direct());
        assert_eq!(session.connection.word(), "gh");
    }

    #[test]
    fn user_agents_name_the_front_end() {
        assert_eq!(user_agent("prmarmot-cli", "0.8.1"), "prmarmot-cli/0.8.1");
    }
}
