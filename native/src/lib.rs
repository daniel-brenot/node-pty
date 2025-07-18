#![deny(clippy::all)]
// Copyright (c) 2018, Microsoft Corporation (MIT License).
// 
// Entrypoint for the library for exposing pseudo-terminal
// functionality for multiple platforms



mod unix;
mod win;

pub use unix::*;
pub use win::*;

// Use the mimalloc allocator to have a smaller footprint
// and faster memory allocation
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

//#[macro_use]
//extern crate napi;
#[macro_use]
extern crate napi_derive;

/// Custom macro for simpler errors.
/// Returns an error enum with generic failure and a message provided by the string literal
#[macro_export]
macro_rules! err {
    ( $( $msg:expr ),* ) => {
        {
            $(Err(napi::Error::new(napi::Status::GenericFailure, $msg.to_string())))*
        }
    };
}

/// Bakes the version number into the binary so that it can be detected
/// if the binary version is not up to date with the library version
#[allow(dead_code)]
#[napi]
pub fn version() -> String {
    use serde_json::{Map, Value};
    let package_file = include_str!("../../package.json");
    let package_contents: Map<String, Value> = serde_json::from_str(package_file).expect("Failed to read main package.json file");
    let version = package_contents.get("version").expect("Couldn't find version in main package.json file");
    return String::from(version.as_str().unwrap());
}
