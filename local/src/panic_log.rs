//! A record of each panic, without its message.
//!
//! An app launched from the Dock has no terminal, so a panic's report goes
//! nowhere. This hook appends one line per panic — when, which build, which
//! thread, where in the source — to a small file under the state directory,
//! then hands over to the previous hook (the terminal report, when there is
//! one). The panic's message is never written: it can carry a PR title, a
//! repository name or an error body, and a bug report should not need the
//! file. The file is kept under [`MAX_BYTES`] by dropping its oldest lines.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The record stays under this many bytes: 64 KiB, about 500 panics.
pub const MAX_BYTES: u64 = 64 * 1024;
const FILE_NAME: &str = "panics.log";

/// `$XDG_STATE_HOME/prmarmot/panics.log`.
pub fn path() -> PathBuf {
    crate::config::state_root().join(FILE_NAME)
}

/// Writes every panic to [`path`] and then reports it as before. Call once,
/// first thing in `main`.
pub fn install(version: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let line = line(
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            version,
            thread.name().unwrap_or("unnamed"),
            info.location()
                .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
                .as_deref(),
        );
        // A panic hook must not panic; a record that can't be written is lost.
        let _ = record(&path(), &line);
        previous(info);
    }));
}

/// One record: ISO time, build, thread and source location, tab-separated.
pub fn line(time: &str, version: &str, thread: &str, location: Option<&str>) -> String {
    format!(
        "{time}\tv{version}\t{thread}\t{}",
        location.unwrap_or("unknown location")
    )
}

/// Appends `line` to the record at `path`, creating the directory and the file
/// as needed, and drops the oldest lines when the file would pass [`MAX_BYTES`].
pub fn record(path: &Path, line: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut contents = existing;
    contents.extend_from_slice(line.as_bytes());
    contents.push(b'\n');
    let contents = tail_within(&contents, MAX_BYTES);
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// The last whole lines of `bytes` that fit in `max` bytes.
fn tail_within(bytes: &[u8], max: u64) -> &[u8] {
    let max = usize::try_from(max).unwrap_or(usize::MAX);
    if bytes.len() <= max {
        return bytes;
    }
    let cut = bytes.len() - max;
    // Start at the cut when it is a line start, else after the next newline,
    // so no line is torn.
    let start = if cut == 0 || bytes[cut - 1] == b'\n' {
        cut
    } else {
        bytes[cut..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |nl| cut + nl + 1)
    };
    &bytes[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_names_when_which_build_where_and_never_the_message() {
        let line = line(
            "2026-10-02T08:00:00Z",
            "0.15.0",
            "main",
            Some("src/app.rs:10:5"),
        );
        assert_eq!(line, "2026-10-02T08:00:00Z\tv0.15.0\tmain\tsrc/app.rs:10:5");
        assert_eq!(
            super::line("t", "1.0.0", "worker", None),
            "t\tv1.0.0\tworker\tunknown location"
        );
    }

    #[test]
    fn records_append_and_the_oldest_lines_go_once_the_file_is_full() {
        let dir = std::env::temp_dir().join(format!("prmarmot-panic-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("panics.log");
        record(&path, "first").unwrap();
        record(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");

        let long = "x".repeat(1000);
        for _ in 0..80 {
            record(&path, &long).unwrap();
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.len() as u64 <= MAX_BYTES);
        assert!(!contents.contains("first"), "the oldest line is gone");
        assert!(contents.ends_with(&format!("{long}\n")));
        assert!(
            contents.lines().all(|l| l.len() == 1000),
            "no torn line at the start"
        );
        assert!(!dir.join("nested").join("panics.tmp-0").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tail_keeps_whole_lines_only() {
        assert_eq!(tail_within(b"ab\ncd\nef\n", 5), b"ef\n");
        assert_eq!(tail_within(b"ab\ncd\nef\n", 6), b"cd\nef\n");
        assert_eq!(tail_within(b"ab\ncd\nef\n", 100), b"ab\ncd\nef\n");
        assert_eq!(tail_within(b"abcdefgh", 3), b"");
    }
}
