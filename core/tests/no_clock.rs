//! `prmarmot-core` reads no clock.
//!
//! `core/clippy.toml` disallows the four constructors, but clippy only runs
//! where someone remembers to run it, and the config file is one `rm` away
//! from silently doing nothing. This test states the same rule in a way that
//! `cargo test` enforces, and fails loudly if the lint config is deleted.

use std::fs;
use std::path::{Path, PathBuf};

/// Written in pieces so this file does not trip its own search.
fn banned() -> Vec<String> {
    ["Utc", "Local", "SystemTime", "Instant"]
        .iter()
        .map(|kind| format!("{kind}::{}()", "now"))
        .collect()
}

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn no_source_file_in_core_reads_the_clock() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(
        files.len() > 5,
        "expected to scan core's sources, found {files:?}"
    );

    let banned = banned();
    let mut offenders = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        for (line_number, line) in text.lines().enumerate() {
            // Tests inside a source file pass their own fixed time too, so the
            // rule applies to the whole file, comments excepted.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for call in &banned {
                if line.contains(call.as_str()) {
                    offenders.push(format!("{}:{}", file.display(), line_number + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "core must take the time as a parameter; these read the clock instead: {offenders:?}"
    );
}

#[test]
fn the_clock_lint_is_still_configured() {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("clippy.toml");
    let text = fs::read_to_string(&config)
        .unwrap_or_else(|e| panic!("core/clippy.toml is missing ({e}); it disallows clock reads"));
    assert!(text.contains("disallowed-methods"));
    for path in [
        "chrono::Utc::now",
        "chrono::Local::now",
        "std::time::SystemTime::now",
        "std::time::Instant::now",
    ] {
        assert!(
            text.contains(path),
            "core/clippy.toml no longer disallows {path}"
        );
    }
}
