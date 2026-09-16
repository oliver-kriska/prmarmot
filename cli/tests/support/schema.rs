//! Checks CLI output against the published JSON Schemas in `cli/schema/`.
//! Shared by the unit tests (every event type, built in-process) and the
//! integration tests (the real binary against the offline fake `gh`).
//!
//! The published schemas leave objects open, because board@1 and event@1
//! only ever gain fields and consumers must skip what they don't know. Here
//! every object is closed, so a field the CLI prints but the schema doesn't
//! describe fails the tests instead of shipping undocumented.

use serde_json::Value;

pub const BOARD: &str = include_str!("../../schema/board-v1.schema.json");
pub const EVENT: &str = include_str!("../../schema/event-v1.schema.json");

#[derive(Debug, Clone, Copy)]
pub enum Schema {
    Board,
    Event,
}

/// Every error in `instance`, one per line; empty when it conforms.
pub fn violations(schema: Schema, instance: &Value) -> Vec<String> {
    let board = closed(BOARD);
    let root = match schema {
        Schema::Board => board.clone(),
        Schema::Event => closed(EVENT),
    };
    let board_id = board["$id"]
        .as_str()
        .expect("board schema has an $id")
        .to_owned();
    let registry = jsonschema::Registry::new()
        .add(board_id, board)
        .and_then(|registry| registry.prepare())
        .expect("board schema registers");
    let validator = jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&root)
        .expect("schema compiles");
    validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect()
}

/// Panics with every violation and the offending document.
pub fn assert_conforms(schema: Schema, instance: &Value) {
    let errors = violations(schema, instance);
    assert!(
        errors.is_empty(),
        "{schema:?} output doesn't match cli/schema:\n  {}\n{}",
        errors.join("\n  "),
        serde_json::to_string_pretty(instance).unwrap_or_default()
    );
}

fn closed(text: &str) -> Value {
    let mut schema = serde_json::from_str(text).expect("schema is JSON");
    close(&mut schema);
    schema
}

/// Adds `unevaluatedProperties: false` to every object schema that lists its
/// properties. Conditional parts (`if`/`then`) carry no `type` and stay open.
fn close(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            let is_object = match map.get("type") {
                Some(Value::String(kind)) => kind == "object",
                Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "object"),
                _ => false,
            };
            if is_object
                && map.contains_key("properties")
                && !map.contains_key("additionalProperties")
            {
                map.insert("unevaluatedProperties".into(), Value::Bool(false));
            }
            for (key, value) in map.iter_mut() {
                if key != "const" && key != "enum" {
                    close(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(close),
        _ => {}
    }
}
