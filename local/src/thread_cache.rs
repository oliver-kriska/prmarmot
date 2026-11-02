//! Caches for the bounded follow-ups only: the threads (a TTL) and the
//! Linear tickets (kept until forgotten). Board searches stay live.
use prmarmot_core::{
    attention::SnapshotNamespace,
    github::{
        query::{MAX_TICKET_IDS, TICKETS_QUERY, TURNS_QUERY},
        GhError, GithubTransport,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
/// Tickets are a few hundred bytes each once compacted.
const MAX_TICKET_ENTRIES: usize = 2048;
/// A PR with no Linear comment yet is asked about again after this long:
/// Linear can post it well after the PR opens (26 minutes on
/// construct-mobile#822). A ticket found is kept until forgotten.
const TICKET_MISS_TTL: u64 = 30 * 60;

#[derive(Serialize, Deserialize)]
struct Entry {
    fetched_at: u64,
    node: Value,
}
#[derive(Serialize, Deserialize)]
struct Cache {
    version: u8,
    namespace: SnapshotNamespace,
    entries: BTreeMap<String, Entry>,
}

pub struct ThreadCacheTransport {
    inner: Arc<dyn GithubTransport>,
    namespace: SnapshotNamespace,
    lock: Mutex<()>,
}

impl ThreadCacheTransport {
    pub fn new(inner: Arc<dyn GithubTransport>, namespace: SnapshotNamespace) -> Self {
        Self {
            inner,
            namespace,
            lock: Mutex::new(()),
        }
    }

    fn cached(
        &self,
        variables: &[(&str, &str)],
        ids: &[String],
        path: &Path,
        ttl: u64,
        now: u64,
    ) -> Result<Value, GhError> {
        // Queue switches can overlap. Serialize follow-ups so one refresh can
        // use another's result, and atomic writers cannot race on the same file.
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut cache = std::fs::metadata(path)
            .ok()
            .filter(|meta| meta.len() <= MAX_BYTES)
            .and_then(|_| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
            .filter(|cache| cache.version == 1 && cache.namespace == self.namespace)
            .unwrap_or_else(|| Cache {
                version: 1,
                namespace: self.namespace.clone(),
                entries: BTreeMap::new(),
            });
        cache.entries.retain(|_, entry| {
            now.checked_sub(entry.fetched_at)
                .is_some_and(|age| age < ttl)
        });
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !cache.entries.contains_key(*id))
            .cloned()
            .collect();
        let mut rate = Value::Null;
        let mut fresh = BTreeMap::new();
        if !missing.is_empty() {
            let body = self
                .inner
                .graphql_with_ids(TURNS_QUERY, variables, &missing)?;
            if body
                .get("errors")
                .and_then(Value::as_array)
                .is_some_and(|errors| !errors.is_empty())
            {
                return Ok(body); // Never cache errors or partial error responses.
            }
            let Some(nodes) = body.pointer("/data/turns").and_then(Value::as_array) else {
                return Ok(body);
            };
            rate = body
                .pointer("/data/rateLimit")
                .cloned()
                .unwrap_or(Value::Null);
            for node in nodes {
                if let Some(id) = node
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| missing.iter().any(|requested| requested == id))
                {
                    if node
                        .pointer("/reviewThreads/nodes")
                        .and_then(Value::as_array)
                        .is_some()
                    {
                        fresh.insert(id.to_owned(), node.clone());
                        cache.entries.insert(
                            id.to_owned(),
                            Entry {
                                fetched_at: now,
                                node: node.clone(),
                            },
                        );
                    }
                }
            }
        }
        let nodes: Vec<Value> = ids
            .iter()
            .map(|id| {
                fresh
                    .get(id)
                    .cloned()
                    .or_else(|| cache.entries.get(id).map(|entry| entry.node.clone()))
                    .unwrap_or(Value::Null)
            })
            .collect();
        if !missing.is_empty() {
            while cache.entries.len() > MAX_ENTRIES {
                let oldest = cache
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.fetched_at)
                    .map(|(id, _)| id.clone())
                    .unwrap();
                cache.entries.remove(&oldest);
            }
            if let Ok(bytes) = serde_json::to_vec(&cache) {
                if bytes.len() as u64 <= MAX_BYTES {
                    // Cache failure must not turn a successful GitHub request into an error.
                    let _ = crate::attention_state::atomic_write(path, &bytes);
                }
            }
        }
        // Cached responses never replay an old rate-limit balance or cost.
        Ok(json!({"data": {"turns": nodes, "rateLimit": rate}}))
    }
}

/// The tickets file: each PR's compacted answer ([`prmarmot_core::linear::compact_node`]).
#[derive(Serialize, Deserialize)]
struct TicketEntry {
    fetched_at: u64,
    /// Whether Linear's comment was there; a miss expires, a find does not.
    found: bool,
    node: Value,
}
#[derive(Serialize, Deserialize)]
struct TicketCache {
    version: u8,
    namespace: SnapshotNamespace,
    entries: BTreeMap<String, TicketEntry>,
}

impl ThreadCacheTransport {
    fn tickets_path(&self) -> std::path::PathBuf {
        let name = crate::attention_state::namespaced_path("tickets", &self.namespace);
        crate::config::cache_root().join(name.file_name().unwrap())
    }

    fn load_tickets(&self, path: &Path) -> TicketCache {
        std::fs::metadata(path)
            .ok()
            .filter(|meta| meta.len() <= MAX_BYTES)
            .and_then(|_| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<TicketCache>(&bytes).ok())
            .filter(|cache| cache.version == 1 && cache.namespace == self.namespace)
            .unwrap_or_else(|| TicketCache {
                version: 1,
                namespace: self.namespace.clone(),
                entries: BTreeMap::new(),
            })
    }

    fn save_tickets(path: &Path, cache: &TicketCache) {
        if let Ok(bytes) = serde_json::to_vec(cache) {
            if bytes.len() as u64 <= MAX_BYTES {
                // Cache failure must not turn a successful GitHub request into an error.
                let _ = crate::attention_state::atomic_write(path, &bytes);
            }
        }
    }

    /// Forget these PRs' Linear tickets, so the next refresh reads Linear's
    /// comment on them again (the row menu's "Refresh Linear ticket").
    pub fn forget_tickets(&self, ids: &[String]) {
        self.forget_tickets_at(&self.tickets_path(), ids);
    }

    fn forget_tickets_at(&self, path: &Path, ids: &[String]) {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut cache = self.load_tickets(path);
        let before = cache.entries.len();
        cache.entries.retain(|id, _| !ids.contains(id));
        if cache.entries.len() != before {
            Self::save_tickets(path, &cache);
        }
    }

    /// Answer [`TICKETS_QUERY`] for `ids` from the file, asking GitHub only
    /// about the first [`MAX_TICKET_IDS`] it has no answer for; the others
    /// come back `null` this time and are asked about on a later refresh.
    fn cached_tickets(
        &self,
        variables: &[(&str, &str)],
        ids: &[String],
        path: &Path,
        now: u64,
    ) -> Result<Value, GhError> {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut cache = self.load_tickets(path);
        cache.entries.retain(|_, entry| {
            entry.found
                || now
                    .checked_sub(entry.fetched_at)
                    .is_some_and(|age| age < TICKET_MISS_TTL)
        });
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !cache.entries.contains_key(*id))
            .take(MAX_TICKET_IDS)
            .cloned()
            .collect();
        let mut rate = Value::Null;
        if !missing.is_empty() {
            let body = self
                .inner
                .graphql_with_ids(TICKETS_QUERY, variables, &missing)?;
            if body
                .get("errors")
                .and_then(Value::as_array)
                .is_some_and(|errors| !errors.is_empty())
            {
                return Ok(body); // Never cache errors or partial error responses.
            }
            let Some(nodes) = body.pointer("/data/tickets").and_then(Value::as_array) else {
                return Ok(body);
            };
            rate = body
                .pointer("/data/rateLimit")
                .cloned()
                .unwrap_or(Value::Null);
            for node in nodes {
                let Some(id) = node
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| missing.iter().any(|requested| requested == id))
                else {
                    continue;
                };
                if node
                    .pointer("/comments/nodes")
                    .and_then(Value::as_array)
                    .is_none()
                {
                    continue;
                }
                cache.entries.insert(
                    id.to_owned(),
                    TicketEntry {
                        fetched_at: now,
                        found: prmarmot_core::linear::linkback(node).is_some(),
                        node: prmarmot_core::linear::compact_node(node),
                    },
                );
            }
            while cache.entries.len() > MAX_TICKET_ENTRIES {
                let oldest = cache
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.fetched_at)
                    .map(|(id, _)| id.clone())
                    .unwrap();
                cache.entries.remove(&oldest);
            }
            Self::save_tickets(path, &cache);
        }
        let nodes: Vec<Value> = ids
            .iter()
            .map(|id| {
                cache
                    .entries
                    .get(id)
                    .map_or(Value::Null, |entry| entry.node.clone())
            })
            .collect();
        // Cached responses never replay an old rate-limit balance or cost.
        Ok(json!({"data": {"tickets": nodes, "rateLimit": rate}}))
    }
}

impl GithubTransport for ThreadCacheTransport {
    fn graphql(&self, query: &str, variables: &[(&str, &str)]) -> Result<Value, GhError> {
        self.inner.graphql(query, variables)
    }
    fn graphql_with_ids(
        &self,
        query: &str,
        variables: &[(&str, &str)],
        ids: &[String],
    ) -> Result<Value, GhError> {
        if query == TICKETS_QUERY {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            return self.cached_tickets(variables, ids, &self.tickets_path(), now);
        }
        if query != TURNS_QUERY {
            return self.inner.graphql_with_ids(query, variables, ids);
        }
        let file = crate::config::load();
        let ttl = file.thread_refresh_secs.unwrap_or(600).max(30);
        let name = crate::attention_state::namespaced_path("threads", &self.namespace);
        let path = crate::config::cache_root().join(name.file_name().unwrap());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.cached(variables, ids, &path, ttl, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake(Mutex<Vec<Vec<String>>>);
    impl GithubTransport for Fake {
        fn graphql(&self, _: &str, _: &[(&str, &str)]) -> Result<Value, GhError> {
            panic!("unexpected query")
        }
        fn graphql_with_ids(
            &self,
            _: &str,
            _: &[(&str, &str)],
            ids: &[String],
        ) -> Result<Value, GhError> {
            self.0.lock().unwrap().push(ids.to_vec());
            Ok(
                json!({"data":{"turns": ids.iter().map(|id| json!({"id":id,"reviewThreads":{"nodes":[]}})).collect::<Vec<_>>(), "rateLimit":{"remaining":123,"cost":2}}}),
            )
        }
    }
    #[test]
    fn hits_partial_misses_expiry_and_account_isolation() {
        let path = std::env::temp_dir().join(format!(
            "prmarmot-thread-cache-test-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let fake = Arc::new(Fake(Mutex::new(vec![])));
        let cache =
            ThreadCacheTransport::new(fake.clone(), SnapshotNamespace::new("github.com", "one"));
        cache.cached(&[], &["A".into()], &path, 60, 100).unwrap();
        let hit = cache.cached(&[], &["A".into()], &path, 60, 110).unwrap();
        assert!(hit["data"]["rateLimit"].is_null());
        cache
            .cached(&[], &["A".into(), "B".into()], &path, 60, 120)
            .unwrap();
        cache.cached(&[], &["A".into()], &path, 60, 160).unwrap();
        let other =
            ThreadCacheTransport::new(fake.clone(), SnapshotNamespace::new("github.com", "two"));
        other.cached(&[], &["A".into()], &path, 60, 161).unwrap();
        assert_eq!(
            *fake.0.lock().unwrap(),
            vec![vec!["A"], vec!["B"], vec!["A"], vec!["A"]]
        );
        std::fs::remove_file(path).unwrap();
    }
    /// Answers the tickets query: PRs whose id starts with `L` carry Linear's
    /// comment, the others none.
    struct Tickets(Mutex<Vec<Vec<String>>>);
    impl GithubTransport for Tickets {
        fn graphql(&self, _: &str, _: &[(&str, &str)]) -> Result<Value, GhError> {
            unreachable!()
        }
        fn graphql_with_ids(
            &self,
            query: &str,
            _: &[(&str, &str)],
            ids: &[String],
        ) -> Result<Value, GhError> {
            assert_eq!(query, TICKETS_QUERY);
            self.0.lock().unwrap().push(ids.to_vec());
            let node = |id: &String| {
                let comments = if id.starts_with('L') {
                    json!([{"author": {"__typename": "Bot"}, "body": format!(
                        "<!-- linear-linkback -->\n<details>\n<summary><a href=\"https://linear.app/acme/issue/ENG-1/x\">ENG-1 Fix {id}</a></summary>\n{}</details>",
                        "a long description ".repeat(100)
                    )}])
                } else {
                    json!([{"author": {"__typename": "Bot"}, "body": "<!-- coverage -->"}])
                };
                json!({"id": id, "comments": {"nodes": comments}})
            };
            Ok(json!({"data": {
                "tickets": ids.iter().map(node).collect::<Vec<_>>(),
                "rateLimit": {"remaining": 4000, "cost": 1}
            }}))
        }
    }

    #[test]
    fn a_ticket_found_is_kept_until_forgotten_and_a_miss_is_asked_again() {
        let path = std::env::temp_dir().join(format!(
            "prmarmot-ticket-cache-test-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let fake = Arc::new(Tickets(Mutex::new(vec![])));
        let cache =
            ThreadCacheTransport::new(fake.clone(), SnapshotNamespace::new("github.com", "one"));
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let first = cache
            .cached_tickets(&[], &ids(&["L1", "N1"]), &path, 100)
            .unwrap();
        let ticket = prmarmot_core::linear::linkback(&first["data"]["tickets"][0]).unwrap();
        assert_eq!(ticket.id, "ENG-1");
        assert_eq!(ticket.title.as_deref(), Some("Fix L1"));
        // The file keeps the summary line, not the long description.
        assert!(std::fs::metadata(&path).unwrap().len() < 1024);
        // Within the hour nothing is asked; the miss is asked again after it.
        let hit = cache
            .cached_tickets(&[], &ids(&["L1", "N1"]), &path, 200)
            .unwrap();
        assert!(hit["data"]["rateLimit"].is_null());
        cache
            .cached_tickets(&[], &ids(&["L1", "N1"]), &path, 100 + TICKET_MISS_TTL)
            .unwrap();
        // A day later the ticket found still stands; forgetting it asks again.
        cache
            .cached_tickets(&[], &ids(&["L1"]), &path, 100 + 86_400)
            .unwrap();
        cache.forget_tickets_at(&path, &ids(&["L1"]));
        cache
            .cached_tickets(&[], &ids(&["L1"]), &path, 100 + 86_401)
            .unwrap();
        assert_eq!(
            *fake.0.lock().unwrap(),
            vec![ids(&["L1", "N1"]), ids(&["N1"]), ids(&["L1"])]
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn at_most_thirty_unknown_tickets_are_asked_about_at_once() {
        let path = std::env::temp_dir().join(format!(
            "prmarmot-ticket-cache-cap-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let fake = Arc::new(Tickets(Mutex::new(vec![])));
        let cache =
            ThreadCacheTransport::new(fake.clone(), SnapshotNamespace::new("github.com", "one"));
        let ids: Vec<String> = (0..45).map(|n| format!("L{n}")).collect();
        let body = cache.cached_tickets(&[], &ids, &path, 100).unwrap();
        let answered = body["data"]["tickets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| !node.is_null())
            .count();
        assert_eq!(answered, MAX_TICKET_IDS);
        cache.cached_tickets(&[], &ids, &path, 110).unwrap();
        let asked = fake.0.lock().unwrap();
        assert_eq!(asked[0].len(), MAX_TICKET_IDS);
        assert_eq!(asked[1], ids[MAX_TICKET_IDS..]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failures_are_not_cached_and_corrupt_files_are_ignored() {
        struct Failing(Mutex<usize>);
        impl GithubTransport for Failing {
            fn graphql(&self, _: &str, _: &[(&str, &str)]) -> Result<Value, GhError> {
                unreachable!()
            }
            fn graphql_with_ids(
                &self,
                _: &str,
                _: &[(&str, &str)],
                _: &[String],
            ) -> Result<Value, GhError> {
                *self.0.lock().unwrap() += 1;
                Ok(json!({"errors":[{"message":"timeout"}], "data":{"turns":[]}}))
            }
        }
        let path = std::env::temp_dir().join(format!(
            "prmarmot-thread-cache-errors-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, b"invalid json").unwrap();
        let fake = Arc::new(Failing(Mutex::new(0)));
        let cache =
            ThreadCacheTransport::new(fake.clone(), SnapshotNamespace::new("github.com", "one"));
        for now in [100, 110] {
            let body = cache.cached(&[], &["A".into()], &path, 60, now).unwrap();
            assert!(body.get("errors").is_some());
        }
        assert_eq!(*fake.0.lock().unwrap(), 2);
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid json");
        std::fs::remove_file(path).unwrap();
    }
}
