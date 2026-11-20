//! Step 4: decrypt the whole sync fixture from the test account password.
//! Plaintexts are the ones `tests/fixtures/record.py` encrypted.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use expect_test::expect;
use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng};
use serde_json::Value;
use std::sync::LazyLock;
use uuid::Uuid;
use vault_core::{
    crypto::{DecryptError, EncString, Kdf, derive_master_key},
    proto::{cipher::CipherResponse, sync::SyncResponse},
    vault::{CipherContent, CipherView, VaultError, VaultIndex, VaultKeys, decrypt_user_key},
};

#[test]
fn index_lists_every_cipher_readable_and_sorted() {
    let index = VaultIndex::decrypt(&SYNC.folders, &SYNC.ciphers, &KEYS);
    let table: String = index
        .ciphers
        .iter()
        .map(|row| {
            let kind = format!("{:?}", row.kind);
            let name = row.name.as_str();
            let subtitle = row.subtitle.as_str();
            format!("{name:<20} {kind:<10} {subtitle:<26} {}\n", row.readable)
        })
        .collect();
    expect![[r#"
        Card                 Card       Visa, *1111                true
        Example login        Login      alice                      true
        Identity             Identity   Alice Doe                  true
        Login with own key   Login      alice                      true
        Org login            Login      alice                      true
        Passkey login        Login      alice                      true
        Secure note          SecureNote                            true
        SSH ECDSA P-256      SshKey     SHA256:5ebcL6pGLO/bna7pdLuCEVZrv0dyZR2CUVwUaf894sY true
        SSH key              SshKey     SHA256:ZmgqKvxbNRTD9mdMtSspaUDeAzl1fzt3iTaGLpNnA/8 true
        SSH RSA              SshKey     SHA256:OsYKc+9lhaCtKcPHVjpGlGoqD1i57PiulTHKK4KQ/EA true
    "#]]
    .assert_eq(&table);
}

#[test]
fn index_folder_decrypts() {
    let index = VaultIndex::decrypt(&SYNC.folders, &SYNC.ciphers, &KEYS);
    let folders: Vec<(&str, bool)> = index
        .folders
        .iter()
        .map(|folder| (folder.name.as_str(), folder.readable))
        .collect();
    assert_eq!(folders, [("Work", true)]);
}

#[test]
fn login_decrypts() {
    let view = open("Example login");
    assert_eq!(view.notes.as_deref().map(String::as_str), Some("a note"));
    assert_eq!(view.folder_id, Some(SYNC.folders[0].id));
    let CipherContent::Login(login) = &view.content else {
        panic!("not a login");
    };
    assert_eq!(text(&login.username), Some("alice"));
    assert_eq!(text(&login.password), Some("hunter2"));
    assert_eq!(text(&login.totp), Some("JBSWY3DPEHPK3PXP"));
    let uris: Vec<(Option<&str>, Option<u8>)> = login
        .uris
        .iter()
        .map(|uri| (text(&uri.uri), uri.match_rule))
        .collect();
    assert_eq!(
        uris,
        [
            (Some("https://example.com"), None),
            (Some("https://example.org/login"), Some(3)),
        ]
    );
    assert!(login.fido2_credentials.is_empty());
}

#[test]
fn login_custom_fields_decrypt() {
    let view = open("Example login");
    type Row<'a> = (u8, Option<&'a str>, Option<&'a str>, Option<u32>);
    let fields: Vec<Row<'_>> = view
        .fields
        .iter()
        .map(|field| {
            (
                field.kind,
                text(&field.name),
                text(&field.value),
                field.linked_id,
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            (0, Some("pin"), Some("1234"), None),
            (1, Some("secret"), Some("hidden"), None),
            (3, Some("linked"), None, Some(100)),
        ]
    );
}

#[test]
fn secure_note_decrypts() {
    let view = open("Secure note");
    assert!(matches!(view.content, CipherContent::SecureNote));
    assert_eq!(text(&view.notes), Some("note body"));
    assert!(view.favorite);
    assert!(view.reprompt);
}

#[test]
fn card_decrypts() {
    let CipherContent::Card(card) = open("Card").content else {
        panic!("not a card");
    };
    assert_eq!(text(&card.cardholder_name), Some("Alice A"));
    assert_eq!(text(&card.brand), Some("Visa"));
    assert_eq!(text(&card.number), Some("4111111111111111"));
    assert_eq!(text(&card.exp_month), Some("12"));
    assert_eq!(text(&card.exp_year), Some("2030"));
    assert_eq!(text(&card.code), Some("123"));
}

#[test]
fn identity_decrypts() {
    let CipherContent::Identity(identity) = open("Identity").content else {
        panic!("not an identity");
    };
    assert_eq!(text(&identity.title), Some("Ms"));
    assert_eq!(text(&identity.first_name), Some("Alice"));
    assert_eq!(text(&identity.last_name), Some("Doe"));
    assert_eq!(text(&identity.email), Some("alice@example.com"));
    assert_eq!(text(&identity.city), Some("Berlin"));
    assert_eq!(text(&identity.country), Some("DE"));
    assert_eq!(text(&identity.middle_name), None);
}

#[test]
fn ssh_key_decrypts() {
    let CipherContent::SshKey(ssh_key) = open("SSH key").content else {
        panic!("not an SSH key");
    };
    let private_key = text(&ssh_key.private_key).unwrap();
    assert!(private_key.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
    assert!(private_key.ends_with("-----END OPENSSH PRIVATE KEY-----\n"));
    assert_eq!(
        text(&ssh_key.public_key),
        Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHRGCdOK0sas0lD2GYJ02q/4b0ho62jqQi+5xCN0ZMJR")
    );
    assert_eq!(
        text(&ssh_key.key_fingerprint),
        Some("SHA256:ZmgqKvxbNRTD9mdMtSspaUDeAzl1fzt3iTaGLpNnA/8")
    );
}

#[test]
fn org_cipher_decrypts_with_org_key() {
    let cipher = cipher_named("Org login");
    assert!(cipher.organization_id.is_some());
    let CipherContent::Login(login) = CipherView::decrypt(cipher, &KEYS).unwrap().content else {
        panic!("not a login");
    };
    assert_eq!(text(&login.password), Some("hunter2"));
}

#[test]
fn org_cipher_without_org_key_is_unreadable() {
    let user_key = decrypt_user_key(SYNC.profile.key.as_ref().unwrap(), &MASTER).unwrap();
    let mut profile = SYNC.profile.clone();
    profile.organizations.clear();
    let keys = VaultKeys::new(user_key, &profile);
    let cipher = cipher_named("Org login");
    assert_eq!(
        CipherView::decrypt(cipher, &keys).err(),
        Some(VaultError::MissingOrganizationKey(
            cipher.organization_id.unwrap()
        ))
    );
    let index = VaultIndex::decrypt(&SYNC.folders, &SYNC.ciphers, &keys);
    let row = index
        .ciphers
        .iter()
        .find(|row| row.id == cipher.id)
        .unwrap();
    assert!(!row.readable);
    assert!(row.name.is_empty());
}

#[test]
fn cipher_with_own_key_decrypts() {
    let cipher = cipher_named("Login with own key");
    assert!(cipher.key.is_some());
    let CipherContent::Login(login) = CipherView::decrypt(cipher, &KEYS).unwrap().content else {
        panic!("not a login");
    };
    assert_eq!(text(&login.username), Some("alice"));
    assert_eq!(text(&login.password), Some("hunter2"));
}

#[test]
fn cipher_own_key_is_required() {
    // Its fields are under its own key, so the user key alone must fail.
    let mut cipher = cipher_named("Login with own key").clone();
    cipher.key = None;
    assert_eq!(
        CipherView::decrypt(&cipher, &KEYS).err(),
        Some(VaultError::Decrypt(DecryptError::Mac))
    );
}

#[test]
fn fido2_credential_decrypts() {
    let CipherContent::Login(login) = open("Passkey login").content else {
        panic!("not a login");
    };
    let [credential] = login.fido2_credentials.as_slice() else {
        panic!("expected one passkey");
    };
    assert!(Uuid::parse_str(&credential.credential_id).is_ok());
    assert_eq!(
        URL_SAFE_NO_PAD.encode(&*credential.key.to_pkcs8_der()),
        FIXTURE_KEY_VALUE
    );
    assert_eq!(credential.rp_id.as_str(), "example.com");
    assert_eq!(text(&credential.rp_name), Some("Example"));
    assert_eq!(text(&credential.user_handle), Some("dXNlcg"));
    assert_eq!(text(&credential.user_name), Some("alice"));
    assert_eq!(text(&credential.user_display_name), Some("Alice"));
    assert_eq!(credential.counter, 0);
    assert!(credential.discoverable);
    assert_eq!(credential.creation_date, "2026-11-20T00:00:00.000Z");
}

// Test 14: `fido2Credentials` round trip.

#[test]
fn fido2_credentials_round_trip() {
    let cipher = cipher_named("Passkey login");
    let view = CipherView::decrypt(cipher, &KEYS).unwrap();
    let key = KEYS.cipher_key(cipher).unwrap();
    let mut rng = ChaCha20Rng::seed_from_u64(14);
    let edits = view.encrypt(&key, &cipher.raw, &mut rng);
    let written = CipherResponse::from_object(
        cipher
            .update_body(edits, Uuid::nil())
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    // Each stored field, as the plaintext under its JSON key.
    let plaintexts = |cipher: &CipherResponse| -> Vec<Vec<(String, Option<String>)>> {
        let login = cipher.raw["login"].as_object().unwrap();
        login["fido2Credentials"]
            .as_array()
            .unwrap()
            .iter()
            .map(|credential| {
                let mut fields: Vec<(String, Option<String>)> = credential
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(name, value)| {
                        let plain = match (name.as_str(), value) {
                            ("creationDate", Value::String(date)) => Some(date.clone()),
                            (_, Value::String(text)) => {
                                let encrypted: EncString = text.parse().unwrap();
                                let bytes = encrypted.decrypt(&key).unwrap();
                                Some(String::from_utf8(bytes.to_vec()).unwrap())
                            }
                            (_, Value::Null) => None,
                            (_, other) => panic!("unexpected {name}: {other}"),
                        };
                        (name.clone(), plain)
                    })
                    .collect();
                fields.sort();
                fields
            })
            .collect()
    };
    let before = plaintexts(cipher);
    assert_eq!(before.len(), 1);
    assert!(
        before[0]
            .iter()
            .any(|(name, plain)| name == "keyValue" && plain.as_deref() == Some(FIXTURE_KEY_VALUE))
    );
    assert_eq!(plaintexts(&written), before);
}

#[test]
fn token_key_matches_profile_key() {
    let (_, body) = fixture("token_success.json");
    let token: vault_core::proto::identity::TokenResponse =
        vault_core::proto::from_value(body).unwrap();
    let from_token = decrypt_user_key(token.key.as_ref().unwrap(), &MASTER).unwrap();
    // Both keys open the same folder name.
    let keys = VaultKeys::new(from_token, &SYNC.profile);
    let index = VaultIndex::decrypt(&SYNC.folders, &[], &keys);
    assert_eq!(index.folders[0].name.as_str(), "Work");
}

#[test]
fn malformed_enc_string_leaves_only_its_item_unreadable() {
    let (_, mut body) = fixture("sync.json");
    let ciphers = body["ciphers"].as_array_mut().unwrap();
    let broken_id: Uuid = ciphers[0]["id"].as_str().unwrap().parse().unwrap();
    ciphers[0]["name"] = Value::from("2.not base64|at all");
    ciphers[0]["notes"] = Value::from("no header");
    let sync = SyncResponse::from_json(body).unwrap();
    let index = VaultIndex::decrypt(&sync.folders, &sync.ciphers, &KEYS);
    assert_eq!(index.ciphers.len(), SYNC.ciphers.len());
    for row in &index.ciphers {
        assert_eq!(row.readable, row.id != broken_id, "row {}", row.id);
    }
    // A write sends the odd string back as received.
    let broken = sync.ciphers.iter().find(|c| c.id == broken_id).unwrap();
    assert_eq!(broken.name.to_string(), "2.not base64|at all");
}

#[test]
fn wrong_password_fails_mac() {
    let master = derive_master_key("wrong", EMAIL, KDF).unwrap();
    assert_eq!(
        decrypt_user_key(SYNC.profile.key.as_ref().unwrap(), &master).err(),
        Some(VaultError::Decrypt(DecryptError::Mac))
    );
}

const EMAIL: &str = "fixture-user@example.com";
const PASSWORD: &str = "fixture-test-password-1";
const KDF: Kdf = Kdf::Pbkdf2 {
    iterations: 600_000,
};

/// The plaintext `keyValue` of the passkey in `sync.json` (see `SOURCES.md`).
const FIXTURE_KEY_VALUE: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgPzagifK8K2Q2B5J-aFfT1VEQQLqsfY_kBl1B21YNfFqhRANCAARGumfqpTsKwcQ7pwuvelYmfl3oBrYn3oxBVNLMkl0sFRjvrj1Sg0403Um5r-9vMD098TNKtVSBHREZ2WEMJjCn";

static SYNC: LazyLock<SyncResponse> = LazyLock::new(|| {
    let (_, body) = fixture("sync.json");
    SyncResponse::from_json(body).unwrap()
});

// 600,000 PBKDF2 rounds are slow in a debug build, so derive once.
static MASTER: LazyLock<vault_core::crypto::MasterKey> =
    LazyLock::new(|| derive_master_key(PASSWORD, EMAIL, KDF).unwrap());

static KEYS: LazyLock<VaultKeys> = LazyLock::new(|| {
    let user_key = decrypt_user_key(SYNC.profile.key.as_ref().unwrap(), &MASTER).unwrap();
    VaultKeys::new(user_key, &SYNC.profile)
});

/// Opens the cipher whose name decrypts to `name`.
fn open(name: &str) -> CipherView {
    CipherView::decrypt(cipher_named(name), &KEYS).unwrap()
}

fn cipher_named(name: &str) -> &'static vault_core::proto::cipher::CipherResponse {
    SYNC.ciphers
        .iter()
        .find(|cipher| {
            CipherView::decrypt(cipher, &KEYS).is_ok_and(|view| view.name.as_str() == name)
        })
        .unwrap_or_else(|| panic!("no cipher named {name}"))
}

fn text(value: &Option<vault_core::crypto::SecretString>) -> Option<&str> {
    value.as_deref().map(String::as_str)
}

fn fixture(name: &str) -> (u16, Value) {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let mut file: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let status = file["status"].as_u64().unwrap().try_into().unwrap();
    (status, file["body"].take())
}
