//! Embeds which commit an XCFramework was built from, for `core_build()`.
//!
//! Two XCFrameworks both saying "0.1.0" cannot be told apart, and the iPad
//! links whichever one was built last. The short commit, whether the Rust the
//! iPad reuses had uncommitted changes, and the build profile settle it. No
//! dependency: `git` is asked directly, and outside a checkout (a source
//! tarball, a vendored copy) every value reads "unknown" rather than failing
//! the build.

use std::path::Path;
use std::process::Command;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };

    let commit = git(&["rev-parse", "--short=12", "HEAD"]).filter(|sha| !sha.is_empty());
    // Only the crates that end up in the XCFramework count: an edited README
    // or desktop file does not make the iPad's core a different one.
    let dirty = commit.as_ref().and_then(|_| {
        git(&[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ":/core",
            ":/local",
            ":/ffi",
            ":/Cargo.lock",
        ])
        .map(|changes| !changes.is_empty())
    });

    println!(
        "cargo:rustc-env=PRMARMOT_FFI_COMMIT={}",
        commit.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo:rustc-env=PRMARMOT_FFI_DIRTY={}",
        match dirty {
            Some(true) => "dirty",
            Some(false) => "clean",
            None => "unknown",
        }
    );
    println!(
        "cargo:rustc-env=PRMARMOT_FFI_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );

    // Rerun when the commit moves or the sources change, and only then.
    // Every path is checked first: a watched path that does not exist makes
    // Cargo rerun this script on every build.
    let mut watch = vec![
        "build.rs".to_owned(),
        "src".to_owned(),
        "Cargo.toml".to_owned(),
        "../core/src".to_owned(),
        "../core/Cargo.toml".to_owned(),
        "../local/src".to_owned(),
        "../local/Cargo.toml".to_owned(),
        "../Cargo.lock".to_owned(),
    ];
    if commit.is_some() {
        let mut git_paths = vec![
            "HEAD".to_owned(),
            "index".to_owned(),
            "packed-refs".to_owned(),
        ];
        git_paths.extend(git(&["symbolic-ref", "-q", "HEAD"]));
        for path in git_paths {
            watch.extend(git(&[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &path,
            ]));
        }
    }
    for path in watch {
        let full = Path::new(&dir).join(&path);
        if full.exists() {
            println!("cargo:rerun-if-changed={}", full.display());
        }
    }
}
