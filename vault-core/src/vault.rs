//! Decrypted vault data: the list index built at unlock, and one opened item.
//!
//! The decrypt path is master key, then user key, then the RSA private key,
//! then org keys, then the cipher's own key if it has one, then the fields.

use crate::{
    crypto::{
        DecryptError, EncString, MasterKey, P256SigningKey, PrivateKey, SecretString, SymmetricKey,
    },
    proto::{
        cipher::{CipherResponse, Fido2CredentialData, FolderResponse},
        sync::Profile,
    },
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rand_core::CryptoRngCore;
use serde_json::{Map, Value};
use std::{borrow::Cow, cmp::Ordering, fmt};
use uuid::Uuid;
use zeroize::Zeroizing;

/// What the list shows: every folder and cipher with only its list fields
/// decrypted.
#[derive(PartialEq, Eq)]
pub struct VaultIndex {
    /// Sorted by name.
    pub folders: Vec<FolderSummary>,
    /// Sorted by name, then id.
    pub ciphers: Vec<CipherSummary>,
    /// One entry per SshKey cipher outside the trash whose public key
    /// decrypts and parses, in the order of [`ciphers`](Self::ciphers).
    pub ssh_keys: Vec<SshKeySummary>,
}

impl VaultIndex {
    /// Decrypts the list fields of every folder and cipher. An item that
    /// fails is kept with [`readable`](CipherSummary::readable) false, so one
    /// odd item never hides the rest.
    pub fn decrypt(
        folders: &[FolderResponse],
        cipher_responses: &[CipherResponse],
        keys: &VaultKeys,
    ) -> Self {
        let mut folders: Vec<FolderSummary> = folders
            .iter()
            .map(|folder| match decrypt_text(&folder.name, &keys.user) {
                Ok(name) => FolderSummary {
                    id: folder.id,
                    name,
                    readable: true,
                },
                Err(_) => FolderSummary {
                    id: folder.id,
                    name: SecretString::default(),
                    readable: false,
                },
            })
            .collect();
        folders.sort_by(|left, right| compare_names(&left.name, &right.name));
        let mut ciphers: Vec<CipherSummary> = cipher_responses
            .iter()
            .map(|cipher| CipherSummary::decrypt(cipher, keys))
            .collect();
        ciphers.sort_by(|left, right| {
            compare_names(&left.name, &right.name).then(left.id.cmp(&right.id))
        });
        let ssh_keys = ciphers
            .iter()
            .filter(|summary| summary.kind == CipherKind::SshKey && !summary.deleted)
            .filter_map(|summary| {
                let cipher = cipher_responses
                    .iter()
                    .find(|cipher| cipher.id == summary.id)?;
                SshKeySummary::decrypt(cipher, keys)
            })
            .collect();
        Self {
            folders,
            ciphers,
            ssh_keys,
        }
    }
}

/// The public half of one SshKey cipher, for the SSH agent's list.
#[derive(PartialEq, Eq)]
pub struct SshKeySummary {
    pub cipher_id: Uuid,
    /// Public key in SSH wire form, decrypted from `sshKey.publicKey`.
    pub public_key: Vec<u8>,
    /// From `sshKey.keyFingerprint`, or the SHA-256 fingerprint of
    /// [`public_key`](Self::public_key) when the cipher has none.
    pub fingerprint: String,
}

impl SshKeySummary {
    /// [`None`] when the public key is missing, does not decrypt, or is not
    /// an OpenSSH public key.
    fn decrypt(cipher: &CipherResponse, keys: &VaultKeys) -> Option<Self> {
        let key = keys.cipher_key(cipher).ok()?;
        let data = cipher.ssh_key.as_ref()?;
        let text = decrypt_text(data.public_key.as_ref()?, &key).ok()?;
        let public_key = ssh_key::PublicKey::from_openssh(text.trim()).ok()?;
        let fingerprint = match decrypt_option(data.key_fingerprint.as_ref(), &key) {
            Ok(Some(fingerprint)) => fingerprint.as_str().to_owned(),
            Ok(None) | Err(_) => public_key.fingerprint(ssh_key::HashAlg::Sha256).to_string(),
        };
        Some(Self {
            cipher_id: cipher.id,
            public_key: public_key.to_bytes().ok()?,
            fingerprint,
        })
    }
}

/// Decrypts `sshKey.privateKey` of `cipher`, the OpenSSH PEM text.
pub fn decrypt_ssh_private_key(
    cipher: &CipherResponse,
    keys: &VaultKeys,
) -> Result<Option<SecretString>, VaultError> {
    let key = keys.cipher_key(cipher)?;
    decrypt_option(
        cipher
            .ssh_key
            .as_ref()
            .and_then(|data| data.private_key.as_ref()),
        &key,
    )
}

/// The keys that open the vault, held while unlocked.
pub struct VaultKeys {
    user: SymmetricKey,
    organizations: Vec<(Uuid, SymmetricKey)>,
}

impl VaultKeys {
    /// Opens the private key in `profile` with `user` and each org key with
    /// the private key. An org key that fails to open is left out, so only
    /// that org's ciphers become unreadable. So does every org key when the
    /// private key fails.
    pub fn new(user: SymmetricKey, profile: &Profile) -> Self {
        let private_key = profile
            .private_key
            .as_ref()
            .and_then(|wrapped| wrapped.decrypt(&user).ok())
            .and_then(|der| PrivateKey::from_pkcs8_der(&der).ok());
        let organizations = match &private_key {
            None => Vec::new(),
            Some(private_key) => profile
                .organizations
                .iter()
                .filter_map(|organization| {
                    let bytes = organization.key.as_ref()?.decrypt_rsa(private_key).ok()?;
                    Some((organization.id, key_from_slice(&bytes).ok()?))
                })
                .collect(),
        };
        Self {
            user,
            organizations,
        }
    }

    /// The key that opens `cipher`'s fields: the org key or the user key,
    /// or the cipher's own key opened with that one.
    pub fn cipher_key(&self, cipher: &CipherResponse) -> Result<Cow<'_, SymmetricKey>, VaultError> {
        let outer = match cipher.organization_id {
            None => &self.user,
            Some(organization_id) => self
                .organizations
                .iter()
                .find(|(id, _)| *id == organization_id)
                .map(|(_, key)| key)
                .ok_or(VaultError::MissingOrganizationKey(organization_id))?,
        };
        match &cipher.key {
            None => Ok(Cow::Borrowed(outer)),
            Some(wrapped) => Ok(Cow::Owned(key_from_slice(&wrapped.decrypt(outer)?)?)),
        }
    }

    /// The user key, which also opens folders.
    pub fn user(&self) -> &SymmetricKey {
        &self.user
    }

    /// The key a new cipher is encrypted with: the org key for
    /// `organization_id`, or the user key for [`None`].
    pub fn outer_key(&self, organization_id: Option<Uuid>) -> Result<&SymmetricKey, VaultError> {
        match organization_id {
            None => Ok(&self.user),
            Some(organization_id) => self
                .organizations
                .iter()
                .find(|(id, _)| *id == organization_id)
                .map(|(_, key)| key)
                .ok_or(VaultError::MissingOrganizationKey(organization_id)),
        }
    }
}

/// Opens the user key (the token `Key` or `profile.key`) with the master
/// key. A wrong password fails with [`VaultError::Decrypt`] of
/// [`DecryptError::Mac`].
pub fn decrypt_user_key(
    wrapped: &EncString,
    master: &MasterKey,
) -> Result<SymmetricKey, VaultError> {
    key_from_slice(&wrapped.decrypt(&master.stretch())?)
}

#[derive(PartialEq, Eq)]
pub struct FolderSummary {
    pub id: Uuid,
    pub name: SecretString,
    /// False when the name did not decrypt; `name` is then empty.
    pub readable: bool,
}

/// One list row.
#[derive(PartialEq, Eq)]
pub struct CipherSummary {
    pub id: Uuid,
    pub kind: CipherKind,
    pub name: SecretString,
    /// The login username, card brand and last digits, identity name or SSH
    /// key fingerprint. Empty when there is none.
    pub subtitle: SecretString,
    pub folder_id: Option<Uuid>,
    pub organization_id: Option<Uuid>,
    pub favorite: bool,
    pub deleted: bool,
    /// False when a key or field failed; `name` and `subtitle` are then
    /// empty.
    pub readable: bool,
}

impl CipherSummary {
    /// Decrypts the list fields of `cipher`.
    pub fn decrypt(cipher: &CipherResponse, keys: &VaultKeys) -> Self {
        let decrypted = keys.cipher_key(cipher).and_then(|key| {
            let name = decrypt_text(&cipher.name, &key)?;
            let subtitle = subtitle(cipher, &key)?;
            Ok((name, subtitle))
        });
        let readable = decrypted.is_ok();
        let (name, subtitle) = decrypted.unwrap_or_default();
        Self {
            id: cipher.id,
            kind: CipherKind::from(cipher.kind),
            name,
            subtitle,
            folder_id: cipher.folder_id,
            organization_id: cipher.organization_id,
            favorite: cipher.favorite,
            deleted: cipher.deleted_date.is_some(),
            readable,
        }
    }
}

fn subtitle(cipher: &CipherResponse, key: &SymmetricKey) -> Result<SecretString, VaultError> {
    let text = match CipherKind::from(cipher.kind) {
        CipherKind::Login => decrypt_option(
            cipher
                .login
                .as_ref()
                .and_then(|login| login.username.as_ref()),
            key,
        )?,
        CipherKind::Card => match &cipher.card {
            None => None,
            Some(card) => {
                let brand = decrypt_option(card.brand.as_ref(), key)?;
                let number = decrypt_option(card.number.as_ref(), key)?;
                let mut text = SecretString::default();
                if let Some(brand) = &brand {
                    text.push_str(brand);
                }
                if let Some(number) = number.as_ref().filter(|number| number.len() >= 4)
                    && let Some(last) = number.get(number.len() - 4..)
                {
                    if !text.is_empty() {
                        text.push_str(", ");
                    }
                    text.push('*');
                    text.push_str(last);
                }
                Some(text)
            }
        },
        CipherKind::Identity => match &cipher.identity {
            None => None,
            Some(identity) => {
                let first = decrypt_option(identity.first_name.as_ref(), key)?;
                let last = decrypt_option(identity.last_name.as_ref(), key)?;
                let mut text = SecretString::default();
                for part in [first, last].iter().flatten() {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(part);
                }
                Some(text)
            }
        },
        CipherKind::SshKey => decrypt_option(
            cipher
                .ssh_key
                .as_ref()
                .and_then(|ssh_key| ssh_key.key_fingerprint.as_ref()),
            key,
        )?,
        CipherKind::SecureNote | CipherKind::Unknown(_) => None,
    };
    Ok(text.unwrap_or_default())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CipherKind {
    Login,
    SecureNote,
    Card,
    Identity,
    SshKey,
    /// A type this client does not model, such as Vaultwarden's 6-8.
    Unknown(u8),
}

impl From<CipherKind> for u8 {
    fn from(kind: CipherKind) -> Self {
        match kind {
            CipherKind::Login => 1,
            CipherKind::SecureNote => 2,
            CipherKind::Card => 3,
            CipherKind::Identity => 4,
            CipherKind::SshKey => 5,
            CipherKind::Unknown(other) => other,
        }
    }
}

impl From<u8> for CipherKind {
    fn from(kind: u8) -> Self {
        match kind {
            1 => Self::Login,
            2 => Self::SecureNote,
            3 => Self::Card,
            4 => Self::Identity,
            5 => Self::SshKey,
            other => Self::Unknown(other),
        }
    }
}

/// One opened item. Dropped (and wiped) when the item closes. A clone holds
/// its own copy of every secret, passkey keys included, and wipes it on drop.
#[derive(Clone)]
pub struct CipherView {
    /// [`None`] while creating.
    pub id: Option<Uuid>,
    /// The org that owns the item; [`None`] for the personal vault. A create
    /// with an org goes to that org's `collection_ids`. An edit keeps the
    /// owner of the server copy.
    pub organization_id: Option<Uuid>,
    /// The org collections that hold the item; empty for a personal item.
    pub collection_ids: Vec<Uuid>,
    pub kind: CipherKind,
    pub name: SecretString,
    pub notes: Option<SecretString>,
    pub folder_id: Option<Uuid>,
    pub favorite: bool,
    pub reprompt: bool,
    pub content: CipherContent,
    pub fields: Vec<CustomField>,
    pub password_history: Vec<PasswordHistoryEntry>,
}

impl CipherView {
    /// Decrypts every field of `cipher`. Any failure fails the whole item.
    pub fn decrypt(cipher: &CipherResponse, keys: &VaultKeys) -> Result<Self, VaultError> {
        let key = keys.cipher_key(cipher)?;
        let key = &*key;
        let kind = CipherKind::from(cipher.kind);
        let content = match kind {
            CipherKind::Login => match &cipher.login {
                None => CipherContent::Login(LoginView::default()),
                Some(login) => CipherContent::Login(LoginView {
                    username: decrypt_option(login.username.as_ref(), key)?,
                    password: decrypt_option(login.password.as_ref(), key)?,
                    totp: decrypt_option(login.totp.as_ref(), key)?,
                    password_revision_date: login.password_revision_date.clone(),
                    uris: login
                        .uris
                        .iter()
                        .map(|uri| {
                            Ok(UriView {
                                uri: decrypt_option(uri.uri.as_ref(), key)?,
                                match_rule: uri.match_rule,
                            })
                        })
                        .collect::<Result<_, VaultError>>()?,
                    fido2_credentials: login
                        .fido2_credentials
                        .iter()
                        .map(|credential| Fido2CredentialView::decrypt(credential, key))
                        .collect::<Result<_, _>>()?,
                }),
            },
            CipherKind::SecureNote => CipherContent::SecureNote,
            CipherKind::Card => {
                let card = cipher.card.as_ref();
                CipherContent::Card(CardView {
                    cardholder_name: decrypt_option(
                        card.and_then(|card| card.cardholder_name.as_ref()),
                        key,
                    )?,
                    brand: decrypt_option(card.and_then(|card| card.brand.as_ref()), key)?,
                    number: decrypt_option(card.and_then(|card| card.number.as_ref()), key)?,
                    exp_month: decrypt_option(card.and_then(|card| card.exp_month.as_ref()), key)?,
                    exp_year: decrypt_option(card.and_then(|card| card.exp_year.as_ref()), key)?,
                    code: decrypt_option(card.and_then(|card| card.code.as_ref()), key)?,
                })
            }
            CipherKind::Identity => {
                let identity = cipher.identity.as_ref();
                let field =
                    |pick: fn(&crate::proto::cipher::IdentityData) -> Option<&EncString>| {
                        decrypt_option(identity.and_then(pick), key)
                    };
                CipherContent::Identity(Box::new(IdentityView {
                    title: field(|data| data.title.as_ref())?,
                    first_name: field(|data| data.first_name.as_ref())?,
                    middle_name: field(|data| data.middle_name.as_ref())?,
                    last_name: field(|data| data.last_name.as_ref())?,
                    address1: field(|data| data.address1.as_ref())?,
                    address2: field(|data| data.address2.as_ref())?,
                    address3: field(|data| data.address3.as_ref())?,
                    city: field(|data| data.city.as_ref())?,
                    state: field(|data| data.state.as_ref())?,
                    postal_code: field(|data| data.postal_code.as_ref())?,
                    country: field(|data| data.country.as_ref())?,
                    company: field(|data| data.company.as_ref())?,
                    email: field(|data| data.email.as_ref())?,
                    phone: field(|data| data.phone.as_ref())?,
                    ssn: field(|data| data.ssn.as_ref())?,
                    username: field(|data| data.username.as_ref())?,
                    passport_number: field(|data| data.passport_number.as_ref())?,
                    license_number: field(|data| data.license_number.as_ref())?,
                }))
            }
            CipherKind::SshKey => {
                let ssh_key = cipher.ssh_key.as_ref();
                CipherContent::SshKey(SshKeyView {
                    private_key: decrypt_option(
                        ssh_key.and_then(|data| data.private_key.as_ref()),
                        key,
                    )?,
                    public_key: decrypt_option(
                        ssh_key.and_then(|data| data.public_key.as_ref()),
                        key,
                    )?,
                    key_fingerprint: decrypt_option(
                        ssh_key.and_then(|data| data.key_fingerprint.as_ref()),
                        key,
                    )?,
                })
            }
            CipherKind::Unknown(_) => CipherContent::Unknown,
        };
        Ok(Self {
            id: Some(cipher.id),
            organization_id: cipher.organization_id,
            collection_ids: cipher.collection_ids.clone(),
            kind,
            name: decrypt_text(&cipher.name, key)?,
            notes: decrypt_option(cipher.notes.as_ref(), key)?,
            folder_id: cipher.folder_id,
            favorite: cipher.favorite,
            reprompt: cipher.reprompt == 1,
            content,
            fields: cipher
                .fields
                .iter()
                .map(|field| {
                    Ok(CustomField {
                        kind: field.kind,
                        name: decrypt_option(field.name.as_ref(), key)?,
                        value: decrypt_option(field.value.as_ref(), key)?,
                        linked_id: field.linked_id,
                    })
                })
                .collect::<Result<_, VaultError>>()?,
            password_history: cipher
                .password_history
                .iter()
                .map(|entry| {
                    Ok(PasswordHistoryEntry {
                        password: decrypt_text(&entry.password, key)?,
                        last_used_date: entry.last_used_date.clone(),
                    })
                })
                .collect::<Result<_, VaultError>>()?,
        })
    }
}

impl CipherView {
    /// How many old passwords [`password_history`](Self::password_history)
    /// keeps, as Bitwarden does.
    pub const PASSWORD_HISTORY_LENGTH: usize = 5;

    /// Records a login password change against `base`, the server copy this
    /// view edits: when `base` had a non-empty password and this view holds
    /// a different one (or none), the old password goes first in
    /// [`password_history`](Self::password_history), dated `now`, the list
    /// is cut to [`PASSWORD_HISTORY_LENGTH`](Self::PASSWORD_HISTORY_LENGTH),
    /// and [`LoginView::password_revision_date`] becomes `now`. Otherwise
    /// nothing changes. `now` is an ISO 8601 date.
    ///
    /// Fails when the old password does not decrypt with `key`, the key
    /// that opens `base`.
    pub fn record_password_change(
        &mut self,
        base: &CipherResponse,
        key: &SymmetricKey,
        now: &str,
    ) -> Result<(), VaultError> {
        let CipherContent::Login(login) = &mut self.content else {
            return Ok(());
        };
        let old = base.login.as_ref().and_then(|data| data.password.as_ref());
        let Some(old) = decrypt_option(old, key)? else {
            return Ok(());
        };
        if old.is_empty() || login.password.as_ref() == Some(&old) {
            return Ok(());
        }
        login.password_revision_date = Some(now.to_owned());
        self.password_history.insert(
            0,
            PasswordHistoryEntry {
                password: old,
                last_used_date: now.to_owned(),
            },
        );
        self.password_history
            .truncate(Self::PASSWORD_HISTORY_LENGTH);
        Ok(())
    }

    /// The JSON keys of a cipher write for this view, every string encrypted
    /// as type 2 under `key`. Pass the result as the `edits` of
    /// [`CipherResponse::update_body`], or as the cipher of
    /// [`create_body`](crate::proto::cipher::create_body).
    ///
    /// `raw` is the cipher as the server sent it, or an empty map for a new
    /// one. Each nested object (`login`, `card`, ...) starts from its copy in
    /// `raw`, so keys this client does not model go back unchanged. The
    /// passkeys are written from [`LoginView::fido2_credentials`].
    pub fn encrypt(
        &self,
        key: &SymmetricKey,
        raw: &Map<String, Value>,
        rng: &mut impl CryptoRngCore,
    ) -> Map<String, Value> {
        let mut encrypt = |text: Option<&SecretString>| match text {
            None => Value::Null,
            Some(text) => Value::String(EncString::encrypt(text.as_bytes(), key, rng).to_string()),
        };
        let mut edits = Map::new();
        edits.insert("type".into(), Value::from(u8::from(self.kind)));
        edits.insert("name".into(), encrypt(Some(&self.name)));
        edits.insert("notes".into(), encrypt(self.notes.as_ref()));
        edits.insert(
            "folderId".into(),
            self.folder_id
                .map_or(Value::Null, |id| Value::String(id.to_string())),
        );
        edits.insert("favorite".into(), Value::Bool(self.favorite));
        edits.insert("reprompt".into(), Value::from(u8::from(self.reprompt)));
        let fields: Vec<Value> = self
            .fields
            .iter()
            .map(|field| {
                serde_json::json!({
                    "type": field.kind,
                    "name": encrypt(field.name.as_ref()),
                    "value": encrypt(field.value.as_ref()),
                    "linkedId": field.linked_id,
                })
            })
            .collect();
        edits.insert("fields".into(), Value::Array(fields));
        let history: Vec<Value> = self
            .password_history
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "password": encrypt(Some(&entry.password)),
                    "lastUsedDate": entry.last_used_date,
                })
            })
            .collect();
        edits.insert("passwordHistory".into(), Value::Array(history));
        let (name, values): (&str, Vec<(&str, Value)>) = match &self.content {
            CipherContent::Login(login) => {
                let uris: Vec<Value> = login
                    .uris
                    .iter()
                    .map(|uri| {
                        serde_json::json!({
                            "uri": encrypt(uri.uri.as_ref()),
                            "match": uri.match_rule,
                        })
                    })
                    .collect();
                let first_uri = uris.first().map_or(Value::Null, |uri| uri["uri"].clone());
                let passkeys: Vec<Value> = login
                    .fido2_credentials
                    .iter()
                    .map(|credential| {
                        let key_value =
                            Zeroizing::new(URL_SAFE_NO_PAD.encode(&*credential.key.to_pkcs8_der()));
                        serde_json::json!({
                            "credentialId": encrypt(Some(&credential.credential_id)),
                            "keyType": encrypt(Some(&secret("public-key"))),
                            "keyAlgorithm": encrypt(Some(&secret("ECDSA"))),
                            "keyCurve": encrypt(Some(&secret("P-256"))),
                            "keyValue": encrypt(Some(&key_value)),
                            "rpId": encrypt(Some(&credential.rp_id)),
                            "rpName": encrypt(credential.rp_name.as_ref()),
                            "userHandle": encrypt(credential.user_handle.as_ref()),
                            "userName": encrypt(credential.user_name.as_ref()),
                            "userDisplayName": encrypt(credential.user_display_name.as_ref()),
                            "counter": encrypt(Some(&secret(&credential.counter.to_string()))),
                            "discoverable": encrypt(Some(&secret(
                                if credential.discoverable { "true" } else { "false" },
                            ))),
                            "creationDate": credential.creation_date,
                        })
                    })
                    .collect();
                (
                    "login",
                    vec![
                        ("username", encrypt(login.username.as_ref())),
                        ("password", encrypt(login.password.as_ref())),
                        ("totp", encrypt(login.totp.as_ref())),
                        (
                            "passwordRevisionDate",
                            login
                                .password_revision_date
                                .clone()
                                .map_or(Value::Null, Value::String),
                        ),
                        ("uri", first_uri),
                        ("uris", Value::Array(uris)),
                        ("fido2Credentials", Value::Array(passkeys)),
                    ],
                )
            }
            CipherContent::SecureNote => ("secureNote", vec![("type", Value::from(0))]),
            CipherContent::Card(card) => (
                "card",
                vec![
                    ("cardholderName", encrypt(card.cardholder_name.as_ref())),
                    ("brand", encrypt(card.brand.as_ref())),
                    ("number", encrypt(card.number.as_ref())),
                    ("expMonth", encrypt(card.exp_month.as_ref())),
                    ("expYear", encrypt(card.exp_year.as_ref())),
                    ("code", encrypt(card.code.as_ref())),
                ],
            ),
            CipherContent::Identity(identity) => (
                "identity",
                vec![
                    ("title", encrypt(identity.title.as_ref())),
                    ("firstName", encrypt(identity.first_name.as_ref())),
                    ("middleName", encrypt(identity.middle_name.as_ref())),
                    ("lastName", encrypt(identity.last_name.as_ref())),
                    ("address1", encrypt(identity.address1.as_ref())),
                    ("address2", encrypt(identity.address2.as_ref())),
                    ("address3", encrypt(identity.address3.as_ref())),
                    ("city", encrypt(identity.city.as_ref())),
                    ("state", encrypt(identity.state.as_ref())),
                    ("postalCode", encrypt(identity.postal_code.as_ref())),
                    ("country", encrypt(identity.country.as_ref())),
                    ("company", encrypt(identity.company.as_ref())),
                    ("email", encrypt(identity.email.as_ref())),
                    ("phone", encrypt(identity.phone.as_ref())),
                    ("ssn", encrypt(identity.ssn.as_ref())),
                    ("username", encrypt(identity.username.as_ref())),
                    ("passportNumber", encrypt(identity.passport_number.as_ref())),
                    ("licenseNumber", encrypt(identity.license_number.as_ref())),
                ],
            ),
            CipherContent::SshKey(ssh_key) => (
                "sshKey",
                vec![
                    ("privateKey", encrypt(ssh_key.private_key.as_ref())),
                    ("publicKey", encrypt(ssh_key.public_key.as_ref())),
                    ("keyFingerprint", encrypt(ssh_key.key_fingerprint.as_ref())),
                ],
            ),
            CipherContent::Unknown => return edits,
        };
        let mut object = raw
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_object())
            .cloned()
            .unwrap_or_default();
        for (field, value) in values {
            object.retain(|existing, _| !existing.eq_ignore_ascii_case(field));
            object.insert(field.to_owned(), value);
        }
        edits.insert(name.to_owned(), Value::Object(object));
        edits
    }
}

/// The part of a [`CipherView`] that depends on its [`CipherKind`]; the
/// variant always matches [`CipherView::kind`].
#[derive(Clone)]
pub enum CipherContent {
    Login(LoginView),
    SecureNote,
    Card(CardView),
    /// Boxed, because it is far larger than the other variants.
    Identity(Box<IdentityView>),
    SshKey(SshKeyView),
    Unknown,
}

#[derive(Clone, Default)]
pub struct LoginView {
    pub username: Option<SecretString>,
    pub password: Option<SecretString>,
    pub totp: Option<SecretString>,
    pub password_revision_date: Option<String>,
    pub uris: Vec<UriView>,
    pub fido2_credentials: Vec<Fido2CredentialView>,
}

#[derive(Clone)]
pub struct UriView {
    pub uri: Option<SecretString>,
    /// The match rule; [`None`] means the account default.
    pub match_rule: Option<u8>,
}

#[derive(Clone)]
pub struct Fido2CredentialView {
    /// GUID text, as Bitwarden stores it.
    pub credential_id: SecretString,
    /// Stored as base64url PKCS#8 in `keyValue`.
    pub key: P256SigningKey,
    pub rp_id: SecretString,
    pub rp_name: Option<SecretString>,
    /// Base64url.
    pub user_handle: Option<SecretString>,
    pub user_name: Option<SecretString>,
    pub user_display_name: Option<SecretString>,
    pub counter: u32,
    pub discoverable: bool,
    pub creation_date: String,
}

impl Fido2CredentialView {
    fn decrypt(data: &Fido2CredentialData, key: &SymmetricKey) -> Result<Self, VaultError> {
        let counter = decrypt_text(&data.counter, key)?;
        let discoverable = decrypt_text(&data.discoverable, key)?;
        let key_value = decrypt_text(&data.key_value, key)?;
        // Bitwarden writes base64url with no padding; accept standard base64
        // and padding too, as older clients wrote.
        let der = URL_SAFE_NO_PAD
            .decode(key_value.trim_end_matches('='))
            .or_else(|_| STANDARD.decode(key_value.as_str()))
            .map(Zeroizing::new)
            .map_err(|_| VaultError::Fido2Field)?;
        Ok(Self {
            credential_id: decrypt_text(&data.credential_id, key)?,
            key: P256SigningKey::from_pkcs8_der(&der).map_err(|_| VaultError::Fido2Field)?,
            rp_id: decrypt_text(&data.rp_id, key)?,
            rp_name: decrypt_option(data.rp_name.as_ref(), key)?,
            user_handle: decrypt_option(data.user_handle.as_ref(), key)?,
            user_name: decrypt_option(data.user_name.as_ref(), key)?,
            user_display_name: decrypt_option(data.user_display_name.as_ref(), key)?,
            counter: counter.parse().map_err(|_| VaultError::Fido2Field)?,
            discoverable: match discoverable.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(VaultError::Fido2Field),
            },
            creation_date: data.creation_date.clone(),
        })
    }
}

#[derive(Clone)]
pub struct CardView {
    pub cardholder_name: Option<SecretString>,
    pub brand: Option<SecretString>,
    pub number: Option<SecretString>,
    pub exp_month: Option<SecretString>,
    pub exp_year: Option<SecretString>,
    pub code: Option<SecretString>,
}

#[derive(Clone)]
pub struct IdentityView {
    pub title: Option<SecretString>,
    pub first_name: Option<SecretString>,
    pub middle_name: Option<SecretString>,
    pub last_name: Option<SecretString>,
    pub address1: Option<SecretString>,
    pub address2: Option<SecretString>,
    pub address3: Option<SecretString>,
    pub city: Option<SecretString>,
    pub state: Option<SecretString>,
    pub postal_code: Option<SecretString>,
    pub country: Option<SecretString>,
    pub company: Option<SecretString>,
    pub email: Option<SecretString>,
    pub phone: Option<SecretString>,
    pub ssn: Option<SecretString>,
    pub username: Option<SecretString>,
    pub passport_number: Option<SecretString>,
    pub license_number: Option<SecretString>,
}

#[derive(Clone)]
pub struct SshKeyView {
    pub private_key: Option<SecretString>,
    pub public_key: Option<SecretString>,
    pub key_fingerprint: Option<SecretString>,
}

#[derive(Clone)]
pub struct CustomField {
    /// 0 text, 1 hidden, 2 boolean, 3 linked.
    pub kind: u8,
    pub name: Option<SecretString>,
    pub value: Option<SecretString>,
    /// For a linked field, the id of the cipher field it shows.
    pub linked_id: Option<u32>,
}

#[derive(Clone)]
pub struct PasswordHistoryEntry {
    pub password: SecretString,
    pub last_used_date: String,
}

/// Why a key or item did not decrypt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultError {
    Decrypt(DecryptError),
    /// A decrypted string is not UTF-8.
    Utf8,
    /// A decrypted key is not 64 bytes.
    KeyLength,
    /// The cipher belongs to an org whose key we do not hold.
    MissingOrganizationKey(Uuid),
    /// A passkey `counter` or `discoverable` is not a number or boolean, or
    /// its `keyValue` is not a P-256 PKCS#8 key.
    Fido2Field,
}

impl From<DecryptError> for VaultError {
    fn from(error: DecryptError) -> Self {
        Self::Decrypt(error)
    }
}

impl fmt::Display for VaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decrypt(error) => write!(formatter, "decrypt failed: {error}"),
            Self::Utf8 => formatter.write_str("decrypted text is not UTF-8"),
            Self::KeyLength => formatter.write_str("decrypted key is not 64 bytes"),
            Self::MissingOrganizationKey(id) => write!(formatter, "no key for organization {id}"),
            Self::Fido2Field => {
                formatter.write_str("bad passkey counter, discoverable flag or key")
            }
        }
    }
}

impl std::error::Error for VaultError {}

fn decrypt_text(encrypted: &EncString, key: &SymmetricKey) -> Result<SecretString, VaultError> {
    let mut bytes = encrypted.decrypt(key)?;
    // Move the buffer out so the string owns the only copy.
    match String::from_utf8(std::mem::take(&mut *bytes)) {
        Ok(text) => Ok(Zeroizing::new(text)),
        Err(error) => {
            drop(Zeroizing::new(error.into_bytes()));
            Err(VaultError::Utf8)
        }
    }
}

fn secret(text: &str) -> SecretString {
    Zeroizing::new(text.to_owned())
}

fn decrypt_option(
    encrypted: Option<&EncString>,
    key: &SymmetricKey,
) -> Result<Option<SecretString>, VaultError> {
    encrypted.map(|value| decrypt_text(value, key)).transpose()
}

fn key_from_slice(bytes: &[u8]) -> Result<SymmetricKey, VaultError> {
    let bytes: &[u8; 64] = bytes.try_into().map_err(|_| VaultError::KeyLength)?;
    Ok(SymmetricKey::from_bytes(bytes))
}

/// Case-insensitive order that allocates nothing, so no copy of a secret
/// name is left behind.
fn compare_names(left: &str, right: &str) -> Ordering {
    left.chars()
        .flat_map(char::to_lowercase)
        .cmp(right.chars().flat_map(char::to_lowercase))
        .then_with(|| left.cmp(right))
}
