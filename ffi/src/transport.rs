//! The two traits Swift implements, and the bridge that lets `prmarmot-core`
//! use them.
//!
//! **Why a bridge at all.** Core's `GithubTransport` is blocking by design:
//! one request per refresh, run on a background thread, no async runtime
//! anywhere (the "don't pull in a runtime you don't use" guardrail from the
//! PRFlow post-mortem). UniFFI's foreign traits, on the other hand, must be
//! `async` — a Swift implementation that blocks would block whatever thread
//! UniFFI called it on, and `URLSession` is callback-based regardless. So the
//! two halves have to meet somewhere, and this is that place: a worker thread
//! runs core's blocking algorithm, and an async loop on this side ferries each
//! request out to Swift and the answer back. No runtime, no `block_on`, no
//! chance of deadlocking a shared executor.
//!
//! **Why Swift is a dumb pipe.** The foreign transport receives a finished URL,
//! a token and a JSON body, and returns whatever the server said. Building the
//! GraphQL body (including the typed `tracked: [ID!]!` array) and deciding what
//! a 403 means stay in Rust, where they are tested once for every front end.
//! See `prmarmot_core::github::response`.

use std::sync::Arc;

use prmarmot_core::github::response::{classify, graphql_body, ResponseMeta};
use prmarmot_core::github::{graphql_url, GhError, GithubTransport as CoreTransport};
use serde_json::Value;

use crate::error::FfiError;

/// One HTTP header, because UniFFI has no map type worth the conversion.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// A finished request: POST `body` to `url` as `token`.
///
/// The implementation adds `Authorization: Bearer <token>`,
/// `Content-Type: application/json`, `Accept: application/json` and a
/// `User-Agent` (GitHub answers a request without one with a silent 403).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GraphqlRequest {
    pub url: String,
    pub token: String,
    /// The complete JSON request body.
    pub body: String,
    /// What to send as `User-Agent`.
    pub user_agent: String,
}

/// What came back. Report the status GitHub sent rather than turning it into
/// an error: this side knows which statuses mean "wait", "sign in again" and
/// "that repository is not visible to you", and Swift does not have to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: String,
}

/// Sending one request to GitHub. Implemented in Swift with `URLSession`.
///
/// Throw only when there was no answer at all — offline, timed out, cancelled.
/// Any answer, including 401, 403 and 502, belongs in [`HttpResponse`].
///
/// **Cycle rule:** an implementation must not hold a strong reference to the
/// [`crate::client::BoardClient`] it was given to. Rust holds the transport,
/// so a strong reference back is a retain cycle that ARC cannot break and
/// UniFFI cannot see. Use `weak` in Swift. See `ffi/README.md`.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait GithubTransport: Send + Sync {
    async fn send(&self, request: GraphqlRequest) -> Result<HttpResponse, FfiError>;
}

/// Where the API token comes from, asked once per fetch.
///
/// On iPad this reads the keychain and, when the device-flow token is close to
/// expiry, refreshes it — which is why it is `async` and why it may throw
/// [`FfiError::NotAuthenticated`] to mean "show the sign-in screen".
///
/// The same cycle rule as [`GithubTransport`] applies.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait TokenSource: Send + Sync {
    async fn token(&self) -> Result<String, FfiError>;
}

/// What the worker thread asks for, and what it gets back.
type Answer = Result<Value, GhError>;

struct Call {
    request: GraphqlRequest,
    reply: async_channel::Sender<Answer>,
}

/// The `prmarmot-core` transport the worker thread sees. Every call blocks
/// until [`run`]'s loop has been round the foreign transport and back.
struct Bridged {
    calls: async_channel::Sender<Call>,
    url: String,
    token: String,
    user_agent: String,
}

impl CoreTransport for Bridged {
    fn graphql(&self, query: &str, variables: &[(&str, &str)]) -> Answer {
        self.graphql_with_ids(query, variables, &[])
    }

    fn graphql_with_ids(&self, query: &str, variables: &[(&str, &str)], ids: &[String]) -> Answer {
        let body = graphql_body(query, variables, ids).to_string();
        let (reply, answers) = async_channel::bounded(1);
        let call = Call {
            request: GraphqlRequest {
                url: self.url.clone(),
                token: self.token.clone(),
                body,
                user_agent: self.user_agent.clone(),
            },
            reply,
        };
        // A closed channel means `run` gave up (the task was cancelled); the
        // worker should stop with an error rather than hang.
        self.calls
            .send_blocking(call)
            .map_err(|_| GhError::Network("the request was cancelled".into()))?;
        answers
            .recv_blocking()
            .map_err(|_| GhError::Network("the request was cancelled".into()))?
    }
}

/// Run one blocking core operation, with its GitHub calls served by the
/// foreign transport.
///
/// `work` is core's own code — `fetch_board_scoped_with_tracked`,
/// `fetch_more_board_scoped`, `fetch_tracked` — unchanged and still blocking.
/// It runs on its own thread for the length of one fetch. Each of those makes
/// exactly one request today, but nothing here assumes that.
pub(crate) async fn run<T, F>(
    transport: Arc<dyn GithubTransport>,
    tokens: Arc<dyn TokenSource>,
    host: &str,
    user_agent: &str,
    work: F,
) -> Result<T, FfiError>
where
    F: FnOnce(&dyn CoreTransport) -> Result<T, GhError> + Send + 'static,
    T: Send + 'static,
{
    // One token per fetch: a refresh mid-board would be worse than a retry.
    let token = tokens.token().await?;
    let url = graphql_url(host);
    let user_agent = user_agent.to_owned();

    let (calls, requests) = async_channel::bounded::<Call>(1);
    let (finished, outcome) = async_channel::bounded::<Result<T, GhError>>(1);

    std::thread::Builder::new()
        .name("prmarmot-fetch".into())
        .spawn(move || {
            let bridged = Bridged {
                calls,
                url,
                token,
                user_agent,
            };
            let result = work(&bridged);
            // Dropping the transport closes the request channel, which is how
            // the loop below learns there is nothing more to serve. It has to
            // happen before the result is sent, or the loop would wait on a
            // channel nobody will close.
            drop(bridged);
            let _ = finished.send_blocking(result);
        })
        .map_err(|e| FfiError::Network {
            message: format!("could not start the fetch: {e}"),
        })?;

    while let Ok(call) = requests.recv().await {
        let answer = serve(transport.as_ref(), call.request).await;
        if call.reply.send(answer).await.is_err() {
            break;
        }
    }

    match outcome.recv().await {
        Ok(result) => result.map_err(FfiError::from),
        Err(_) => Err(FfiError::Network {
            message: "the fetch stopped before it finished".into(),
        }),
    }
}

/// One round trip: ask Swift, then decide what the answer means.
async fn serve(transport: &dyn GithubTransport, request: GraphqlRequest) -> Answer {
    let response = transport.send(request).await.map_err(GhError::from)?;
    let headers: Vec<(&str, &str)> = response
        .headers
        .iter()
        .map(|header| (header.name.as_str(), header.value.as_str()))
        .collect();
    let meta = ResponseMeta::new(response.status).with_headers(headers);
    classify(meta, &response.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A transport that answers from a script, and records what it was asked.
    struct Scripted {
        answers: Mutex<Vec<Result<HttpResponse, FfiError>>>,
        seen: Mutex<Vec<GraphqlRequest>>,
    }

    impl Scripted {
        fn new(answers: Vec<Result<HttpResponse, FfiError>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl GithubTransport for Scripted {
        async fn send(&self, request: GraphqlRequest) -> Result<HttpResponse, FfiError> {
            self.seen.lock().unwrap().push(request);
            let mut answers = self.answers.lock().unwrap();
            if answers.is_empty() {
                return Err(FfiError::Network {
                    message: "the script ran out".into(),
                });
            }
            answers.remove(0)
        }
    }

    struct FixedToken(&'static str);

    #[async_trait::async_trait]
    impl TokenSource for FixedToken {
        async fn token(&self) -> Result<String, FfiError> {
            if self.0.is_empty() {
                return Err(FfiError::NotAuthenticated);
            }
            Ok(self.0.to_owned())
        }
    }

    fn ok(body: &str) -> Result<HttpResponse, FfiError> {
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: body.to_owned(),
        })
    }

    #[test]
    fn a_blocking_core_call_is_served_by_the_async_foreign_transport() {
        let transport = Scripted::new(vec![ok(r#"{"data":{"ok":true}}"#)]);
        let seen = transport.clone();
        let out: Value = futures::executor::block_on(run(
            transport,
            Arc::new(FixedToken("ghu_x")),
            "github.com",
            "prmarmot-test/0",
            |core| core.graphql("query($q:String!){ok}", &[("q", "is:open")]),
        ))
        .unwrap();
        assert_eq!(out["data"]["ok"], true);

        let seen = seen.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].url, "https://api.github.com/graphql");
        assert_eq!(seen[0].token, "ghu_x");
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["variables"]["q"], "is:open");
    }

    #[test]
    fn several_requests_in_one_operation_are_served_in_order() {
        let transport = Scripted::new(vec![ok(r#"{"n":1}"#), ok(r#"{"n":2}"#)]);
        let out: Vec<i64> = futures::executor::block_on(run(
            transport,
            Arc::new(FixedToken("t")),
            "github.com",
            "prmarmot-test/0",
            |core| {
                let first = core.graphql("a", &[])?;
                let second = core.graphql("b", &[])?;
                Ok(vec![
                    first["n"].as_i64().unwrap(),
                    second["n"].as_i64().unwrap(),
                ])
            },
        ))
        .unwrap();
        assert_eq!(out, vec![1, 2]);
    }

    #[test]
    fn the_host_decides_the_endpoint() {
        let transport = Scripted::new(vec![ok("{}")]);
        let seen = transport.clone();
        let _: Value = futures::executor::block_on(run(
            transport,
            Arc::new(FixedToken("t")),
            "ghe.acme.test",
            "prmarmot-test/0",
            |core| core.graphql("q", &[]),
        ))
        .unwrap();
        assert_eq!(
            seen.seen.lock().unwrap()[0].url,
            "https://ghe.acme.test/api/graphql"
        );
    }

    #[test]
    fn no_token_means_no_request_at_all() {
        let transport = Scripted::new(vec![ok("{}")]);
        let seen = transport.clone();
        let error = futures::executor::block_on(run(
            transport,
            Arc::new(FixedToken("")),
            "github.com",
            "prmarmot-test/0",
            |core| core.graphql("q", &[]),
        ))
        .unwrap_err();
        assert_eq!(error, FfiError::NotAuthenticated);
        assert!(seen.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn github_statuses_are_read_on_this_side_of_the_boundary() {
        let limited = Ok(HttpResponse {
            status: 403,
            headers: vec![
                Header {
                    name: "x-ratelimit-remaining".into(),
                    value: "0".into(),
                },
                Header {
                    name: "X-RateLimit-Reset".into(),
                    value: "1750".into(),
                },
            ],
            body: "{}".into(),
        });
        let error = futures::executor::block_on(run(
            Scripted::new(vec![limited]),
            Arc::new(FixedToken("t")),
            "github.com",
            "prmarmot-test/0",
            |core| core.graphql("q", &[]),
        ))
        .unwrap_err();
        assert_eq!(
            error,
            FfiError::RateLimited {
                reset_epoch: Some(1750)
            }
        );

        let unauthorized = Ok(HttpResponse {
            status: 401,
            headers: Vec::new(),
            body: r#"{"message":"Bad credentials"}"#.into(),
        });
        let error = futures::executor::block_on(run(
            Scripted::new(vec![unauthorized]),
            Arc::new(FixedToken("t")),
            "github.com",
            "prmarmot-test/0",
            |core| core.graphql("q", &[]),
        ))
        .unwrap_err();
        assert_eq!(error, FfiError::NotAuthenticated);
    }

    #[test]
    fn a_transport_that_throws_reaches_the_caller_unchanged() {
        let error = futures::executor::block_on(run(
            Scripted::new(vec![Err(FfiError::Network {
                message: "the network connection was lost".into(),
            })]),
            Arc::new(FixedToken("t")),
            "github.com",
            "prmarmot-test/0",
            |core| core.graphql("q", &[]),
        ))
        .unwrap_err();
        assert_eq!(
            error,
            FfiError::Network {
                message: "the network connection was lost".into()
            }
        );
    }
}
