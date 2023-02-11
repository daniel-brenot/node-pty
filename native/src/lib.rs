#![deny(clippy::all)]
/// Copyright (c) 2022, Daniel Brenot (MIT License)
/// 
/// Entrypoint for the library for exposing pseudo-terminal
/// functionality for multiple platforms



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
    return env!("CARGO_PKG_VERSION").to_string();
}
