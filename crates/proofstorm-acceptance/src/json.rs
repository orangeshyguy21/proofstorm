//! Fail-loud JSON accessors.
//!
//! Every accessor takes an RFC 6901 pointer and reports the pointer plus the
//! enclosing document when the field is absent or the wrong type. Ports use
//! these instead of `value["key"]` so a renamed or dropped field aborts the
//! gate, matching Python's `KeyError`.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

fn at<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value> {
    value
        .pointer(pointer)
        .ok_or_else(|| anyhow!("missing {pointer} in {value}"))
}

pub fn string<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    let found = at(value, pointer)?;
    found
        .as_str()
        .ok_or_else(|| anyhow!("{pointer} is not a string: {found}"))
}

pub fn integer(value: &Value, pointer: &str) -> Result<u64> {
    let found = at(value, pointer)?;
    found
        .as_u64()
        .ok_or_else(|| anyhow!("{pointer} is not a non-negative integer: {found}"))
}

pub fn boolean(value: &Value, pointer: &str) -> Result<bool> {
    let found = at(value, pointer)?;
    found
        .as_bool()
        .ok_or_else(|| anyhow!("{pointer} is not a boolean: {found}"))
}

pub fn array<'a>(value: &'a Value, pointer: &str) -> Result<&'a Vec<Value>> {
    let found = at(value, pointer)?;
    found
        .as_array()
        .ok_or_else(|| anyhow!("{pointer} is not an array: {found}"))
}

pub fn object<'a>(value: &'a Value, pointer: &str) -> Result<&'a serde_json::Map<String, Value>> {
    let found = at(value, pointer)?;
    found
        .as_object()
        .ok_or_else(|| anyhow!("{pointer} is not an object: {found}"))
}

/// Assert a pointer holds exactly `expected`.
pub fn equals(value: &Value, pointer: &str, expected: &Value) -> Result<()> {
    let found = at(value, pointer)?;
    if found != expected {
        bail!("{pointer} is {found}, expected {expected}");
    }
    Ok(())
}

/// Assert a serialized value stays within an agent-response budget.
pub fn within_bytes(value: &Value, limit: usize, label: &str) -> Result<()> {
    let encoded = serde_json::to_vec(value)?;
    if encoded.len() > limit {
        bail!("{label} is {} bytes, over the {limit} limit", encoded.len());
    }
    Ok(())
}
