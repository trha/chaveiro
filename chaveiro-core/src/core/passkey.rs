//! Passkey provider requests and replies, and the WebAuthn bytes the core
//! builds for them.

mod cbor;

use super::{
    api::send_api,
    request::{ApiCall, ApiKind, Write},
};
use serde_json::Map;

use super::{
    config::Config,
    effect::{Body, Effect, Method},
    request::{CounterBump, PasskeyWrite, RequestId, Table, WriteAction},
    state::Unlocked,
};
use crate::{
    crypto::{P256SigningKey, SecretString},
    proto::cipher::{CipherResponse, create_body},
    vault::{
        CipherContent, CipherKind, CipherView, Fido2CredentialView, LoginView, UriView, VaultKeys,
    },
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::{Builder, Uuid};
use zeroize::Zeroizing;

/// A decoded MakeCredential or GetAssertion from the platform's passkey
/// provider API. The shell runs the platform's user verification before it
/// sends one, so every reply sets the UV flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasskeyRequest {
    MakeCredential {
        rp_id: String,
        rp_name: Option<String>,
        user_handle: Vec<u8>,
        user_name: String,
        user_display_name: String,
        client_data_hash: [u8; 32],
        require_resident: bool,
        /// Credential ids the authenticator must not already hold.
        exclude: Vec<Vec<u8>>,
    },
    GetAssertion {
        rp_id: String,
        client_data_hash: [u8; 32],
        /// Credential ids; empty for the discoverable flow.
        allow: Vec<Vec<u8>>,
    },
}

impl PasskeyRequest {
    pub fn rp_id(&self) -> &str {
        match self {
            Self::MakeCredential { rp_id, .. } | Self::GetAssertion { rp_id, .. } => rp_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasskeyReply {
    Created {
        credential_id: Vec<u8>,
        authenticator_data: Vec<u8>,
        public_key_cose: Vec<u8>,
        /// CBOR `{fmt: "none", attStmt: {}, authData}`.
        attestation_object: Vec<u8>,
    },
    Asserted {
        credential_id: Vec<u8>,
        authenticator_data: Vec<u8>,
        /// DER ECDSA P-256 over `authenticator_data || client_data_hash`.
        signature: Vec<u8>,
        user_handle: Option<Vec<u8>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasskeyError {
    /// The user cancelled, did not unlock in time, no passkey fits, or the
    /// save failed.
    NotAllowed,
}

/// One passkey as the platform lists it. It holds no key material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasskeyCatalogEntry {
    pub credential_id: Vec<u8>,
    pub rp_id: String,
    pub user_name: String,
    pub user_display_name: String,
}

/// A passkey request that waits for the user: for unlock, or for a pick
/// from [`choices`](Self::choices).
pub struct PasskeyPrompt<'a> {
    /// The id the shell gave the request in
    /// [`Input::Passkey`](super::Input::Passkey), so the shell can find its
    /// own data for it.
    pub id: RequestId,
    pub request: &'a PasskeyRequest,
    /// Empty while locked. Answer with
    /// [`UserAction::ChoosePasskey`](super::UserAction::ChoosePasskey).
    pub choices: &'a [PasskeyChoice],
}

/// One option of a [`PasskeyPrompt`]: a login to add the new passkey to, a
/// new login, or a passkey to sign in with.
pub struct PasskeyChoice {
    /// The login's name; for a new login, the relying party's name.
    pub name: SecretString,
    pub user_name: SecretString,
    pub(super) target: Target,
}

impl PasskeyChoice {
    /// True for the "new login" option of a create.
    pub fn new_login(&self) -> bool {
        matches!(self.target, Target::NewLogin)
    }
}

pub(super) enum Target {
    NewLogin,
    Login(Uuid),
    Credential {
        cipher_id: Uuid,
        credential_id: Vec<u8>,
    },
}

/// A passkey request the core holds until unlock or a pick.
pub(super) struct Pending {
    pub(super) id: RequestId,
    pub(super) request: PasskeyRequest,
    /// Epoch ms; a [`Tick`](super::Input::Tick) at or past it answers
    /// [`PasskeyError::NotAllowed`].
    pub(super) deadline: u64,
    /// Empty while locked, and cleared at lock.
    pub(super) choices: Vec<PasskeyChoice>,
}

/// How long (ms) a passkey request waits for unlock or a pick.
pub(super) const WAIT: u64 = 120 * 1000;

/// The options for `request`, or [`PasskeyError::NotAllowed`] when a create
/// hits an excluded credential or no stored passkey fits an assertion. A
/// create always ends with the "new login" option.
pub(super) fn choices(
    request: &PasskeyRequest,
    ciphers: &[CipherResponse],
    keys: &VaultKeys,
) -> Result<Vec<PasskeyChoice>, PasskeyError> {
    let mut choices = Vec::new();
    for (cipher_id, view) in logins(ciphers, keys) {
        let CipherContent::Login(login) = &view.content else {
            continue;
        };
        let user_name = login.username.clone().unwrap_or_default();
        match request {
            PasskeyRequest::MakeCredential { rp_id, exclude, .. } => {
                let excluded = login.fido2_credentials.iter().any(|credential| {
                    credential.rp_id.as_str() == rp_id
                        && credential_id_bytes(&credential.credential_id)
                            .is_some_and(|id| exclude.contains(&id))
                });
                if excluded {
                    return Err(PasskeyError::NotAllowed);
                }
                let matches = login.uris.iter().any(|uri| {
                    uri.uri
                        .as_deref()
                        .and_then(|uri| uri_host(uri))
                        .is_some_and(|host| host_matches(&host, rp_id))
                });
                if matches {
                    choices.push(PasskeyChoice {
                        name: view.name.clone(),
                        user_name,
                        target: Target::Login(cipher_id),
                    });
                }
            }
            PasskeyRequest::GetAssertion { rp_id, allow, .. } => {
                for credential in &login.fido2_credentials {
                    let Some(id) = credential_id_bytes(&credential.credential_id) else {
                        continue;
                    };
                    let allowed = if allow.is_empty() {
                        credential.discoverable
                    } else {
                        allow.contains(&id)
                    };
                    if credential.rp_id.as_str() == rp_id && allowed {
                        choices.push(PasskeyChoice {
                            name: view.name.clone(),
                            user_name: credential
                                .user_name
                                .clone()
                                .unwrap_or_else(|| user_name.clone()),
                            target: Target::Credential {
                                cipher_id,
                                credential_id: id,
                            },
                        });
                    }
                }
            }
        }
    }
    match request {
        PasskeyRequest::MakeCredential {
            rp_id,
            rp_name,
            user_name,
            ..
        } => choices.push(PasskeyChoice {
            name: Zeroizing::new(rp_name.clone().unwrap_or_else(|| rp_id.clone())),
            user_name: Zeroizing::new(user_name.clone()),
            target: Target::NewLogin,
        }),
        PasskeyRequest::GetAssertion { .. } if choices.is_empty() => {
            return Err(PasskeyError::NotAllowed);
        }
        PasskeyRequest::GetAssertion { .. } => {}
    }
    Ok(choices)
}

/// Signs an assertion with `counter` in the authenticator data.
pub(super) fn assert(
    credential: &Fido2CredentialView,
    client_data_hash: &[u8; 32],
    counter: u32,
) -> PasskeyReply {
    let authenticator_data = authenticator_data(
        &credential.rp_id,
        USER_PRESENT | USER_VERIFIED,
        counter,
        &[],
    );
    let mut message = authenticator_data.clone();
    message.extend_from_slice(client_data_hash);
    PasskeyReply::Asserted {
        credential_id: credential_id_bytes(&credential.credential_id).unwrap_or_default(),
        signature: credential.key.sign(&message),
        authenticator_data,
        user_handle: credential
            .user_handle
            .as_ref()
            .and_then(|handle| URL_SAFE_NO_PAD.decode(handle.trim_end_matches('=')).ok()),
    }
}

/// A fresh passkey for a MakeCredential, with counter 0, and its reply.
/// [`None`] for a GetAssertion.
pub(super) fn create(
    request: &PasskeyRequest,
    now_ms: u64,
    rng: &mut impl CryptoRngCore,
) -> Option<(Fido2CredentialView, PasskeyReply)> {
    let PasskeyRequest::MakeCredential {
        rp_id,
        rp_name,
        user_handle,
        user_name,
        user_display_name,
        require_resident,
        ..
    } = request
    else {
        return None;
    };
    let key = P256SigningKey::generate(rng);
    let mut random = [0u8; 16];
    rng.fill_bytes(&mut random);
    let credential_id = Builder::from_random_bytes(random).into_uuid();
    let (x, y) = key.public_xy();
    let public_key_cose = cbor::cose_es256_key(&x, &y);
    // Attested credential data: a zero AAGUID, the id length and id, then
    // the COSE key.
    let mut attested = vec![0u8; 16];
    attested.extend_from_slice(&16u16.to_be_bytes());
    attested.extend_from_slice(credential_id.as_bytes());
    attested.extend_from_slice(&public_key_cose);
    let authenticator_data =
        authenticator_data(rp_id, USER_PRESENT | USER_VERIFIED | ATTESTED, 0, &attested);
    let reply = PasskeyReply::Created {
        credential_id: credential_id.as_bytes().to_vec(),
        attestation_object: cbor::attestation_none(&authenticator_data),
        authenticator_data,
        public_key_cose,
    };
    let text = |value: &str| Zeroizing::new(value.to_owned());
    let credential = Fido2CredentialView {
        credential_id: text(&credential_id.to_string()),
        key,
        rp_id: text(rp_id),
        rp_name: rp_name.as_deref().map(text),
        user_handle: Some(Zeroizing::new(URL_SAFE_NO_PAD.encode(user_handle))),
        user_name: Some(text(user_name)),
        user_display_name: Some(text(user_display_name)),
        counter: 0,
        discoverable: *require_resident,
        creation_date: iso_date(now_ms),
    };
    Some((credential, reply))
}

/// A new personal login named after the relying party, for a passkey whose
/// user picked "new login".
pub(super) fn new_login(request: &PasskeyRequest) -> CipherView {
    let (rp_id, rp_name, user_name) = match request {
        PasskeyRequest::MakeCredential {
            rp_id,
            rp_name,
            user_name,
            ..
        } => (rp_id, rp_name.as_ref(), Some(user_name)),
        PasskeyRequest::GetAssertion { rp_id, .. } => (rp_id, None, None),
    };
    CipherView {
        id: None,
        organization_id: None,
        collection_ids: Vec::new(),
        kind: CipherKind::Login,
        name: Zeroizing::new(rp_name.unwrap_or(rp_id).clone()),
        notes: None,
        folder_id: None,
        favorite: false,
        reprompt: false,
        content: CipherContent::Login(LoginView {
            username: user_name.map(|name| Zeroizing::new(name.clone())),
            uris: rp_uri(rp_id)
                .map(|uri| UriView {
                    uri: Some(Zeroizing::new(uri.into())),
                    match_rule: None,
                })
                .into_iter()
                .collect(),
            ..LoginView::default()
        }),
        fields: Vec::new(),
        password_history: Vec::new(),
    }
}

/// Every passkey in the live (not trashed) logins of `ciphers`. A login
/// that does not decrypt is left out.
pub(super) fn catalog(ciphers: &[CipherResponse], keys: &VaultKeys) -> Vec<PasskeyCatalogEntry> {
    let holders: Vec<CipherResponse> = ciphers
        .iter()
        .filter(|cipher| {
            cipher
                .login
                .as_ref()
                .is_some_and(|login| !login.fido2_credentials.is_empty())
        })
        .cloned()
        .collect();
    let mut entries = Vec::new();
    for (_, view) in logins(&holders, keys) {
        let CipherContent::Login(login) = view.content else {
            continue;
        };
        for credential in &login.fido2_credentials {
            let Some(credential_id) = credential_id_bytes(&credential.credential_id) else {
                continue;
            };
            let text = |value: &Option<SecretString>| value.as_deref().cloned().unwrap_or_default();
            entries.push(PasskeyCatalogEntry {
                credential_id,
                rp_id: credential.rp_id.to_string(),
                user_name: text(&credential.user_name),
                user_display_name: text(&credential.user_display_name),
            });
        }
    }
    entries
}

/// The raw credential id of a stored passkey: the 16 bytes of a GUID, or
/// the bytes after a `b64.` prefix, as Bitwarden stores ids that are not
/// GUIDs.
pub(super) fn credential_id_bytes(text: &str) -> Option<Vec<u8>> {
    match text.strip_prefix("b64.") {
        Some(encoded) => URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')).ok(),
        None => Uuid::parse_str(text).ok().map(|id| id.as_bytes().to_vec()),
    }
}

const USER_PRESENT: u8 = 0x01;
const USER_VERIFIED: u8 = 0x04;
const ATTESTED: u8 = 0x40;

/// SHA-256 of `rp_id`, the flags, the big-endian counter, then `attested`.
fn authenticator_data(rp_id: &str, flags: u8, counter: u32, attested: &[u8]) -> Vec<u8> {
    let mut data = Sha256::digest(rp_id.as_bytes()).to_vec();
    data.push(flags);
    data.extend_from_slice(&counter.to_be_bytes());
    data.extend_from_slice(attested);
    data
}

/// The live logins of `ciphers` that decrypt, with their ids.
fn logins<'a>(
    ciphers: &'a [CipherResponse],
    keys: &'a VaultKeys,
) -> impl Iterator<Item = (Uuid, CipherView)> + 'a {
    ciphers
        .iter()
        .filter(|cipher| {
            CipherKind::from(cipher.kind) == CipherKind::Login && cipher.deleted_date.is_none()
        })
        .filter_map(|cipher| Some((cipher.id, CipherView::decrypt(cipher, keys).ok()?)))
}

/// The host of a login URI, lowercased. A URI with no scheme, such as
/// `example.com/login`, is read as `https`, as Bitwarden's clients do.
fn uri_host(uri: &str) -> Option<String> {
    let url = match Url::parse(uri) {
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            let base = Url::parse("https://base.invalid/").expect("a fixed, valid URL");
            // `//host/path` is a scheme-relative reference, so it keeps
            // the base's scheme and replaces the rest.
            base.join(&["//", uri].concat()).ok()?
        }
        parsed => parsed.ok()?,
    };
    url.host_str().map(str::to_owned)
}

/// `https://{rp_id}/`, the URI a new login gets for a passkey. `None` when
/// `rp_id` is not a valid host.
fn rp_uri(rp_id: &str) -> Option<Url> {
    let mut url = Url::parse("https://base.invalid/").expect("a fixed, valid URL");
    url.set_host(Some(rp_id)).ok()?;
    Some(url)
}

/// True when `host` is `rp_id` or one of its subdomains.
fn host_matches(host: &str, rp_id: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let rp_id = rp_id.to_ascii_lowercase();
    host == rp_id || host.ends_with(&format!(".{rp_id}"))
}

/// `now_ms` as `2026-11-20T17:16:20.743Z`.
pub(super) fn iso_date(now_ms: u64) -> String {
    let seconds = now_ms / 1000;
    let days = (seconds / 86_400) as i64;
    let second_of_day = seconds % 86_400;
    // Howard Hinnant's `civil_from_days`.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        second_of_day / 3600,
        second_of_day / 60 % 60,
        second_of_day % 60,
        now_ms % 1000
    )
}

/// Holds `pending` in `slot`, refusing the request it replaces.
pub(super) fn hold_passkey(
    slot: &mut Option<Box<Pending>>,
    pending: Pending,
    effects: &mut Vec<Effect>,
) {
    if let Some(older) = slot.replace(Box::new(pending)) {
        refuse_passkey(older.id, effects);
    }
}

pub(super) fn refuse_passkey(id: RequestId, effects: &mut Vec<Effect>) {
    effects.push(Effect::PasskeyReply {
        id,
        reply: Err(PasskeyError::NotAllowed),
    });
}

/// Answers `pending` at once when it is an assertion that only one passkey
/// fits. Otherwise holds it with its choices for the user to pick, or
/// refuses it when nothing fits.
pub(super) fn serve_passkey(
    config: &Config,
    requests: &mut Table,
    unlocked: &mut Unlocked,
    now_ms: u64,
    rng: &mut impl CryptoRngCore,
    mut pending: Pending,
    effects: &mut Vec<Effect>,
) {
    let choices = match &unlocked.account.cache {
        None => Err(PasskeyError::NotAllowed),
        Some(cache) => choices(&pending.request, &cache.ciphers, &unlocked.keys),
    };
    match choices {
        Err(_) => refuse_passkey(pending.id, effects),
        Ok(mut choices)
            if choices.len() == 1
                && matches!(pending.request, PasskeyRequest::GetAssertion { .. }) =>
        {
            let target = choices.remove(0).target;
            answer_passkey(
                config,
                requests,
                unlocked,
                now_ms,
                rng,
                pending.id,
                &pending.request,
                target,
                effects,
            );
        }
        Ok(choices) => {
            pending.choices = choices;
            hold_passkey(&mut unlocked.account.passkey, pending, effects);
            effects.push(Effect::Rerender);
        }
    }
}

/// For an assertion: signs with the chosen passkey, replies at once and
/// queues the counter write. For a create: adds a new passkey to the chosen
/// login, or to a new login, and saves it; the reply follows once the
/// server stores it. Refuses request `id` when the chosen item is gone or
/// does not decrypt.
#[allow(clippy::too_many_arguments)]
pub(super) fn answer_passkey(
    config: &Config,
    requests: &mut Table,
    unlocked: &mut Unlocked,
    now_ms: u64,
    rng: &mut impl CryptoRngCore,
    id: RequestId,
    request: &PasskeyRequest,
    target: Target,
    effects: &mut Vec<Effect>,
) {
    let base = |cipher_id: Uuid| {
        unlocked
            .account
            .cache
            .as_ref()?
            .ciphers
            .iter()
            .find(|cipher| cipher.id == cipher_id)
            .cloned()
    };
    let call = match (target, request) {
        (
            Target::Credential {
                cipher_id,
                credential_id,
            },
            PasskeyRequest::GetAssertion {
                client_data_hash, ..
            },
        ) => {
            let view =
                base(cipher_id).and_then(|base| CipherView::decrypt(&base, &unlocked.keys).ok());
            let credential = view.as_ref().and_then(|view| match &view.content {
                CipherContent::Login(login) => login.fido2_credentials.iter().find(|credential| {
                    credential_id_bytes(&credential.credential_id).as_ref() == Some(&credential_id)
                }),
                _ => None,
            });
            let Some(credential) = credential else {
                return refuse_passkey(id, effects);
            };
            let signed = &mut unlocked.account.signed;
            let index = match signed
                .iter()
                .position(|(signed_id, _)| *signed_id == credential_id)
            {
                Some(index) => index,
                None => {
                    signed.push((credential_id, 0));
                    signed.len() - 1
                }
            };
            let counter = credential.counter.max(signed[index].1).saturating_add(1);
            signed[index].1 = counter;
            effects.push(Effect::PasskeyReply {
                id,
                reply: Ok(assert(credential, client_data_hash, counter)),
            });
            let bump = CounterBump {
                cipher_id,
                credential_id: credential.credential_id.clone(),
                counter,
            };
            return counter_write(config, requests, unlocked, now_ms, rng, bump, effects);
        }
        (Target::NewLogin, PasskeyRequest::MakeCredential { .. }) => {
            let Some((credential, reply)) = create(request, now_ms, rng) else {
                return refuse_passkey(id, effects);
            };
            let mut view = new_login(request);
            if let CipherContent::Login(login) = &mut view.content {
                login.fido2_credentials.push(credential);
            }
            let edits = view.encrypt(unlocked.keys.user(), &Map::new(), rng);
            ApiCall {
                method: Method::Post,
                path: vec!["ciphers".into()],
                query: &[],
                body: Body::Json(create_body(edits, unlocked.account.user_id, None)),
                kind: ApiKind::Write(Write {
                    cipher_id: None,
                    action: WriteAction::Create,
                    view: None,
                    passkey: Some(PasskeyWrite::Created {
                        id,
                        reply: Box::new(reply),
                    }),
                }),
                retried: false,
            }
        }
        (Target::Login(cipher_id), PasskeyRequest::MakeCredential { .. }) => {
            let opened = base(cipher_id).and_then(|base| {
                let key = unlocked.keys.cipher_key(&base).ok()?;
                let view = CipherView::decrypt(&base, &unlocked.keys).ok()?;
                Some((base, key, view))
            });
            let (Some((base, key, mut view)), Some((credential, reply))) =
                (opened, create(request, now_ms, rng))
            else {
                return refuse_passkey(id, effects);
            };
            let CipherContent::Login(login) = &mut view.content else {
                return refuse_passkey(id, effects);
            };
            login.fido2_credentials.push(credential);
            let edits = view.encrypt(&key, &base.raw, rng);
            ApiCall {
                method: Method::Put,
                path: vec!["ciphers".into(), cipher_id.to_string()],
                query: &[],
                body: Body::Json(base.update_body(edits, unlocked.account.user_id)),
                kind: ApiKind::Write(Write {
                    cipher_id: Some(cipher_id),
                    action: WriteAction::Update,
                    view: None,
                    passkey: Some(PasskeyWrite::Created {
                        id,
                        reply: Box::new(reply),
                    }),
                }),
                retried: false,
            }
        }
        _ => return refuse_passkey(id, effects),
    };
    send_api(
        config,
        requests,
        &mut unlocked.account,
        now_ms,
        call,
        effects,
    );
}

/// Writes `bump` into its passkey, unless the cached copy already holds an
/// equal or higher counter, or the passkey is gone.
pub(super) fn counter_write(
    config: &Config,
    requests: &mut Table,
    unlocked: &mut Unlocked,
    now_ms: u64,
    rng: &mut impl CryptoRngCore,
    bump: CounterBump,
    effects: &mut Vec<Effect>,
) {
    let base = unlocked.account.cache.as_ref().and_then(|cache| {
        cache
            .ciphers
            .iter()
            .find(|cipher| cipher.id == bump.cipher_id)
    });
    let Some(base) = base else {
        return;
    };
    let (Ok(key), Ok(mut view)) = (
        unlocked.keys.cipher_key(base),
        CipherView::decrypt(base, &unlocked.keys),
    ) else {
        return;
    };
    let CipherContent::Login(login) = &mut view.content else {
        return;
    };
    let credential = login
        .fido2_credentials
        .iter_mut()
        .find(|credential| *credential.credential_id == *bump.credential_id);
    let Some(credential) = credential.filter(|credential| credential.counter < bump.counter) else {
        return;
    };
    credential.counter = bump.counter;
    let edits = view.encrypt(&key, &base.raw, rng);
    let call = ApiCall {
        method: Method::Put,
        path: vec!["ciphers".into(), bump.cipher_id.to_string()],
        query: &[],
        body: Body::Json(base.update_body(edits, unlocked.account.user_id)),
        kind: ApiKind::Write(Write {
            cipher_id: Some(bump.cipher_id),
            action: WriteAction::Update,
            view: None,
            passkey: Some(PasskeyWrite::Counter(bump)),
        }),
        retried: false,
    };
    send_api(
        config,
        requests,
        &mut unlocked.account,
        now_ms,
        call,
        effects,
    );
}
