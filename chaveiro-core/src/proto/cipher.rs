//! Cipher and folder responses, and the bodies of cipher and folder writes.

use super::{enc_option, enc_string, from_value, identity::ErrorResponse, null_default};
use crate::crypto::EncString;
use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;

/// One cipher as the server sends it.
///
/// The typed fields are read from [`raw`](Self::raw) with keys lowercased,
/// so they always agree with it. A write starts from `raw`, so keys this
/// client does not model (`fido2Credentials` it does not edit,
/// `archivedDate`, a new cipher type) go back to the server unchanged.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct CipherResponse {
    pub id: Uuid,
    #[serde(rename = "organizationid", default)]
    pub organization_id: Option<Uuid>,
    #[serde(rename = "folderid", default)]
    pub folder_id: Option<Uuid>,
    /// The JSON `type`. Unknown values are kept.
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(rename = "revisiondate")]
    pub revision_date: String,
    #[serde(rename = "deleteddate", default)]
    pub deleted_date: Option<String>,
    /// The cipher's own key, under the user or org key.
    #[serde(default, deserialize_with = "enc_option")]
    pub key: Option<EncString>,
    #[serde(rename = "collectionids", default, deserialize_with = "null_default")]
    pub collection_ids: Vec<Uuid>,
    #[serde(default, deserialize_with = "null_default")]
    pub edit: bool,
    #[serde(rename = "viewpassword", default, deserialize_with = "null_default")]
    pub view_password: bool,
    #[serde(deserialize_with = "enc_string")]
    pub name: EncString,
    #[serde(default, deserialize_with = "enc_option")]
    pub notes: Option<EncString>,
    #[serde(default, deserialize_with = "null_default")]
    pub favorite: bool,
    /// 0 none, 1 ask for the master password.
    #[serde(default, deserialize_with = "null_default")]
    pub reprompt: u8,
    #[serde(default)]
    pub login: Option<LoginData>,
    #[serde(default)]
    pub card: Option<CardData>,
    #[serde(default)]
    pub identity: Option<IdentityData>,
    #[serde(rename = "sshkey", default)]
    pub ssh_key: Option<SshKeyData>,
    #[serde(default, deserialize_with = "null_default")]
    pub fields: Vec<FieldData>,
    #[serde(rename = "passwordhistory", default, deserialize_with = "null_default")]
    pub password_history: Vec<PasswordHistoryData>,
    /// The whole object as received, in its original key case.
    #[serde(skip)]
    pub raw: Map<String, Value>,
}

impl CipherResponse {
    /// Reads a cipher from its JSON object and keeps the object as
    /// [`raw`](Self::raw).
    pub fn from_object(object: Map<String, Value>) -> Result<Self, serde_json::Error> {
        let mut cipher: Self = from_value(Value::Object(object.clone()))?;
        cipher.raw = object;
        Ok(cipher)
    }

    /// Reads a cipher from a write response body (`POST` or `PUT`).
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        match serde_json::from_slice(bytes)? {
            Value::Object(object) => Self::from_object(object),
            _ => Err(serde::de::Error::custom("a cipher must be a JSON object")),
        }
    }

    /// The body of `PUT /api/ciphers/{id}`: [`raw`](Self::raw) with each key
    /// of `edits` written over the key of the same name in any case, plus
    /// `lastKnownRevisionDate` from [`revision_date`](Self::revision_date)
    /// and `encryptedFor`.
    ///
    /// `encrypted_for` is the user id (the JWT `sub`). Vaultwarden answers
    /// 422 without it.
    pub fn update_body(&self, edits: Map<String, Value>, encrypted_for: Uuid) -> Value {
        let mut body = self.raw.clone();
        let fixed = [
            (
                "lastKnownRevisionDate".to_owned(),
                Value::String(self.revision_date.clone()),
            ),
            (
                "encryptedFor".to_owned(),
                Value::String(encrypted_for.to_string()),
            ),
        ];
        for (key, value) in edits.into_iter().chain(fixed) {
            set_key(&mut body, key, value);
        }
        Value::Object(body)
    }
}

/// The body of a cipher create. A personal cipher goes to
/// `POST /api/ciphers` as the cipher object itself. An org cipher (with
/// `collection_ids`) goes to `POST /api/ciphers/create` as
/// `{cipher, collectionIds}`. Both carry `encryptedFor`, the user id.
pub fn create_body(
    mut cipher: Map<String, Value>,
    encrypted_for: Uuid,
    collection_ids: Option<&[Uuid]>,
) -> Value {
    set_key(
        &mut cipher,
        "encryptedFor".to_owned(),
        Value::String(encrypted_for.to_string()),
    );
    match collection_ids {
        None => Value::Object(cipher),
        Some(ids) => serde_json::json!({
            "cipher": cipher,
            "collectionIds": ids.iter().map(Uuid::to_string).collect::<Vec<_>>(),
        }),
    }
}

/// The body of `POST /api/folders` and `PUT /api/folders/{id}`.
pub fn folder_body(name: &EncString) -> Value {
    serde_json::json!({ "name": name.to_string() })
}

/// True when a failed cipher write means our copy is older than the
/// server's: a 400 whose message says "out of date".
pub fn is_out_of_date(status: u16, error: &ErrorResponse) -> bool {
    status == 400
        && error
            .message()
            .is_some_and(|message| message.to_lowercase().contains("out of date"))
}

/// Writes `value` under `key`, first removing every key that differs from it
/// only in case, so the server never sees two spellings of one key.
fn set_key(object: &mut Map<String, Value>, key: String, value: Value) {
    object.retain(|existing, _| !existing.eq_ignore_ascii_case(&key));
    object.insert(key, value);
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct FolderResponse {
    pub id: Uuid,
    #[serde(deserialize_with = "enc_string")]
    pub name: EncString,
    #[serde(rename = "revisiondate", default)]
    pub revision_date: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct LoginData {
    #[serde(default, deserialize_with = "enc_option")]
    pub username: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub password: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub totp: Option<EncString>,
    #[serde(rename = "passwordrevisiondate", default)]
    pub password_revision_date: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub uris: Vec<UriData>,
    #[serde(
        rename = "fido2credentials",
        default,
        deserialize_with = "null_default"
    )]
    pub fido2_credentials: Vec<Fido2CredentialData>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct UriData {
    #[serde(default, deserialize_with = "enc_option")]
    pub uri: Option<EncString>,
    /// The URI match rule; [`None`] means the account default.
    #[serde(rename = "match", default)]
    pub match_rule: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Fido2CredentialData {
    #[serde(rename = "credentialid", deserialize_with = "enc_string")]
    pub credential_id: EncString,
    #[serde(rename = "keytype", deserialize_with = "enc_string")]
    pub key_type: EncString,
    #[serde(rename = "keyalgorithm", deserialize_with = "enc_string")]
    pub key_algorithm: EncString,
    #[serde(rename = "keycurve", deserialize_with = "enc_string")]
    pub key_curve: EncString,
    #[serde(rename = "keyvalue", deserialize_with = "enc_string")]
    pub key_value: EncString,
    #[serde(rename = "rpid", deserialize_with = "enc_string")]
    pub rp_id: EncString,
    #[serde(rename = "rpname", default, deserialize_with = "enc_option")]
    pub rp_name: Option<EncString>,
    #[serde(rename = "userhandle", default, deserialize_with = "enc_option")]
    pub user_handle: Option<EncString>,
    #[serde(rename = "username", default, deserialize_with = "enc_option")]
    pub user_name: Option<EncString>,
    #[serde(rename = "userdisplayname", default, deserialize_with = "enc_option")]
    pub user_display_name: Option<EncString>,
    #[serde(deserialize_with = "enc_string")]
    pub counter: EncString,
    #[serde(deserialize_with = "enc_string")]
    pub discoverable: EncString,
    #[serde(rename = "creationdate")]
    pub creation_date: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct CardData {
    #[serde(rename = "cardholdername", default, deserialize_with = "enc_option")]
    pub cardholder_name: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub brand: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub number: Option<EncString>,
    #[serde(rename = "expmonth", default, deserialize_with = "enc_option")]
    pub exp_month: Option<EncString>,
    #[serde(rename = "expyear", default, deserialize_with = "enc_option")]
    pub exp_year: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub code: Option<EncString>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct IdentityData {
    #[serde(default, deserialize_with = "enc_option")]
    pub title: Option<EncString>,
    #[serde(rename = "firstname", default, deserialize_with = "enc_option")]
    pub first_name: Option<EncString>,
    #[serde(rename = "middlename", default, deserialize_with = "enc_option")]
    pub middle_name: Option<EncString>,
    #[serde(rename = "lastname", default, deserialize_with = "enc_option")]
    pub last_name: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub address1: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub address2: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub address3: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub city: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub state: Option<EncString>,
    #[serde(rename = "postalcode", default, deserialize_with = "enc_option")]
    pub postal_code: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub country: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub company: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub email: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub phone: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub ssn: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub username: Option<EncString>,
    #[serde(rename = "passportnumber", default, deserialize_with = "enc_option")]
    pub passport_number: Option<EncString>,
    #[serde(rename = "licensenumber", default, deserialize_with = "enc_option")]
    pub license_number: Option<EncString>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct SshKeyData {
    #[serde(rename = "privatekey", default, deserialize_with = "enc_option")]
    pub private_key: Option<EncString>,
    #[serde(rename = "publickey", default, deserialize_with = "enc_option")]
    pub public_key: Option<EncString>,
    #[serde(rename = "keyfingerprint", default, deserialize_with = "enc_option")]
    pub key_fingerprint: Option<EncString>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct FieldData {
    /// 0 text, 1 hidden, 2 boolean, 3 linked.
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default, deserialize_with = "enc_option")]
    pub name: Option<EncString>,
    #[serde(default, deserialize_with = "enc_option")]
    pub value: Option<EncString>,
    /// For a linked field, the id of the cipher field it shows.
    #[serde(rename = "linkedid", default)]
    pub linked_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct PasswordHistoryData {
    #[serde(deserialize_with = "enc_string")]
    pub password: EncString,
    #[serde(rename = "lastuseddate")]
    pub last_used_date: String,
}
