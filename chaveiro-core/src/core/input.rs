use super::{
    effect::StoreSlot,
    passkey::PasskeyRequest,
    request::RequestId,
    ssh_agent::{AgentRequestId, SshAsk},
};
use crate::{
    crypto::{KdfError, MasterKey, SecretString},
    vault::{CipherView, CopyField},
};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

pub enum Input {
    /// The shell read every slot at boot.
    Started {
        stored: StoredAccount,
    },
    User(UserAction),
    Http {
        id: RequestId,
        response: Result<HttpResponse, HttpError>,
    },
    Derived {
        id: RequestId,
        result: Result<MasterKey, KdfError>,
    },
    Stored {
        id: RequestId,
        result: Result<(), StorageError>,
    },
    /// Timer; drives the lock timeout and the revision poll.
    Tick,
    /// The device key's secret for a challenge, or why it failed.
    DeviceSecret {
        id: RequestId,
        result: Result<SecretBytes, DeviceKeyError>,
    },
    /// A decoded MakeCredential or GetAssertion from the passkey provider.
    Passkey {
        id: RequestId,
        request: PasskeyRequest,
    },
    /// One SSH agent frame without its length prefix. Each one gets exactly
    /// one [`Effect::SshAgentReply`](super::Effect::SshAgentReply) with the same `id`.
    SshAgent {
        id: AgentRequestId,
        frame: Vec<u8>,
    },
    /// Another process wrote a shared slot ([`None`]: it was cleared).
    StorageChanged {
        slot: StoreSlot,
        bytes: Option<Vec<u8>>,
    },
    /// The user's settings, sent at start and on every change. Until the
    /// first one, the core uses [`Settings::default`].
    Settings(Settings),
}

/// The settings the core reads. The shell owns the settings file and sends
/// this part in [`Input::Settings`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// For SSH keys with no `ssh-ask` field.
    pub ssh_ask: SshAsk,
}

/// The bytes of each storage slot at boot, [`None`] for an empty slot.
#[derive(Clone, Debug, Default)]
pub struct StoredAccount {
    pub account: Option<Vec<u8>>,
    pub refresh_token: Option<Vec<u8>>,
    pub vault_cache: Option<Vec<u8>>,
    pub device_wrap: Option<Vec<u8>>,
    pub two_factor_token: Option<Vec<u8>>,
}

pub enum UserAction {
    /// `server` is the URL the user typed or picked, such as
    /// `https://vault.bitwarden.com`; see [`Config::server_urls`](super::Config::server_urls). A URL that
    /// is not `http` or `https` gives [`LoginError::ServerUrl`](super::LoginError::ServerUrl).
    Login {
        server: Url,
        email: String,
        password: SecretString,
    },
    /// Log in with the account's personal API key. The vault then waits
    /// locked for the master password.
    LoginApiKey {
        server: Url,
        email: String,
        client_id: String,
        client_secret: SecretString,
    },
    /// A second-factor code for one of the offered providers.
    SubmitCode {
        provider: u8,
        code: SecretString,
        /// Ask the server for a token that skips the second factor at the
        /// next login with this email. Logout keeps it.
        remember: bool,
    },
    /// The code from the new-device email.
    SubmitNewDeviceOtp {
        code: SecretString,
    },
    /// Ask the server to email a second-factor code.
    SendEmailCode,
    /// Leave a login in progress, or refuse the waiting
    /// [`PasskeyPrompt`](super::PasskeyPrompt).
    Cancel,
    Unlock {
        password: SecretString,
    },
    /// Unlock with the device key, when [`Screen::Locked`](super::Screen::Locked) offers it.
    DeviceUnlock,
    /// Set up device unlock while unlocked. It asks the device key twice.
    EnableDeviceUnlock,
    DisableDeviceUnlock,
    Lock,
    Logout,
    Sync,
    Open(Uuid),
    Close,
    /// Save an edited item, or create one when its `id` is [`None`]. A new
    /// item goes to the org in [`CipherView::organization_id`] and its
    /// [`collection_ids`](CipherView::collection_ids), or to the personal
    /// vault. An edit that changes a login password moves the old one into
    /// the password history.
    Save(Box<CipherView>),
    Trash(Uuid),
    Restore(Uuid),
    /// Delete for good.
    Delete(Uuid),
    /// Move an item into `folder`, or out of any folder with [`None`]. The
    /// item need not be open: the server changes only its folder, so
    /// nothing is decrypted.
    MoveToFolder {
        id: Uuid,
        folder: Option<Uuid>,
    },
    /// Turn on or off whether an item asks for the master password before
    /// it opens. The item need not be open: its cached encrypted copy goes
    /// back with only this flag changed, so nothing is decrypted.
    SetReprompt {
        id: Uuid,
        on: bool,
    },
    /// Copy one field of an item to the clipboard through
    /// [`Effect::Copy`](super::Effect::Copy). The item need not be open, and the open item stays
    /// as it is. An item with the master password re-prompt flag refuses a
    /// [guarded](CopyField::guarded) field. A missing or unreadable field
    /// does nothing.
    Copy {
        id: Uuid,
        field: CopyField,
    },
    CreateFolder {
        name: SecretString,
    },
    RenameFolder {
        id: Uuid,
        name: SecretString,
    },
    /// Delete a folder; its items stay, outside any folder.
    DeleteFolder(Uuid),
    /// Settle the open item's conflict. Keeping the user's copy saves it
    /// over the server's at once, as a [`Save`](Self::Save) would; keeping
    /// the server's drops the user's.
    ResolveConflict {
        keep_mine: bool,
    },
    /// Answer the [`PasskeyPrompt`](super::PasskeyPrompt) with the choice at this index.
    /// [`Cancel`](Self::Cancel) refuses it.
    ChoosePasskey(usize),
    /// Sign the request in [`VaultScreen::ssh_approval`](super::VaultScreen::ssh_approval).
    ApproveSsh,
    /// Refuse the request in [`VaultScreen::ssh_approval`](super::VaultScreen::ssh_approval).
    DenySsh,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The request got no response: offline, DNS, TLS or a timeout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpError {
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageError {
    pub message: String,
}

pub type SecretBytes = Zeroizing<Vec<u8>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceKeyError {
    Unavailable,
    Cancelled,
    /// Two secrets over the same challenge differ.
    NotDeterministic,
    Failed(String),
}
