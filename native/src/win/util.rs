#![deny(clippy::all)]
// Copyright (c) 2018, Microsoft Corporation (MIT License).
// 
// This file provides general functionality
// reused for windows PseudoTerminals

use crate::err;
use std::{convert::TryFrom, collections::HashMap};
use winsafe::WString;

/// Converts a hashmap of key value pairs to a wide string of values.
/// Each key value pair is converted into the format
/// `key=value`, separated by a null character.
/// The string is also terminated with an additional null character
/// to denote the actual end of the string
pub fn map_to_wstring(map: HashMap<String, String>) -> WString {
    return WString::from_str_vec(&map.into_iter()
        .map(|entry| { format!("{}={}", entry.0, entry.1) })
        .collect::<Vec<String>>());
}

/// Converts a i32 value from javascript into a i16 value used
/// for the size of a terminal session
pub fn coerce_i16(inval: i32) -> napi::Result<i16> {
    return match i16::try_from(inval) {
        Ok(i) => Ok(i),
        Err(_) => err!(format!("Failed to coerce value {} to u16", inval))
    };
}


