//! The real `prmarmot-cli` binary, run against the offline fake `gh` in
//! `scripts/demo/`, prints JSON that matches the published schemas in
//! `cli/schema/`. Event types a short run can't produce are checked
//! in-process by the unit tests in `src/watch.rs`.

#[path = "support/schema.rs"]
mod schema_check;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use schema_check::{assert_conforms, Schema};
use serde_json::Value;

const CONFIG: &str = r#"
default_reviewers = ["alex", "sam"]

[issue_link]
pattern = "DEMO-[0-9]+"
url_template = "https://example.com/issues/{id}"
"#;

/// A scratch home with the demo config, removed when dropped.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "prmarmot-cli-schema-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let config = root.join("config").join("prmarmot");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("config.toml"), CONFIG).unwrap();
        Self(root)
    }

    /// Runs the CLI with only the fake `gh` reachable; returns the exit code
    /// and stdout.
    fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        let fake_gh = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/demo");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path =
            std::env::join_paths(std::iter::once(fake_gh).chain(std::env::split_paths(&path)))
                .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_prmarmot-cli"));
        command
            .args(args)
            .current_dir(&self.0)
            .env("PATH", path)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_STATE_HOME", self.0.join("state"));
        for name in [
            "PRMARMOT_REPO",
            "PRMARMOT_REFRESH_SECS",
            "PRMARMOT_THEME",
            "PRMARMOT_ISSUE_PATTERN",
            "PRMARMOT_ISSUE_URL_TEMPLATE",
            "PRMARMOT_DEFAULT_REVIEWERS",
        ] {
            command.env_remove(name);
        }
        let output = command.output().expect("prmarmot-cli runs");
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        )
    }

    fn board(&self, args: &[&str]) -> Value {
        let (code, stdout) = self.run(args);
        assert_eq!(code, Some(0), "prmarmot-cli {args:?} failed:\n{stdout}");
        let board: Value = serde_json::from_str(&stdout).expect("--json prints one document");
        assert_conforms(Schema::Board, &board);
        board
    }

    fn events(&self, args: &[&str], exit: i32) -> Vec<Value> {
        let (code, stdout) = self.run(args);
        assert_eq!(code, Some(exit), "prmarmot-cli {args:?}:\n{stdout}");
        stdout
            .lines()
            .map(|line| {
                let event: Value = serde_json::from_str(line).expect("each line is JSON");
                assert_conforms(Schema::Event, &event);
                event
            })
            .collect()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn section_keys(board: &Value) -> Vec<&str> {
    board["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| section["key"].as_str().unwrap())
        .collect()
}

fn types(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect()
}

#[test]
fn every_board_view_matches_the_board_schema() {
    let sandbox = Sandbox::new();
    let views: [&[&str]; 6] = [
        &["mine", "--repo", "demo-labs/atlas", "--json"],
        &["mine", "--all-repos", "--json"],
        &["mine", "--all-repos", "--authored", "--json"],
        &["mine", "--all-repos", "--watched", "--changed", "--json"],
        &["review", "--all-repos", "--json"],
        &["review", "--repo", "demo-labs/atlas", "--json"],
    ];
    // Concurrently: each fake `gh` call starts a Python interpreter.
    let boards: Vec<Value> = std::thread::scope(|scope| {
        let runs: Vec<_> = views
            .iter()
            .map(|args| scope.spawn(|| sandbox.board(args)))
            .collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    });
    let [mine, involving, _, _, review, _] = &boards[..] else {
        unreachable!()
    };

    // The fixtures reach the parts of a PR the schema describes.
    assert!(section_keys(mine).contains(&"action"), "{mine}");
    let prs: Vec<&Value> = mine["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|section| section["prs"].as_array().unwrap())
        .collect();
    assert!(prs.iter().any(|pr| pr["stack"].is_object()));
    assert!(prs.iter().any(|pr| pr["issue"].is_object()));
    assert!(prs.iter().any(|pr| pr["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|blocker| blocker["type"] == "no_reviewers")));
    assert_eq!(involving["view"], "Involving me");
    assert!(section_keys(review).contains(&"todo"), "{review}");

    // The check is strict: a field the schema doesn't describe fails it.
    let mut surprise = review.clone();
    surprise["sections"][0]["prs"][0]["surprise"] = Value::Bool(true);
    let errors = schema_check::violations(Schema::Board, &surprise);
    assert!(
        errors.iter().any(|error| error.contains("surprise")),
        "{errors:?}"
    );
}

#[test]
fn a_wait_that_is_met_matches_the_event_schema() {
    let events = Sandbox::new().events(
        &[
            "watch",
            "--pr",
            "demo-labs/atlas#415",
            "--until",
            "approved",
            "--json",
        ],
        0,
    );
    assert_eq!(types(&events), ["ready", "until"]);
    assert_eq!(events[1]["outcome"], "met");
}

#[test]
fn a_wait_that_can_no_longer_be_met_matches_the_event_schema() {
    let events = Sandbox::new().events(
        &[
            "watch",
            "--pr",
            "demo-labs/atlas#418",
            "--until",
            "ci-pass",
            "--json",
        ],
        5,
    );
    assert_eq!(types(&events), ["ready", "until"]);
    assert_eq!(events[1]["reasons"][0]["reason"], "ci_failed");
}

#[test]
fn a_wait_that_times_out_matches_the_event_schema() {
    let events = Sandbox::new().events(
        &[
            "watch",
            "--pr",
            "demo-labs/atlas#410",
            "--until",
            "approved,mergeable",
            "--timeout",
            "1s",
            "--json",
        ],
        6,
    );
    assert_eq!(types(&events), ["ready", "until"]);
    assert_eq!(events[1]["outcome"], "timeout");
}

#[test]
fn the_published_schemas_are_valid_draft_2020_12() {
    for text in [schema_check::BOARD, schema_check::EVENT] {
        let schema: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        if let Err(error) = jsonschema::meta::validate(&schema) {
            panic!("{}: {error}", schema["$id"]);
        }
    }
}
