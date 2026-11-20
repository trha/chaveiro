//! Tests 5 and 6, plus the identity and write fixtures, for `proto`.

use expect_test::expect;
use proptest::prelude::*;
use serde_json::{Map, Value};
use uuid::Uuid;
use vault_core::proto::{
    self,
    cipher::{CipherResponse, is_out_of_date},
    identity::{ErrorResponse, PreloginResponse, TokenResponse},
    sync::SyncResponse,
};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Test 5: any mix of key case parses to the same sync, and each
    /// cipher's `raw` keeps the case it was sent with.
    #[test]
    fn sync_parse_ignores_key_case(seed in any::<u64>()) {
        let body = fixture_body("sync.json");
        let mut state = seed | 1;
        let recased = recase_keys(&body, &mut state);
        let expected = SyncResponse::from_json(body).unwrap();
        let actual = SyncResponse::from_json(recased.clone()).unwrap();

        prop_assert_eq!(&actual.profile, &expected.profile);
        prop_assert_eq!(&actual.folders, &expected.folders);
        prop_assert_eq!(&actual.collections, &expected.collections);
        prop_assert_eq!(actual.ciphers.len(), expected.ciphers.len());
        let sent_ciphers = recased
            .as_object()
            .unwrap()
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("ciphers"))
            .and_then(|(_, value)| value.as_array())
            .unwrap();
        for ((actual, expected), sent) in actual.ciphers.iter().zip(&expected.ciphers).zip(sent_ciphers) {
            prop_assert_eq!(&without_raw(actual), &without_raw(expected));
            prop_assert_eq!(&Value::Object(actual.raw.clone()), sent);
        }
    }

    /// Test 6: with no edits, the update body holds every key and value of
    /// `raw`, extra keys included.
    #[test]
    fn update_body_keeps_raw(
        extras in proptest::collection::btree_map("x[a-zA-Z0-9]{0,12}", json_leaf(), 0..8),
        user in any::<u128>(),
    ) {
        let mut object = first_fixture_cipher();
        object.extend(extras);
        let cipher = CipherResponse::from_object(object.clone()).unwrap();
        let encrypted_for = Uuid::from_u128(user);

        let body = cipher.update_body(Map::new(), encrypted_for);
        let body = body.as_object().unwrap();
        for (key, value) in &object {
            prop_assert_eq!(body.get(key), Some(value), "key {}", key);
        }
        prop_assert_eq!(&body["lastKnownRevisionDate"], &object["revisionDate"]);
        prop_assert_eq!(&body["encryptedFor"], &Value::String(encrypted_for.to_string()));
    }
}

#[test]
fn update_body_edit_replaces_key_in_any_case() {
    let mut object = first_fixture_cipher();
    let name = object.remove("name").unwrap();
    object.insert("Name".to_owned(), name);
    let cipher = CipherResponse::from_object(object).unwrap();
    let mut edits = Map::new();
    edits.insert("name".to_owned(), Value::String("2.new".to_owned()));
    let body = cipher.update_body(edits, Uuid::nil());
    let names: Vec<&String> = body
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| key.eq_ignore_ascii_case("name"))
        .collect();
    assert_eq!(names, ["name"]);
    assert_eq!(body["name"], "2.new");
}

#[test]
fn prelogin_gives_pbkdf2() {
    let prelogin: PreloginResponse = parse_fixture("prelogin.json");
    expect![[r#"
        Some(
            Pbkdf2 {
                iterations: 600000,
            },
        )
    "#]]
    .assert_debug_eq(&prelogin.kdf());
}

#[test]
fn token_success_has_keys_and_tokens() {
    let token: TokenResponse = parse_fixture("token_success.json");
    expect![[r#"
        access_token: ey...
        refresh_token: true
        expires_in: 7200
        key: true
        private_key: true
    "#]]
    .assert_eq(&format!(
        "access_token: {}...\nrefresh_token: {}\nexpires_in: {}\nkey: {}\nprivate_key: {}\n",
        &token.access_token[..2],
        token.refresh_token.is_some(),
        token.expires_in,
        token.key.is_some(),
        token.private_key.is_some(),
    ));
}

#[test]
fn token_two_factor_lists_authenticator() {
    let error: ErrorResponse = parse_fixture("token_two_factor_required.json");
    expect![[r#"
        SecondFactor {
            providers: [
                0,
            ],
        }
    "#]]
    .assert_debug_eq(&error.token_failure());
}

#[test]
fn token_new_device_asks_for_otp() {
    let error: ErrorResponse = parse_fixture("token_new_device_otp_required.json");
    expect![[r#"
        NewDeviceOtp
    "#]]
    .assert_debug_eq(&error.token_failure());
}

#[test]
fn token_wrong_password_vaultwarden() {
    let error: ErrorResponse = parse_fixture("token_wrong_password_vaultwarden.json");
    expect![[r#"
        Rejected {
            message: "Username or password is incorrect. Try again",
        }
    "#]]
    .assert_debug_eq(&error.token_failure());
}

#[test]
fn token_wrong_password_bitwarden() {
    let error: ErrorResponse = parse_fixture("token_wrong_password_bitwarden.json");
    expect![[r#"
        Rejected {
            message: "Username or password is incorrect. Try again.",
        }
    "#]]
    .assert_debug_eq(&error.token_failure());
}

#[test]
fn cipher_put_out_of_date_detected() {
    let (status, body) = fixture("cipher_put_out_of_date.json");
    let error: ErrorResponse = proto::from_value(body).unwrap();
    expect![[r#"
        (
            400,
            ErrorResponse {
                error: Some(
                    "",
                ),
                error_description: Some(
                    "",
                ),
                message: Some(
                    "The client copy of this cipher is out of date. Resync the client and try again.",
                ),
                error_model: Some(
                    ErrorModel {
                        message: Some(
                            "The client copy of this cipher is out of date. Resync the client and try again.",
                        ),
                    },
                ),
                two_factor_providers: None,
            },
        )
    "#]].assert_debug_eq(&(status, &error));
    assert!(is_out_of_date(status, &error));
}

#[test]
fn cipher_write_responses_parse() {
    for name in ["cipher_post_login.json", "cipher_put_success.json"] {
        let (_, body) = fixture(name);
        let bytes = serde_json::to_vec(&body).unwrap();
        let cipher = CipherResponse::from_slice(&bytes).unwrap();
        assert_eq!(Value::Object(cipher.raw), body, "{name}");
    }
}

/// A copy of `value` with each object key's ASCII letters flipped to upper
/// or lower case by a xorshift stream from `state`.
fn recase_keys(value: &Value, state: &mut u64) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let key: String = key
                        .chars()
                        .map(|letter| {
                            *state ^= *state << 13;
                            *state ^= *state >> 7;
                            *state ^= *state << 17;
                            if *state & 1 == 1 {
                                letter.to_ascii_uppercase()
                            } else {
                                letter.to_ascii_lowercase()
                            }
                        })
                        .collect();
                    (key, recase_keys(value, state))
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| recase_keys(item, state)).collect())
        }
        other => other.clone(),
    }
}

fn json_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        ".{0,20}".prop_map(Value::String),
    ]
}

fn without_raw(cipher: &CipherResponse) -> CipherResponse {
    CipherResponse {
        raw: Map::new(),
        ..cipher.clone()
    }
}

fn first_fixture_cipher() -> Map<String, Value> {
    fixture_body("sync.json")["ciphers"][0]
        .as_object()
        .unwrap()
        .clone()
}

fn parse_fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
    proto::from_value(fixture_body(name)).unwrap()
}

fn fixture_body(name: &str) -> Value {
    fixture(name).1
}

/// A fixture's HTTP status and body.
fn fixture(name: &str) -> (u16, Value) {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let mut file: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let status = file["status"].as_u64().unwrap().try_into().unwrap();
    (status, file["body"].take())
}
