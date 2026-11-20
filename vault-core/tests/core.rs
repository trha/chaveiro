//! Steps 5 and 6: drive `Core::handle` with the fixtures as HTTP responses
//! and assert the effects. Plan tests 7-13, 15 and 18-20, and the WP-11 gaps.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use expect_test::expect;
use hkdf::Hkdf;
use p256::{
    ecdsa::{Signature, VerifyingKey, signature::Verifier},
    pkcs8::DecodePrivateKey,
};
use proptest::prelude::*;
use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use ssh_key::PublicKey;
use std::{collections::HashMap, sync::LazyLock};
use url::Url;
use uuid::Uuid;
use vault_core::{
    core::{
        AgentRequestId, Body, Challenge, Config, Core, DeviceKeyError, Effect, HttpRequest,
        HttpResponse, Input, LoginError, Method, PasskeyCatalogEntry, PasskeyError, PasskeyReply,
        PasskeyRequest, RequestId, Screen, StoreSlot, StoredAccount, UnlockError, UserAction,
        VaultScreen,
        ssh_agent::{AgentRequest, parse_request},
    },
    crypto::{EncString, Kdf, MasterKey, SecretString, SymmetricKey, derive_master_key},
    proto::{cache::CacheDocument, cipher::CipherResponse, sync::SyncResponse},
    vault::{
        CipherContent, CipherView, PasswordHistoryEntry, VaultIndex, VaultKeys, decrypt_user_key,
    },
};
use zeroize::Zeroizing;

// Test 7: login paths.

#[test]
fn plain_login_unlocks_and_syncs() {
    let mut harness = Harness::logged_out();
    let effects = harness.user(UserAction::Login {
        email: EMAIL.into(),
        password: secret(PASSWORD),
    });
    let (prelogin, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/identity/accounts/prelogin
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        {
          "email": "fixture-user@example.com"
        }
    "#]]
    .assert_eq(&render(request, &[]));
    let effects = harness.answer_fixture(prelogin, "prelogin.json");
    let (derive, password, email, kdf) = one_derive(&effects);
    assert_eq!(
        (password.as_str(), email.as_str(), kdf),
        (PASSWORD, EMAIL, KDF)
    );
    let effects = harness.derived(derive, PASSWORD);
    let (token, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/identity/connect/token
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        client_id=desktop
        deviceIdentifier=00000000-0000-0000-0000-000000001234
        deviceName=test
        deviceType=6
        grant_type=password
        password=<secret>
        scope=api offline_access
        username=fixture-user@example.com
    "#]]
    .assert_eq(&render(request, &["deviceIdentifier"]));
    assert_eq!(form(request)["password"], MASTER.password_hash(PASSWORD));
    let effects = harness.answer_fixture(token, "token_success.json");
    assert_logged_in(&mut harness, &effects);
}

#[test]
fn two_factor_by_authenticator() {
    let mut harness = Harness::logged_out();
    let token = harness.login_until_token();
    harness.answer_fixture(token, "token_two_factor_required.json");
    assert!(matches!(
        harness.core.screen(),
        Screen::SecondFactor { challenge: Challenge::TwoFactor { providers }, error: None, .. }
            if providers == &[0]
    ));
    let effects = harness.user(UserAction::SubmitCode {
        provider: 0,
        code: secret("123456"),
        remember: false,
    });
    let (token, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/identity/connect/token
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        client_id=desktop
        deviceIdentifier=<id>
        deviceName=test
        deviceType=6
        grant_type=password
        password=<secret>
        scope=api offline_access
        twoFactorProvider=0
        twoFactorRemember=0
        twoFactorToken=123456
        username=fixture-user@example.com
    "#]]
    .assert_eq(&render(request, &["twoFactorToken"]));
    assert_eq!(form(request)["password"], MASTER.password_hash(PASSWORD));
    let effects = harness.answer_fixture(token, "token_success.json");
    assert_logged_in(&mut harness, &effects);
}

const REMEMBER_TOKEN: &str = "remember-token-from-server";

/// Logs in with an authenticator code and "remember", and returns the
/// bytes the core stores in [`StoreSlot::TwoFactorToken`].
fn remembered_slot() -> Vec<u8> {
    let mut harness = Harness::logged_out();
    let token = harness.login_until_token();
    harness.answer_fixture(token, "token_two_factor_required.json");
    let effects = harness.user(UserAction::SubmitCode {
        provider: 0,
        code: secret("123456"),
        remember: true,
    });
    let (token, request) = one_http(&effects);
    assert_eq!(form(request)["twoFactorRemember"], "1");
    let mut body = fixture("token_success.json").1;
    body["TwoFactorToken"] = Value::from(REMEMBER_TOKEN);
    let effects = harness.answer(token, 200, &body);
    assert_logged_in(&mut harness, &effects);
    stored_bytes(&effects, StoreSlot::TwoFactorToken)
}

/// Logged out, booted with `slot` in [`StoreSlot::TwoFactorToken`].
fn logged_out_remembering(slot: Vec<u8>) -> Harness {
    let mut harness = Harness::new();
    harness.send(Input::Started {
        stored: StoredAccount {
            two_factor_token: Some(slot),
            ..StoredAccount::default()
        },
    });
    harness
}

#[test]
fn remembered_token_skips_the_second_factor() {
    let mut harness = logged_out_remembering(remembered_slot());
    let (_, form) = harness.login_until_token_form();
    assert_eq!(form["twoFactorProvider"], "5");
    assert_eq!(form["twoFactorToken"], REMEMBER_TOKEN);
    assert_eq!(form["twoFactorRemember"], "0");
}

#[test]
fn a_code_without_remember_stores_no_token() {
    let mut harness = Harness::logged_out();
    let token = harness.login_until_token();
    harness.answer_fixture(token, "token_two_factor_required.json");
    let effects = harness.user(UserAction::SubmitCode {
        provider: 0,
        code: secret("123456"),
        remember: false,
    });
    let (token, _) = one_http(&effects);
    let effects = harness.answer_fixture(token, "token_success.json");
    assert!(!slots(&effects).contains(&StoreSlot::TwoFactorToken));
}

#[test]
fn stale_remembered_token_falls_back_to_the_code() {
    let mut harness = logged_out_remembering(remembered_slot());
    let token = harness.login_until_token();
    let effects = harness.answer_fixture(token, "token_two_factor_required.json");
    assert!(cleared(&effects).contains(&StoreSlot::TwoFactorToken));
    assert!(matches!(
        harness.core.screen(),
        Screen::SecondFactor {
            challenge: Challenge::TwoFactor { .. },
            error: None,
            ..
        }
    ));
    let effects = harness.user(UserAction::SubmitCode {
        provider: 0,
        code: secret("123456"),
        remember: false,
    });
    let (token, request) = one_http(&effects);
    assert_eq!(form(request)["twoFactorProvider"], "0");
    // A second refusal is a wrong code, and asks again.
    harness.answer_fixture(token, "token_two_factor_required.json");
    assert!(matches!(
        harness.core.screen(),
        Screen::SecondFactor {
            error: Some(LoginError::InvalidCode),
            ..
        }
    ));
    // The forgotten token stays forgotten at the next login.
    harness.user(UserAction::Cancel);
    let (_, form) = harness.login_until_token_form();
    assert!(!form.contains_key("twoFactorToken"));
}

#[test]
fn remembered_token_is_only_sent_for_its_email() {
    let mut harness = logged_out_remembering(remembered_slot());
    let effects = harness.user(UserAction::Login {
        email: "someone-else@example.com".into(),
        password: secret(PASSWORD),
    });
    let (prelogin, _) = one_http(&effects);
    let effects = harness.answer_fixture(prelogin, "prelogin.json");
    let (derive, ..) = one_derive(&effects);
    let effects = harness.derived(derive, PASSWORD);
    let (_, request) = one_http(&effects);
    assert!(!form(request).contains_key("twoFactorToken"));
}

#[test]
fn remembered_token_from_another_process_is_used() {
    let mut harness = Harness::logged_out();
    harness.send(Input::StorageChanged {
        slot: StoreSlot::TwoFactorToken,
        bytes: Some(remembered_slot()),
    });
    let (_, form) = harness.login_until_token_form();
    assert_eq!(form["twoFactorProvider"], "5");
}

#[test]
fn logout_keeps_the_remembered_token() {
    let mut harness = logged_out_remembering(remembered_slot());
    let token = harness.login_until_token();
    let effects = harness.answer_fixture(token, "token_success.json");
    assert_logged_in(&mut harness, &effects);
    let effects = harness.user(UserAction::Logout);
    assert!(!cleared(&effects).contains(&StoreSlot::TwoFactorToken));
    let (_, form) = harness.login_until_token_form();
    assert_eq!(form["twoFactorProvider"], "5");
}

#[test]
fn two_factor_by_email() {
    let mut harness = Harness::logged_out();
    let token = harness.login_until_token();
    let mut body = fixture("token_two_factor_required.json").1;
    body["TwoFactorProviders2"] = serde_json::json!({ "1": { "Email": "f***@example.com" } });
    harness.answer(token, 400, &body);
    assert!(matches!(
        harness.core.screen(),
        Screen::SecondFactor { challenge: Challenge::TwoFactor { providers }, .. } if providers == &[1]
    ));
    let effects = harness.user(UserAction::SendEmailCode);
    let (send, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/api/two-factor/send-email-login
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        {
          "email": "fixture-user@example.com",
          "masterPasswordHash": "<secret>"
        }
    "#]]
    .assert_eq(&render(request, &[]));
    assert_eq!(
        json_object(request)["masterPasswordHash"],
        MASTER.password_hash(PASSWORD)
    );
    assert!(http(&harness.answer(send, 200, &Value::Null)).is_empty());
    let effects = harness.user(UserAction::SubmitCode {
        provider: 1,
        code: secret("654321"),
        remember: false,
    });
    let (token, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/identity/connect/token
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        client_id=desktop
        deviceIdentifier=<id>
        deviceName=test
        deviceType=6
        grant_type=password
        password=<secret>
        scope=api offline_access
        twoFactorProvider=1
        twoFactorRemember=0
        twoFactorToken=654321
        username=fixture-user@example.com
    "#]]
    .assert_eq(&render(request, &["twoFactorToken"]));
    let effects = harness.answer_fixture(token, "token_success.json");
    assert_logged_in(&mut harness, &effects);
}

#[test]
fn new_device_otp() {
    let mut harness = Harness::logged_out();
    let token = harness.login_until_token();
    harness.answer_fixture(token, "token_new_device_otp_required.json");
    assert!(matches!(
        harness.core.screen(),
        Screen::SecondFactor {
            challenge: Challenge::NewDevice,
            ..
        }
    ));
    let effects = harness.user(UserAction::SubmitNewDeviceOtp {
        code: secret("99887766"),
    });
    let (token, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/identity/connect/token
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        client_id=desktop
        deviceIdentifier=<id>
        deviceName=test
        deviceType=6
        grant_type=password
        newDeviceOtp=99887766
        password=<secret>
        scope=api offline_access
        username=fixture-user@example.com
    "#]]
    .assert_eq(&render(request, &["newDeviceOtp"]));
    let effects = harness.answer_fixture(token, "token_success.json");
    assert_logged_in(&mut harness, &effects);
}

#[test]
fn wrong_password_vaultwarden() {
    assert_wrong_password("token_wrong_password_vaultwarden.json");
}

#[test]
fn wrong_password_bitwarden() {
    assert_wrong_password("token_wrong_password_bitwarden.json");
}

#[test]
fn api_key_then_unlock() {
    let mut harness = Harness::logged_out();
    let effects = harness.user(UserAction::LoginApiKey {
        email: EMAIL.into(),
        client_id: "user.fixture".into(),
        client_secret: secret("api-secret"),
    });
    let (token, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/identity/connect/token
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        client_id=user.fixture
        client_secret=api-secret
        deviceIdentifier=<id>
        deviceName=test
        deviceType=6
        grant_type=client_credentials
        scope=api
    "#]]
    .assert_eq(&render(request, &["client_secret"]));
    let effects = harness.answer_fixture(token, "token_success.json");
    assert!(matches!(harness.core.screen(), Screen::Locked { .. }));
    assert_eq!(
        slots(&effects),
        [StoreSlot::Account, StoreSlot::RefreshToken]
    );
    let effects = harness.settle_sync(&effects, REVISION);
    assert_eq!(slots(&effects), [StoreSlot::VaultCache]);
    assert!(matches!(harness.core.screen(), Screen::Locked { .. }));
    let effects = harness.user(UserAction::Unlock {
        password: secret(PASSWORD),
    });
    let (derive, _, _, kdf) = one_derive(&effects);
    assert_eq!(kdf, KDF);
    harness.derived(derive, PASSWORD);
    assert_eq!(vault(&harness.core).index.ciphers.len(), SYNC.ciphers.len());
}

// Test 8: stale responses.

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    #[test]
    fn stale_answers_change_nothing(steps in prop::collection::vec(step(), 1..14)) {
        let mut harness = Harness::stored(REVISION);
        let mut pending: Vec<Pending> = Vec::new();
        let mut stale: Vec<Pending> = Vec::new();
        for step in steps {
            let before_vault = matches!(harness.core.screen(), Screen::Vault(_));
            let effects = match step {
                Step::Unlock => harness.user(UserAction::Unlock { password: secret(PASSWORD) }),
                Step::Lock => harness.user(UserAction::Lock),
                Step::Sync => harness.user(UserAction::Sync),
                Step::Answer(index) if !pending.is_empty() => {
                    let item = pending.remove(index % pending.len());
                    let effects = harness.reply(&item, true);
                    stale.push(item);
                    effects
                }
                Step::Stale(index) if !stale.is_empty() => {
                    let item = stale[index % stale.len()].clone();
                    let before = digest(&harness.core);
                    let effects = harness.reply(&item, false);
                    prop_assert!(effects.is_empty(), "a stale answer made effects");
                    prop_assert_eq!(digest(&harness.core), before);
                    Vec::new()
                }
                _ => Vec::new(),
            };
            if before_vault && matches!(step, Step::Lock) {
                let locked = matches!(harness.core.screen(), Screen::Locked { busy: false, .. });
                prop_assert!(locked, "Lock did not lock");
                // Lock drops every request of the unlocked session.
                stale.append(&mut pending);
            }
            pending.extend(effects.iter().filter_map(Pending::from_effect));
        }
        harness.user(UserAction::Lock);
        let unlocked = matches!(harness.core.screen(), Screen::Vault(_));
        prop_assert!(!unlocked, "still unlocked after Lock");
    }
}

// Test 11: device unlock.

#[test]
fn enroll_seals_the_user_key() {
    let mut harness = Harness::unlocked(REVISION);
    let stored = harness.enroll(b"same secret", b"same secret");
    let wrap: Value = serde_json::from_slice(&stored).unwrap();
    let sealed: EncString = wrap["sealed_user_key"].as_str().unwrap().parse().unwrap();
    // Spec: HKDF-SHA256, salt "vault-core device v1", 64 bytes.
    let mut okm = [0u8; 64];
    Hkdf::<Sha256>::new(Some(b"vault-core device v1"), b"same secret")
        .expand(&[], &mut okm)
        .unwrap();
    let opened = sealed
        .decrypt(&SymmetricKey::from_bytes(&okm))
        .expect("the sealed key opens with the spec's key");
    let user_key = SYNC
        .profile
        .key
        .as_ref()
        .unwrap()
        .decrypt(&MASTER.stretch())
        .unwrap();
    assert_eq!(
        opened[..],
        user_key[..],
        "the sealed bytes are the user key"
    );
    assert_eq!(wrap["user_id"], USER_ID.to_string());
    assert!(vault(&harness.core).device_unlock);
}

#[test]
fn enroll_asks_twice_over_one_challenge() {
    let mut harness = Harness::unlocked(REVISION);
    let effects = harness.user(UserAction::EnableDeviceUnlock);
    let (first, challenge, create) = one_device_secret(&effects);
    assert!(create, "the first ask creates the key");
    assert!(vault(&harness.core).device_enrolling);
    let effects = harness.device_secret(first, Ok(b"s"));
    let (_, again, create) = one_device_secret(&effects);
    assert_eq!(again, challenge);
    assert!(!create);
}

#[test]
fn matching_secret_unlocks() {
    let mut harness = Harness::unlocked(REVISION);
    let stored = harness.enroll(b"same secret", b"same secret");
    harness.user(UserAction::Lock);
    assert!(matches!(
        harness.core.screen(),
        Screen::Locked {
            device_unlock: true,
            busy: false,
            ..
        }
    ));
    harness.device_unlock(b"same secret");
    assert_eq!(vault(&harness.core).index.ciphers.len(), SYNC.ciphers.len());
    // After a restart, the stored record alone unlocks.
    let mut restarted = Harness::stored_with(REVISION, Some(stored));
    restarted.device_unlock(b"same secret");
    assert_eq!(
        vault(&restarted.core).index.ciphers.len(),
        SYNC.ciphers.len()
    );
}

#[test]
fn different_secrets_at_enroll_store_nothing() {
    let mut harness = Harness::unlocked(REVISION);
    let effects = harness.user(UserAction::EnableDeviceUnlock);
    let (first, ..) = one_device_secret(&effects);
    let effects = harness.device_secret(first, Ok(b"one"));
    let (second, ..) = one_device_secret(&effects);
    let effects = harness.device_secret(second, Ok(b"two"));
    assert!(slots(&effects).is_empty(), "stored {:?}", slots(&effects));
    let screen = vault(&harness.core);
    assert!(!screen.device_unlock);
    assert_eq!(screen.device_error, Some(&DeviceKeyError::NotDeterministic));
}

#[test]
fn failed_mac_clears_the_record_and_asks_for_the_password() {
    let mut harness = Harness::unlocked(REVISION);
    harness.enroll(b"enrolled", b"enrolled");
    harness.user(UserAction::Lock);
    let effects = harness.user(UserAction::DeviceUnlock);
    let (id, ..) = one_device_secret(&effects);
    let effects = harness.device_secret(id, Ok(b"after a biometric reset"));
    assert_eq!(cleared(&effects), [StoreSlot::DeviceWrap]);
    assert!(http(&effects).is_empty());
    assert!(matches!(
        harness.core.screen(),
        Screen::Locked {
            error: Some(UnlockError::DeviceKeyChanged),
            device_unlock: false,
            ..
        }
    ));
    harness.unlock();
}

#[test]
fn cancelled_prompt_stays_locked_without_error() {
    let mut harness = Harness::unlocked(REVISION);
    harness.enroll(b"s", b"s");
    harness.user(UserAction::Lock);
    let effects = harness.user(UserAction::DeviceUnlock);
    let (id, ..) = one_device_secret(&effects);
    assert!(matches!(
        harness.core.screen(),
        Screen::Locked { busy: true, .. }
    ));
    let effects = harness.device_secret(id, Err(DeviceKeyError::Cancelled));
    assert!(cleared(&effects).is_empty());
    assert!(matches!(
        harness.core.screen(),
        Screen::Locked {
            error: None,
            busy: false,
            device_unlock: true,
            ..
        }
    ));
}

#[test]
fn lock_during_enroll_drops_the_first_secret() {
    let mut harness = Harness::unlocked(REVISION);
    let effects = harness.user(UserAction::EnableDeviceUnlock);
    let (first, ..) = one_device_secret(&effects);
    let effects = harness.device_secret(first, Ok(b"s"));
    let (second, ..) = one_device_secret(&effects);
    harness.user(UserAction::Lock);
    let effects = harness.device_secret(second, Ok(b"s"));
    assert!(effects.is_empty(), "a late answer after lock is stale");
    assert!(matches!(
        harness.core.screen(),
        Screen::Locked {
            device_unlock: false,
            ..
        }
    ));
}

#[test]
fn disable_clears_the_record() {
    let mut harness = Harness::unlocked(REVISION);
    harness.enroll(b"s", b"s");
    let effects = harness.user(UserAction::DisableDeviceUnlock);
    assert_eq!(cleared(&effects), [StoreSlot::DeviceWrap]);
    assert!(!vault(&harness.core).device_unlock);
    harness.user(UserAction::Lock);
    assert!(harness.user(UserAction::DeviceUnlock).is_empty());
}

#[test]
fn record_of_another_user_is_ignored() {
    let mut harness = Harness::unlocked(REVISION);
    let stored = harness.enroll(b"s", b"s");
    let mut wrap: Value = serde_json::from_slice(&stored).unwrap();
    wrap["user_id"] = Value::from(Uuid::from_u128(1).to_string());
    let harness = Harness::stored_with(REVISION, Some(serde_json::to_vec(&wrap).unwrap()));
    assert!(matches!(
        harness.core.screen(),
        Screen::Locked {
            device_unlock: false,
            ..
        }
    ));
}

// Test 9: token refresh.

#[test]
fn missing_token_queues_request_behind_refresh() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.unlock();
    // Stored sessions start with no access token, so the sync waits.
    let (refresh, request) = one_http(&effects);
    assert_eq!(
        request.url.as_str(),
        "https://vault.test/identity/connect/token"
    );
    let form = form(request);
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], STORED_REFRESH);
    let effects = harness.answer_fixture(refresh, "token_success.json");
    let (_, request) = one_http(&effects);
    assert!(
        request
            .url
            .as_str()
            .ends_with("/api/accounts/revision-date")
    );
    assert_eq!(bearer(request), Some(TOKEN.access_token.as_str()));
    // The refresh brought a new refresh token, which is stored.
    assert_eq!(slots(&effects), [StoreSlot::RefreshToken]);
}

#[test]
fn expired_token_refreshes_first() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.unlock();
    harness.settle_sync(&effects, REVISION);
    harness.now_ms += TOKEN.expires_in * 1000;
    let effects = harness.user(UserAction::Sync);
    let (_, request) = one_http(&effects);
    assert_eq!(form(request)["grant_type"], "refresh_token");
}

#[test]
fn unauthorized_refreshes_once_then_logs_out() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.unlock();
    let (refresh, _) = one_http(&effects);
    let effects = harness.answer_fixture(refresh, "token_success.json");
    let (check, _) = one_http(&effects);
    let effects = harness.answer(check, 401, &Value::Null);
    let (refresh, request) = one_http(&effects);
    assert_eq!(form(request)["grant_type"], "refresh_token");
    let effects = harness.answer_fixture(refresh, "token_success.json");
    let (retry, request) = one_http(&effects);
    assert!(
        request
            .url
            .as_str()
            .ends_with("/api/accounts/revision-date")
    );
    let effects = harness.answer(retry, 401, &Value::Null);
    assert!(http(&effects).is_empty());
    assert!(cleared(&effects).contains(&StoreSlot::RefreshToken));
    assert!(matches!(
        harness.core.screen(),
        Screen::Login {
            email: EMAIL,
            error: Some(LoginError::SessionExpired),
            ..
        }
    ));
}

#[test]
fn refused_refresh_logs_out() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.unlock();
    let (refresh, _) = one_http(&effects);
    harness.answer(
        refresh,
        400,
        &serde_json::json!({ "error": "invalid_grant" }),
    );
    assert!(matches!(
        harness.core.screen(),
        Screen::Login {
            email: EMAIL,
            error: Some(LoginError::SessionExpired),
            ..
        }
    ));
}

#[test]
fn tick_after_timeout_locks() {
    let mut harness = Harness::stored(REVISION);
    harness.unlock();
    harness.now_ms += LOCK_AFTER - 1;
    harness.send(Input::Tick);
    assert!(matches!(harness.core.screen(), Screen::Vault(_)));
    harness.now_ms += 1;
    harness.send(Input::Tick);
    assert!(matches!(harness.core.screen(), Screen::Locked { .. }));
}

#[test]
fn offline_sync_keeps_cached_index() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.unlock();
    let (refresh, _) = one_http(&effects);
    harness.send(Input::Http {
        id: refresh,
        response: Err(vault_core::core::HttpError {
            message: "offline".into(),
        }),
    });
    let screen = vault(&harness.core);
    assert_eq!(screen.sync_error, Some("offline"));
    assert_eq!(screen.index.ciphers.len(), SYNC.ciphers.len());
}

#[test]
fn wrong_unlock_password_stays_locked() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.user(UserAction::Unlock {
        password: secret("wrong"),
    });
    let (derive, ..) = one_derive(&effects);
    harness.derived(derive, "wrong");
    assert!(matches!(
        harness.core.screen(),
        Screen::Locked {
            error: Some(vault_core::core::UnlockError::WrongPassword),
            ..
        }
    ));
}

// Test 10: edits and conflicts.

#[test]
fn save_sends_update_and_stores_cache() {
    let mut harness = Harness::unlocked(REVISION);
    harness.user(UserAction::Open(EXAMPLE_ID));
    let open = vault(&harness.core).open.expect("the item is open");
    assert_eq!(open.view.name.as_str(), "Example login");
    let effects = harness.user(UserAction::Save(Box::new(edited("Edited login"))));
    let (put, request) = one_http(&effects);
    expect![[r#"
        PUT https://vault.test/api/ciphers/1b40a535-3c7f-4cae-bb72-dd7399c24af1
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        {
          "archivedDate": null,
          "attachments": null,
          "bankAccount": null,
          "card": null,
          "collectionIds": [],
          "creationDate": "<date>",
          "deletedDate": null,
          "driversLicense": null,
          "edit": true,
          "encryptedFor": "581cff7e-18ba-4f0d-9125-ded4464f585e",
          "favorite": false,
          "fields": [
            {
              "linkedId": null,
              "name": "<enc>",
              "type": 0,
              "value": "<enc>"
            },
            {
              "linkedId": null,
              "name": "<enc>",
              "type": 1,
              "value": "<enc>"
            },
            {
              "linkedId": 100,
              "name": "<enc>",
              "type": 3,
              "value": null
            }
          ],
          "folderId": "<id>",
          "id": "<id>",
          "identity": null,
          "key": null,
          "lastKnownRevisionDate": "2026-11-20T17:16:18.641520Z",
          "login": {
            "fido2Credentials": [],
            "password": "<enc>",
            "passwordRevisionDate": null,
            "totp": "<enc>",
            "uri": "<enc>",
            "uris": [
              {
                "match": null,
                "uri": "<enc>"
              },
              {
                "match": 3,
                "uri": "<enc>"
              }
            ],
            "username": "<enc>"
          },
          "name": "<enc>",
          "notes": "<enc>",
          "object": "cipherDetails",
          "organizationId": null,
          "organizationUseTotp": true,
          "passport": null,
          "passwordHistory": [],
          "permissions": {
            "delete": true,
            "restore": true
          },
          "reprompt": 0,
          "revisionDate": "<date>",
          "secureNote": null,
          "sshKey": null,
          "type": 1,
          "viewPassword": true
        }
    "#]]
    .assert_eq(&render(request, &["lastKnownRevisionDate"]));
    let body = json_object(request);
    assert_eq!(body["encryptedFor"], USER_ID.to_string());
    assert_eq!(
        body["lastKnownRevisionDate"],
        example_cipher().revision_date.as_str()
    );
    let sent = CipherResponse::from_object(body.clone()).unwrap();
    let sent = CipherView::decrypt(&sent, &KEYS).unwrap();
    assert_eq!(sent.name.as_str(), "Edited login");
    // The view's own fields round-trip; passkeys and unknown keys come from raw.
    assert_eq!(sent.fields.len(), 3);
    let effects = harness.answer_fixture(put, "cipher_put_success.json");
    let stored = stored_document(&effects);
    assert_eq!(stored.revision, PUT_REVISION);
    let saved = put_success_cipher();
    let saved_name = CipherView::decrypt(&saved, &KEYS).unwrap().name;
    let screen = vault(&harness.core);
    let row = screen
        .index
        .ciphers
        .iter()
        .find(|row| row.id == EXAMPLE_ID)
        .unwrap();
    assert_eq!(row.name, saved_name);
    assert_eq!(screen.open.unwrap().view.name, saved_name);
    assert_eq!(screen.revision, Some(PUT_REVISION));
}

#[test]
fn out_of_date_save_syncs_then_shows_conflict() {
    let mut harness = Harness::unlocked(REVISION);
    harness.user(UserAction::Open(EXAMPLE_ID));
    let effects = harness.user(UserAction::Save(Box::new(edited("Edited login"))));
    let (put, _) = one_http(&effects);
    let effects = harness.answer_fixture(put, "cipher_put_out_of_date.json");
    // The server copy changed: theirs is the PUT fixture.
    let mut sync = fixture("sync.json").1;
    let theirs = put_success_cipher();
    for cipher in sync["ciphers"].as_array_mut().unwrap() {
        if cipher["id"] == EXAMPLE_ID.to_string() {
            *cipher = Value::Object(theirs.raw.clone());
        }
    }
    harness.settle_sync_with(&effects, REVISION + 10_000, &sync);
    let screen = vault(&harness.core);
    let conflict = screen.open.unwrap().conflict().expect("a conflict screen");
    assert_eq!(conflict.mine.name.as_str(), "Edited login");
    assert_eq!(
        conflict.theirs.name,
        CipherView::decrypt(&theirs, &KEYS).unwrap().name
    );
    harness.user(UserAction::ResolveConflict { keep_mine: true });
    let open = vault(&harness.core).open.unwrap();
    assert!(open.conflict().is_none());
    assert_eq!(open.view.name.as_str(), "Edited login");
    let effects = harness.user(UserAction::Save(Box::new(edited("Edited login"))));
    let (_, request) = one_http(&effects);
    assert_eq!(
        json_object(request)["lastKnownRevisionDate"],
        theirs.revision_date.as_str()
    );
}

#[test]
fn sync_while_editing_marks_stale_and_save_conflicts() {
    let mut harness = Harness::unlocked(REVISION);
    harness.user(UserAction::Open(EXAMPLE_ID));
    let mut sync = fixture("sync.json").1;
    for cipher in sync["ciphers"].as_array_mut().unwrap() {
        if cipher["id"] == EXAMPLE_ID.to_string() {
            *cipher = Value::Object(put_success_cipher().raw);
        }
    }
    let effects = harness.user(UserAction::Sync);
    harness.settle_sync_with(&effects, REVISION + 10_000, &sync);
    assert!(vault(&harness.core).open.unwrap().stale);
    let effects = harness.user(UserAction::Save(Box::new(edited("Edited login"))));
    assert!(
        http(&effects).is_empty(),
        "a stale save must not reach the server"
    );
    assert!(vault(&harness.core).open.unwrap().conflict().is_some());
}

#[test]
fn create_posts_with_encrypted_for() {
    let mut harness = Harness::unlocked(REVISION);
    let mut view = edited("New login");
    view.id = None;
    let effects = harness.user(UserAction::Save(Box::new(view)));
    let (_, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/api/ciphers
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        {
          "encryptedFor": "581cff7e-18ba-4f0d-9125-ded4464f585e",
          "favorite": false,
          "fields": [
            {
              "linkedId": null,
              "name": "<enc>",
              "type": 0,
              "value": "<enc>"
            },
            {
              "linkedId": null,
              "name": "<enc>",
              "type": 1,
              "value": "<enc>"
            },
            {
              "linkedId": 100,
              "name": "<enc>",
              "type": 3,
              "value": null
            }
          ],
          "folderId": "<id>",
          "login": {
            "fido2Credentials": [],
            "password": "<enc>",
            "passwordRevisionDate": null,
            "totp": "<enc>",
            "uri": "<enc>",
            "uris": [
              {
                "match": null,
                "uri": "<enc>"
              },
              {
                "match": 3,
                "uri": "<enc>"
              }
            ],
            "username": "<enc>"
          },
          "name": "<enc>",
          "notes": "<enc>",
          "passwordHistory": [],
          "reprompt": 0,
          "type": 1
        }
    "#]]
    .assert_eq(&render(request, &[]));
    assert_eq!(json_object(request)["encryptedFor"], USER_ID.to_string());
}

// WP-11: org create, folders, cloned passkeys, password history, server URLs
// and the index revision.

#[test]
fn org_create_posts_cipher_with_collections() {
    let mut harness = Harness::unlocked(REVISION);
    let mut view = edited("Org login");
    view.id = None;
    view.organization_id = Some(ORGANIZATION_ID);
    view.collection_ids = vec![COLLECTION_ID];
    let effects = harness.user(UserAction::Save(Box::new(view)));
    let (post, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/api/ciphers/create
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        {
          "cipher": {
            "encryptedFor": "581cff7e-18ba-4f0d-9125-ded4464f585e",
            "favorite": false,
            "fields": [
              {
                "linkedId": null,
                "name": "<enc>",
                "type": 0,
                "value": "<enc>"
              },
              {
                "linkedId": null,
                "name": "<enc>",
                "type": 1,
                "value": "<enc>"
              },
              {
                "linkedId": 100,
                "name": "<enc>",
                "type": 3,
                "value": null
              }
            ],
            "folderId": "<id>",
            "login": {
              "fido2Credentials": [],
              "password": "<enc>",
              "passwordRevisionDate": null,
              "totp": "<enc>",
              "uri": "<enc>",
              "uris": [
                {
                  "match": null,
                  "uri": "<enc>"
                },
                {
                  "match": 3,
                  "uri": "<enc>"
                }
              ],
              "username": "<enc>"
            },
            "name": "<enc>",
            "notes": "<enc>",
            "organizationId": "6cb3dadc-aa89-4725-92d5-c767d2ffc003",
            "passwordHistory": [],
            "reprompt": 0,
            "type": 1
          },
          "collectionIds": [
            "630a9d36-58ee-4b40-9b2c-93213c113712"
          ]
        }
    "#]]
    .assert_eq(&render(request, &["collectionIds", "organizationId"]));
    let body = json_object(request);
    let mut cipher = body["cipher"].as_object().expect("a cipher object").clone();
    assert_eq!(cipher["encryptedFor"], USER_ID.to_string());
    // The server answers with the stored cipher: ours plus an id and a date.
    let new_id = Uuid::from_u128(0xC0FFEE);
    cipher.insert("id".into(), Value::String(new_id.to_string()));
    cipher.insert("revisionDate".into(), "2026-11-21T00:00:00.000Z".into());
    cipher.insert("collectionIds".into(), body["collectionIds"].clone());
    // Only the org key opens it, so this decrypt proves which key was used.
    let sent = CipherResponse::from_object(cipher.clone()).unwrap();
    assert_eq!(
        CipherView::decrypt(&sent, &KEYS).unwrap().name.as_str(),
        "Org login"
    );
    harness.answer(post, 200, &Value::Object(cipher));
    let screen = vault(&harness.core);
    let row = screen
        .index
        .ciphers
        .iter()
        .find(|row| row.id == new_id)
        .expect("the new item is listed");
    assert_eq!(row.organization_id, Some(ORGANIZATION_ID));
    assert_eq!(row.name.as_str(), "Org login");
}

#[test]
fn folder_create_adds_to_index_and_cache() {
    let mut harness = Harness::unlocked(REVISION);
    let effects = harness.user(UserAction::CreateFolder {
        name: secret("Work"),
    });
    let (post, request) = one_http(&effects);
    expect![[r#"
        POST https://vault.test/api/folders
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        {
          "name": "<enc>"
        }
    "#]]
    .assert_eq(&render(request, &[]));
    let name = json_object(request)["name"].clone();
    assert_eq!(decrypt_user_text(&name), "Work");
    let new_id = Uuid::from_u128(0xF01D);
    let effects = harness.answer(
        post,
        200,
        &serde_json::json!({
            "Id": new_id.to_string(),
            "Name": name,
            "RevisionDate": "2026-11-21T00:00:00.000Z",
            "Object": "folder",
        }),
    );
    assert_eq!(folder_name(&harness.core, new_id).as_deref(), Some("Work"));
    let stored = stored_sync(&effects);
    let folder = stored
        .folders
        .iter()
        .find(|folder| folder.id == new_id)
        .expect("the stored document holds the folder");
    assert_eq!(folder.name.to_string(), name.as_str().unwrap());
    assert_eq!(stored_document(&effects).revision, NOV_21_MS);
}

#[test]
fn folder_rename_puts_new_name() {
    let mut harness = Harness::unlocked(REVISION);
    let effects = harness.user(UserAction::RenameFolder {
        id: FOLDER_ID,
        name: secret("Renamed"),
    });
    let (put, request) = one_http(&effects);
    expect![[r#"
        PUT https://vault.test/api/folders/9cb34896-8975-41f2-8c5e-d1f7a34bb391
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6

        {
          "name": "<enc>"
        }
    "#]]
    .assert_eq(&render(request, &[]));
    let name = json_object(request)["name"].clone();
    assert_eq!(decrypt_user_text(&name), "Renamed");
    let effects = harness.answer(
        put,
        200,
        &serde_json::json!({
            "id": FOLDER_ID.to_string(),
            "name": name,
            "revisionDate": "2026-11-21T00:00:00.000Z",
        }),
    );
    assert_eq!(
        folder_name(&harness.core, FOLDER_ID).as_deref(),
        Some("Renamed")
    );
    let stored = stored_sync(&effects);
    let folders: Vec<_> = stored
        .folders
        .iter()
        .filter(|folder| folder.id == FOLDER_ID)
        .collect();
    assert_eq!(folders.len(), 1, "the old copy is replaced");
}

#[test]
fn folder_delete_moves_its_items_out() {
    let mut harness = Harness::unlocked(REVISION);
    assert_eq!(example_cipher().folder_id, Some(FOLDER_ID));
    let effects = harness.user(UserAction::DeleteFolder(FOLDER_ID));
    let (delete, request) = one_http(&effects);
    expect![[r#"
        DELETE https://vault.test/api/folders/9cb34896-8975-41f2-8c5e-d1f7a34bb391
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6
    "#]]
    .assert_eq(&render(request, &[]));
    let effects = harness.send(Input::Http {
        id: delete,
        response: Ok(HttpResponse {
            status: 200,
            body: Vec::new(),
        }),
    });
    assert_eq!(folder_name(&harness.core, FOLDER_ID), None);
    let screen = vault(&harness.core);
    let row = screen
        .index
        .ciphers
        .iter()
        .find(|row| row.id == EXAMPLE_ID)
        .unwrap();
    assert_eq!(row.folder_id, None);
    let stored = stored_sync(&effects);
    assert!(stored.folders.iter().all(|folder| folder.id != FOLDER_ID));
    let cipher = stored
        .ciphers
        .iter()
        .find(|cipher| cipher.id == EXAMPLE_ID)
        .unwrap();
    assert_eq!(cipher.folder_id, None);
}

#[test]
fn edit_from_clone_keeps_passkeys() {
    let mut harness = Harness::unlocked(REVISION);
    let cipher = passkey_cipher();
    harness.user(UserAction::Open(cipher.id));
    let mut view = vault(&harness.core).open.unwrap().view.clone();
    view.name = secret("Renamed passkey login");
    let effects = harness.user(UserAction::Save(Box::new(view)));
    let (_, request) = one_http(&effects);
    let sent = CipherResponse::from_object(json_object(request)).unwrap();
    let sent = CipherView::decrypt(&sent, &KEYS).unwrap();
    let before = CipherView::decrypt(&cipher, &KEYS).unwrap();
    let passkeys = |view: &CipherView| -> Vec<(String, Vec<u8>, u32)> {
        let CipherContent::Login(login) = &view.content else {
            panic!("not a login");
        };
        login
            .fido2_credentials
            .iter()
            .map(|credential| {
                (
                    credential.credential_id.to_string(),
                    credential.key.to_pkcs8_der().to_vec(),
                    credential.counter,
                )
            })
            .collect()
    };
    assert!(!passkeys(&before).is_empty());
    assert_eq!(passkeys(&sent), passkeys(&before));
    assert_eq!(sent.name.as_str(), "Renamed passkey login");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 4, ..ProptestConfig::default() })]

    /// Spec: a changed password goes first in the history, dated now, the
    /// history keeps at most five, and `passwordRevisionDate` becomes now.
    /// An unchanged password leaves both alone.
    #[test]
    fn password_change_appends_history(prior in 0..8usize, change in any::<bool>()) {
        let mut harness = Harness::unlocked(REVISION);
        harness.user(UserAction::Open(EXAMPLE_ID));
        let mut view = vault(&harness.core).open.unwrap().view.clone();
        let old = login_password(&view).expect("the example login has a password");
        view.password_history = (0..prior)
            .map(|index| PasswordHistoryEntry {
                password: secret(&format!("older-{index}")),
                last_used_date: "2026-01-01T00:00:00.000Z".into(),
            })
            .collect();
        let old_revision_date = match &view.content {
            CipherContent::Login(login) => login.password_revision_date.clone(),
            _ => unreachable!("the example is a login"),
        };
        if change && let CipherContent::Login(login) = &mut view.content {
            login.password = Some(secret("brand-new-password"));
        }
        let effects = harness.user(UserAction::Save(Box::new(view)));
        let (_, request) = one_http(&effects);
        let sent = CipherResponse::from_object(json_object(request)).unwrap();
        let sent = CipherView::decrypt(&sent, &KEYS).unwrap();
        let history: Vec<(String, String)> = sent
            .password_history
            .iter()
            .map(|entry| (entry.password.to_string(), entry.last_used_date.clone()))
            .collect();
        let older = (0..prior).map(|index| {
            (format!("older-{index}"), "2026-01-01T00:00:00.000Z".to_owned())
        });
        let expected: Vec<(String, String)> = if change {
            std::iter::once((old, START_ISO.to_owned())).chain(older).take(5).collect()
        } else {
            older.collect()
        };
        prop_assert_eq!(history, expected);
        let CipherContent::Login(login) = &sent.content else {
            panic!("not a login");
        };
        let revision_date = if change { Some(START_ISO.to_owned()) } else { old_revision_date };
        prop_assert_eq!(&login.password_revision_date, &revision_date);
    }
}

#[test]
fn server_urls_cloud_and_self_hosted() {
    let com = Some((
        "https://identity.bitwarden.com/",
        "https://api.bitwarden.com/",
    ));
    let cases = [
        ("https://vault.bitwarden.com", com),
        ("https://vault.bitwarden.com/", com),
        ("https://bitwarden.com", com),
        (
            "https://vault.bitwarden.eu",
            Some((
                "https://identity.bitwarden.eu/",
                "https://api.bitwarden.eu/",
            )),
        ),
        (
            "http://localhost:8000/",
            Some((
                "http://localhost:8000/identity",
                "http://localhost:8000/api",
            )),
        ),
        (
            "https://example.org/vault",
            Some((
                "https://example.org/vault/identity",
                "https://example.org/vault/api",
            )),
        ),
        (
            "https://example.org/vault/",
            Some((
                "https://example.org/vault/identity",
                "https://example.org/vault/api",
            )),
        ),
        ("ftp://example.org", None),
        ("mailto:someone@example.org", None),
    ];
    for (server, expected) in cases {
        let urls = Config::server_urls(&Url::parse(server).unwrap());
        let urls = urls
            .as_ref()
            .ok()
            .map(|urls| (urls.identity.as_str(), urls.api.as_str()));
        assert_eq!(urls, expected, "{server}");
    }
}

#[test]
fn index_revision_changes_only_with_index() {
    let mut harness = Harness::unlocked(REVISION);
    let first = vault(&harness.core).index_revision;
    // Open, close and a sync that brings the same data leave the list alone.
    harness.user(UserAction::Open(EXAMPLE_ID));
    harness.user(UserAction::Close);
    let effects = harness.user(UserAction::Sync);
    harness.settle_sync(&effects, REVISION + 1);
    assert_eq!(vault(&harness.core).index_revision, first);
    // A renamed item changes a summary.
    harness.user(UserAction::Open(EXAMPLE_ID));
    let effects = harness.user(UserAction::Save(Box::new(edited("Edited login"))));
    let (put, _) = one_http(&effects);
    harness.answer_fixture(put, "cipher_put_success.json");
    let renamed = vault(&harness.core).index_revision;
    assert_ne!(renamed, first);
    // A new session never repeats a number.
    harness.user(UserAction::Lock);
    harness.unlock();
    assert!(vault(&harness.core).index_revision > renamed);
}
// Test 15: two cores, one storage.

proptest! {
    #![proptest_config(ProptestConfig { cases: 8, ..ProptestConfig::default() })]

    #[test]
    fn second_core_takes_newer_cache(
        writes in 1..6usize,
        deliveries in prop::collection::vec(any::<prop::sample::Index>(), 1..10),
    ) {
        let mut writer = Harness::unlocked(REVISION);
        let mut reader = Harness::unlocked(REVISION);
        let mut documents = Vec::new();
        for write in 0..writes {
            let name = format!("name {write}");
            let effects = writer.user(UserAction::Save(Box::new(edited(&name))));
            let (put, request) = one_http(&effects);
            // The server echoes the cipher with its new revision date.
            let mut echo = json_object(request);
            echo.insert(
                "revisionDate".into(),
                Value::from(format!("2026-11-21T00:00:0{write}.000Z")),
            );
            let effects = writer.answer(put, 200, &Value::Object(echo));
            documents.push((write, stored_bytes(&effects, StoreSlot::VaultCache)));
        }
        // Oracle: revisions are whole seconds after 2026-11-21T00:00:00Z.
        let revision_of = |write: usize| NOV_21_MS + write as u64 * 1000;
        let mut newest: Option<usize> = None;
        for delivery in deliveries {
            let (write, bytes) = &documents[delivery.index(documents.len())];
            reader.send(Input::StorageChanged { slot: StoreSlot::VaultCache, bytes: Some(bytes.clone()) });
            newest = Some(newest.map_or(*write, |newest| newest.max(*write)));
            let newest = newest.unwrap();
            let screen = vault(&reader.core);
            prop_assert_eq!(screen.revision, Some(revision_of(newest)));
            let row = screen.index.ciphers.iter().find(|row| row.id == EXAMPLE_ID).unwrap();
            prop_assert_eq!(row.name.as_str(), format!("name {newest}"));
        }
    }
}

// Test 12: passkey create.

proptest! {
    #![proptest_config(ProptestConfig { cases: 4, ..ProptestConfig::default() })]

    #[test]
    fn created_passkey_signs_the_next_assertion(
        parties in prop::collection::btree_map(
            "[a-z]{1,10}\\.(test|org)",
            prop::collection::vec(any::<u8>(), 1..64),
            1..4,
        ),
    ) {
        let mut harness = Harness::unlocked(REVISION);
        for (index, (rp_id, user_handle)) in parties.into_iter().enumerate() {
            let client_data_hash = [index as u8; 32];
            let effects = harness.send(Input::Passkey {
                id: RequestId::from_shell(100),
                request: PasskeyRequest::MakeCredential {
                    rp_id: rp_id.clone(),
                    rp_name: None,
                    user_handle: user_handle.clone(),
                    user_name: "bob".into(),
                    user_display_name: "Bob".into(),
                    client_data_hash,
                    require_resident: true,
                    exclude: Vec::new(),
                },
            });
            prop_assert!(passkey_replies(&effects).is_empty());
            let prompt = harness.core.passkey_prompt().expect("a create asks where to save");
            // No fixture login has a URI on this rp id, so only "new login" is offered.
            prop_assert_eq!(prompt.choices.len(), 1);
            prop_assert!(prompt.choices[0].new_login());
            let effects = harness.user(UserAction::ChoosePasskey(0));
            prop_assert!(passkey_replies(&effects).is_empty(), "no reply before the save");
            let (post, request) = one_http(&effects);
            prop_assert_eq!(request.method, Method::Post);
            let cipher_id = Uuid::from_u128(0xabc0 + index as u128);
            let mut echo = json_object(request);
            echo.insert("id".into(), Value::from(cipher_id.to_string()));
            echo.insert("revisionDate".into(), Value::from("2026-11-21T00:00:00.000Z"));
            let saved = CipherResponse::from_object(echo.clone()).unwrap();
            let saved = CipherView::decrypt(&saved, &KEYS).unwrap();
            let CipherContent::Login(login) = &saved.content else {
                panic!("a passkey is saved in a login");
            };
            prop_assert_eq!(login.fido2_credentials.len(), 1);
            prop_assert_eq!(login.fido2_credentials[0].rp_id.as_str(), rp_id.as_str());
            prop_assert_eq!(login.fido2_credentials[0].counter, 0);
            let effects = harness.answer(post, 200, &Value::Object(echo));
            let replies = passkey_replies(&effects);            let [(_, Ok(PasskeyReply::Created {
                credential_id,
                authenticator_data,
                public_key_cose,
                attestation_object,
            }))] = replies.as_slice() else {
                panic!("expected one created reply");
            };
            let listed = catalogs(&effects).iter().any(|entries| {
                entries.iter().any(|entry| {
                    &entry.credential_id == credential_id && entry.rp_id == rp_id
                })
            });
            prop_assert!(listed, "the catalog lists the new passkey");
            prop_assert_eq!(
                Some(credential_id.clone()),
                Uuid::parse_str(&login.fido2_credentials[0].credential_id)
                    .ok()
                    .map(|id| id.as_bytes().to_vec()),
            );
            // The oracle reads the WebAuthn layout field by field.
            let data = AuthenticatorData::parse(authenticator_data);
            prop_assert_eq!(data.rp_id_hash, Sha256::digest(rp_id.as_bytes()).to_vec());
            prop_assert_eq!(data.flags, 0x01 | 0x04 | 0x40);
            prop_assert_eq!(data.counter, 0);
            let attested = data.attested.expect("AT carries attested credential data");
            prop_assert_eq!(&attested.credential_id, credential_id);
            prop_assert_eq!(&attested.public_key_cose, public_key_cose);
            let Cbor::Map(cose) = cbor(public_key_cose) else {
                panic!("a COSE key is a map");
            };
            prop_assert_eq!(map_get(&cose, &Cbor::Int(1)), &Cbor::Int(2));
            prop_assert_eq!(map_get(&cose, &Cbor::Int(3)), &Cbor::Int(-7));
            prop_assert_eq!(map_get(&cose, &Cbor::Int(-1)), &Cbor::Int(1));
            let (Cbor::Bytes(x), Cbor::Bytes(y)) =
                (map_get(&cose, &Cbor::Int(-2)), map_get(&cose, &Cbor::Int(-3)))
            else {
                panic!("x and y are byte strings");
            };
            let Cbor::Map(attestation) = cbor(attestation_object) else {
                panic!("an attestation object is a map");
            };
            prop_assert_eq!(map_get(&attestation, &Cbor::Text("fmt".into())), &Cbor::Text("none".into()));
            prop_assert_eq!(map_get(&attestation, &Cbor::Text("attStmt".into())), &Cbor::Map(Vec::new()));
            prop_assert_eq!(
                map_get(&attestation, &Cbor::Text("authData".into())),
                &Cbor::Bytes(authenticator_data.clone()),
            );
            let mut point = vec![0x04];
            point.extend_from_slice(x);
            point.extend_from_slice(y);
            let verifying = VerifyingKey::from_sec1_bytes(&point).unwrap();

            let assertion_hash = [0xa0 + index as u8; 32];
            let effects = harness.send(Input::Passkey {
                id: RequestId::from_shell(101),
                request: PasskeyRequest::GetAssertion {
                    rp_id: rp_id.clone(),
                    client_data_hash: assertion_hash,
                    allow: vec![credential_id.clone()],
                },
            });
            let replies = passkey_replies(&effects);            let [(id, Ok(PasskeyReply::Asserted {
                credential_id: asserted_id,
                authenticator_data,
                signature,
                user_handle: asserted_handle,
            }))] = replies.as_slice() else {
                panic!("expected one assertion");
            };
            prop_assert_eq!(*id, RequestId::from_shell(101));
            prop_assert_eq!(asserted_id, credential_id);
            prop_assert_eq!(asserted_handle.as_ref(), Some(&user_handle));
            prop_assert!(verifies(&verifying, authenticator_data, &assertion_hash, signature));
            let data = AuthenticatorData::parse(authenticator_data);
            prop_assert_eq!(data.flags, 0x01 | 0x04);
            prop_assert_eq!(data.counter, 1);
        }
    }
}

// Test 13: passkey assert.

#[test]
fn assertion_picks_by_allow_list_and_counts_up() {
    let mut harness = Harness::unlocked(REVISION);
    let verifying = fixture_verifying_key();
    let mut last = 0;
    for round in 0..3u8 {
        let client_data_hash = [round; 32];
        let effects = harness.send(get_assertion(
            7,
            "example.com",
            client_data_hash,
            vec![fixture_credential_id(), vec![1, 2, 3]],
        ));
        let replies = passkey_replies(&effects);
        let [
            (
                _,
                Ok(PasskeyReply::Asserted {
                    credential_id,
                    authenticator_data,
                    signature,
                    user_handle,
                }),
            ),
        ] = replies.as_slice()
        else {
            panic!("expected one assertion");
        };
        assert_eq!(credential_id, &fixture_credential_id());
        assert_eq!(user_handle.as_deref(), Some(&b"user"[..]));
        assert!(verifies(
            &verifying,
            authenticator_data,
            &client_data_hash,
            signature
        ));
        let data = AuthenticatorData::parse(authenticator_data);
        assert_eq!(data.rp_id_hash, Sha256::digest(b"example.com").to_vec());
        assert!(data.counter > last, "the counter rises");
        last = data.counter;
        // The reply comes first; the counter write follows.
        let (_, request) = one_http(&effects);
        assert_eq!(request.method, Method::Put);
    }
    let effects = harness.send(get_assertion(8, "example.com", [0; 32], vec![vec![9; 16]]));
    assert_eq!(
        passkey_replies(&effects),
        [(RequestId::from_shell(8), Err(PasskeyError::NotAllowed))]
    );
    let effects = harness.send(get_assertion(
        9,
        "other.test",
        [0; 32],
        vec![fixture_credential_id()],
    ));
    assert_eq!(
        passkey_replies(&effects),
        [(RequestId::from_shell(9), Err(PasskeyError::NotAllowed))]
    );
}

#[test]
fn discoverable_assertion_offers_each_passkey_for_the_rp() {
    let mut harness = Harness::unlocked(REVISION);
    harness.send(Input::Passkey {
        id: RequestId::from_shell(1),
        request: PasskeyRequest::MakeCredential {
            rp_id: "example.com".into(),
            rp_name: Some("Example".into()),
            user_handle: b"second".to_vec(),
            user_name: "carol".into(),
            user_display_name: "Carol".into(),
            client_data_hash: [1; 32],
            require_resident: true,
            exclude: Vec::new(),
        },
    });
    let prompt = harness.core.passkey_prompt().unwrap();
    let new_login = prompt
        .choices
        .iter()
        .position(|choice| choice.new_login())
        .unwrap();
    assert_eq!(new_login, prompt.choices.len() - 1, "new login comes last");
    let effects = harness.user(UserAction::ChoosePasskey(new_login));
    let (post, request) = one_http(&effects);
    let mut echo = json_object(request);
    echo.insert("id".into(), Value::from(Uuid::from_u128(0xdd).to_string()));
    echo.insert(
        "revisionDate".into(),
        Value::from("2026-11-21T00:00:00.000Z"),
    );
    let effects = harness.answer(post, 200, &Value::Object(echo));
    let replies = passkey_replies(&effects);
    let [(_, Ok(PasskeyReply::Created { credential_id, .. }))] = replies.as_slice() else {
        panic!("expected a created reply");
    };
    let created = credential_id.clone();

    let effects = harness.send(get_assertion(2, "example.com", [2; 32], Vec::new()));
    assert!(passkey_replies(&effects).is_empty(), "two passkeys: ask");
    let prompt = harness.core.passkey_prompt().unwrap();
    let mut names: Vec<&str> = prompt
        .choices
        .iter()
        .map(|choice| choice.user_name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["alice", "carol"]);
    let carol = prompt
        .choices
        .iter()
        .position(|choice| choice.user_name.as_str() == "carol")
        .unwrap();
    let effects = harness.user(UserAction::ChoosePasskey(carol));
    let replies = passkey_replies(&effects);
    let [(id, Ok(PasskeyReply::Asserted { credential_id, .. }))] = replies.as_slice() else {
        panic!("expected an assertion");
    };
    assert_eq!(*id, RequestId::from_shell(2));
    assert_eq!(credential_id, &created);
    assert!(harness.core.passkey_prompt().is_none());
}

#[test]
fn locked_request_waits_for_unlock() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.send(get_assertion(
        3,
        "example.com",
        [3; 32],
        vec![fixture_credential_id()],
    ));
    assert!(passkey_replies(&effects).is_empty());
    let prompt = harness.core.passkey_prompt().expect("the request waits");
    assert!(prompt.choices.is_empty());
    let effects = harness.unlock();
    let replies = passkey_replies(&effects);
    let [(id, Ok(PasskeyReply::Asserted { .. }))] = replies.as_slice() else {
        panic!("unlock answers the waiting request");
    };
    assert_eq!(*id, RequestId::from_shell(3));
}

#[test]
fn locked_request_times_out() {
    let mut harness = Harness::stored(REVISION);
    let start = harness.now_ms;
    harness.send(get_assertion(4, "example.com", [4; 32], Vec::new()));
    harness.now_ms = start + PASSKEY_WAIT - 1;
    assert!(passkey_replies(&harness.send(Input::Tick)).is_empty());
    harness.now_ms = start + PASSKEY_WAIT;
    assert_eq!(
        passkey_replies(&harness.send(Input::Tick)),
        [(RequestId::from_shell(4), Err(PasskeyError::NotAllowed))]
    );
    assert!(harness.core.passkey_prompt().is_none());
    harness.send(get_assertion(5, "example.com", [5; 32], Vec::new()));
    assert_eq!(
        passkey_replies(&harness.user(UserAction::Cancel)),
        [(RequestId::from_shell(5), Err(PasskeyError::NotAllowed))]
    );
}

// Test 15, passkey part: concurrent counter writes.

proptest! {
    #![proptest_config(ProptestConfig { cases: 4, ..ProptestConfig::default() })]

    #[test]
    fn concurrent_counters_end_at_the_highest(
        steps in prop::collection::vec((any::<bool>(), any::<bool>(), any::<prop::sample::Index>()), 1..14),
    ) {
        let mut server = Server::new();
        let mut cores = [Harness::unlocked(REVISION), Harness::unlocked(REVISION)];
        let mut outboxes: [Vec<(RequestId, HttpRequest)>; 2] = [Vec::new(), Vec::new()];
        let mut signed: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        let mut assert = |core: usize, cores: &mut [Harness; 2], outboxes: &mut [Vec<_>; 2]| {
            let effects = cores[core].send(get_assertion(1, "example.com", [0; 32], vec![fixture_credential_id()]));
            for (_, reply) in passkey_replies(&effects) {
                let Ok(PasskeyReply::Asserted { authenticator_data, .. }) = reply else {
                    panic!("the fixture passkey always signs");
                };
                signed[core].push(AuthenticatorData::parse(&authenticator_data).counter);
            }
            outboxes[core].extend(owned_http(&effects));
        };
        for (assert_now, second, pick) in steps {
            let core = usize::from(second);
            if assert_now || outboxes[core].is_empty() {
                assert(core, &mut cores, &mut outboxes);
            } else {
                let (id, request) = outboxes[core].remove(pick.index(outboxes[core].len()));
                let effects = server.answer(&mut cores[core], id, &request);
                outboxes[core].extend(owned_http(&effects));
            }
        }
        // Deliver everything still queued, oldest first.
        for _ in 0..200 {
            let Some(core) = (0..2).find(|&core| !outboxes[core].is_empty()) else {
                break;
            };
            let (id, request) = outboxes[core].remove(0);
            let effects = server.answer(&mut cores[core], id, &request);
            outboxes[core].extend(owned_http(&effects));
        }
        prop_assert!(outboxes.iter().all(Vec::is_empty), "the writes settle");
        for counters in &signed {
            prop_assert!(counters.windows(2).all(|pair| pair[0] < pair[1]), "{counters:?}");
        }
        let highest = signed.iter().flatten().copied().max().unwrap_or(0);
        prop_assert_eq!(server.counter(), highest);
    }
}
// Test 18: cache freshness.

#[test]
fn revision_poll_every_five_minutes() {
    let mut harness = Harness::unlocked(REVISION);
    let start = harness.now_ms;
    harness.now_ms = start + FIVE_MINUTES - 1;
    assert!(http(&harness.send(Input::Tick)).is_empty());
    harness.now_ms = start + FIVE_MINUTES;
    let effects = harness.send(Input::Tick);
    let (check, request) = one_http(&effects);
    expect![[r#"
        GET https://vault.test/api/accounts/revision-date
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6
    "#]]
    .assert_eq(&render(request, &[]));
    harness.now_ms += 1;
    assert!(
        http(&harness.send(Input::Tick)).is_empty(),
        "one check at a time"
    );
    assert!(
        harness
            .answer(check, 200, &Value::from(REVISION))
            .is_empty()
    );
    harness.now_ms = start + FIVE_MINUTES + FIVE_MINUTES - 1;
    assert!(http(&harness.send(Input::Tick)).is_empty());
    harness.now_ms = start + FIVE_MINUTES * 2;
    let effects = harness.send(Input::Tick);
    let (check, _) = one_http(&effects);
    let effects = harness.answer(check, 200, &Value::from(REVISION + 1));
    let (_, request) = one_http(&effects);
    expect![[r#"
        GET https://vault.test/api/sync?excludeDomains=true
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6
    "#]]
    .assert_eq(&render(request, &[]));
}

#[test]
fn older_revision_reply_does_nothing() {
    let mut harness = Harness::unlocked(REVISION);
    harness.now_ms += FIVE_MINUTES;
    let effects = harness.send(Input::Tick);
    let (check, _) = one_http(&effects);
    assert!(
        harness
            .answer(check, 200, &Value::from(REVISION - 1))
            .is_empty()
    );
}

// Test 19: cache revision rule.

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, ..ProptestConfig::default() })]

    #[test]
    fn kept_revision_never_decreases(events in prop::collection::vec(cache_event(), 1..12)) {
        let mut harness = Harness::unlocked(REVISION);
        let sync = fixture("sync.json").1;
        let mut kept = REVISION;
        for event in events {
            match event {
                CacheEvent::Stored { revision, other_user, format } => {
                    let document = CacheDocument {
                        format,
                        user_id: if other_user { Uuid::from_u128(7) } else { USER_ID },
                        revision,
                        synced_at: 0,
                        sync: sync.clone(),
                    };
                    harness.send(Input::StorageChanged {
                        slot: StoreSlot::VaultCache,
                        bytes: Some(document.to_vec()),
                    });
                    if !other_user && format == CacheDocument::FORMAT {
                        kept = kept.max(revision);
                    }
                }
                CacheEvent::Synced { revision } => {
                    let effects = harness.user(UserAction::Sync);
                    harness.settle_sync_with(&effects, revision, &sync);
                    kept = kept.max(revision);
                }
            }
            prop_assert_eq!(vault(&harness.core).revision, Some(kept));
        }
    }
}

// Test 20: cache document round trip.

#[test]
fn write_patches_stored_document() {
    let mut harness = Harness::unlocked(REVISION);
    let effects = harness.user(UserAction::Save(Box::new(edited("Edited login"))));
    let (put, _) = one_http(&effects);
    let effects = harness.answer_fixture(put, "cipher_put_success.json");
    let stored = stored_document(&effects);
    assert_eq!(stored.revision, PUT_REVISION);
    assert_eq!(stored.user_id, USER_ID);
    // Every key but `ciphers` is the sync body as received.
    let mut original = fixture("sync.json").1;
    let original_ciphers = original.as_object_mut().unwrap().remove("ciphers").unwrap();
    let mut rest = stored.sync.clone();
    rest.as_object_mut().unwrap().remove("ciphers");
    assert_eq!(rest, original);
    // The ciphers are the synced ones with the written one replaced.
    let reparsed = SyncResponse::from_json(stored.sync).unwrap();
    let mut expected: Vec<Map<String, Value>> = original_ciphers
        .as_array()
        .unwrap()
        .iter()
        .map(|cipher| cipher.as_object().unwrap().clone())
        .filter(|cipher| cipher["id"] != EXAMPLE_ID.to_string())
        .chain([put_success_cipher().raw])
        .collect();
    let mut actual: Vec<Map<String, Value>> = reparsed
        .ciphers
        .into_iter()
        .map(|cipher| cipher.raw)
        .collect();
    let by_id = |cipher: &Map<String, Value>| cipher["id"].as_str().unwrap().to_owned();
    expected.sort_by_key(by_id);
    actual.sort_by_key(by_id);
    assert_eq!(actual, expected);
}

// Test 21: SSH agent protocol.

proptest! {
    #[test]
    fn accepted_frames_are_built_by_the_draft_layout(frame in agent_frame()) {
        match parse_request(&frame) {
            Ok(AgentRequest::List) => prop_assert_eq!(frame, vec![11]),
            Ok(AgentRequest::Sign { public_key, data, flags }) => {
                prop_assert_eq!(frame, sign_frame(&public_key, &data, flags));
            }
            Ok(AgentRequest::Other(kind)) => {
                prop_assert_eq!(Some(&kind), frame.first());
                prop_assert!(kind != 11 && kind != 13);
            }
            Err(_) => {}
        }
    }

    #[test]
    fn sign_frame_round_trips_and_rejects_cut_or_extended(
        public_key in prop::collection::vec(any::<u8>(), 0..40),
        data in prop::collection::vec(any::<u8>(), 0..40),
        flags in any::<u32>(),
        extra in prop::collection::vec(any::<u8>(), 1..6),
    ) {
        let frame = sign_frame(&public_key, &data, flags);
        prop_assert_eq!(
            parse_request(&frame),
            Ok(AgentRequest::Sign { public_key, data, flags })
        );
        for length in 0..frame.len() {
            prop_assert!(parse_request(&frame[..length]).is_err(), "cut at {}", length);
        }
        let mut longer = frame.clone();
        longer.extend_from_slice(&extra);
        prop_assert!(parse_request(&longer).is_err());
        let mut list = vec![11];
        list.extend_from_slice(&extra);
        prop_assert!(parse_request(&list).is_err());
    }
}

#[test]
fn list_answers_vault_keys_unlocked_and_nothing_locked() {
    let mut harness = Harness::stored(REVISION);
    let effects = harness.send(agent(1, vec![11]));
    assert_eq!(
        ssh_replies(&effects),
        [(AgentRequestId(1), vec![12, 0, 0, 0, 0])]
    );
    let effects = harness.unlock();
    harness.settle_sync(&effects, REVISION);
    let replies = ssh_replies(&harness.send(agent(2, vec![11])));
    let [(AgentRequestId(2), frame)] = replies.as_slice() else {
        panic!("one reply");
    };
    let mut listed = decode_identities(frame);
    listed.sort();
    let names: HashMap<Uuid, String> = vault(&harness.core)
        .index
        .ciphers
        .iter()
        .map(|row| (row.id, row.name.as_str().to_owned()))
        .collect();
    let mut expected: Vec<(Vec<u8>, String)> = SSH_KEYS
        .iter()
        .map(|key| (wire_blob(key.public), names[&key.cipher_id].clone()))
        .collect();
    expected.sort();
    assert_eq!(listed, expected);
    assert_eq!(names[&SSH_KEYS[1].cipher_id], "SSH RSA");
    assert_eq!(names[&SSH_KEYS[2].cipher_id], "SSH ECDSA P-256");
}

// Test 22: SSH agent sign.

#[test]
fn approved_sign_verifies_for_each_key_type() {
    let mut harness = Harness::unlocked(REVISION);
    let cases = [
        (&SSH_KEYS[0], 0, Some("ssh-ed25519")),
        (&SSH_KEYS[1], 2, Some("rsa-sha2-256")),
        (&SSH_KEYS[1], 4, Some("rsa-sha2-512")),
        (&SSH_KEYS[1], 0, None),
        (&SSH_KEYS[2], 0, Some("ecdsa-sha2-nistp256")),
    ];
    for (number, (key, flags, algorithm)) in (10..).zip(cases) {
        let data = format!("session {number}").into_bytes();
        let frame = sign_frame(&wire_blob(key.public), &data, flags);
        assert!(ssh_replies(&harness.send(agent(number, frame))).is_empty());
        {
            let screen = vault(&harness.core);
            let approval = screen.ssh_approval.expect("the request waits");
            assert_eq!(approval.fingerprint, key.fingerprint);
            assert_eq!(approval.id, AgentRequestId(number));
            assert!(!approval.name.is_empty());
        }
        let replies = ssh_replies(&harness.user(UserAction::ApproveSsh));
        let [(id, reply)] = replies.as_slice() else {
            panic!("one reply");
        };
        assert_eq!(*id, AgentRequestId(number));
        match algorithm {
            None => assert_eq!(reply, &[5], "plain ssh-rsa is refused"),
            Some(algorithm) => {
                assert_ne!(reply, &[5], "{algorithm} signs");
                let signature = decode_signature(reply);
                assert_eq!(signature.algorithm().as_str(), algorithm);
                let public_key = PublicKey::from_openssh(key.public).unwrap();
                Verifier::verify(&public_key, &data, &signature).expect("the signature verifies");
            }
        }
        assert!(vault(&harness.core).ssh_approval.is_none());
    }
}

#[test]
fn deny_timeout_unknown_key_and_lock_fail() {
    let mut harness = Harness::unlocked(REVISION);
    let blob = wire_blob(SSH_KEYS[0].public);
    harness.send(agent(1, sign_frame(&blob, b"deny", 0)));
    assert_eq!(
        ssh_replies(&harness.user(UserAction::DenySsh)),
        [(AgentRequestId(1), vec![5])]
    );
    let start = harness.now_ms;
    harness.send(agent(2, sign_frame(&blob, b"late", 0)));
    harness.now_ms = start + SSH_WAIT - 1;
    assert!(ssh_replies(&harness.send(Input::Tick)).is_empty());
    harness.now_ms = start + SSH_WAIT;
    assert_eq!(
        ssh_replies(&harness.send(Input::Tick)),
        [(AgentRequestId(2), vec![5])]
    );
    let mut unknown = blob.clone();
    *unknown.last_mut().unwrap() ^= 1;
    assert_eq!(
        ssh_replies(&harness.send(agent(3, sign_frame(&unknown, b"x", 0)))),
        [(AgentRequestId(3), vec![5])]
    );
    assert_eq!(
        ssh_replies(&harness.send(agent(4, vec![17]))),
        [(AgentRequestId(4), vec![5])]
    );
    harness.send(agent(5, sign_frame(&blob, b"lock", 0)));
    harness.send(agent(6, sign_frame(&blob, b"lock", 0)));
    assert_eq!(
        ssh_replies(&harness.user(UserAction::Lock)),
        [(AgentRequestId(5), vec![5]), (AgentRequestId(6), vec![5])]
    );
}

#[test]
fn locked_sign_waits_for_unlock_then_approval() {
    let mut harness = Harness::stored(REVISION);
    let key = &SSH_KEYS[2];
    let frame = sign_frame(&wire_blob(key.public), b"after unlock", 0);
    assert!(ssh_replies(&harness.send(agent(7, frame))).is_empty());
    let effects = harness.unlock();
    assert!(ssh_replies(&effects).is_empty());
    assert_eq!(
        vault(&harness.core)
            .ssh_approval
            .map(|approval| approval.fingerprint),
        Some(key.fingerprint)
    );
    let replies = ssh_replies(&harness.user(UserAction::ApproveSsh));
    let [(AgentRequestId(7), reply)] = replies.as_slice() else {
        panic!("one reply");
    };
    let public_key = PublicKey::from_openssh(key.public).unwrap();
    Verifier::verify(&public_key, b"after unlock", &decode_signature(reply))
        .expect("the signature verifies");
}

// Harness, fixtures and helpers.

/// The SshKey ciphers in `sync.json`; see `SOURCES.md`.
const SSH_KEYS: [SshFixture; 3] = [
    SshFixture {
        cipher_id: Uuid::from_u128(0x4747ede0_e366_4302_849a_f8023d70e7bd),
        public: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHRGCdOK0sas0lD2GYJ02q/4b0ho62jqQi+5xCN0ZMJR",
        fingerprint: "SHA256:ZmgqKvxbNRTD9mdMtSspaUDeAzl1fzt3iTaGLpNnA/8",
    },
    SshFixture {
        cipher_id: Uuid::from_u128(0x4757a3c2_5d1e_4c7a_9b0e_2f6a8d1c3e01),
        public: "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCpikroCgxtXOF6eONX3Jv+nHSavLC0j8qvcs+m689qmVC9yi8trDUGWDsmt7Pnt72/5BWaIhoskWW1q0IQBSTRx8fbkl8iN0bE+kNFVFrMpnr61c1inpHPS9nod57FINv7hADuNh6LqSi18imhy2+EKK4MXBez5gDABaxcPac7MT6ZwzA472QQ/O+3/j849xfI+pPJSH/cl+L6fRfQB6oAt9l73jl2/aGRtZgG6K8JzVspY9GGk4mk1cYxOTqaTCURi7zgjmvyk4aSpsZK/iMRQ/N9gKsj6Hk+T8T5bmqqhb0wOdeDNGoHuyQ9EcozGgp8+gjgRdS+bAmdrjOU2ibJ",
        fingerprint: "SHA256:OsYKc+9lhaCtKcPHVjpGlGoqD1i57PiulTHKK4KQ/EA",
    },
    SshFixture {
        cipher_id: Uuid::from_u128(0x4767b4d3_6e2f_4d8b_8c1f_3a7b9e2d4f02),
        public: "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBKvW7ZPnEVwa5zXu72R+0URYg88Aoc4yBPn5HM8Y3ZATqMOd90Np+LTAc82ijxeBwr64RtIcb4zauboLyEkBDls=",
        fingerprint: "SHA256:5ebcL6pGLO/bna7pdLuCEVZrv0dyZR2CUVwUaf894sY",
    },
];
const SSH_WAIT: u64 = 60 * 1000;

struct SshFixture {
    cipher_id: Uuid,
    /// OpenSSH public key line, without a comment.
    public: &'static str,
    fingerprint: &'static str,
}

const EMAIL: &str = "fixture-user@example.com";
const PASSWORD: &str = "fixture-test-password-1";
const KDF: Kdf = Kdf::Pbkdf2 {
    iterations: 600_000,
};
const STORED_REFRESH: &str = "stored-refresh-token";
const ORGANIZATION_ID: Uuid = Uuid::from_u128(0x6cb3dadc_aa89_4725_92d5_c767d2ffc003);
const COLLECTION_ID: Uuid = Uuid::from_u128(0x630a9d36_58ee_4b40_9b2c_93213c113712);
/// The fixture folder that holds "Example login".
const FOLDER_ID: Uuid = Uuid::from_u128(0x9cb34896_8975_41f2_8c5e_d1f7a34bb391);
/// [`START`] as an ISO date, worked out by hand.
const START_ISO: &str = "2026-11-20T18:40:00.000Z";
const DEVICE_ID: Uuid = Uuid::from_u128(0x1234);
const USER_ID: Uuid = Uuid::from_u128(0x581cff7e_18ba_4f0d_9125_ded4464f585e);
const EXAMPLE_ID: Uuid = Uuid::from_u128(0x1b40a535_3c7f_4cae_bb72_dd7399c24af1);
/// `revision_date.json`.
const REVISION: u64 = 1_795_194_978_722;
/// `cipher_put_success.json` `revisionDate` 2026-11-20T17:16:20.743050Z:
/// 20777 days and 62180 s after the epoch.
const PUT_REVISION: u64 = (20_777 * 86_400 + 62_180) * 1000 + 743;
/// 2026-11-21T00:00:00Z.
const NOV_21_MS: u64 = 20_778 * 86_400 * 1000;
const FIVE_MINUTES: u64 = 5 * 60 * 1000;
const LOCK_AFTER: u64 = 24 * 60 * 60 * 1000;
const START: u64 = 1_795_200_000_000;

static SYNC: LazyLock<SyncResponse> =
    LazyLock::new(|| SyncResponse::from_json(fixture("sync.json").1).unwrap());

// 600,000 PBKDF2 rounds are slow, so the oracle derives once.
static MASTER: LazyLock<MasterKey> =
    LazyLock::new(|| derive_master_key(PASSWORD, EMAIL, KDF).unwrap());

static KEYS: LazyLock<VaultKeys> = LazyLock::new(|| {
    let user_key = decrypt_user_key(SYNC.profile.key.as_ref().unwrap(), &MASTER).unwrap();
    VaultKeys::new(user_key, &SYNC.profile)
});

static TOKEN: LazyLock<vault_core::proto::identity::TokenResponse> =
    LazyLock::new(|| vault_core::proto::from_value(fixture("token_success.json").1).unwrap());

struct Harness {
    core: Core,
    now_ms: u64,
    rng: ChaCha20Rng,
}

impl Harness {
    fn logged_out() -> Self {
        let mut harness = Self::new();
        harness.send(Input::Started {
            stored: StoredAccount::default(),
        });
        assert!(matches!(harness.core.screen(), Screen::Login { .. }));
        harness
    }

    /// Locked, from slots that hold an account, a refresh token and a
    /// cache of the sync fixture at `revision`.
    fn stored(revision: u64) -> Self {
        Self::stored_with(revision, None)
    }

    /// [`stored`](Self::stored), with `device_wrap` in its slot.
    fn stored_with(revision: u64, device_wrap: Option<Vec<u8>>) -> Self {
        let mut harness = Self::new();
        let account = serde_json::json!({
            "email": EMAIL,
            "user_id": USER_ID,
            "kdf": { "type": "Pbkdf2", "iterations": 600_000 },
        });
        let cache = CacheDocument {
            format: CacheDocument::FORMAT,
            user_id: USER_ID,
            revision,
            synced_at: 0,
            sync: fixture("sync.json").1,
        };
        harness.send(Input::Started {
            stored: StoredAccount {
                account: Some(serde_json::to_vec(&account).unwrap()),
                refresh_token: Some(STORED_REFRESH.as_bytes().to_vec()),
                vault_cache: Some(cache.to_vec()),
                device_wrap,
                two_factor_token: None,
            },
        });
        assert!(matches!(harness.core.screen(), Screen::Locked { .. }));
        harness
    }

    /// Unlocked from storage with a fresh access token and the sync after
    /// unlock done at `revision`.
    fn unlocked(revision: u64) -> Self {
        let mut harness = Self::stored(revision);
        let effects = harness.unlock();
        harness.settle_sync(&effects, revision);
        harness
    }

    fn new() -> Self {
        Self {
            core: Core::new(Config {
                identity_url: Url::parse("https://vault.test/identity").unwrap(),
                api_url: Url::parse("https://vault.test/api").unwrap(),
                client_id: "desktop".into(),
                client_name: "desktop".into(),
                client_version: "2026.11.0".into(),
                device_type: 6,
                device_id: DEVICE_ID,
                device_name: "test".into(),
                lock_after: LOCK_AFTER,
            }),
            now_ms: START,
            rng: ChaCha20Rng::seed_from_u64(7),
        }
    }

    /// Unlocks a locked harness with the right password.
    fn unlock(&mut self) -> Vec<Effect> {
        let effects = self.user(UserAction::Unlock {
            password: secret(PASSWORD),
        });
        let (derive, ..) = one_derive(&effects);
        let effects = self.derived(derive, PASSWORD);
        assert!(matches!(self.core.screen(), Screen::Vault(_)));
        effects
    }

    /// Enrolls device unlock, answering the two asks with `first` and
    /// `second`, and returns the bytes stored in the `DeviceWrap` slot.
    fn enroll(&mut self, first: &[u8], second: &[u8]) -> Vec<u8> {
        let effects = self.user(UserAction::EnableDeviceUnlock);
        let (id, ..) = one_device_secret(&effects);
        let effects = self.device_secret(id, Ok(first));
        let (id, ..) = one_device_secret(&effects);
        let effects = self.device_secret(id, Ok(second));
        stored_bytes(&effects, StoreSlot::DeviceWrap)
    }

    /// Unlocks a locked harness with the device key's `secret`.
    fn device_unlock(&mut self, secret: &[u8]) -> Vec<Effect> {
        let effects = self.user(UserAction::DeviceUnlock);
        let (id, _, create) = one_device_secret(&effects);
        assert!(!create, "unlock never creates the key");
        let effects = self.device_secret(id, Ok(secret));
        assert!(
            matches!(self.core.screen(), Screen::Vault(_)),
            "{}",
            digest(&self.core)
        );
        effects
    }

    fn device_secret(
        &mut self,
        id: RequestId,
        result: Result<&[u8], DeviceKeyError>,
    ) -> Vec<Effect> {
        self.send(Input::DeviceSecret {
            id,
            result: result.map(|bytes| Zeroizing::new(bytes.to_vec())),
        })
    }

    /// Logs in up to the token request and returns its id.
    fn login_until_token(&mut self) -> RequestId {
        let effects = self.user(UserAction::Login {
            email: EMAIL.into(),
            password: secret(PASSWORD),
        });
        let (prelogin, _) = one_http(&effects);
        let effects = self.answer_fixture(prelogin, "prelogin.json");
        let (derive, ..) = one_derive(&effects);
        let effects = self.derived(derive, PASSWORD);
        one_http(&effects).0
    }

    /// [`login_until_token`](Self::login_until_token), and the token
    /// request's form.
    fn login_until_token_form(&mut self) -> (RequestId, HashMap<&'static str, String>) {
        let effects = self.user(UserAction::Login {
            email: EMAIL.into(),
            password: secret(PASSWORD),
        });
        let (prelogin, _) = one_http(&effects);
        let effects = self.answer_fixture(prelogin, "prelogin.json");
        let (derive, ..) = one_derive(&effects);
        let effects = self.derived(derive, PASSWORD);
        let (token, request) = one_http(&effects);
        let form = form(request)
            .into_iter()
            .map(|(name, value)| (name, value.to_owned()))
            .collect();
        (token, form)
    }

    /// Answers the refresh, revision check and sync that `effects` start,
    /// with the sync fixture, and returns the effects of the sync answer.
    fn settle_sync(&mut self, effects: &[Effect], revision: u64) -> Vec<Effect> {
        self.settle_sync_with(effects, revision, &fixture("sync.json").1)
    }

    fn settle_sync_with(&mut self, effects: &[Effect], revision: u64, sync: &Value) -> Vec<Effect> {
        let mut effects: Vec<Effect> = effects.iter().filter_map(copy_http).collect();
        loop {
            let Some((id, url)) = http(&effects)
                .into_iter()
                .map(|(id, request)| (id, request.url.to_string()))
                .next_back()
            else {
                panic!("no request left to settle");
            };
            effects = if url.ends_with("/connect/token") {
                self.answer_fixture(id, "token_success.json")
            } else if url.ends_with("/accounts/revision-date") {
                self.answer(id, 200, &Value::from(revision))
            } else if url.ends_with("/sync?excludeDomains=true") {
                return self.answer(id, 200, sync);
            } else {
                panic!("unexpected request {url}");
            };
        }
    }

    fn send(&mut self, input: Input) -> Vec<Effect> {
        self.core.handle(self.now_ms, &mut self.rng, input)
    }

    fn user(&mut self, action: UserAction) -> Vec<Effect> {
        self.send(Input::User(action))
    }

    fn answer(&mut self, id: RequestId, status: u16, body: &Value) -> Vec<Effect> {
        self.send(Input::Http {
            id,
            response: Ok(HttpResponse {
                status,
                body: serde_json::to_vec(body).unwrap(),
            }),
        })
    }

    fn answer_fixture(&mut self, id: RequestId, name: &str) -> Vec<Effect> {
        let (status, body) = fixture(name);
        self.answer(id, status, &body)
    }

    fn derived(&mut self, id: RequestId, password: &str) -> Vec<Effect> {
        let result = derive_master_key(password, EMAIL, KDF);
        self.send(Input::Derived { id, result })
    }

    /// Answers `item` as the server or the shell would. `live` derives the
    /// real key; a stale derive gets a cheap wrong one.
    fn reply(&mut self, item: &Pending, live: bool) -> Vec<Effect> {
        match item {
            Pending::Derive(id) if live => self.derived(*id, PASSWORD),
            Pending::Derive(id) => {
                let result = derive_master_key("x", "y", Kdf::Pbkdf2 { iterations: 5000 });
                self.send(Input::Derived { id: *id, result })
            }
            Pending::Store(id) => self.send(Input::Stored {
                id: *id,
                result: Ok(()),
            }),
            Pending::Http(id, url) if url.ends_with("/connect/token") => {
                self.answer_fixture(*id, "token_success.json")
            }
            Pending::Http(id, url) if url.ends_with("/accounts/revision-date") => {
                self.answer(*id, 200, &Value::from(REVISION + 1))
            }
            Pending::Http(id, _) => self.answer_fixture(*id, "sync.json"),
        }
    }
}

#[derive(Clone, Debug)]
enum Pending {
    Http(RequestId, String),
    Derive(RequestId),
    Store(RequestId),
}

impl Pending {
    fn from_effect(effect: &Effect) -> Option<Self> {
        match effect {
            Effect::Http { id, request } => Some(Self::Http(*id, request.url.to_string())),
            Effect::Derive { id, .. } => Some(Self::Derive(*id)),
            Effect::Store { id, .. } => Some(Self::Store(*id)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Unlock,
    Lock,
    Sync,
    Answer(usize),
    Stale(usize),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        1 => Just(Step::Unlock),
        1 => Just(Step::Lock),
        1 => Just(Step::Sync),
        3 => any::<usize>().prop_map(Step::Answer),
        2 => any::<usize>().prop_map(Step::Stale),
    ]
}

#[derive(Clone, Debug)]
enum CacheEvent {
    Stored {
        revision: u64,
        other_user: bool,
        format: u32,
    },
    Synced {
        revision: u64,
    },
}

fn cache_event() -> impl Strategy<Value = CacheEvent> {
    let revision = REVISION - 5..REVISION + 20;
    prop_oneof![
        (
            revision.clone(),
            any::<bool>(),
            prop_oneof![Just(1u32), Just(2u32)]
        )
            .prop_map(|(revision, other_user, format)| CacheEvent::Stored {
                revision,
                other_user,
                format,
            }),
        revision.prop_map(|revision| CacheEvent::Synced { revision }),
    ]
}

/// The screen as text, to compare before and after an input.
fn digest(core: &Core) -> String {
    match core.screen() {
        Screen::Starting => "starting".into(),
        Screen::Login { email, error, busy } => format!("login {email} {error:?} {busy}"),
        Screen::SecondFactor {
            email,
            challenge,
            error,
        } => format!("second factor {email} {challenge:?} {error:?}"),
        Screen::Locked {
            email,
            error,
            busy,
            device_unlock,
        } => format!("locked {email} {error:?} {busy} {device_unlock}"),
        Screen::Vault(screen) => {
            let names: Vec<&str> = screen
                .index
                .ciphers
                .iter()
                .map(|row| row.name.as_str())
                .collect();
            format!(
                "vault {names:?} {} {:?} {:?} {:?} {:?} {}",
                screen.syncing,
                screen.sync_error,
                screen.error,
                screen.revision,
                screen.synced_at,
                screen.open.is_some()
            )
        }
    }
}

fn assert_logged_in(harness: &mut Harness, effects: &[Effect]) {
    // A remembered second factor may add its own slot.
    let slots: Vec<StoreSlot> = slots(effects)
        .into_iter()
        .filter(|slot| *slot != StoreSlot::TwoFactorToken)
        .collect();
    assert_eq!(slots, [StoreSlot::Account, StoreSlot::RefreshToken]);
    assert_eq!(
        stored_bytes(effects, StoreSlot::RefreshToken),
        TOKEN.refresh_token.as_ref().unwrap().as_bytes()
    );
    let account: Value =
        serde_json::from_slice(&stored_bytes(effects, StoreSlot::Account)).unwrap();
    assert_eq!(account["email"], EMAIL);
    assert_eq!(account["user_id"], USER_ID.to_string());
    let (_, request) = one_http(effects);
    expect![[r#"
        GET https://vault.test/api/accounts/revision-date
        Authorization: Bearer <secret>
        Bitwarden-Client-Name: desktop
        Bitwarden-Client-Version: 2026.11.0
        Device-Type: 6
    "#]]
    .assert_eq(&render(request, &[]));
    assert_eq!(bearer(request), Some(TOKEN.access_token.as_str()));
    let screen = vault(&harness.core);
    assert!(screen.index.ciphers.is_empty());
    assert!(screen.syncing);
    let effects = harness.settle_sync(effects, REVISION);
    let stored = stored_document(&effects);
    assert_eq!(stored.revision, REVISION);
    assert_eq!(stored.synced_at, harness.now_ms);
    let screen = vault(&harness.core);
    assert!(!screen.syncing);
    assert_eq!(screen.revision, Some(REVISION));
    let expected = VaultIndex::decrypt(&SYNC.folders, &SYNC.ciphers, &KEYS);
    let names = |index: &VaultIndex| -> Vec<String> {
        index
            .ciphers
            .iter()
            .map(|row| row.name.to_string())
            .collect()
    };
    assert_eq!(names(screen.index), names(&expected));
}

fn assert_wrong_password(name: &str) {
    let mut harness = Harness::logged_out();
    let token = harness.login_until_token();
    let effects = harness.answer_fixture(token, name);
    assert!(http(&effects).is_empty());
    let Screen::Login {
        email,
        error: Some(LoginError::Rejected(message)),
        busy: false,
    } = harness.core.screen()
    else {
        panic!("not a rejected login");
    };
    assert_eq!(email, EMAIL);
    assert!(message.contains("incorrect"), "{message}");
}

fn vault(core: &Core) -> VaultScreen<'_> {
    match core.screen() {
        Screen::Vault(screen) => screen,
        _ => panic!("not unlocked: {}", digest(core)),
    }
}

/// The sync fixture's "Example login", decrypted, renamed to `name`.
fn edited(name: &str) -> CipherView {
    let mut view = CipherView::decrypt(&example_cipher(), &KEYS).unwrap();
    view.name = secret(name);
    view
}

fn example_cipher() -> CipherResponse {
    SYNC.ciphers
        .iter()
        .find(|cipher| cipher.id == EXAMPLE_ID)
        .unwrap()
        .clone()
}

fn put_success_cipher() -> CipherResponse {
    let Value::Object(object) = fixture("cipher_put_success.json").1 else {
        panic!("not an object");
    };
    CipherResponse::from_object(object).unwrap()
}

fn http(effects: &[Effect]) -> Vec<(RequestId, &HttpRequest)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Http { id, request } => Some((*id, request)),
            _ => None,
        })
        .collect()
}

fn one_http(effects: &[Effect]) -> (RequestId, &HttpRequest) {
    match http(effects).as_slice() {
        [one] => *one,
        other => panic!(
            "expected one request, got {:?}",
            other
                .iter()
                .map(|(_, request)| &request.url)
                .collect::<Vec<_>>()
        ),
    }
}

fn copy_http(effect: &Effect) -> Option<Effect> {
    match effect {
        Effect::Http { id, request } => Some(Effect::Http {
            id: *id,
            request: HttpRequest {
                method: request.method,
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            },
        }),
        _ => None,
    }
}

fn one_derive(effects: &[Effect]) -> (RequestId, &SecretString, &String, Kdf) {
    let derives: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Derive {
                id,
                password,
                email,
                kdf,
            } => Some((*id, password, email, *kdf)),
            _ => None,
        })
        .collect();
    match derives.as_slice() {
        [one] => *one,
        _ => panic!("expected one derive, got {}", derives.len()),
    }
}

fn form(request: &HttpRequest) -> HashMap<&'static str, &str> {
    let Body::Form(pairs) = &request.body else {
        panic!("not a form body");
    };
    pairs
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect()
}

fn json_object(request: &HttpRequest) -> Map<String, Value> {
    match &request.body {
        Body::Json(Value::Object(object)) => object.clone(),
        _ => panic!("not a JSON object body"),
    }
}

/// `request` as text for a snapshot: `METHOD url`, the headers sorted, then
/// the body, as JSON with sorted keys or as form pairs one per line, sorted.
/// EncStrings show as `<enc>`, ids as `<id>`, dates as `<date>` and
/// passwords, codes and tokens as `<secret>`, except under a key in `keep`.
fn render(request: &HttpRequest, keep: &[&str]) -> String {
    let method = match request.method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Delete => "DELETE",
    };
    let mut lines = vec![format!("{method} {}", request.url)];
    let mut headers: Vec<String> = request
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {}", redact(name, value, keep)))
        .collect();
    headers.sort();
    lines.extend(headers);
    match &request.body {
        Body::None => {}
        Body::Json(value) => {
            lines.push(String::new());
            let value = redact_json("", value, keep);
            lines.push(serde_json::to_string_pretty(&value).unwrap());
        }
        Body::Form(pairs) => {
            lines.push(String::new());
            let mut pairs: Vec<String> = pairs
                .iter()
                .map(|(key, value)| format!("{key}={}", redact(key, value, keep)))
                .collect();
            pairs.sort();
            lines.extend(pairs);
        }
    }
    lines.join("\n") + "\n"
}

/// `value` with each object's keys sorted and each string redacted by
/// [`redact`] under the key that holds it.
fn redact_json(key: &str, value: &Value, keep: &[&str]) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<(&String, &Value)> = object.iter().collect();
            entries.sort_by_key(|(key, _)| *key);
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), redact_json(key, value, keep)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_json(key, item, keep))
                .collect(),
        ),
        Value::String(text) => Value::String(redact(key, text, keep)),
        other => other.clone(),
    }
}

fn redact(key: &str, value: &str, keep: &[&str]) -> String {
    const SECRETS: &[&str] = &[
        "password",
        "masterPasswordHash",
        "twoFactorToken",
        "newDeviceOtp",
        "client_secret",
        "refresh_token",
    ];
    if keep.contains(&key) {
        value.to_owned()
    } else if value.parse::<EncString>().is_ok() {
        "<enc>".into()
    } else if key == "Authorization" {
        "Bearer <secret>".into()
    } else if SECRETS.contains(&key) {
        "<secret>".into()
    } else if key == "id"
        || key.ends_with("Id")
        || key.ends_with("Ids")
        || key == "deviceIdentifier"
    {
        "<id>".into()
    } else if key.ends_with("Date") {
        "<date>".into()
    } else {
        value.to_owned()
    }
}

fn bearer(request: &HttpRequest) -> Option<&str> {
    request
        .headers
        .iter()
        .find(|(name, _)| *name == "Authorization")
        .and_then(|(_, value)| value.strip_prefix("Bearer "))
}

fn one_device_secret(effects: &[Effect]) -> (RequestId, [u8; 32], bool) {
    let asks: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::DeviceSecret {
                id,
                challenge,
                create,
            } => Some((*id, *challenge, *create)),
            _ => None,
        })
        .collect();
    match asks.as_slice() {
        [one] => *one,
        other => panic!("expected one device-secret ask, got {}", other.len()),
    }
}

fn slots(effects: &[Effect]) -> Vec<StoreSlot> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Store { slot, .. } => Some(*slot),
            _ => None,
        })
        .collect()
}

fn cleared(effects: &[Effect]) -> Vec<StoreSlot> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Clear { slot } => Some(*slot),
            _ => None,
        })
        .collect()
}

fn stored_bytes(effects: &[Effect], wanted: StoreSlot) -> Vec<u8> {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Store { slot, bytes, .. } if *slot == wanted => Some(bytes.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no store of {wanted:?}"))
}

fn stored_document(effects: &[Effect]) -> CacheDocument {
    CacheDocument::from_slice(&stored_bytes(effects, StoreSlot::VaultCache)).unwrap()
}

/// A type 2 string from a request body, decrypted with the user key.
fn decrypt_user_text(value: &Value) -> String {
    let encrypted: EncString = value.as_str().expect("a string").parse().unwrap();
    String::from_utf8(encrypted.decrypt(KEYS.user()).unwrap().to_vec()).unwrap()
}

/// The decrypted name of folder `id` on the vault screen.
fn folder_name(core: &Core, id: Uuid) -> Option<String> {
    vault(core)
        .index
        .folders
        .iter()
        .find(|folder| folder.id == id)
        .map(|folder| folder.name.to_string())
}

/// The sync body of the cache document stored by `effects`.
fn stored_sync(effects: &[Effect]) -> SyncResponse {
    SyncResponse::from_json(stored_document(effects).sync).unwrap()
}

fn login_password(view: &CipherView) -> Option<String> {
    match &view.content {
        CipherContent::Login(login) => login.password.as_ref().map(|text| text.to_string()),
        _ => None,
    }
}
fn secret(text: &str) -> SecretString {
    Zeroizing::new(text.to_owned())
}

fn fixture(name: &str) -> (u16, Value) {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let mut file: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let status = file["status"].as_u64().unwrap().try_into().unwrap();
    (status, file["body"].take())
}

/// The plaintext `keyValue` of the passkey in `sync.json` (see `SOURCES.md`).
const FIXTURE_KEY_VALUE: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgPzagifK8K2Q2B5J-aFfT1VEQQLqsfY_kBl1B21YNfFqhRANCAARGumfqpTsKwcQ7pwuvelYmfl3oBrYn3oxBVNLMkl0sFRjvrj1Sg0403Um5r-9vMD098TNKtVSBHREZ2WEMJjCn";
/// How long a passkey request waits for unlock, per the plan.
const PASSKEY_WAIT: u64 = 120 * 1000;

fn get_assertion(id: u64, rp_id: &str, client_data_hash: [u8; 32], allow: Vec<Vec<u8>>) -> Input {
    Input::Passkey {
        id: RequestId::from_shell(id),
        request: PasskeyRequest::GetAssertion {
            rp_id: rp_id.into(),
            client_data_hash,
            allow,
        },
    }
}

fn passkey_cipher() -> CipherResponse {
    SYNC.ciphers
        .iter()
        .find(|cipher| {
            cipher
                .login
                .as_ref()
                .is_some_and(|login| !login.fido2_credentials.is_empty())
        })
        .unwrap()
        .clone()
}

/// The 16 bytes of the fixture passkey's GUID.
fn fixture_credential_id() -> Vec<u8> {
    let view = CipherView::decrypt(&passkey_cipher(), &KEYS).unwrap();
    let CipherContent::Login(login) = view.content else {
        panic!("the passkey cipher is a login");
    };
    Uuid::parse_str(&login.fido2_credentials[0].credential_id)
        .unwrap()
        .as_bytes()
        .to_vec()
}

fn fixture_verifying_key() -> VerifyingKey {
    let der = URL_SAFE_NO_PAD.decode(FIXTURE_KEY_VALUE).unwrap();
    VerifyingKey::from(&p256::SecretKey::from_pkcs8_der(&der).unwrap().public_key())
}

/// True when `signature` (DER) signs `authenticator_data || client_data_hash`.
fn verifies(
    key: &VerifyingKey,
    authenticator_data: &[u8],
    client_data_hash: &[u8; 32],
    signature: &[u8],
) -> bool {
    let signed = [authenticator_data, client_data_hash].concat();
    Signature::from_der(signature).is_ok_and(|signature| key.verify(&signed, &signature).is_ok())
}

/// WebAuthn authenticator data, read by offset.
struct AuthenticatorData {
    rp_id_hash: Vec<u8>,
    flags: u8,
    counter: u32,
    attested: Option<Attested>,
}

struct Attested {
    credential_id: Vec<u8>,
    public_key_cose: Vec<u8>,
}

impl AuthenticatorData {
    fn parse(bytes: &[u8]) -> Self {
        let flags = bytes[32];
        let attested = (flags & 0x40 != 0).then(|| {
            // 16 bytes of AAGUID at 37, then a two-byte length at 53.
            let length = usize::from(u16::from_be_bytes([bytes[53], bytes[54]]));
            Attested {
                credential_id: bytes[55..55 + length].to_vec(),
                public_key_cose: bytes[55 + length..].to_vec(),
            }
        });
        Self {
            rp_id_hash: bytes[..32].to_vec(),
            flags,
            counter: u32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]),
            attested,
        }
    }
}

/// A CBOR value, as far as passkey replies use it.
#[derive(Debug, PartialEq)]
enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Map(Vec<(Cbor, Cbor)>),
}

/// Decodes one CBOR item that fills `bytes`.
fn cbor(bytes: &[u8]) -> Cbor {
    let mut rest = bytes;
    let value = cbor_item(&mut rest);
    assert!(rest.is_empty(), "trailing bytes");
    value
}

fn cbor_item(rest: &mut &[u8]) -> Cbor {
    let mut take = |count: usize| {
        let (taken, left) = rest.split_at(count);
        *rest = left;
        taken.to_vec()
    };
    let first = take(1)[0];
    let argument = match first & 0x1f {
        small @ 0..24 => u64::from(small),
        24 => u64::from(take(1)[0]),
        25 => u64::from(u16::from_be_bytes(take(2).try_into().unwrap())),
        26 => u64::from(u32::from_be_bytes(take(4).try_into().unwrap())),
        27 => u64::from_be_bytes(take(8).try_into().unwrap()),
        other => panic!("unsupported additional info {other}"),
    };
    match first >> 5 {
        0 => Cbor::Int(argument as i64),
        1 => Cbor::Int(-1 - argument as i64),
        2 => Cbor::Bytes(take(argument as usize)),
        3 => Cbor::Text(String::from_utf8(take(argument as usize)).unwrap()),
        5 => Cbor::Map(
            (0..argument)
                .map(|_| (cbor_item(rest), cbor_item(rest)))
                .collect(),
        ),
        major => panic!("unsupported major type {major}"),
    }
}

fn map_get<'a>(map: &'a [(Cbor, Cbor)], key: &Cbor) -> &'a Cbor {
    &map.iter()
        .find(|(candidate, _)| candidate == key)
        .unwrap_or_else(|| panic!("no key {key:?}"))
        .1
}

/// One API server holding the fixture passkey login, shared by two cores.
/// It takes a write only when `lastKnownRevisionDate` is its current one.
struct Server {
    cipher: Map<String, Value>,
    writes: u64,
}

impl Server {
    fn new() -> Self {
        Self {
            cipher: passkey_cipher().raw,
            writes: 0,
        }
    }

    /// The account revision (epoch ms): `revision_date.json` before any
    /// write, then one second after 2026-11-21T00:00:00Z per write.
    fn revision(&self) -> u64 {
        match self.writes {
            0 => REVISION,
            writes => NOV_21_MS + writes * 1000,
        }
    }

    fn answer(
        &mut self,
        harness: &mut Harness,
        id: RequestId,
        request: &HttpRequest,
    ) -> Vec<Effect> {
        let url = request.url.as_str();
        if url.ends_with("/connect/token") {
            harness.answer_fixture(id, "token_success.json")
        } else if url.ends_with("/accounts/revision-date") {
            harness.answer(id, 200, &Value::from(self.revision()))
        } else if url.ends_with("/sync?excludeDomains=true") {
            let mut sync = fixture("sync.json").1;
            for cipher in sync["ciphers"].as_array_mut().unwrap() {
                if cipher["id"] == self.cipher["id"] {
                    *cipher = Value::Object(self.cipher.clone());
                }
            }
            harness.answer(id, 200, &sync)
        } else if request.method == Method::Put {
            let mut body = json_object(request);
            if body.remove("lastKnownRevisionDate") != self.cipher.get("revisionDate").cloned() {
                return harness.answer_fixture(id, "cipher_put_out_of_date.json");
            }
            body.remove("encryptedFor");
            self.writes += 1;
            body.insert(
                "revisionDate".into(),
                Value::from(format!(
                    "2026-11-21T00:{:02}:{:02}.000Z",
                    self.writes / 60,
                    self.writes % 60
                )),
            );
            self.cipher = body.clone();
            harness.answer(id, 200, &Value::Object(body))
        } else {
            panic!("unexpected request {url}");
        }
    }

    fn counter(&self) -> u32 {
        let cipher = CipherResponse::from_object(self.cipher.clone()).unwrap();
        let CipherContent::Login(login) = CipherView::decrypt(&cipher, &KEYS).unwrap().content
        else {
            panic!("the passkey cipher is a login");
        };
        login.fido2_credentials[0].counter
    }
}

fn passkey_replies(effects: &[Effect]) -> Vec<(RequestId, Result<PasskeyReply, PasskeyError>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::PasskeyReply { id, reply } => Some((*id, reply.clone())),
            _ => None,
        })
        .collect()
}

fn catalogs(effects: &[Effect]) -> Vec<Vec<PasskeyCatalogEntry>> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::PasskeyCatalog { entries } => Some(entries.clone()),
            _ => None,
        })
        .collect()
}

fn agent(id: u64, frame: Vec<u8>) -> Input {
    Input::SshAgent {
        id: AgentRequestId(id),
        frame,
    }
}

fn ssh_replies(effects: &[Effect]) -> Vec<(AgentRequestId, Vec<u8>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SshAgentReply { id, frame } => Some((*id, frame.clone())),
            _ => None,
        })
        .collect()
}

/// The key blob of an OpenSSH public key line: its base64 field.
fn wire_blob(public: &str) -> Vec<u8> {
    let field = public.split(' ').nth(1).unwrap();
    base64::engine::general_purpose::STANDARD
        .decode(field)
        .unwrap()
}

/// `SSH_AGENTC_SIGN_REQUEST` laid out by hand from the draft: type 13,
/// string key blob, string data, uint32 flags.
fn sign_frame(public_key: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
    let length = |bytes: &[u8]| (bytes.len() as u32).to_be_bytes();
    [
        &[13][..],
        &length(public_key),
        public_key,
        &length(data),
        data,
        &flags.to_be_bytes(),
    ]
    .concat()
}

/// Splits a big-endian u32 length and that many bytes off `bytes`.
fn split_string(bytes: &[u8]) -> (&[u8], &[u8]) {
    let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    bytes[4..].split_at(length)
}

/// The (key blob, comment) pairs of an `SSH_AGENT_IDENTITIES_ANSWER`.
fn decode_identities(frame: &[u8]) -> Vec<(Vec<u8>, String)> {
    assert_eq!(frame[0], 12);
    let count = u32::from_be_bytes(frame[1..5].try_into().unwrap());
    let mut rest = &frame[5..];
    let pairs = (0..count)
        .map(|_| {
            let (blob, after) = split_string(rest);
            let (comment, after) = split_string(after);
            rest = after;
            (blob.to_vec(), String::from_utf8(comment.to_vec()).unwrap())
        })
        .collect();
    assert!(rest.is_empty(), "no bytes after the last key");
    pairs
}

/// The signature in an `SSH_AGENT_SIGN_RESPONSE`.
fn decode_signature(frame: &[u8]) -> ssh_key::Signature {
    assert_eq!(frame[0], 14, "a sign response");
    let (blob, rest) = split_string(&frame[1..]);
    assert!(rest.is_empty());
    ssh_key::Signature::try_from(blob).unwrap()
}

/// Random bytes, and sign frames with random fields, cuts and tails.
fn agent_frame() -> impl Strategy<Value = Vec<u8>> {
    let bytes = || prop::collection::vec(any::<u8>(), 0..24);
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..48),
        bytes().prop_map(|tail| [&[11][..], &tail].concat()),
        bytes().prop_map(|tail| [&[13][..], &tail].concat()),
        (bytes(), bytes(), any::<u32>(), 0..80usize, bytes()).prop_map(
            |(public_key, data, flags, cut, tail)| {
                let mut frame = sign_frame(&public_key, &data, flags);
                frame.truncate(cut.max(1));
                frame.extend_from_slice(&tail);
                frame
            }
        ),
    ]
}

fn owned_http(effects: &[Effect]) -> Vec<(RequestId, HttpRequest)> {
    effects
        .iter()
        .filter_map(copy_http)
        .map(|effect| match effect {
            Effect::Http { id, request } => (id, request),
            _ => unreachable!("copy_http copies only requests"),
        })
        .collect()
}
