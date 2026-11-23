use super::{
    config::Server,
    input::DeviceKeyError,
    passkey::{PasskeyCatalogEntry, Pending},
    record::Cache,
    request::{ApiCall, CounterBump, RequestId},
    screen::{Challenge, ItemError, LoginError, OpenItem, UnlockError},
};
use crate::{
    core::ssh_agent,
    crypto::{Kdf, MasterKey, SecretString},
    vault::{CipherView, VaultIndex, VaultKeys},
};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Login progress, and the account once a token arrived.
pub(super) enum State {
    Starting,
    LoggedOut {
        form: LoginForm,
        error: Option<LoginError>,
    },
    Prelogin {
        server: Server,
        email: String,
        password: SecretString,
        request: RequestId,
    },
    Deriving {
        email: String,
        password: SecretString,
        kdf: Kdf,
        purpose: DerivePurpose,
        request: RequestId,
    },
    TokenRequest {
        server: Server,
        email: String,
        /// From prelogin; [`None`] for an API key, whose token response
        /// carries it.
        kdf: Option<Kdf>,
        credentials: Credentials,
        /// The challenge the request answers with a code. While set, the UI
        /// stays on the second-factor screen.
        challenge: Option<Challenge>,
        request: RequestId,
    },
    SecondFactor {
        server: Server,
        email: String,
        kdf: Option<Kdf>,
        master: Box<MasterKey>,
        password_hash: SecretString,
        challenge: Challenge,
        error: Option<LoginError>,
    },
    Locked {
        account: Account,
        error: Option<UnlockError>,
    },
    Unlocked(Box<Unlocked>),
}

#[derive(Default)]
pub(super) struct LoginForm {
    pub(super) server: Option<Url>,
    pub(super) email: String,
}

#[expect(
    clippy::large_enum_variant,
    reason = "the other login states hold a Server inline too, so boxing it here saves nothing"
)]
pub(super) enum DerivePurpose {
    Login(Server),
    Unlock(Box<Account>),
}

pub(super) enum Credentials {
    /// `master` stays until the user key opens; it is boxed so moves
    /// between states copy no key bytes.
    Password {
        master: Box<MasterKey>,
        password_hash: SecretString,
        answer: Option<Answer>,
    },
    ApiKey,
}

pub(super) enum Answer {
    TwoFactor {
        provider: u8,
        code: SecretString,
        remember: bool,
    },
    NewDevice {
        code: SecretString,
    },
    /// The stored token, sent before the server asks.
    Remembered {
        token: SecretString,
    },
}

/// The signed-in account. `cache` is still encrypted, so it stays while
/// locked.
pub(super) struct Account {
    pub(super) server: Server,
    pub(super) email: String,
    pub(super) user_id: Uuid,
    pub(super) kdf: Kdf,
    pub(super) tokens: Tokens,
    /// Boxed, because it is far larger than the rest of the account.
    pub(super) cache: Option<Box<Cache>>,
    /// The stored device-unlock record, if any.
    pub(super) device_wrap: Option<DeviceWrap>,
    /// API calls held until a refresh brings a fresh access token.
    pub(super) waiting: Vec<ApiCall>,
    /// A refresh is in flight.
    pub(super) refreshing: bool,
    /// The passkey request that waits for unlock or a pick. It outlives a
    /// lock, but its choices do not.
    pub(super) passkey: Option<Box<Pending>>,
    /// The catalog last sent in [`Effect::PasskeyCatalog`](super::Effect::PasskeyCatalog).
    pub(super) catalog: Vec<PasskeyCatalogEntry>,
    /// The highest counter signed per credential id this session, so two
    /// assertions sign rising counters before the first write lands.
    pub(super) signed: Vec<(Vec<u8>, u32)>,
    /// SSH sign requests that wait for unlock or approval, oldest first.
    /// Lock and logout fail them all.
    pub(super) ssh: Vec<ssh_agent::Pending>,
}

pub(super) struct Tokens {
    pub(super) access: Option<SecretString>,
    /// [`None`] after an API key login on servers that send none.
    pub(super) refresh: Option<SecretString>,
    /// Epoch ms.
    pub(super) expires_at: u64,
}

pub(super) struct Unlocked {
    pub(super) account: Account,
    pub(super) keys: VaultKeys,
    pub(super) index: VaultIndex,
    pub(super) open: Option<OpenItem>,
    pub(super) sync: SyncState,
    pub(super) error: Option<ItemError>,
    pub(super) last_activity: u64,
    /// When the last revision check or sync started.
    pub(super) last_check: u64,
    /// Counter writes that came back out of date, sent again after the
    /// next sync when they are still higher.
    pub(super) retries: Vec<CounterBump>,
    pub(super) device_error: Option<DeviceKeyError>,
    /// Public keys in [`SshAsk::UntilLock`](super::SshAsk::UntilLock) mode that the user approved this
    /// session. Lock drops them with the rest of `Unlocked`.
    pub(super) ssh_grants: Vec<Vec<u8>>,
    /// Item edits waiting for the pending write to their item, oldest
    /// first, at most one of each kind per item.
    pub(super) edits: Vec<(Uuid, ItemEdit)>,
}

/// An edit of an item by id, which needs nothing decrypted; see
/// [`UserAction::MoveToFolder`](super::UserAction::MoveToFolder) and [`UserAction::SetReprompt`](super::UserAction::SetReprompt).
pub(super) enum ItemEdit {
    Folder(Option<Uuid>),
    Reprompt(bool),
}

/// The record in [`StoreSlot::DeviceWrap`](super::StoreSlot::DeviceWrap), stored as JSON. It holds no
/// secret in the clear, so it stays while locked.
#[derive(Serialize, Deserialize)]
pub(super) struct DeviceWrap {
    pub(super) user_id: Uuid,
    pub(super) challenge: [u8; 32],
    /// Type 2 EncString of the 64-byte user key under
    /// [`device_key`](crate::crypto::device_key)`(secret)`.
    pub(super) sealed_user_key: String,
}

impl DeviceWrap {
    /// [`None`] for a malformed record or one of another user.
    pub(super) fn parse(bytes: &[u8], user_id: Uuid) -> Option<Self> {
        serde_json::from_slice::<Self>(bytes)
            .ok()
            .filter(|wrap| wrap.user_id == user_id)
    }
}

pub(super) enum SyncState {
    Idle,
    Running,
    Failed(String),
}

pub(super) enum ConflictStage {
    None,
    /// A write came back out of date; waiting for the sync that brings the
    /// server copy.
    Syncing {
        mine: Box<CipherView>,
    },
    Ready {
        mine: Box<CipherView>,
        theirs: Box<CipherView>,
    },
}
