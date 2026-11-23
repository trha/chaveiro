use super::login::secret;
use crate::{
    crypto::{Kdf, SecretString},
    proto::{
        cache::CacheDocument,
        cipher::{CipherResponse, FolderResponse},
        sync::{Profile, SyncResponse},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;
use uuid::Uuid;

/// The record in [`StoreSlot::TwoFactorToken`](super::StoreSlot::TwoFactorToken). The server accepts `token`
/// only for `email`, and only `server` may see it.
pub(super) struct RememberedLogin {
    pub(super) server: Url,
    pub(super) email: String,
    pub(super) token: SecretString,
}

#[derive(Serialize, Deserialize)]
pub(super) struct RememberedRecord<'a> {
    pub(super) server: Url,
    pub(super) email: &'a str,
    pub(super) token: &'a str,
}

impl RememberedLogin {
    /// [`None`] for a malformed record, or one written before records named
    /// their server.
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        let record = serde_json::from_slice::<RememberedRecord<'_>>(bytes).ok()?;
        Some(Self {
            server: record.server,
            email: record.email.to_owned(),
            token: secret(record.token),
        })
    }

    pub(super) fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&RememberedRecord {
            server: self.server.clone(),
            email: &self.email,
            token: &self.token,
        })
        .expect("a remembered login always serializes")
    }

    /// Whether this token may go with a login of `email` (in any ASCII
    /// case) to `server`.
    pub(super) fn fits(&self, server: &Url, email: &str) -> bool {
        self.server == *server && self.email.eq_ignore_ascii_case(email)
    }
}

/// The cached sync in memory: each cipher is its own record, and
/// [`to_document`](Self::to_document) rebuilds the stored document from
/// them.
pub(super) struct Cache {
    /// See [`CacheDocument::revision`].
    pub(super) revision: u64,
    pub(super) synced_at: u64,
    /// The sync body without its `ciphers` key, as received.
    pub(super) rest: Map<String, Value>,
    pub(super) profile: Profile,
    pub(super) folders: Vec<FolderResponse>,
    pub(super) ciphers: Vec<CipherResponse>,
}

impl Cache {
    pub(super) fn from_document(document: CacheDocument) -> Option<Self> {
        Self::from_sync(document.sync, document.revision, document.synced_at)
    }

    /// [`None`] when `body` is not a sync response.
    pub(super) fn from_sync(body: Value, revision: u64, synced_at: u64) -> Option<Self> {
        let sync = SyncResponse::from_json(body.clone()).ok()?;
        let Value::Object(mut rest) = body else {
            return None;
        };
        rest.retain(|key, _| !key.eq_ignore_ascii_case("ciphers"));
        Some(Self {
            revision,
            synced_at,
            rest,
            profile: sync.profile,
            folders: sync.folders,
            ciphers: sync.ciphers,
        })
    }

    /// Puts `folder` (typed, and as the server sent it) in place of the
    /// folder `id`, or removes that folder for [`None`], in both
    /// [`folders`](Self::folders) and the stored sync body. Removing a folder
    /// also takes every cipher out of it, as the server does.
    pub(super) fn set_folder(
        &mut self,
        id: Uuid,
        folder: Option<(FolderResponse, Map<String, Value>)>,
    ) {
        let key = self
            .rest
            .keys()
            .find(|key| key.eq_ignore_ascii_case("folders"))
            .cloned()
            .unwrap_or_else(|| "folders".to_owned());
        let stored = self
            .rest
            .entry(key)
            .or_insert_with(|| Value::Array(Vec::new()));
        if !stored.is_array() {
            *stored = Value::Array(Vec::new());
        }
        let stored = stored
            .as_array_mut()
            .expect("the value was just made a list");
        let same_id = |value: &Value| {
            value.as_object().is_some_and(|object| {
                object.iter().any(|(key, value)| {
                    key.eq_ignore_ascii_case("id")
                        && value.as_str().and_then(|text| text.parse().ok()) == Some(id)
                })
            })
        };
        stored.retain(|value| !same_id(value));
        self.folders.retain(|existing| existing.id != id);
        match folder {
            Some((typed, object)) => {
                stored.push(Value::Object(object));
                self.folders.push(typed);
            }
            None => {
                for cipher in &mut self.ciphers {
                    if cipher.folder_id == Some(id) {
                        cipher.folder_id = None;
                        cipher
                            .raw
                            .retain(|key, _| !key.eq_ignore_ascii_case("folderid"));
                        cipher.raw.insert("folderId".to_owned(), Value::Null);
                    }
                }
            }
        }
    }

    pub(super) fn to_document(&self, user_id: Uuid) -> CacheDocument {
        let mut sync = self.rest.clone();
        sync.insert(
            "ciphers".to_owned(),
            Value::Array(
                self.ciphers
                    .iter()
                    .map(|cipher| Value::Object(cipher.raw.clone()))
                    .collect(),
            ),
        );
        CacheDocument {
            format: CacheDocument::FORMAT,
            user_id,
            revision: self.revision,
            synced_at: self.synced_at,
            sync: Value::Object(sync),
        }
    }
}

/// The `Account` slot.
#[derive(Serialize, Deserialize)]
pub(super) struct AccountRecord {
    /// Absent in records written before the core took the server per
    /// login; such an account boots logged out.
    #[serde(default)]
    pub(super) server: Option<Url>,
    pub(super) email: String,
    pub(super) user_id: Uuid,
    pub(super) kdf: KdfRecord,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub(super) enum KdfRecord {
    Pbkdf2 {
        iterations: u32,
    },
    Argon2id {
        iterations: u32,
        memory_mebibytes: u32,
        parallelism: u32,
    },
}

impl From<Kdf> for KdfRecord {
    fn from(kdf: Kdf) -> Self {
        match kdf {
            Kdf::Pbkdf2 { iterations } => Self::Pbkdf2 { iterations },
            Kdf::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            } => Self::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            },
        }
    }
}

impl From<KdfRecord> for Kdf {
    fn from(record: KdfRecord) -> Self {
        match record {
            KdfRecord::Pbkdf2 { iterations } => Self::Pbkdf2 { iterations },
            KdfRecord::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            } => Self::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            },
        }
    }
}

/// The `twoFactorProvider` number for a remembered token.
pub(super) const REMEMBER_PROVIDER: u8 = 5;
