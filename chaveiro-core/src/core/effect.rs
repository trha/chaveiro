use super::{
    passkey::{PasskeyCatalogEntry, PasskeyError, PasskeyReply},
    request::RequestId,
    ssh_agent::AgentRequestId,
};
use crate::crypto::{Kdf, SecretString};
use serde_json::Value;
use url::Url;

pub enum Effect {
    Http {
        id: RequestId,
        request: HttpRequest,
    },
    /// Run [`derive_master_key`](crate::crypto::derive_master_key) off the
    /// UI thread and answer with [`Input::Derived`](super::Input::Derived).
    Derive {
        id: RequestId,
        password: SecretString,
        email: String,
        kdf: Kdf,
    },
    /// Write `bytes` over the slot, with OS protection when
    /// [`StoreSlot::protected`], and answer with [`Input::Stored`](super::Input::Stored).
    Store {
        id: RequestId,
        slot: StoreSlot,
        bytes: Vec<u8>,
    },
    Clear {
        slot: StoreSlot,
    },
    /// The shell renders again.
    Rerender,
    /// Put `text` on the clipboard, kept out of clipboard history and
    /// sync, and clear it later if nothing replaced it.
    Copy {
        text: SecretString,
    },
    /// Ask the device key for its secret over `challenge`, behind a
    /// biometric prompt. `create` makes the key first if it is missing.
    DeviceSecret {
        id: RequestId,
        challenge: [u8; 32],
        create: bool,
    },
    /// Answer a passkey request through the platform's provider API.
    PasskeyReply {
        id: RequestId,
        reply: Result<PasskeyReply, PasskeyError>,
    },
    /// Tell the platform which passkeys we hold.
    PasskeyCatalog {
        entries: Vec<PasskeyCatalogEntry>,
    },
    /// The reply frame for [`Input::SshAgent`](super::Input::SshAgent), without its length prefix.
    SshAgentReply {
        id: AgentRequestId,
        frame: Vec<u8>,
    },
}

pub struct HttpRequest {
    pub method: Method,
    pub url: Url,
    pub headers: Vec<(&'static str, String)>,
    pub body: Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

#[derive(Clone, Debug)]
pub enum Body {
    None,
    Json(Value),
    /// `application/x-www-form-urlencoded`; the shell encodes each pair.
    Form(Vec<(&'static str, SecretString)>),
}

/// One storage slot. The shell writes each one atomically: a temporary file
/// in the same folder, flushed, then renamed over the old one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreSlot {
    /// Email, user id and KDF, as plain JSON.
    Account,
    RefreshToken,
    /// A [`CacheDocument`](crate::proto::cache::CacheDocument).
    VaultCache,
    /// The challenge and the user key sealed under the device-key secret.
    DeviceWrap,
    /// The email and the server's remembered second-factor token, as JSON.
    TwoFactorToken,
}

impl StoreSlot {
    /// The shell must store this slot with OS protection.
    pub fn protected(self) -> bool {
        !matches!(self, Self::Account)
    }
}
