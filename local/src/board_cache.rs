//! The board as it was when the app last ran — every queue's rows and when
//! each was synced — so the next launch shows those rows while the first
//! refresh runs, instead of an empty table. A file another version wrote is
//! read too — after an update the first launch would otherwise wait out
//! every view's first search on an empty table — as long as its
//! [`SCHEMA_VERSION`] matches and its rows still parse; a row field the older
//! version did not write takes its default (`BoardRow` is `serde(default)`).
//! Those rows show with their age until the refresh replaces them. A file
//! for another account or host is ignored.

use std::fs;
use std::path::{Path, PathBuf};

use prmarmot_core::attention::SnapshotNamespace;
use prmarmot_core::board::{BoardRow, BoardScope, Mode};
use serde::{Deserialize, Serialize};

/// The file's shape. Bump it when a saved row would read back wrongly rather
/// than fail to parse (a field that changed meaning); a new field needs only
/// a default.
pub const SCHEMA_VERSION: u32 = 1;
/// At most this many queues are kept: the app's cache holds the three views
/// of twelve pins and All repositories, and the open queue is one more.
pub const MAX_QUEUES: usize = 40;
/// The file's bound both ways: a board past it is not written, and a file
/// past it is not read.
pub const MAX_BYTES: usize = 32 * 1024 * 1024;

/// One queue's rows as last loaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedBoard {
    pub scope: BoardScope,
    pub mode: Mode,
    pub rows: Vec<BoardRow>,
    /// When the rows were synced, Unix seconds.
    pub synced_at: i64,
    pub truncated: bool,
    pub total: Option<u64>,
}

#[derive(Serialize)]
struct FileRef<'a> {
    schema_version: u32,
    app_version: &'a str,
    namespace: &'a SnapshotNamespace,
    boards: &'a [CachedBoard],
}

#[derive(Deserialize)]
struct File {
    namespace: SnapshotNamespace,
    boards: Vec<CachedBoard>,
}

#[derive(Deserialize)]
struct Header {
    schema_version: u32,
}

/// The queues the last run left for `namespace`, whichever version wrote
/// them, or none: a missing, unreadable, oversized, foreign or unparsable
/// file reads as an empty board, never as an error, because the refresh that
/// follows is the truth anyway.
pub fn load(namespace: &SnapshotNamespace) -> Vec<CachedBoard> {
    load_at(&path(namespace), namespace)
}

fn load_at(path: &Path, namespace: &SnapshotNamespace) -> Vec<CachedBoard> {
    let Ok(metadata) = fs::metadata(path) else {
        return Vec::new();
    };
    if metadata.len() > MAX_BYTES as u64 {
        return Vec::new();
    }
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    from_bytes(&bytes, namespace).unwrap_or_default()
}

/// What `to_bytes` wrote, when it was this account's in this shape.
pub fn from_bytes(bytes: &[u8], namespace: &SnapshotNamespace) -> Result<Vec<CachedBoard>, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("file is too large ({} bytes)", bytes.len()));
    }
    // The shape first, so another schema is named as that rather than as
    // whatever field it no longer matches.
    let header: Header = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if header.schema_version != SCHEMA_VERSION {
        return Err(format!("unsupported schema {}", header.schema_version));
    }
    let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.namespace != *namespace {
        return Err("account/host namespace mismatch".into());
    }
    let mut boards = file.boards;
    boards.truncate(MAX_QUEUES);
    Ok(boards)
}

pub fn to_bytes(
    namespace: &SnapshotNamespace,
    app_version: &str,
    boards: &[CachedBoard],
) -> Result<Vec<u8>, String> {
    let boards = &boards[..boards.len().min(MAX_QUEUES)];
    let bytes = serde_json::to_vec(&FileRef {
        schema_version: SCHEMA_VERSION,
        app_version,
        namespace,
        boards,
    })
    .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err(format!("board is too large ({} bytes)", bytes.len()));
    }
    Ok(bytes)
}

/// Write the board for the next launch. Best effort for the caller: a
/// failed write costs the next launch its first frame of rows, nothing else.
pub fn save(
    namespace: &SnapshotNamespace,
    app_version: &str,
    boards: &[CachedBoard],
) -> Result<(), String> {
    let path = path(namespace);
    crate::attention_state::atomic_write(&path, &to_bytes(namespace, app_version, boards)?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Beside the attention file, named the same way.
pub fn path(namespace: &SnapshotNamespace) -> PathBuf {
    crate::attention_state::namespaced_path("board", namespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespace() -> SnapshotNamespace {
        SnapshotNamespace::new("github.com", "octocat")
    }

    fn board(number: u64) -> CachedBoard {
        CachedBoard {
            scope: BoardScope::Repository("acme/widgets".into()),
            mode: Mode::Review,
            rows: vec![BoardRow {
                id: format!("PR_{number}"),
                number,
                title: "Fix | pipes in titles".into(),
                ..BoardRow::default()
            }],
            synced_at: 1_760_000_000,
            truncated: true,
            total: Some(61),
        }
    }

    #[test]
    fn the_board_round_trips_for_the_build_and_account_that_wrote_it() {
        let bytes = to_bytes(&namespace(), "0.18.2", &[board(7)]).unwrap();
        let boards = from_bytes(&bytes, &namespace()).unwrap();
        assert_eq!(boards.len(), 1);
        assert_eq!(
            boards[0].scope,
            BoardScope::Repository("acme/widgets".into())
        );
        assert_eq!(boards[0].mode, Mode::Review);
        assert_eq!(boards[0].rows[0].number, 7);
        assert_eq!(boards[0].rows[0].title, "Fix | pipes in titles");
        assert_eq!(boards[0].synced_at, 1_760_000_000);
        assert!(boards[0].truncated);
        assert_eq!(boards[0].total, Some(61));
    }

    #[test]
    fn another_account_host_or_schema_reads_as_an_empty_board() {
        let bytes = to_bytes(&namespace(), "0.18.2", &[board(7)]).unwrap();
        assert!(from_bytes(
            &bytes,
            &SnapshotNamespace::new("github.com", "someone-else"),
        )
        .unwrap_err()
        .contains("namespace"));
        assert!(from_bytes(b"not json", &namespace()).is_err());
        assert!(from_bytes(
            br#"{"schema_version":99,"app_version":"0.18.2"}"#,
            &namespace()
        )
        .unwrap_err()
        .contains("schema 99"));
    }

    #[test]
    fn an_older_versions_board_opens_and_missing_row_fields_take_defaults() {
        let bytes = to_bytes(&namespace(), "0.18.2", &[board(7)]).unwrap();
        // An older build wrote fewer row fields: drop some, as it would have.
        let mut file: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let row = file["boards"][0]["rows"][0].as_object_mut().unwrap();
        for field in ["base_pr", "reviewed_at", "labels", "head_oid"] {
            row.remove(field);
        }
        let older = serde_json::to_vec(&file).unwrap();
        let boards = from_bytes(&older, &namespace()).unwrap();
        assert_eq!(boards[0].rows[0].number, 7);
        assert_eq!(boards[0].rows[0].title, "Fix | pipes in titles");
        assert_eq!(boards[0].rows[0].base_pr, None);
    }

    #[test]
    fn a_missing_or_oversized_file_loads_as_nothing_and_the_bound_holds() {
        let dir = std::env::temp_dir().join(format!(
            "prmarmot-board-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.json");
        assert!(load_at(&path, &namespace()).is_empty());
        let boards: Vec<CachedBoard> = (0..(MAX_QUEUES as u64 + 5)).map(board).collect();
        let bytes = to_bytes(&namespace(), "0.18.2", &boards).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(load_at(&path, &namespace()).len(), MAX_QUEUES);
        fs::write(&path, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(load_at(&path, &namespace()).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
