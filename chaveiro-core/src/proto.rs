//! Serde types for the JSON the server sends and accepts.
//!
//! The server sends object keys in any case (Vaultwarden mixes camelCase and
//! PascalCase), so every type here reads keys lowercased: parse with
//! [`from_slice`] or [`from_value`], never with `serde_json` directly.

pub mod cache;
pub mod cipher;
pub mod identity;
pub mod sync;

use crate::crypto::EncString;
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::{Map, Value};

/// Parses `bytes` as JSON and deserializes `T` with every object key
/// lowercased. Unknown keys are ignored.
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    from_value(serde_json::from_slice(bytes)?)
}

/// Deserializes `T` from `value` with every object key lowercased, at every
/// depth.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(lowercase_keys(value))
}

/// `value` with every object key lowercased, at every depth. When two keys
/// differ only in case, the later one wins.
pub fn lowercase_keys(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key.to_lowercase(), lowercase_keys(value)))
                .collect::<Map<String, Value>>(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(lowercase_keys).collect()),
        other => other,
    }
}

/// Reads JSON `null` as `T::default()`, because Vaultwarden sends `null` for
/// empty lists. Pair it with `#[serde(default)]` for a missing key.
fn null_default<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Reads a required [`EncString`] from its wire form. A malformed string is
/// kept as [`EncString::Unsupported`], see [`lenient`].
fn enc_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<EncString, D::Error> {
    Ok(lenient(String::deserialize(deserializer)?))
}

/// Reads an optional [`EncString`]. `null` and the empty string are [`None`],
/// and a malformed string is kept as [`EncString::Unsupported`].
fn enc_option<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<EncString>, D::Error> {
    match Option::<String>::deserialize(deserializer)? {
        Some(text) if !text.is_empty() => Ok(Some(lenient(text))),
        _ => Ok(None),
    }
}

/// Parses `text`, or keeps it as [`EncString::Unsupported`] when it does not
/// parse, so one bad field makes only its item unreadable and a write sends
/// it back unchanged. The `kind` is the header number, or [`u8::MAX`] when
/// the header is not a number.
fn lenient(text: String) -> EncString {
    match text.parse() {
        Ok(parsed) => parsed,
        Err(_) => EncString::Unsupported {
            kind: text
                .split_once('.')
                .and_then(|(header, _)| header.parse().ok())
                .unwrap_or(u8::MAX),
            raw: text,
        },
    }
}
