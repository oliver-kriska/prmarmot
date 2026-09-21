//! One error type across the boundary.
//!
//! Swift sees this as a thrown `FfiError` with associated values. Every
//! foreign trait method returns it too. A foreign implementation that throws
//! something else — a Swift `Keychain` error, a `DecodingError` — arrives as
//! an unexpected callback error, which becomes `Network` here: without that
//! `From` impl UniFFI's generic converter panics, and the iOS profile aborts
//! on panic (iPad review, 2026-09-21).

use prmarmot_core::attention::{PersistError, RestoreError};
use prmarmot_core::github::GhError;

/// Why an operation could not finish.
///
/// The variants are the ones a front end has to *act* on differently: sign in
/// again, wait, ask for a different repository, show the message. Anything
/// else is `Network` with a sentence already written for a person to read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// No usable token: the caller must sign in again.
    #[error("not signed in to GitHub")]
    NotAuthenticated,
    /// GitHub's API budget is spent. `reset_epoch` is Unix seconds when it
    /// refills, when GitHub said so.
    #[error("GitHub's API rate limit is spent")]
    RateLimited { reset_epoch: Option<u64> },
    /// The repository does not exist, or this account cannot see it.
    #[error("repository not found: {repo}")]
    RepositoryNotFound { repo: String },
    /// `owner/name#number` does not exist, or this account cannot see it.
    #[error("pull request not found: {pull_request}")]
    PullRequestNotFound { pull_request: String },
    /// GitHub answered, but with errors in the GraphQL envelope.
    #[error("GitHub rejected the query: {message}")]
    Api { message: String },
    /// The response was not what this version of the client understands.
    #[error("could not read GitHub's answer: {message}")]
    Parse { message: String },
    /// Anything that went wrong on the way there and back, already phrased for
    /// a person: timeouts, 5xx, no route to host, a cancelled request.
    #[error("{message}")]
    Network { message: String },
    /// The caller passed something this crate cannot use — a row that is not a
    /// row, a state blob that does not validate, a scope with no repository.
    #[error("{message}")]
    Invalid { message: String },
}

impl From<GhError> for FfiError {
    fn from(error: GhError) -> Self {
        match error {
            GhError::NotAuthenticated => Self::NotAuthenticated,
            // `gh` is a desktop concept. On iPad there is no CLI to install,
            // so a missing one can only mean "you are not signed in".
            GhError::NotInstalled => Self::NotAuthenticated,
            GhError::RateLimited { reset_epoch } => Self::RateLimited { reset_epoch },
            GhError::RepositoryNotFound(repo) => Self::RepositoryNotFound { repo },
            GhError::PullRequestNotFound(pull_request) => {
                Self::PullRequestNotFound { pull_request }
            }
            // GitHub repeats one message per field it refused; say it once.
            GhError::GraphqlErrors(messages) => Self::Api {
                message: prmarmot_core::github::access::unique_messages(&messages).join("; "),
            },
            GhError::Parse(message) => Self::Parse { message },
            GhError::Network(message) => Self::Network { message },
            GhError::NeedsRepository => Self::Invalid {
                message: GhError::NeedsRepository.to_string(),
            },
        }
    }
}

/// Back the other way, for the bridge: a Swift transport's error has to reach
/// core's blocking `GithubTransport`, which only speaks `GhError`.
impl From<FfiError> for GhError {
    fn from(error: FfiError) -> Self {
        match error {
            FfiError::NotAuthenticated => Self::NotAuthenticated,
            FfiError::RateLimited { reset_epoch } => Self::RateLimited { reset_epoch },
            FfiError::RepositoryNotFound { repo } => Self::RepositoryNotFound(repo),
            FfiError::PullRequestNotFound { pull_request } => {
                Self::PullRequestNotFound(pull_request)
            }
            FfiError::Api { message } => Self::GraphqlErrors(vec![message]),
            FfiError::Parse { message } => Self::Parse(message),
            FfiError::Network { message } | FfiError::Invalid { message } => Self::Network(message),
        }
    }
}

/// A foreign trait method threw an error that is not an `FfiError`. UniFFI
/// picks this impl up by itself; there is no attribute to set.
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(error: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Network {
            message: format!("Something on this device failed: {}", error.reason),
        }
    }
}

impl From<PersistError> for FfiError {
    fn from(error: PersistError) -> Self {
        Self::Invalid {
            message: format!("could not save the attention state: {error}"),
        }
    }
}

impl From<RestoreError> for FfiError {
    fn from(error: RestoreError) -> Self {
        Self::Invalid {
            message: format!("could not read the attention state: {error}"),
        }
    }
}

impl FfiError {
    pub(crate) fn invalid(message: impl std::fmt::Display) -> Self {
        Self::Invalid {
            message: message.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FfiError;
    use uniffi::{LiftReturn, UnexpectedUniFFICallbackError};

    /// The path UniFFI takes when a Swift callback throws something that is
    /// not an `FfiError`. Without the `From` impl it panicked, and the iOS
    /// profile aborts on panic, so a Keychain failure in the token source
    /// ended the app.
    #[test]
    fn a_foreign_error_that_is_not_ours_arrives_as_a_network_error() {
        let result = <Result<String, FfiError> as LiftReturn<crate::UniFfiTag>>::
            handle_callback_unexpected_error(UnexpectedUniFFICallbackError::new(
                "Keychain.Failure.save(-25308)",
            ));
        assert!(
            matches!(&result, Err(FfiError::Network { message }) if message.contains("-25308")),
            "{result:?}"
        );
    }
}
