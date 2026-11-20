//! The `GET /api/sync` response.

use super::{
    cipher::{CipherResponse, FolderResponse},
    enc_option, enc_string, from_value, null_default,
};
use crate::crypto::EncString;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

/// The parts of a sync this client reads.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct SyncResponse {
    pub profile: Profile,
    #[serde(default, deserialize_with = "null_default")]
    pub folders: Vec<FolderResponse>,
    #[serde(default, deserialize_with = "null_default")]
    pub collections: Vec<CollectionResponse>,
    /// Read by [`from_slice`](Self::from_slice) from the original objects,
    /// so each keeps its [`raw`](CipherResponse::raw) key case.
    #[serde(skip)]
    pub ciphers: Vec<CipherResponse>,
}

impl SyncResponse {
    /// Parses a sync body. Keys are read in any case.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        Self::from_json(serde_json::from_slice(bytes)?)
    }

    /// Parses a sync body that is already JSON, such as the `sync` of a
    /// [`CacheDocument`](super::cache::CacheDocument).
    pub fn from_json(mut body: Value) -> Result<Self, serde_json::Error> {
        let ciphers = match body.as_object_mut() {
            Some(object) => {
                let keys: Vec<String> = object
                    .keys()
                    .filter(|key| key.eq_ignore_ascii_case("ciphers"))
                    .cloned()
                    .collect();
                // The last spelling wins, as in `lowercase_keys`.
                keys.iter()
                    .filter_map(|key| object.remove(key))
                    .next_back()
                    .unwrap_or(Value::Null)
            }
            None => {
                return Err(serde::de::Error::custom(
                    "a sync body must be a JSON object",
                ));
            }
        };
        let mut sync: Self = from_value(body)?;
        sync.ciphers = match ciphers {
            Value::Null => Vec::new(),
            Value::Array(items) => items
                .into_iter()
                .map(|item| match item {
                    Value::Object(object) => CipherResponse::from_object(object),
                    _ => Err(serde::de::Error::custom("a cipher must be a JSON object")),
                })
                .collect::<Result<_, _>>()?,
            _ => return Err(serde::de::Error::custom("`ciphers` must be a list")),
        };
        Ok(sync)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Profile {
    /// The user id, which cipher writes send as `encryptedFor`.
    pub id: Uuid,
    pub email: String,
    /// The user key under the stretched master key.
    #[serde(default, deserialize_with = "enc_option")]
    pub key: Option<EncString>,
    /// The PKCS#8 private key under the user key.
    #[serde(rename = "privatekey", default, deserialize_with = "enc_option")]
    pub private_key: Option<EncString>,
    #[serde(default, deserialize_with = "null_default")]
    pub organizations: Vec<ProfileOrganization>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct ProfileOrganization {
    pub id: Uuid,
    /// The org key under the user's private key (type 4).
    #[serde(default, deserialize_with = "enc_option")]
    pub key: Option<EncString>,
    /// Plain text.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct CollectionResponse {
    pub id: Uuid,
    #[serde(rename = "organizationid")]
    pub organization_id: Uuid,
    /// Under the org key.
    #[serde(deserialize_with = "enc_string")]
    pub name: EncString,
}
