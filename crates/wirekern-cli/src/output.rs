use serde::Serialize;
use wirekern::{Error, WireError};

// Output-stream contract, one rule, no exceptions:
//   --json      → stdout carries exactly one JSON document; stderr is quiet.
//   no --json   → stdout stays EMPTY; every line, results included, is on
//                 stderr. Scripts that want data on stdout pass --json.
// Human lines therefore never share a stream with machine documents, and
// `wirekern … | other` without --json pipes nothing — by design.

/// Success output. `--json` writes the document to stdout; human mode
/// writes the rendered line to **stderr** (see the contract above).
pub fn emit_ok<T: Serialize>(value: &T, json: bool, human: impl FnOnce() -> String) {
    if json {
        println!("{}", serde_json::to_string(value).expect("json"));
    } else {
        eprintln!("{}", human());
    }
}

/// JSON document on stdout. JSON mode only — never call it for human output.
pub fn emit_raw(value: &serde_json::Value) {
    println!("{}", serde_json::to_string(value).expect("json"));
}

/// A human-mode informational line: stderr, always.
pub fn human_line(line: impl AsRef<str>) {
    eprintln!("{}", line.as_ref());
}

/// Failure output. `--json` writes the `WireError` document to stdout;
/// human mode writes the error to stderr.
pub fn emit_err(e: &Error, json: bool) {
    if json {
        let w = WireError::from(e);
        println!("{}", serde_json::to_string(&w).expect("json"));
    } else {
        eprintln!("{e}");
    }
}
