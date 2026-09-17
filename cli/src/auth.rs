//! `prmarmot-cli auth login | status | logout` — signing in to GitHub without
//! the `gh` CLI.
//!
//! The protocol lives in `prmarmot-core::github::device_flow` and the storage
//! in `prmarmot-local::auth`; this file is the terminal around them: printing
//! the one-time code, sleeping between polls, and saying what happened.

use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

use prmarmot_core::github::device_flow::{DeviceFlow, DevicePoll, TokenSet};
use prmarmot_core::github::{viewer_login, GhError};
use prmarmot_local::auth::{token_store, StoredAuth, TokenKind};
use prmarmot_local::config::{self, AuthSettings};
use prmarmot_local::session;

/// What `auth` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Login {
        /// Read a personal access token from stdin instead of running the
        /// device flow (the shape `gh auth login --with-token` uses).
        with_token: bool,
    },
    Status,
    Logout,
}

/// Flags shared by the three actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub host: Option<String>,
    pub client_id: Option<String>,
}

/// How a run ended, so `main` can pick an exit code without re-deciding.
pub enum Outcome {
    Ok,
    /// Not signed in; the caller exits with the auth code.
    NotSignedIn,
    Failed(GhError),
}

pub fn run(action: Action, options: Options, version: &str) -> Outcome {
    let mut warnings = Vec::new();
    let file = config::try_load().unwrap_or_else(|warning| {
        warnings.push(warning);
        config::FileConfig::default()
    });
    let mut settings = config::auth_settings(&file, options.host.as_deref(), None, &mut warnings);
    if let Some(client_id) = options.client_id.filter(|id| !id.trim().is_empty()) {
        settings.client_id = client_id;
    }
    for warning in &warnings {
        eprintln!("prmarmot-cli: {warning}");
    }
    let agent = session::user_agent("prmarmot-cli", version);
    match action {
        Action::Login { with_token: true } => login_with_token(&settings, &agent),
        Action::Login { with_token: false } => login_with_device_flow(&settings, &agent),
        Action::Status => status(&settings, &agent),
        Action::Logout => logout(&settings),
    }
}

fn login_with_token(settings: &AuthSettings, agent: &str) -> Outcome {
    let token = match read_token_from_stdin() {
        Ok(token) => token,
        Err(message) => {
            eprintln!("prmarmot-cli: {message}");
            return Outcome::NotSignedIn;
        }
    };
    let probe = session::probe_transport(&settings.host, &token, agent);
    let login = match viewer_login(&probe) {
        Ok(login) => login,
        Err(error) => return Outcome::Failed(error),
    };
    let stored = StoredAuth::pat(&settings.host, &token).with_login(&login);
    store(settings, &stored, &login, "a personal access token")
}

fn read_token_from_stdin() -> Result<String, String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err("--with-token reads the token from standard input, e.g. \
             `gh auth token | prmarmot-cli auth login --with-token`"
            .into());
    }
    let mut text = String::new();
    stdin
        .read_to_string(&mut text)
        .map_err(|e| format!("could not read the token: {e}"))?;
    let token = text.trim().to_owned();
    if token.is_empty() {
        return Err("no token on standard input".into());
    }
    Ok(token)
}

fn login_with_device_flow(settings: &AuthSettings, agent: &str) -> Outcome {
    if settings.client_id_is_placeholder() {
        eprintln!(
            "prmarmot-cli: no GitHub client ID is configured for {host}, so the device flow \
             cannot start.\n  Set one with PRMARMOT_CLIENT_ID or [auth] client_id in {config}, \
             or sign in with a token:\n    gh auth token | prmarmot-cli auth login --with-token",
            host = settings.host,
            config = config::config_path().display()
        );
        return Outcome::NotSignedIn;
    }
    let transport = session::auth_transport(&settings.host, agent);
    let flow = DeviceFlow::new(&settings.host, &settings.client_id);
    let code = match flow.start(transport.as_ref()) {
        Ok(code) => code,
        Err(error) => return Outcome::Failed(error),
    };

    println!("One-time code: {}", code.user_code);
    println!("Open {} and enter it.", code.verification_uri);
    println!(
        "Waiting for you to finish (the code expires in {}).",
        minutes(code.expires_in_secs)
    );
    let _ = std::io::stdout().flush();

    let token = match poll_until_signed_in(&flow, transport.as_ref(), &code) {
        Ok(Some(token)) => token,
        Ok(None) => return Outcome::NotSignedIn,
        Err(error) => return Outcome::Failed(error),
    };

    let stored = StoredAuth::device(&settings.host, &settings.client_id, token);
    let probe = session::probe_transport(&settings.host, &stored.token.access_token, agent);
    let login = match viewer_login(&probe) {
        Ok(login) => login,
        Err(error) => return Outcome::Failed(error),
    };
    let stored = stored.with_login(&login);
    store(settings, &stored, &login, "the device flow")
}

/// The poll loop. Core stays free of timers; the sleeping happens here.
fn poll_until_signed_in(
    flow: &DeviceFlow,
    transport: &dyn prmarmot_core::github::AuthTransport,
    code: &prmarmot_core::github::device_flow::DeviceCode,
) -> Result<Option<TokenSet>, GhError> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in_secs);
    let mut interval = Duration::from_secs(code.interval_secs);
    while Instant::now() < deadline {
        std::thread::sleep(interval);
        match flow.poll(transport, &code.device_code, chrono::Utc::now().timestamp())? {
            DevicePoll::Pending => {}
            DevicePoll::SlowDown { interval_secs } => interval = Duration::from_secs(interval_secs),
            DevicePoll::Token(token) => return Ok(Some(*token)),
            DevicePoll::Expired => {
                eprintln!("prmarmot-cli: the code expired — run `prmarmot-cli auth login` again");
                return Ok(None);
            }
            DevicePoll::Denied => {
                eprintln!("prmarmot-cli: the request was declined at GitHub");
                return Ok(None);
            }
        }
    }
    eprintln!("prmarmot-cli: the code expired — run `prmarmot-cli auth login` again");
    Ok(None)
}

fn store(settings: &AuthSettings, stored: &StoredAuth, login: &str, how: &str) -> Outcome {
    let store = token_store(settings.store);
    if let Err(message) = store.save(stored) {
        return Outcome::Failed(GhError::Network(message));
    }
    println!("Signed in to {} as {login} with {how}.", settings.host);
    println!("Token stored in {}.", store.describe());
    Outcome::Ok
}

fn status(settings: &AuthSettings, agent: &str) -> Outcome {
    let store = token_store(settings.store);
    println!("{}", settings.host);
    match store.load(&settings.host) {
        Err(message) => eprintln!("prmarmot-cli: {message}"),
        Ok(None) => {}
        Ok(Some(stored)) => {
            let how = match stored.kind {
                TokenKind::Device => "the device flow",
                TokenKind::Token => "a personal access token",
            };
            let who = stored.login.clone().unwrap_or_else(|| "?".into());
            println!("  Signed in as {who} with {how}.");
            println!("  Token stored in {}.", store.describe());
            let now = chrono::Utc::now().timestamp();
            if let Some(expires) = stored.token.expires_at {
                println!("  Access token {}.", expiry_phrase(expires, now));
            }
            if let Some(expires) = stored.token.refresh_expires_at {
                println!("  Refresh token {}.", expiry_phrase(expires, now));
            }
            if stored.token.needs_refresh(now) && !stored.token.can_refresh(now) {
                println!("  Sign in again: `prmarmot-cli auth login`.");
                return Outcome::NotSignedIn;
            }
            return Outcome::Ok;
        }
    }

    // Nothing stored: say whether `gh` would carry this machine anyway.
    match prmarmot_core::github::gh_cli::current_login() {
        Ok(login) => {
            println!("  Not signed in directly; using the GitHub CLI as {login}.");
            let _ = agent;
            Outcome::Ok
        }
        Err(GhError::NotInstalled) => {
            println!("  Not signed in, and the GitHub CLI is not installed.");
            println!("  Run `prmarmot-cli auth login`.");
            Outcome::NotSignedIn
        }
        Err(_) => {
            println!("  Not signed in, and the GitHub CLI is not signed in either.");
            println!("  Run `prmarmot-cli auth login`.");
            Outcome::NotSignedIn
        }
    }
}

fn logout(settings: &AuthSettings) -> Outcome {
    let store = token_store(settings.store);
    match store.load(&settings.host) {
        Err(message) => return Outcome::Failed(GhError::Network(message)),
        Ok(None) => {
            println!("No stored token for {}.", settings.host);
            return Outcome::Ok;
        }
        Ok(Some(_)) => {}
    }
    if let Err(message) = store.delete(&settings.host) {
        return Outcome::Failed(GhError::Network(message));
    }
    println!("Removed the stored token for {}.", settings.host);
    // Honest wording: revoking the grant at GitHub needs the client secret we
    // deliberately do not have, so the user does that half themselves.
    println!(
        "To revoke PR Marmot's access at GitHub as well, visit https://{}/settings/applications.",
        settings.host
    );
    Outcome::Ok
}

/// "expires in 7h 32m" / "expired 2h ago", without pulling in a date library
/// the CLI does not otherwise need for this.
fn expiry_phrase(expires_epoch: i64, now_epoch: i64) -> String {
    let delta = expires_epoch - now_epoch;
    if delta <= 0 {
        format!("expired {} ago", duration_words(-delta as u64))
    } else {
        format!("expires in {}", duration_words(delta as u64))
    }
}

fn duration_words(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn minutes(secs: u64) -> String {
    let minutes = secs / 60;
    if minutes <= 1 {
        format!("{secs} seconds")
    } else {
        format!("{minutes} minutes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_reads_in_plain_words() {
        assert_eq!(expiry_phrase(7_200, 0), "expires in 2h 0m");
        assert_eq!(expiry_phrase(0, 7_200), "expired 2h 0m ago");
        assert_eq!(expiry_phrase(15_897_600, 0), "expires in 184d 0h");
        assert_eq!(expiry_phrase(300, 0), "expires in 5m");
    }

    #[test]
    fn the_code_lifetime_reads_as_minutes() {
        assert_eq!(minutes(900), "15 minutes");
        assert_eq!(minutes(45), "45 seconds");
    }
}
