//! The offline cache document, stored in the `VaultCache` slot.
//!
//! The core writes this JSON itself, so it is read with plain `serde_json`
//! in its own key case.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// One stored copy of the vault.
///
/// `revision` is the account revision that `sync` reflects, so of two
/// documents for the same `user_id` the one with the higher `revision` is
/// the newer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheDocument {
    /// [`CacheDocument::FORMAT`].
    pub format: u32,
    pub user_id: Uuid,
    /// Account revision (epoch ms): the value of
    /// `GET /api/accounts/revision-date` read just before the sync, or the
    /// server time of our own last write.
    pub revision: u64,
    /// Our clock (epoch ms), for "last synced" in the UI.
    pub synced_at: u64,
    /// The `GET /api/sync` body as received, patched by our writes.
    pub sync: Value,
}

impl CacheDocument {
    /// The format this build writes and reads.
    pub const FORMAT: u32 = 1;

    /// Reads a stored document. A document of another
    /// [`format`](Self::format) gives [`None`] without parsing the rest, and
    /// so does one that is not valid JSON of this shape.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        #[derive(Deserialize)]
        struct Header {
            format: u32,
        }
        let header: Header = serde_json::from_slice(bytes).ok()?;
        if header.format != Self::FORMAT {
            return None;
        }
        serde_json::from_slice(bytes).ok()
    }

    /// The bytes to store.
    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a JSON value with string keys always serializes")
    }
}
