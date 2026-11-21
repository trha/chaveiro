//! The SSH agent protocol (draft-miller-ssh-agent) and signing with a vault
//! key. Frames here carry no 4-byte length prefix; the shell adds and strips
//! it.

use crate::{
    proto::cipher::CipherResponse,
    vault::{SshKeySummary, VaultKeys, decrypt_ssh_private_key},
};
use rsa::{
    BigUint, RsaPrivateKey,
    pkcs1v15::SigningKey,
    signature::{SignatureEncoding, Signer},
};
use sha2::{Sha256, Sha512};
use ssh_encoding::{Decode, Encode, Reader};
use ssh_key::{
    Algorithm, HashAlg, Mpint, PrivateKey, Signature,
    private::{KeypairData, RsaKeypair},
};

/// One decoded agent request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRequest {
    /// `SSH_AGENTC_REQUEST_IDENTITIES`.
    List,
    /// `SSH_AGENTC_SIGN_REQUEST`.
    Sign {
        /// The key blob in SSH wire form.
        public_key: Vec<u8>,
        data: Vec<u8>,
        /// [`RSA_SHA2_256`] and [`RSA_SHA2_512`] pick the RSA hash.
        flags: u32,
    },
    /// Any other message type, answered with [`failure_reply`]. Its body is
    /// not read.
    Other(u8),
}

/// The frame is empty, or a list or sign request is cut short or has bytes
/// after its last field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentParseError;

/// Chosen by the shell, one per frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AgentRequestId(pub u64);

/// The sign request that waits for the user, shown on
/// [`VaultScreen::ssh_approval`](crate::core::VaultScreen::ssh_approval).
pub struct SshApproval<'a> {
    /// The id the shell gave the request in
    /// [`Input::SshAgent`](super::Input::SshAgent). The shell keeps its own
    /// data for the request, such as the calling process, under this id.
    pub id: AgentRequestId,
    /// The item name; empty when the key left the vault since the request.
    pub name: &'a str,
    /// Empty when the key left the vault since the request.
    pub fingerprint: &'a str,
    /// Approval lets this key sign without asking until lock.
    pub remember: bool,
}

/// When a sign request asks the user. The global mode is
/// [`Settings::ssh_ask`](crate::core::Settings::ssh_ask); a key's `ssh-ask`
/// custom field overrides it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SshAsk {
    /// Every request asks.
    #[default]
    Always,
    /// No request asks. A request made while locked still waits for unlock.
    Never,
    /// The first request for a key asks, whatever program sent it. Once
    /// approved, the key signs without asking until lock.
    UntilLock,
}

impl SshAsk {
    /// Name of the custom field that sets a key's mode.
    pub const FIELD: &str = "ssh-ask";

    /// Reads a field value: `always`, `never` or `until-lock`. Any other
    /// value gives [`Always`](Self::Always), so a typo never drops the
    /// prompt.
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "never" => Self::Never,
            "until-lock" => Self::UntilLock,
            _ => Self::Always,
        }
    }
}

/// Flag of a sign request: sign an RSA key with `rsa-sha2-256`.
pub const RSA_SHA2_256: u32 = 2;
/// Flag of a sign request: sign an RSA key with `rsa-sha2-512`.
pub const RSA_SHA2_512: u32 = 4;

/// Decodes one frame.
pub fn parse_request(frame: &[u8]) -> Result<AgentRequest, AgentParseError> {
    let mut rest = frame;
    let request = match u8::decode(&mut rest).map_err(|_| AgentParseError)? {
        REQUEST_IDENTITIES => AgentRequest::List,
        SIGN_REQUEST => {
            let mut sign = || -> Result<AgentRequest, ssh_encoding::Error> {
                Ok(AgentRequest::Sign {
                    public_key: Vec::decode(&mut rest)?,
                    data: Vec::decode(&mut rest)?,
                    flags: u32::decode(&mut rest)?,
                })
            };
            sign().map_err(|_| AgentParseError)?
        }
        other => return Ok(AgentRequest::Other(other)),
    };
    match rest.is_finished() {
        true => Ok(request),
        false => Err(AgentParseError),
    }
}

/// `SSH_AGENT_IDENTITIES_ANSWER` with each key blob and its comment.
pub fn identities_reply(keys: &[(&[u8], &str)]) -> Vec<u8> {
    let mut frame = vec![IDENTITIES_ANSWER];
    let mut write = || -> Result<(), ssh_encoding::Error> {
        keys.len().encode(&mut frame)?;
        for (public_key, comment) in keys {
            public_key.encode(&mut frame)?;
            comment.encode(&mut frame)?;
        }
        Ok(())
    };
    write().expect("agent fields are far below 4 GiB");
    frame
}

/// `SSH_AGENT_SIGN_RESPONSE` holding `signature` in SSH wire form.
pub fn signature_reply(signature: &Signature) -> Vec<u8> {
    let mut frame = vec![SIGN_RESPONSE];
    signature
        .encode_prefixed(&mut frame)
        .expect("a signature is far below 4 GiB");
    frame
}

/// `SSH_AGENT_FAILURE`.
pub fn failure_reply() -> Vec<u8> {
    vec![FAILURE]
}

/// How long a sign request waits for unlock and approval, in ms.
pub(crate) const WAIT: u64 = 60 * 1000;

/// A sign request that waits for unlock or for the user's answer. It holds
/// no private key.
pub(crate) struct Pending {
    pub id: AgentRequestId,
    pub public_key: Vec<u8>,
    pub data: Vec<u8>,
    pub flags: u32,
    /// Epoch ms; a [`Tick`](crate::core::Input::Tick) at or past it fails
    /// the request.
    pub deadline: u64,
}

/// The reply to an approved `pending`: a signature by the vault key whose
/// public key matches the request, or a failure. The private key is
/// decrypted for this call only and wiped before it returns.
pub(crate) fn approved_reply(
    pending: &Pending,
    ssh_keys: &[SshKeySummary],
    ciphers: &[CipherResponse],
    keys: &VaultKeys,
) -> Vec<u8> {
    let signature = ssh_keys
        .iter()
        .find(|key| key.public_key == pending.public_key)
        .and_then(|key| ciphers.iter().find(|cipher| cipher.id == key.cipher_id))
        .and_then(|cipher| decrypt_ssh_private_key(cipher, keys).ok().flatten())
        .and_then(|pem| sign(&pem, &pending.public_key, &pending.data, pending.flags));
    match signature {
        Some(signature) => signature_reply(&signature),
        None => failure_reply(),
    }
}

/// Signs `data` with the OpenSSH private key `pem`. [`None`] when the key
/// does not parse, its public half is not `public_key`, its type is not
/// Ed25519, RSA or ECDSA P-256/P-384, or it is RSA and `flags` asks for
/// neither SHA-2 hash (plain `ssh-rsa` is SHA-1).
fn sign(pem: &str, public_key: &[u8], data: &[u8], flags: u32) -> Option<Signature> {
    let private_key = PrivateKey::from_openssh(pem).ok()?;
    if private_key.public_key().to_bytes().ok()? != public_key {
        return None;
    }
    match private_key.key_data() {
        // OpenSSH's agent checks the 256 flag first, so do we.
        KeypairData::Rsa(keypair) if flags & RSA_SHA2_256 != 0 => {
            let signing_key = SigningKey::<Sha256>::new(rsa_private_key(keypair)?);
            let signature = signing_key.try_sign(data).ok()?.to_vec();
            Signature::new(
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha256),
                },
                signature,
            )
            .ok()
        }
        KeypairData::Rsa(keypair) if flags & RSA_SHA2_512 != 0 => {
            let signing_key = SigningKey::<Sha512>::new(rsa_private_key(keypair)?);
            let signature = signing_key.try_sign(data).ok()?.to_vec();
            Signature::new(
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha512),
                },
                signature,
            )
            .ok()
        }
        KeypairData::Ed25519(_) | KeypairData::Ecdsa(_) => private_key.try_sign(data).ok(),
        _ => None,
    }
}

/// Builds the `rsa` key by hand, because ssh-key 0.6.7's own conversion
/// passes the prime `p` twice and so always fails.
fn rsa_private_key(keypair: &RsaKeypair) -> Option<RsaPrivateKey> {
    let number = |mpint: &Mpint| mpint.as_positive_bytes().map(BigUint::from_bytes_be);
    RsaPrivateKey::from_components(
        number(&keypair.public.n)?,
        number(&keypair.public.e)?,
        number(&keypair.private.d)?,
        vec![number(&keypair.private.p)?, number(&keypair.private.q)?],
    )
    .ok()
}

const FAILURE: u8 = 5;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;
