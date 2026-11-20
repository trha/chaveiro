//! Request ids and the table of requests the core waits for.

use super::{Body, PasskeyReply, SecretBytes};
use crate::{crypto::SecretString, vault::CipherView};
use uuid::Uuid;

/// Names one effect whose answer comes back as an input. A [`Core`](super::Core)
/// never hands out the same id twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

impl RequestId {
    /// An id the shell picks for a request it starts, such as
    /// [`Input::Passkey`](super::Input::Passkey). The core only echoes it
    /// back in the answer, so it may equal an id the core handed out.
    pub fn from_shell(value: u64) -> Self {
        Self(value)
    }
}

/// The requests whose answers the core still accepts, each with what it was
/// for. An answer whose id is not here is stale and is dropped.
#[derive(Default)]
pub(super) struct Table {
    next: u64,
    pending: Vec<(RequestId, Purpose)>,
}

impl Table {
    /// A fresh id, recorded with `purpose`.
    pub(super) fn start(&mut self, purpose: Purpose) -> RequestId {
        self.next += 1;
        let id = RequestId(self.next);
        self.pending.push((id, purpose));
        id
    }

    /// Removes `id` and returns its purpose, or [`None`] when the core no
    /// longer waits for it.
    pub(super) fn finish(&mut self, id: RequestId) -> Option<Purpose> {
        let index = self
            .pending
            .iter()
            .position(|(pending, _)| *pending == id)?;
        Some(self.pending.swap_remove(index).1)
    }

    /// Forgets every pending request, so their answers arrive as stale. Ids
    /// keep counting up.
    pub(super) fn clear(&mut self) {
        self.pending.clear();
    }

    /// Forgets every pending request like [`clear`](Self::clear), and
    /// returns what each was for.
    pub(super) fn drain(&mut self) -> Vec<Purpose> {
        self.pending.drain(..).map(|(_, purpose)| purpose).collect()
    }

    /// Forgets the pending requests that match `test`, so their answers
    /// arrive as stale.
    pub(super) fn forget(&mut self, test: impl Fn(&Purpose) -> bool) {
        self.pending.retain(|(_, purpose)| !test(purpose));
    }

    /// True when a pending request matches `test`.
    pub(super) fn any(&self, test: impl Fn(&Purpose) -> bool) -> bool {
        self.pending.iter().any(|(_, purpose)| test(purpose))
    }
}

/// What a pending request was for.
pub(super) enum Purpose {
    Prelogin,
    Derive,
    Token,
    SendEmailCode,
    Refresh,
    Api(ApiCall),
    Store,
    /// A device-unlock enroll over `challenge`. `first` holds the first
    /// secret while the core asks for the second one to compare.
    DeviceEnroll {
        challenge: [u8; 32],
        first: Option<SecretBytes>,
    },
    DeviceUnlock,
}

/// One authorized request to the API server, kept until it is answered so
/// that a `401` can send it again after a token refresh.
pub(super) struct ApiCall {
    pub(super) method: super::Method,
    /// Path segments appended to the API base URL, each percent-encoded.
    pub(super) path: Vec<String>,
    /// Query pairs, form-encoded.
    pub(super) query: &'static [(&'static str, &'static str)],
    pub(super) body: Body,
    pub(super) kind: ApiKind,
    /// True once it was sent again after a `401`; a second `401` ends the
    /// session.
    pub(super) retried: bool,
}

pub(super) enum ApiKind {
    /// `GET /accounts/revision-date`. `force` syncs even when the revision
    /// is not newer than the cache.
    RevisionDate {
        force: bool,
    },
    /// `GET /sync`, with the revision read just before it.
    Sync {
        revision: u64,
    },
    Write(Write),
    Folder(FolderWrite),
}

pub(super) struct Write {
    /// [`None`] for a create.
    pub(super) cipher_id: Option<Uuid>,
    pub(super) action: WriteAction,
    /// The view the user saved, kept so a conflict can show it again.
    pub(super) view: Option<Box<CipherView>>,
    /// Set when the write stores a passkey change.
    pub(super) passkey: Option<PasskeyWrite>,
}

pub(super) enum PasskeyWrite {
    /// A new passkey: answer the provider request `id` with `reply` once
    /// the server stores it.
    Created {
        id: RequestId,
        /// Boxed, because it is far larger than a counter bump.
        reply: Box<PasskeyReply>,
    },
    /// A counter bump after an assertion. On a conflict the higher counter
    /// wins.
    Counter(CounterBump),
}

pub(super) struct CounterBump {
    pub(super) cipher_id: Uuid,
    /// As stored: GUID text, or `b64.` and base64url.
    pub(super) credential_id: SecretString,
    pub(super) counter: u32,
}

/// A folder create, rename or delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FolderWrite {
    Create,
    Rename(Uuid),
    Delete(Uuid),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriteAction {
    Create,
    Update,
    Trash,
    Restore,
    Delete,
}
