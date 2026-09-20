//! Signing in, across the boundary.
//!
//! The protocol is `prmarmot_core::github::device_flow` — pure functions that
//! neither sleep nor read a clock — so the iPad drives the poll loop with its
//! own timer, exactly as `prmarmot-cli` and the desktop app do with theirs.
//! What this file adds is the shape Swift sees: an object that holds the
//! foreign form transport, and a poll result it can switch on.
//!
//! No client secret appears anywhere, because the device flow does not need
//! one — not to get a token and not to refresh one. That is the whole reason
//! PR Marmot needs no server of its own.

use std::sync::Arc;

use prmarmot_core::github::device_flow as core_flow;

use crate::error::FfiError;
use crate::transport::{self, AuthTransport};

/// The code GitHub hands out at the start of a flow.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeviceCode {
    /// The secret half, sent back when polling. Never show this to the user.
    pub device_code: String,
    /// The half the user types, e.g. `WDJB-MJHT`.
    pub user_code: String,
    /// Where they type it, e.g. `https://github.com/login/device`.
    pub verification_uri: String,
    /// Seconds to wait between polls, already clamped.
    pub interval_secs: u64,
    /// Seconds until `device_code` stops working.
    pub expires_in_secs: u64,
}

impl From<core_flow::DeviceCode> for DeviceCode {
    fn from(code: core_flow::DeviceCode) -> Self {
        Self {
            device_code: code.device_code,
            user_code: code.user_code,
            verification_uri: code.verification_uri,
            interval_secs: code.interval_secs,
            expires_in_secs: code.expires_in_secs,
        }
    }
}

/// An access token and, for a GitHub App user token, the refresh half.
/// Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// `None` means it never expires — a personal access token, or an OAuth
    /// App token.
    pub expires_at: Option<i64>,
    pub refresh_expires_at: Option<i64>,
    pub scope: Option<String>,
}

impl From<core_flow::TokenSet> for Token {
    fn from(token: core_flow::TokenSet) -> Self {
        Self {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at: token.expires_at,
            refresh_expires_at: token.refresh_expires_at,
            scope: token.scope,
        }
    }
}

impl From<Token> for core_flow::TokenSet {
    fn from(token: Token) -> Self {
        Self {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at: token.expires_at,
            refresh_expires_at: token.refresh_expires_at,
            scope: token.scope,
        }
    }
}

/// What one poll found.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum DevicePoll {
    /// Nobody has finished at GitHub yet. Wait the interval and poll again.
    Pending,
    /// GitHub asked for a slower pace. Use this interval from now on.
    SlowDown { interval_secs: u64 },
    /// Signed in.
    Token { token: Token },
    /// The code timed out. Start a new flow.
    Expired,
    /// The user said no at GitHub.
    Denied,
}

impl From<core_flow::DevicePoll> for DevicePoll {
    fn from(poll: core_flow::DevicePoll) -> Self {
        match poll {
            core_flow::DevicePoll::Pending => Self::Pending,
            core_flow::DevicePoll::SlowDown { interval_secs } => Self::SlowDown { interval_secs },
            core_flow::DevicePoll::Token(token) => Self::Token {
                token: (*token).into(),
            },
            core_flow::DevicePoll::Expired => Self::Expired,
            core_flow::DevicePoll::Denied => Self::Denied,
        }
    }
}

/// One sign-in, on one host, with one client ID.
#[derive(uniffi::Object)]
pub struct DeviceFlow {
    transport: Arc<dyn AuthTransport>,
    user_agent: String,
    host: String,
    client_id: String,
}

#[uniffi::export]
impl DeviceFlow {
    /// `host` is `github.com` or a GitHub Enterprise Server host; `client_id`
    /// is the registration for **that** host, because a GHES instance has its
    /// own app.
    #[uniffi::constructor]
    pub fn new(
        host: String,
        client_id: String,
        user_agent: String,
        transport: Arc<dyn AuthTransport>,
    ) -> Arc<Self> {
        Arc::new(Self {
            transport,
            user_agent,
            host,
            client_id,
        })
    }

    /// True while the app ships without a registered client ID. Offer the
    /// token path instead of a flow that cannot possibly succeed.
    pub fn client_id_is_placeholder(&self) -> bool {
        core_flow::is_placeholder_client_id(&self.client_id)
    }

    /// Where the user types the code, for the "open GitHub" button.
    pub fn verification_url(&self) -> String {
        self.flow().verification_url()
    }

    /// Ask GitHub for a code. Show `user_code`, open `verification_uri`, then
    /// poll every `interval_secs` until `expires_in_secs` runs out.
    pub async fn start(&self) -> Result<DeviceCode, FfiError> {
        let flow = self.flow();
        self.run(move |transport| flow.start(transport))
            .await
            .map(Into::into)
    }

    /// One poll. `now_epoch` is Unix seconds and is used only to stamp the
    /// token's expiry; core still reads no clock of its own.
    pub async fn poll(&self, device_code: String, now_epoch: i64) -> Result<DevicePoll, FfiError> {
        let flow = self.flow();
        self.run(move |transport| flow.poll(transport, &device_code, now_epoch))
            .await
            .map(Into::into)
    }

    /// Trade a refresh token for a fresh pair. Needs no client secret — the
    /// device-flow exemption — which is why this can happen on the device.
    pub async fn refresh(&self, refresh_token: String, now_epoch: i64) -> Result<Token, FfiError> {
        let flow = self.flow();
        self.run(move |transport| flow.refresh(transport, &refresh_token, now_epoch))
            .await
            .map(Into::into)
    }
}

impl DeviceFlow {
    fn flow(&self) -> core_flow::DeviceFlow {
        core_flow::DeviceFlow::new(&self.host, self.client_id.clone())
    }

    async fn run<T, F>(&self, work: F) -> Result<T, FfiError>
    where
        F: FnOnce(
                &dyn prmarmot_core::github::AuthTransport,
            ) -> Result<T, prmarmot_core::github::GhError>
            + Send
            + 'static,
        T: Send + 'static,
    {
        transport::run_auth(self.transport.clone(), &self.user_agent, work).await
    }
}

/// Whether a stored token needs refreshing before the next request, and
/// whether it still can be. Swift's `TokenSource` asks these two before every
/// fetch rather than reimplementing the five-minute skew.
#[uniffi::export]
pub fn token_needs_refresh(token: Token, now_epoch: i64) -> bool {
    core_flow::TokenSet::from(token).needs_refresh(now_epoch)
}

#[uniffi::export]
pub fn token_can_refresh(token: Token, now_epoch: i64) -> bool {
    core_flow::TokenSet::from(token).can_refresh(now_epoch)
}

/// The one-time-code screen's note on organizations that restrict OAuth
/// apps: core's sentence, the same one the desktop shows, so the iPad does
/// not keep its own copy.
#[uniffi::export]
pub fn organization_approval_note() -> String {
    prmarmot_core::status::organization_approval_note().to_owned()
}

/// What a fine-grained token reaches, and what a classic one reaches: two
/// sentences so a screen that shows the two kinds side by side can put each
/// one in its own card.
#[uniffi::export]
pub fn fine_grained_reach_note() -> String {
    prmarmot_core::status::fine_grained_reach_note().to_owned()
}

/// The companion to [`fine_grained_reach_note`], for the classic-token card.
#[uniffi::export]
pub fn classic_reach_note() -> String {
    prmarmot_core::status::classic_reach_note().to_owned()
}

/// Shown under the header when a pasted fine-grained token is carrying the
/// board while the GitHub CLI is signed in to the same host.
#[uniffi::export]
pub fn pasted_token_reach_notice() -> String {
    prmarmot_core::status::pasted_token_reach_notice().to_owned()
}

/// A pasted personal access token, as a [`Token`] with no expiry.
#[uniffi::export]
pub fn token_from_pat(access_token: String) -> Token {
    core_flow::TokenSet::from_pat(access_token).into()
}
