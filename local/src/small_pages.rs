//! Views GitHub could not answer at full pages, remembered between
//! `prmarmot-cli` runs. GitHub cuts a GraphQL request off at about ten
//! seconds; a view that hit that once is asked again at 30 rows, and the app
//! keeps asking small for as long as it runs. A one-shot CLI run would start
//! at 60 rows every time and pay the timeout before the retry, so it reads and
//! records the fallback here, for an hour.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How long a view stays on small pages after GitHub last gave up on it.
pub const REMEMBER_SECS: i64 = 60 * 60;
/// At most this many views are remembered; the ones remembered longest ago go
/// first.
const MAX_VIEWS: usize = 64;
/// A file larger than this is not ours to read; it is ignored and replaced.
const MAX_BYTES: u64 = 64 * 1024;

/// Which view of whose board: host, account, scope and view name.
pub fn key(host: &str, account: &str, scope: &str, view: &str) -> String {
    format!("{host}|{account}|{scope}|{view}")
}

/// Whether GitHub gave up on `key`'s full pages within the last hour.
pub fn remembered(key: &str, now: i64) -> bool {
    remembered_at(&path(), key, now)
}

/// Record that GitHub gave up on `key`'s full pages at `now`. Best effort: a
/// failed write only costs the next run its first, full-size request.
pub fn remember(key: &str, now: i64) {
    let _ = remember_at(&path(), key, now);
}

fn path() -> PathBuf {
    crate::config::state_root().join("small-pages.json")
}

fn read(path: &Path) -> BTreeMap<String, i64> {
    let too_large = std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_BYTES);
    if too_large {
        return BTreeMap::new();
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn remembered_at(path: &Path, key: &str, now: i64) -> bool {
    read(path)
        .get(key)
        .is_some_and(|since| now - since < REMEMBER_SECS)
}

fn remember_at(path: &Path, key: &str, now: i64) -> std::io::Result<()> {
    let mut views = read(path);
    views.retain(|_, since| now - *since < REMEMBER_SECS);
    views.insert(key.to_owned(), now);
    while views.len() > MAX_VIEWS {
        let oldest = views
            .iter()
            .min_by_key(|(_, since)| **since)
            .map(|(key, _)| key.clone());
        match oldest {
            Some(oldest) => views.remove(&oldest),
            None => break,
        };
    }
    let bytes = serde_json::to_vec(&views).map_err(std::io::Error::other)?;
    crate::attention_state::atomic_write(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "prmarmot-small-pages-{name}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn a_view_is_remembered_for_an_hour_and_only_that_view() {
        let path = temp("hour");
        let _ = std::fs::remove_file(&path);
        let mine = key("github.com", "octocat", "all", "authored");
        let review = key("github.com", "octocat", "all", "review");
        assert!(!remembered_at(&path, &mine, 1_000));
        remember_at(&path, &mine, 1_000).unwrap();
        assert!(remembered_at(&path, &mine, 1_000 + REMEMBER_SECS - 1));
        assert!(!remembered_at(&path, &mine, 1_000 + REMEMBER_SECS));
        assert!(!remembered_at(&path, &review, 1_000));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_file_stays_bounded_and_a_broken_one_is_replaced() {
        let path = temp("bounded");
        std::fs::write(&path, "not json").unwrap();
        for n in 0..(MAX_VIEWS as i64 + 10) {
            remember_at(&path, &key("h", "a", &n.to_string(), "v"), 5_000 + n).unwrap();
        }
        let views = read(&path);
        assert_eq!(views.len(), MAX_VIEWS);
        assert!(
            !views.contains_key(&key("h", "a", "0", "v")),
            "oldest goes first"
        );
        assert!(views.contains_key(&key("h", "a", &(MAX_VIEWS as i64 + 9).to_string(), "v")));
        std::fs::remove_file(path).unwrap();
    }
}
