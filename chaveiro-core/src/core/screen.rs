use super::{
    api::saving,
    input::DeviceKeyError,
    passkey::PasskeyPrompt,
    request::Purpose,
    ssh_agent::{SshApproval, SshAsk, ssh_ask, ssh_key_name},
    state::{ConflictStage, DerivePurpose, State, SyncState},
};
use crate::{
    core::Core,
    crypto::KdfError,
    proto::cipher::CipherResponse,
    vault::{CipherView, VaultError, VaultIndex},
};
use url::Url;

/// What the UI shows.
pub enum Screen<'a> {
    /// Waiting for [`Input::Started`](super::Input::Started).
    Starting,
    Login {
        /// The server of the last account, to fill in the form; [`None`]
        /// before any login.
        server: Option<&'a Url>,
        email: &'a str,
        error: Option<&'a LoginError>,
        /// A login is in flight.
        busy: bool,
    },
    SecondFactor {
        email: &'a str,
        challenge: &'a Challenge,
        error: Option<&'a LoginError>,
        /// A code is being checked.
        busy: bool,
        /// A request to email a code is in flight.
        sending: bool,
    },
    Locked {
        email: &'a str,
        error: Option<&'a UnlockError>,
        busy: bool,
        /// Offer device unlock.
        device_unlock: bool,
    },
    Vault(VaultScreen<'a>),
}

pub struct VaultScreen<'a> {
    pub email: &'a str,
    /// The server the account logs in to, as the user typed it.
    pub server: &'a Url,
    pub index: &'a VaultIndex,
    /// Changes whenever [`index`](Self::index) changes: an item or folder
    /// comes or goes, or any summary differs. Equal numbers mean an equal
    /// index, so the shell filters again only when this changes.
    pub index_revision: u64,
    pub open: Option<&'a OpenItem>,
    pub syncing: bool,
    /// The last sync failed, for example offline; the list is the cached
    /// one.
    pub sync_error: Option<&'a str>,
    pub error: Option<&'a ItemError>,
    /// An item the user saved, new or edited, waits for the server. Once a
    /// new item lands, [`open`](Self::open) holds it; an edited item that is
    /// open shows the server's copy. A second
    /// [`UserAction::Save`](super::UserAction::Save) of a new item, or of the
    /// same item, meanwhile gets [`ItemError::Busy`]; a save of another item
    /// goes out.
    /// Also true while an out-of-date save waits for the sync that brings
    /// the server copy, so [`OpenItem::conflict`] is ready once this turns
    /// false.
    pub saving: bool,
    /// The cache's account revision (epoch ms); [`None`] before the first
    /// sync after login.
    pub revision: Option<u64>,
    /// Our clock (epoch ms) at the last sync.
    pub synced_at: Option<u64>,
    /// Device unlock is set up for this account.
    pub device_unlock: bool,
    /// An enroll waits for the device key.
    pub device_enrolling: bool,
    /// Why the last enroll failed.
    pub device_error: Option<&'a DeviceKeyError>,
    /// The oldest SSH sign request, which waits for
    /// [`UserAction::ApproveSsh`](super::UserAction::ApproveSsh) or [`UserAction::DenySsh`](super::UserAction::DenySsh).
    pub ssh_approval: Option<SshApproval<'a>>,
}

/// The item being viewed or edited.
pub struct OpenItem {
    pub view: CipherView,
    /// The cipher as it was opened. A save sends its revision date, so a
    /// newer server copy fails as a conflict.
    pub(super) base: CipherResponse,
    /// A sync brought a newer server copy; the next save shows a conflict.
    pub stale: bool,
    pub(super) conflict: ConflictStage,
}

impl OpenItem {
    /// Both versions, once a conflict is ready for the user to pick one.
    pub fn conflict(&self) -> Option<Conflict<'_>> {
        match &self.conflict {
            ConflictStage::Ready {
                mine,
                theirs,
                theirs_changed,
            } => Some(Conflict {
                mine,
                theirs,
                theirs_changed,
            }),
            _ => None,
        }
    }
}

pub struct Conflict<'a> {
    pub mine: &'a CipherView,
    pub theirs: &'a CipherView,
    /// When the server copy changed, ISO 8601 as the server wrote it.
    pub theirs_changed: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Challenge {
    /// Provider numbers (0 authenticator, 1 email, ...), sorted.
    TwoFactor {
        providers: Vec<u8>,
        /// The address email codes go to, masked by the server, such as
        /// `f***@example.com`. [`None`] when email is not offered, or the
        /// server named no address.
        masked_email: Option<String>,
    },
    NewDevice,
}

/// The second-factor providers a code can answer, in the order the login
/// screen prefers them: authenticator, email, YubiKey OTP.
pub const CODE_PROVIDERS: [u8; 3] = [0, 1, 3];

impl Challenge {
    /// The provider the second-factor screen opens with: the first of
    /// [`CODE_PROVIDERS`] the server offers. [`None`] for the new-device
    /// check, and when the account has no provider this app can answer.
    pub fn default_provider(&self) -> Option<u8> {
        match self {
            Challenge::TwoFactor { providers, .. } => CODE_PROVIDERS
                .into_iter()
                .find(|provider| providers.contains(provider)),
            Challenge::NewDevice => None,
        }
    }

    /// Email is the account's one method, so there is nothing to pick.
    pub fn only_email(&self) -> bool {
        matches!(self, Challenge::TwoFactor { providers, .. } if providers[..] == [1])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginError {
    /// The server refused, such as a wrong password; the message is for
    /// the user.
    Rejected(String),
    InvalidCode,
    Server {
        status: u16,
    },
    Network(String),
    BadResponse,
    Kdf(KdfError),
    /// The account key did not open with this password.
    Key(VaultError),
    /// The session ended: the refresh token was refused or cleared.
    SessionExpired,
    /// The server URL is not an `http` or `https` URL.
    ServerUrl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnlockError {
    WrongPassword,
    /// No cached vault to unlock yet.
    NoVaultData,
    Kdf(KdfError),
    DeviceKey(DeviceKeyError),
    /// The device secret no longer opens the sealed user key, so device
    /// unlock was turned off; the master password is needed.
    DeviceKeyChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemError {
    NotFound,
    Unreadable(VaultError),
    /// A write for this item is still in flight.
    Busy,
    /// The server or the network refused a write; the message is for the
    /// user.
    Rejected(String),
}

impl Core {
    /// The passkey request that waits for unlock or for a pick, shown over
    /// [`screen`](Self::screen).
    pub fn passkey_prompt(&self) -> Option<PasskeyPrompt<'_>> {
        let account = match &self.state {
            State::Locked { account, .. } => account,
            State::Deriving {
                purpose: DerivePurpose::Unlock(account),
                ..
            } => &**account,
            State::Unlocked(unlocked) => &unlocked.account,
            _ => return None,
        };
        account.passkey.as_ref().map(|pending| PasskeyPrompt {
            id: pending.id,
            request: &pending.request,
            choices: &pending.choices,
        })
    }

    /// What the UI shows. Borrowed, so nothing secret is copied.
    pub fn screen(&self) -> Screen<'_> {
        match &self.state {
            State::Starting => Screen::Starting,
            State::LoggedOut { form, error } => Screen::Login {
                server: form.server.as_ref(),
                email: &form.email,
                error: error.as_ref(),
                busy: false,
            },
            State::TokenRequest {
                email,
                challenge: Some(challenge),
                ..
            } => Screen::SecondFactor {
                email,
                challenge,
                error: None,
                busy: true,
                sending: self.sending_email_code(),
            },
            State::Prelogin { server, email, .. }
            | State::TokenRequest { server, email, .. }
            | State::Deriving {
                email,
                purpose: DerivePurpose::Login(server),
                ..
            } => Screen::Login {
                server: Some(&server.url),
                email,
                error: None,
                busy: true,
            },
            State::Deriving {
                purpose: DerivePurpose::Unlock(account),
                ..
            } => Screen::Locked {
                email: &account.email,
                error: None,
                busy: true,
                device_unlock: account.device_wrap.is_some(),
            },
            State::SecondFactor {
                email,
                challenge,
                error,
                ..
            } => Screen::SecondFactor {
                email,
                challenge,
                error: error.as_ref(),
                busy: false,
                sending: self.sending_email_code(),
            },
            State::Locked { account, error } => Screen::Locked {
                email: &account.email,
                error: error.as_ref(),
                busy: self
                    .requests
                    .any(|purpose| matches!(purpose, Purpose::DeviceUnlock)),
                device_unlock: account.device_wrap.is_some(),
            },
            State::Unlocked(unlocked) => Screen::Vault(VaultScreen {
                email: &unlocked.account.email,
                server: &unlocked.account.server.url,
                index: &unlocked.index,
                index_revision: self.index_revision,
                open: unlocked.open.as_ref(),
                syncing: matches!(unlocked.sync, SyncState::Running),
                sync_error: match &unlocked.sync {
                    SyncState::Failed(message) => Some(message),
                    _ => None,
                },
                error: unlocked.error.as_ref(),
                saving: saving(&self.requests, &unlocked.account)
                    || unlocked
                        .open
                        .as_ref()
                        .is_some_and(|open| matches!(open.conflict, ConflictStage::Syncing { .. })),
                revision: unlocked.account.cache.as_ref().map(|cache| cache.revision),
                synced_at: unlocked.account.cache.as_ref().map(|cache| cache.synced_at),
                device_unlock: unlocked.account.device_wrap.is_some(),
                device_enrolling: self
                    .requests
                    .any(|purpose| matches!(purpose, Purpose::DeviceEnroll { .. })),
                device_error: unlocked.device_error.as_ref(),
                ssh_approval: unlocked.account.ssh.first().map(|pending| {
                    let key = unlocked
                        .index
                        .ssh_keys
                        .iter()
                        .find(|key| key.public_key == pending.public_key);
                    SshApproval {
                        id: pending.id,
                        name: key.map_or("", |key| ssh_key_name(&unlocked.index, key.cipher_id)),
                        fingerprint: key.map_or("", |key| key.fingerprint.as_str()),
                        remember: ssh_ask(&unlocked.index, &self.settings, &pending.public_key)
                            == SshAsk::UntilLock,
                    }
                }),
            }),
        }
    }
}
