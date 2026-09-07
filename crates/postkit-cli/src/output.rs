use postkit::{Error, WireError};
use serde::Serialize;

pub fn emit_ok<T: Serialize>(value: &T, json: bool, human: impl FnOnce() -> String) {
    if json {
        println!("{}", serde_json::to_string(value).expect("json"));
    } else {
        println!("{}", human());
    }
}

pub fn emit_raw(value: &serde_json::Value) {
    println!("{}", serde_json::to_string(value).expect("json"));
}

pub fn emit_err(e: &Error, json: bool) {
    if json {
        let w = WireError::from(e);
        println!("{}", serde_json::to_string(&w).expect("json"));
    } else {
        eprintln!("{e}");
    }
}
