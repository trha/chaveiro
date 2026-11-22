//! The client state machine: inputs in, effects out.

pub mod passkey;
mod request;
pub mod ssh_agent;

pub use passkey::{
    PasskeyCatalogEntry, PasskeyChoice, PasskeyError, PasskeyPrompt, PasskeyReply, PasskeyRequest,
};
pub use request::RequestId;
pub use ssh_agent::{AgentRequestId, SshApproval, SshAsk};

use crate::{
    crypto::{EncString, Kdf, KdfError, MasterKey, SecretString, SymmetricKey, device_key},
    proto::{
        self,
        cache::CacheDocument,
        cipher::{CipherResponse, FolderResponse, create_body, folder_body, is_out_of_date},
        identity::{ErrorResponse, PreloginResponse, TokenFailure, TokenResponse, prelogin_body},
        sync::{Profile, SyncResponse},
    },
    vault::{CipherContent, CipherView, VaultError, VaultIndex, VaultKeys, decrypt_user_key},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use passkey::{Pending, Target};
use rand_core::CryptoRngCore;
use request::{
    ApiCall, ApiKind, CounterBump, FolderWrite, PasskeyWrite, Purpose, Table, Write, WriteAction,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::mem;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The whole client: login, unlock, sync, the cache and cipher writes. It
/// does no I/O: [`handle`](Self::handle) takes one input and returns the
/// effects the shell must run, and each effect's answer comes back as a
/// later input with the same [`RequestId`].
pub struct Core {
    config: Config,
    state: State,
    requests: Table,
    /// See [`VaultScreen::index_revision`]. It counts across sessions, so a
    /// new unlock never repeats a number the shell saw before.
    index_revision: u64,
    /// The remembered second factor from [`StoreSlot::TwoFactorToken`]. It
    /// lives outside [`State`] because it outlives logout.
    remembered: Option<RememberedLogin>,
    /// The last [`Input::Settings`]. It outlives every state.
    settings: Settings,
}

impl Core {
    /// A core that waits for [`Input::Started`].
    pub fn new(config: Config) -> Self {
        Self {
            config,
            state: State::Starting,
            requests: Table::default(),
            index_revision: 0,
            remembered: None,
            settings: Settings::default(),
        }
    }

    /// One step at Unix time `now_ms`.
    ///
    /// An answer ([`Input::Http`], [`Input::Derived`], [`Input::Stored`],
    /// [`Input::DeviceSecret`]) whose id the core no longer waits for, for
    /// example one sent before a lock, changes nothing and returns no
    /// effects. An input that does not fit the current state is ignored.
    pub fn handle(
        &mut self,
        now_ms: u64,
        rng: &mut impl CryptoRngCore,
        input: Input,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        // Every state reads the remembered token the same way.
        if let Input::StorageChanged {
            slot: StoreSlot::TwoFactorToken,
            bytes,
        } = &input
        {
            self.remembered = bytes.as_deref().and_then(RememberedLogin::parse);
            return effects;
        }
        // Every state keeps the settings the same way. A new mode can free
        // requests that wait for approval.
        if let Input::Settings(settings) = input {
            self.settings = settings;
            if let State::Unlocked(unlocked) = &mut self.state {
                release_ssh(unlocked, &self.settings, &mut effects);
            }
            return effects;
        }
        let purpose = match &input {
            Input::Http { id, .. }
            | Input::Derived { id, .. }
            | Input::Stored { id, .. }
            | Input::DeviceSecret { id, .. } => match self.requests.finish(*id) {
                Some(purpose) => Some(purpose),
                None => return effects,
            },
            Input::Started { .. }
            | Input::User(_)
            | Input::Tick
            | Input::Passkey { .. }
            | Input::SshAgent { .. }
            | Input::StorageChanged { .. }
            | Input::Settings(_) => None,
        };
        let config = &self.config;
        let requests = &mut self.requests;
        let index_revision = &mut self.index_revision;
        let remembered = &mut self.remembered;
        let settings = &self.settings;
        let mut state = mem::replace(&mut self.state, State::Starting);
        if let (State::Unlocked(unlocked), Input::User(_)) = (&mut state, &input) {
            unlocked.last_activity = now_ms;
        }
        self.state = match (state, input, purpose) {
            (
                state @ (State::Starting
                | State::LoggedOut { .. }
                | State::Prelogin { .. }
                | State::Deriving { .. }
                | State::TokenRequest { .. }
                | State::SecondFactor { .. }
                | State::Locked { .. }
                | State::Unlocked(_)),
                Input::Stored { result, .. },
                None
                | Some(
                    Purpose::Prelogin
                    | Purpose::Derive
                    | Purpose::Token
                    | Purpose::SendEmailCode
                    | Purpose::Refresh
                    | Purpose::Api(_)
                    | Purpose::Store
                    | Purpose::DeviceEnroll { .. }
                    | Purpose::DeviceUnlock,
                ),
            ) => {
                // A lost write is not fatal: a lost cache leaves the older
                // document, which the revision rule replaces on the next
                // sync, and a lost refresh token means the next start asks
                // for a login.
                drop(result);
                state
            }
            (State::Locked { mut account, error }, Input::Passkey { id, request }, None) => {
                let pending = Pending {
                    id,
                    request,
                    deadline: now_ms.saturating_add(passkey::WAIT),
                    choices: Vec::new(),
                };
                hold_passkey(&mut account.passkey, pending, &mut effects);
                effects.push(Effect::Rerender);
                State::Locked { account, error }
            }
            (
                State::Deriving {
                    email,
                    password,
                    kdf,
                    purpose: DerivePurpose::Unlock(mut account),
                    request,
                },
                Input::Passkey {
                    id,
                    request: passkey_request,
                },
                None,
            ) => {
                let pending = Pending {
                    id,
                    request: passkey_request,
                    deadline: now_ms.saturating_add(passkey::WAIT),
                    choices: Vec::new(),
                };
                hold_passkey(&mut account.passkey, pending, &mut effects);
                effects.push(Effect::Rerender);
                State::Deriving {
                    email,
                    password,
                    kdf,
                    purpose: DerivePurpose::Unlock(account),
                    request,
                }
            }
            (State::Unlocked(mut unlocked), Input::Passkey { id, request }, None) => {
                let pending = Pending {
                    id,
                    request,
                    deadline: now_ms.saturating_add(passkey::WAIT),
                    choices: Vec::new(),
                };
                serve_passkey(
                    config,
                    requests,
                    &mut unlocked,
                    now_ms,
                    rng,
                    pending,
                    &mut effects,
                );
                State::Unlocked(unlocked)
            }
            (
                state @ (State::Starting
                | State::LoggedOut { .. }
                | State::Prelogin { .. }
                | State::Deriving {
                    purpose: DerivePurpose::Login(_),
                    ..
                }
                | State::TokenRequest { .. }
                | State::SecondFactor { .. }),
                Input::Passkey { id, .. },
                None,
            ) => {
                // Logged out or logging in: there is no vault to wait for.
                effects.push(Effect::PasskeyReply {
                    id,
                    reply: Err(PasskeyError::NotAllowed),
                });
                state
            }
            (State::Locked { mut account, error }, Input::SshAgent { id, frame }, None) => {
                hold_ssh(&mut account.ssh, now_ms, id, &frame, &mut effects);
                State::Locked { account, error }
            }
            (
                State::Deriving {
                    email,
                    password,
                    kdf,
                    purpose: DerivePurpose::Unlock(mut account),
                    request,
                },
                Input::SshAgent { id, frame },
                None,
            ) => {
                hold_ssh(&mut account.ssh, now_ms, id, &frame, &mut effects);
                State::Deriving {
                    email,
                    password,
                    kdf,
                    purpose: DerivePurpose::Unlock(account),
                    request,
                }
            }
            (State::Unlocked(mut unlocked), Input::SshAgent { id, frame }, None) => {
                match ssh_agent::parse_request(&frame) {
                    Ok(ssh_agent::AgentRequest::List) => {
                        let keys: Vec<(&[u8], &str)> = unlocked
                            .index
                            .ssh_keys
                            .iter()
                            .map(|key| {
                                (
                                    key.public_key.as_slice(),
                                    ssh_key_name(&unlocked.index, key.cipher_id),
                                )
                            })
                            .collect();
                        effects.push(Effect::SshAgentReply {
                            id,
                            frame: ssh_agent::identities_reply(&keys),
                        });
                    }
                    Ok(ssh_agent::AgentRequest::Sign {
                        public_key,
                        data,
                        flags,
                    }) if unlocked
                        .index
                        .ssh_keys
                        .iter()
                        .any(|key| key.public_key == public_key) =>
                    {
                        unlocked.account.ssh.push(ssh_agent::Pending {
                            id,
                            public_key,
                            data,
                            flags,
                            deadline: now_ms.saturating_add(ssh_agent::WAIT),
                        });
                        // A key that need not ask signs at once, so the
                        // shell sees a rerender only for a new prompt.
                        if !release_ssh(&mut unlocked, settings, &mut effects) {
                            effects.push(Effect::Rerender);
                        }
                    }
                    Ok(
                        ssh_agent::AgentRequest::Sign { .. } | ssh_agent::AgentRequest::Other(_),
                    )
                    | Err(ssh_agent::AgentParseError) => fail_ssh(id, &mut effects),
                }
                State::Unlocked(unlocked)
            }
            (
                state @ (State::Starting
                | State::LoggedOut { .. }
                | State::Prelogin { .. }
                | State::Deriving {
                    purpose: DerivePurpose::Login(_),
                    ..
                }
                | State::TokenRequest { .. }
                | State::SecondFactor { .. }),
                Input::SshAgent { id, frame, .. },
                None,
            ) => {
                // Logged out or logging in: no keys, and no vault to wait for.
                match ssh_agent::parse_request(&frame) {
                    Ok(ssh_agent::AgentRequest::List) => effects.push(Effect::SshAgentReply {
                        id,
                        frame: ssh_agent::identities_reply(&[]),
                    }),
                    Ok(
                        ssh_agent::AgentRequest::Sign { .. } | ssh_agent::AgentRequest::Other(_),
                    )
                    | Err(ssh_agent::AgentParseError) => fail_ssh(id, &mut effects),
                }
                state
            }
            (State::Starting, Input::Started { stored }, None) => {
                effects.push(Effect::Rerender);
                *remembered = stored
                    .two_factor_token
                    .as_deref()
                    .and_then(RememberedLogin::parse);
                let record = stored
                    .account
                    .as_deref()
                    .and_then(|bytes| serde_json::from_slice::<AccountRecord>(bytes).ok());
                // An account without a usable server boots logged out, with
                // its email kept.
                let server = record
                    .as_ref()
                    .and_then(|record| record.server.clone())
                    .and_then(|url| Server::new(url).ok());
                match (record, server) {
                    (None, _) => State::LoggedOut {
                        form: LoginForm::default(),
                        error: None,
                    },
                    (Some(record), None) => State::LoggedOut {
                        form: LoginForm {
                            server: None,
                            email: record.email,
                        },
                        error: None,
                    },
                    (Some(record), Some(server)) => {
                        let refresh = stored
                            .refresh_token
                            .and_then(|bytes| String::from_utf8(bytes).ok())
                            .map(Zeroizing::new);
                        let cache = stored
                            .vault_cache
                            .as_deref()
                            .and_then(CacheDocument::from_slice)
                            .filter(|document| document.user_id == record.user_id)
                            .and_then(Cache::from_document);
                        match (refresh, cache) {
                            (Some(refresh), Some(cache)) => State::Locked {
                                account: Account {
                                    server,
                                    email: record.email,
                                    user_id: record.user_id,
                                    kdf: record.kdf.into(),
                                    tokens: Tokens {
                                        access: None,
                                        refresh: Some(refresh),
                                        expires_at: 0,
                                    },
                                    cache: Some(Box::new(cache)),
                                    device_wrap: stored
                                        .device_wrap
                                        .as_deref()
                                        .and_then(|bytes| DeviceWrap::parse(bytes, record.user_id)),
                                    waiting: Vec::new(),
                                    refreshing: false,
                                    passkey: None,
                                    catalog: Vec::new(),
                                    signed: Vec::new(),
                                    ssh: Vec::new(),
                                },
                                error: None,
                            },
                            _ => State::LoggedOut {
                                form: LoginForm {
                                    server: Some(server.url),
                                    email: record.email,
                                },
                                error: None,
                            },
                        }
                    }
                }
            }
            (
                State::LoggedOut { .. },
                Input::User(UserAction::Login {
                    server: url,
                    email,
                    password,
                }),
                None,
            ) => {
                effects.push(Effect::Rerender);
                match Server::new(url.clone()) {
                    Err(ServerUrlError) => State::LoggedOut {
                        form: LoginForm {
                            server: Some(url),
                            email,
                        },
                        error: Some(LoginError::ServerUrl),
                    },
                    Ok(server) => {
                        let id = requests.start(Purpose::Prelogin);
                        effects.push(Effect::Http {
                            id,
                            request: HttpRequest {
                                method: Method::Post,
                                url: endpoint(
                                    &server.urls.identity,
                                    &["accounts", "prelogin"],
                                    &[],
                                ),
                                headers: client_headers(config),
                                body: Body::Json(prelogin_body(&email)),
                            },
                        });
                        State::Prelogin {
                            server,
                            email,
                            password,
                            request: id,
                        }
                    }
                }
            }
            (
                State::LoggedOut { .. },
                Input::User(UserAction::LoginApiKey {
                    server: url,
                    email,
                    client_id,
                    client_secret,
                }),
                None,
            ) => 'login: {
                effects.push(Effect::Rerender);
                let server = match Server::new(url.clone()) {
                    Ok(server) => server,
                    Err(ServerUrlError) => {
                        break 'login State::LoggedOut {
                            form: LoginForm {
                                server: Some(url),
                                email,
                            },
                            error: Some(LoginError::ServerUrl),
                        };
                    }
                };
                let id = requests.start(Purpose::Token);
                let mut form = device_form(config);
                form.extend([
                    ("grant_type", secret("client_credentials")),
                    ("scope", secret("api")),
                    ("client_id", Zeroizing::new(client_id)),
                    ("client_secret", client_secret),
                ]);
                effects.push(Effect::Http {
                    id,
                    request: token_request(&server.urls.identity, config, form),
                });
                State::TokenRequest {
                    server,
                    email,
                    kdf: None,
                    credentials: Credentials::ApiKey,
                    challenge: None,
                    request: id,
                }
            }
            (
                State::Prelogin {
                    server,
                    email,
                    password,
                    request,
                },
                Input::Http { id, response },
                Some(Purpose::Prelogin),
            ) if id == request => {
                let kdf = match &response {
                    Ok(answer) if answer.status == 200 => {
                        proto::from_slice::<PreloginResponse>(&answer.body)
                            .ok()
                            .and_then(|prelogin| prelogin.kdf())
                            .ok_or(LoginError::BadResponse)
                    }
                    _ => Err(login_error(&response)),
                };
                effects.push(Effect::Rerender);
                match kdf {
                    Err(error) => State::LoggedOut {
                        form: LoginForm {
                            server: Some(server.url),
                            email,
                        },
                        error: Some(error),
                    },
                    Ok(kdf) => {
                        let id = requests.start(Purpose::Derive);
                        effects.push(Effect::Derive {
                            id,
                            password: password.clone(),
                            email: email.clone(),
                            kdf,
                        });
                        State::Deriving {
                            email,
                            password,
                            kdf,
                            purpose: DerivePurpose::Login(server),
                            request: id,
                        }
                    }
                }
            }
            // An answer to an older prelogin.
            (state @ State::Prelogin { .. }, Input::Http { .. }, Some(Purpose::Prelogin)) => state,
            (
                State::Deriving {
                    email,
                    password,
                    kdf,
                    purpose: DerivePurpose::Login(server),
                    request,
                },
                Input::Derived { id, result },
                Some(Purpose::Derive),
            ) if id == request => {
                effects.push(Effect::Rerender);
                match result {
                    Err(error) => State::LoggedOut {
                        form: LoginForm {
                            server: Some(server.url),
                            email,
                        },
                        error: Some(LoginError::Kdf(error)),
                    },
                    Ok(master) => {
                        let master = Box::new(master);
                        let password_hash = Zeroizing::new(master.password_hash(&password));
                        // The token goes only to the server that issued it.
                        let answer = remembered
                            .as_ref()
                            .filter(|login| login.fits(&server.url, &email))
                            .map(|login| Answer::Remembered {
                                token: login.token.clone(),
                            });
                        let id = requests.start(Purpose::Token);
                        effects.push(Effect::Http {
                            id,
                            request: password_token(
                                &server.urls.identity,
                                config,
                                &email,
                                &password_hash,
                                answer.as_ref(),
                            ),
                        });
                        State::TokenRequest {
                            server,
                            email,
                            kdf: Some(kdf),
                            credentials: Credentials::Password {
                                master,
                                password_hash,
                                answer,
                            },
                            challenge: None,
                            request: id,
                        }
                    }
                }
            }
            // An answer to an older derive.
            (
                state @ State::Deriving {
                    purpose: DerivePurpose::Login(_),
                    ..
                },
                Input::Derived { .. },
                Some(Purpose::Derive),
            ) => state,
            (
                State::Deriving {
                    purpose: DerivePurpose::Unlock(account),
                    request,
                    ..
                },
                Input::Derived { id, result },
                Some(Purpose::Derive),
            ) if id == request => {
                effects.push(Effect::Rerender);
                let account = *account;
                let user_key = result.map_err(UnlockError::Kdf).and_then(|master| {
                    let master = Box::new(master);
                    let wrapped = account
                        .cache
                        .as_ref()
                        .and_then(|cache| cache.profile.key.as_ref())
                        .ok_or(UnlockError::NoVaultData)?;
                    decrypt_user_key(wrapped, &master).map_err(|_| UnlockError::WrongPassword)
                });
                match user_key {
                    Ok(user_key) => open_vault(
                        config,
                        settings,
                        requests,
                        index_revision,
                        account,
                        user_key,
                        now_ms,
                        rng,
                        &mut effects,
                    ),
                    Err(error) => State::Locked {
                        account,
                        error: Some(error),
                    },
                }
            }
            // An answer to an older derive.
            (
                state @ State::Deriving {
                    purpose: DerivePurpose::Unlock(_),
                    ..
                },
                Input::Derived { .. },
                Some(Purpose::Derive),
            ) => state,
            (State::Locked { account, error }, Input::User(UserAction::DeviceUnlock), None) => {
                let unlocking = requests.any(|purpose| matches!(purpose, Purpose::DeviceUnlock));
                if let (Some(wrap), false) = (&account.device_wrap, unlocking) {
                    effects.push(Effect::DeviceSecret {
                        id: requests.start(Purpose::DeviceUnlock),
                        challenge: wrap.challenge,
                        create: false,
                    });
                    effects.push(Effect::Rerender);
                }
                State::Locked { account, error }
            }
            (
                State::Locked { mut account, .. },
                Input::DeviceSecret { result, .. },
                Some(Purpose::DeviceUnlock),
            ) => {
                effects.push(Effect::Rerender);
                match (result, account.device_wrap.as_ref()) {
                    (Err(DeviceKeyError::Cancelled), _) => State::Locked {
                        account,
                        error: None,
                    },
                    (Err(error), _) => State::Locked {
                        account,
                        error: Some(UnlockError::DeviceKey(error)),
                    },
                    // The record was cleared while the prompt was open.
                    (Ok(_), None) => State::Locked {
                        account,
                        error: None,
                    },
                    (Ok(secret), Some(wrap)) => {
                        let user_key = wrap
                            .sealed_user_key
                            .parse::<EncString>()
                            .ok()
                            .and_then(|sealed| sealed.decrypt(&device_key(&secret)).ok())
                            .and_then(|bytes| {
                                <&[u8; 64]>::try_from(&bytes[..])
                                    .ok()
                                    .map(SymmetricKey::from_bytes)
                            });
                        drop(secret);
                        match user_key {
                            Some(user_key) => open_vault(
                                config,
                                settings,
                                requests,
                                index_revision,
                                account,
                                user_key,
                                now_ms,
                                rng,
                                &mut effects,
                            ),
                            // A MAC failure: the device key changed (a
                            // biometric reset, a TPM clear), so the record
                            // can never open again.
                            None => {
                                account.device_wrap = None;
                                effects.push(Effect::Clear {
                                    slot: StoreSlot::DeviceWrap,
                                });
                                State::Locked {
                                    account,
                                    error: Some(UnlockError::DeviceKeyChanged),
                                }
                            }
                        }
                    }
                }
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::EnableDeviceUnlock), None) => {
                let enrolling =
                    requests.any(|purpose| matches!(purpose, Purpose::DeviceEnroll { .. }));
                if !enrolling {
                    let mut challenge = [0u8; 32];
                    rng.fill_bytes(&mut challenge);
                    effects.push(Effect::DeviceSecret {
                        id: requests.start(Purpose::DeviceEnroll {
                            challenge,
                            first: None,
                        }),
                        challenge,
                        create: true,
                    });
                    unlocked.device_error = None;
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (
                State::Unlocked(mut unlocked),
                Input::DeviceSecret { result, .. },
                Some(Purpose::DeviceEnroll { challenge, first }),
            ) => {
                effects.push(Effect::Rerender);
                match (result, first) {
                    (Err(DeviceKeyError::Cancelled), _) => {}
                    (Err(error), _) => unlocked.device_error = Some(error),
                    // Ask again over the same challenge, to check that the
                    // platform's secret is deterministic.
                    (Ok(secret), None) => effects.push(Effect::DeviceSecret {
                        id: requests.start(Purpose::DeviceEnroll {
                            challenge,
                            first: Some(secret),
                        }),
                        challenge,
                        create: false,
                    }),
                    (Ok(second), Some(first)) if second != first => {
                        unlocked.device_error = Some(DeviceKeyError::NotDeterministic);
                    }
                    (Ok(secret), Some(_)) => {
                        let sealed = EncString::encrypt(
                            &unlocked.keys.user().to_bytes()[..],
                            &device_key(&secret),
                            rng,
                        );
                        let wrap = DeviceWrap {
                            user_id: unlocked.account.user_id,
                            challenge,
                            sealed_user_key: sealed.to_string(),
                        };
                        effects.push(Effect::Store {
                            id: requests.start(Purpose::Store),
                            slot: StoreSlot::DeviceWrap,
                            bytes: serde_json::to_vec(&wrap)
                                .expect("a device wrap always serializes"),
                        });
                        unlocked.account.device_wrap = Some(wrap);
                        unlocked.device_error = None;
                    }
                }
                State::Unlocked(unlocked)
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::DisableDeviceUnlock), None) => {
                // Forget an enroll in flight, so its answer cannot store a
                // record after this.
                requests.forget(|purpose| matches!(purpose, Purpose::DeviceEnroll { .. }));
                unlocked.account.device_wrap = None;
                unlocked.device_error = None;
                effects.push(Effect::Clear {
                    slot: StoreSlot::DeviceWrap,
                });
                effects.push(Effect::Rerender);
                State::Unlocked(unlocked)
            }
            (
                State::TokenRequest {
                    server,
                    email,
                    kdf,
                    credentials,
                    challenge: _,
                    request,
                },
                Input::Http { id, response },
                Some(Purpose::Token),
            ) if id == request => {
                effects.push(Effect::Rerender);
                match response {
                    Ok(answer) if answer.status == 200 => {
                        let token = proto::from_slice::<TokenResponse>(&answer.body).ok();
                        let kdf = proto::from_slice::<PreloginResponse>(&answer.body)
                            .ok()
                            .and_then(|settings| settings.kdf())
                            .or(kdf);
                        let user_id = token
                            .as_ref()
                            .and_then(|token| jwt_subject(&token.access_token));
                        match (token, kdf, user_id) {
                            (Some(token), Some(kdf), Some(user_id)) => {
                                let record = AccountRecord {
                                    server: Some(server.url.clone()),
                                    email: email.clone(),
                                    user_id,
                                    kdf: kdf.into(),
                                };
                                effects.push(Effect::Store {
                                    id: requests.start(Purpose::Store),
                                    slot: StoreSlot::Account,
                                    bytes: serde_json::to_vec(&record)
                                        .expect("an account record always serializes"),
                                });
                                if let Some(refresh) = &token.refresh_token {
                                    effects.push(Effect::Store {
                                        id: requests.start(Purpose::Store),
                                        slot: StoreSlot::RefreshToken,
                                        bytes: refresh.as_bytes().to_vec(),
                                    });
                                }
                                // Sent only when the user asked to be
                                // remembered.
                                if let Some(token) = &token.two_factor_token {
                                    let login = RememberedLogin {
                                        server: server.url.clone(),
                                        email: email.clone(),
                                        token: secret(token),
                                    };
                                    effects.push(Effect::Store {
                                        id: requests.start(Purpose::Store),
                                        slot: StoreSlot::TwoFactorToken,
                                        bytes: login.to_bytes(),
                                    });
                                    *remembered = Some(login);
                                }
                                let mut account = Account {
                                    server,
                                    email,
                                    user_id,
                                    kdf,
                                    tokens: Tokens {
                                        access: Some(Zeroizing::new(token.access_token)),
                                        refresh: token.refresh_token.map(Zeroizing::new),
                                        expires_at: now_ms
                                            .saturating_add(token.expires_in.saturating_mul(1000)),
                                    },
                                    cache: None,
                                    device_wrap: None,
                                    waiting: Vec::new(),
                                    refreshing: false,
                                    passkey: None,
                                    catalog: Vec::new(),
                                    signed: Vec::new(),
                                    ssh: Vec::new(),
                                };
                                match credentials {
                                    Credentials::ApiKey => {
                                        // The API key gives no key to open the vault, so
                                        // fetch the encrypted vault and wait for the password.
                                        send_api(
                                            config,
                                            requests,
                                            &mut account,
                                            now_ms,
                                            sync_call(true),
                                            &mut effects,
                                        );
                                        State::Locked {
                                            account,
                                            error: None,
                                        }
                                    }
                                    Credentials::Password { master, .. } => {
                                        let user_key = token
                                            .key
                                            .as_ref()
                                            .ok_or(LoginError::BadResponse)
                                            .and_then(|wrapped| {
                                                decrypt_user_key(wrapped, &master)
                                                    .map_err(LoginError::Key)
                                            });
                                        match user_key {
                                            Err(error) => State::LoggedOut {
                                                form: LoginForm {
                                                    server: Some(account.server.url),
                                                    email: account.email,
                                                },
                                                error: Some(error),
                                            },
                                            Ok(user_key) => {
                                                let profile = Profile {
                                                    id: user_id,
                                                    email: account.email.clone(),
                                                    key: token.key,
                                                    private_key: token.private_key,
                                                    organizations: Vec::new(),
                                                };
                                                send_api(
                                                    config,
                                                    requests,
                                                    &mut account,
                                                    now_ms,
                                                    sync_call(true),
                                                    &mut effects,
                                                );
                                                *index_revision += 1;
                                                State::Unlocked(Box::new(Unlocked {
                                                    account,
                                                    keys: VaultKeys::new(user_key, &profile),
                                                    index: VaultIndex {
                                                        folders: Vec::new(),
                                                        ciphers: Vec::new(),
                                                        ssh_keys: Vec::new(),
                                                    },
                                                    open: None,
                                                    sync: SyncState::Running,
                                                    error: None,
                                                    last_activity: now_ms,
                                                    last_check: now_ms,
                                                    retries: Vec::new(),
                                                    device_error: None,
                                                    ssh_grants: Vec::new(),
        edits: Vec::new(),
                                                }))
                                            }
                                        }
                                    }
                                }
                            }
                            _ => State::LoggedOut {
                                form: LoginForm {
                                    server: Some(server.url),
                                    email,
                                },
                                error: Some(LoginError::BadResponse),
                            },
                        }
                    }
                    response => {
                        let failure = match &response {
                            Ok(answer) if answer.status == 400 => {
                                proto::from_slice::<ErrorResponse>(&answer.body)
                                    .unwrap_or_default()
                                    .token_failure()
                            }
                            _ => TokenFailure::Rejected {
                                message: String::new(),
                            },
                        };
                        // A refused remembered token is stale, so forget it.
                        // The user typed nothing, so no error shows, and the
                        // next try sends a code, so this never loops.
                        let answer_error = |answer: Option<Answer>| match answer {
                            None | Some(Answer::Remembered { .. }) => None,
                            Some(Answer::TwoFactor { .. } | Answer::NewDevice { .. }) => {
                                Some(LoginError::InvalidCode)
                            }
                        };
                        if let (
                            TokenFailure::SecondFactor { .. } | TokenFailure::NewDeviceOtp,
                            Credentials::Password {
                                answer: Some(Answer::Remembered { .. }),
                                ..
                            },
                        ) = (&failure, &credentials)
                        {
                            *remembered = None;
                            effects.push(Effect::Clear {
                                slot: StoreSlot::TwoFactorToken,
                            });
                        }
                        match (failure, credentials) {
                            (
                                TokenFailure::SecondFactor {
                                    providers,
                                    masked_email,
                                },
                                Credentials::Password {
                                    master,
                                    password_hash,
                                    answer,
                                },
                            ) => {
                                let challenge = Challenge::TwoFactor {
                                    providers,
                                    masked_email,
                                };
                                let error = answer_error(answer);
                                // With more than one method the code waits for
                                // the user's pick. With email alone there is no pick,
                                // so the first time this login gets here, ask for the code
                                // so it is on its way when the screen opens. A wrong
                                // code returns here with an error and sends
                                // nothing.
                                if error.is_none() && challenge.only_email() {
                                    let id = requests.start(Purpose::SendEmailCode);
                                    effects.push(Effect::Http {
                                        id,
                                        request: send_email_code(
                                            &server,
                                            config,
                                            &email,
                                            &password_hash,
                                        ),
                                    });
                                }
                                State::SecondFactor {
                                    server,
                                    email,
                                    master,
                                    password_hash,
                                    kdf,
                                    challenge,
                                    error,
                                }
                            }
                            (
                                TokenFailure::NewDeviceOtp,
                                Credentials::Password {
                                    master,
                                    password_hash,
                                    answer,
                                },
                            ) => State::SecondFactor {
                                server,
                                email,
                                master,
                                password_hash,
                                kdf,
                                challenge: Challenge::NewDevice,
                                error: answer_error(answer),
                            },
                            (
                                TokenFailure::Rejected { .. },
                                Credentials::Password { .. } | Credentials::ApiKey,
                            )
                            | (
                                TokenFailure::SecondFactor { .. } | TokenFailure::NewDeviceOtp,
                                Credentials::ApiKey,
                            ) => State::LoggedOut {
                                form: LoginForm {
                                    server: Some(server.url),
                                    email,
                                },
                                error: Some(login_error(&response)),
                            },
                        }
                    }
                }
            }
            // An answer to an older token request.
            (state @ State::TokenRequest { .. }, Input::Http { .. }, Some(Purpose::Token)) => state,
            (
                State::SecondFactor {
                    server,
                    email,
                    master,
                    password_hash,
                    kdf,
                    challenge,
                    error,
                },
                Input::User(
                    action
                    @ (UserAction::SubmitCode { .. } | UserAction::SubmitNewDeviceOtp { .. }),
                ),
                None,
            ) => {
                let answer = match (action, &challenge) {
                    (
                        UserAction::SubmitCode {
                            provider,
                            code,
                            remember,
                        },
                        Challenge::TwoFactor { .. },
                    ) => Some(Answer::TwoFactor {
                        provider,
                        code,
                        remember,
                    }),
                    (UserAction::SubmitNewDeviceOtp { code }, Challenge::NewDevice) => {
                        Some(Answer::NewDevice { code })
                    }
                    (UserAction::SubmitCode { .. }, Challenge::NewDevice)
                    | (UserAction::SubmitNewDeviceOtp { .. }, Challenge::TwoFactor { .. }) => None,
                    _ => unreachable!("the pattern above admits only these two"),
                };
                match answer {
                    None => State::SecondFactor {
                        server,
                        email,
                        master,
                        password_hash,
                        kdf,
                        challenge,
                        error,
                    },
                    Some(answer) => {
                        let id = requests.start(Purpose::Token);
                        effects.push(Effect::Http {
                            id,
                            request: password_token(
                                &server.urls.identity,
                                config,
                                &email,
                                &password_hash,
                                Some(&answer),
                            ),
                        });
                        effects.push(Effect::Rerender);
                        State::TokenRequest {
                            server,
                            email,
                            kdf,
                            credentials: Credentials::Password {
                                master,
                                password_hash,
                                answer: Some(answer),
                            },
                            challenge: Some(challenge),
                            request: id,
                        }
                    }
                }
            }
            (
                State::SecondFactor {
                    server,
                    email,
                    master,
                    password_hash,
                    kdf,
                    challenge,
                    ..
                },
                Input::User(UserAction::SendEmailCode),
                None,
            ) => {
                let id = requests.start(Purpose::SendEmailCode);
                effects.push(Effect::Http {
                    id,
                    request: send_email_code(&server, config, &email, &password_hash),
                });
                effects.push(Effect::Rerender);
                State::SecondFactor {
                    server,
                    email,
                    master,
                    password_hash,
                    kdf,
                    challenge,
                    error: None,
                }
            }
            (
                State::SecondFactor {
                    server,
                    email,
                    master,
                    password_hash,
                    kdf,
                    challenge,
                    error,
                },
                Input::Http { response, .. },
                Some(Purpose::SendEmailCode),
            ) => {
                let failed =
                    !matches!(&response, Ok(answer) if (200..300).contains(&answer.status));
                // Either way the screen's `sending` turns off.
                effects.push(Effect::Rerender);
                State::SecondFactor {
                    server,
                    email,
                    master,
                    password_hash,
                    kdf,
                    challenge,
                    error: if failed {
                        Some(login_error(&response))
                    } else {
                        error
                    },
                }
            }
            (
                State::Prelogin { server, email, .. }
                | State::Deriving {
                    email,
                    purpose: DerivePurpose::Login(server),
                    ..
                }
                | State::TokenRequest { server, email, .. }
                | State::SecondFactor { server, email, .. },
                Input::User(UserAction::Cancel),
                None,
            ) => {
                requests.clear();
                effects.push(Effect::Rerender);
                State::LoggedOut {
                    form: LoginForm {
                        server: Some(server.url),
                        email,
                    },
                    error: None,
                }
            }
            (State::Locked { account, .. }, Input::User(UserAction::Unlock { password }), None) => {
                let id = requests.start(Purpose::Derive);
                effects.push(Effect::Derive {
                    id,
                    password: password.clone(),
                    email: account.email.clone(),
                    kdf: account.kdf,
                });
                effects.push(Effect::Rerender);
                State::Deriving {
                    email: account.email.clone(),
                    password,
                    kdf: account.kdf,
                    purpose: DerivePurpose::Unlock(Box::new(account)),
                    request: id,
                }
            }
            (State::Locked { account, .. }, Input::User(UserAction::Logout), None) => {
                logout(requests, account, None, &mut effects)
            }
            (
                State::Locked { mut account, error },
                Input::Http { response, .. },
                Some(
                    purpose @ (Purpose::Prelogin
                    | Purpose::Derive
                    | Purpose::Token
                    | Purpose::SendEmailCode
                    | Purpose::Refresh
                    | Purpose::Api(_)
                    | Purpose::Store
                    | Purpose::DeviceEnroll { .. }
                    | Purpose::DeviceUnlock),
                ),
            ) => {
                match account_answer(
                    config,
                    requests,
                    &mut account,
                    now_ms,
                    purpose,
                    response,
                    &mut effects,
                ) {
                    Err(SessionEnded) => logout(
                        requests,
                        account,
                        Some(LoginError::SessionExpired),
                        &mut effects,
                    ),
                    Ok(_) => State::Locked { account, error },
                }
            }
            (State::Locked { mut account, error }, Input::User(UserAction::Cancel), None) => {
                if let Some(pending) = account.passkey.take() {
                    refuse_passkey(pending.id, &mut effects);
                    effects.push(Effect::Rerender);
                }
                State::Locked { account, error }
            }
            (State::Locked { mut account, error }, Input::Tick, None) => {
                if let Some(pending) = account
                    .passkey
                    .take_if(|pending| now_ms >= pending.deadline)
                {
                    refuse_passkey(pending.id, &mut effects);
                    effects.push(Effect::Rerender);
                }
                expire_ssh(&mut account.ssh, now_ms, &mut effects);
                State::Locked { account, error }
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::ApproveSsh), None) => {
                if !unlocked.account.ssh.is_empty() {
                    let pending = unlocked.account.ssh.remove(0);
                    let frame = match &unlocked.account.cache {
                        None => ssh_agent::failure_reply(),
                        Some(cache) => ssh_agent::approved_reply(
                            &pending,
                            &unlocked.index.ssh_keys,
                            &cache.ciphers,
                            &unlocked.keys,
                        ),
                    };
                    effects.push(Effect::SshAgentReply {
                        id: pending.id,
                        frame,
                    });
                    if ssh_ask(&unlocked.index, settings, &pending.public_key)
                        == SshAsk::UntilLock
                        && !unlocked.ssh_grants.contains(&pending.public_key)
                    {
                        unlocked.ssh_grants.push(pending.public_key);
                        // Later requests for the same key need not ask now.
                        release_ssh(&mut unlocked, settings, &mut effects);
                    }
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::DenySsh), None) => {
                if !unlocked.account.ssh.is_empty() {
                    let pending = unlocked.account.ssh.remove(0);
                    fail_ssh(pending.id, &mut effects);
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::Cancel), None) => {
                if let Some(pending) = unlocked.account.passkey.take() {
                    refuse_passkey(pending.id, &mut effects);
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (
                State::Unlocked(mut unlocked),
                Input::User(UserAction::ChoosePasskey(index)),
                None,
            ) => {
                let chosen = unlocked
                    .account
                    .passkey
                    .take_if(|pending| index < pending.choices.len());
                if let Some(mut pending) = chosen {
                    let target = pending.choices.swap_remove(index).target;
                    answer_passkey(
                        config,
                        requests,
                        &mut unlocked,
                        now_ms,
                        rng,
                        pending.id,
                        &pending.request,
                        target,
                        &mut effects,
                    );
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (State::Unlocked(unlocked), Input::User(UserAction::Lock), None) => {
                lock(requests, unlocked, &mut effects)
            }
            (State::Unlocked(unlocked), Input::User(UserAction::Logout), None) => {
                logout(requests, unlocked.account, None, &mut effects)
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::Sync), None) => {
                if !matches!(unlocked.sync, SyncState::Running) {
                    unlocked.sync = SyncState::Running;
                    unlocked.last_check = now_ms;
                    send_api(
                        config,
                        requests,
                        &mut unlocked.account,
                        now_ms,
                        sync_call(true),
                        &mut effects,
                    );
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::Open(cipher_id)), None) => {
                let cipher =
                    unlocked.account.cache.as_ref().and_then(|cache| {
                        cache.ciphers.iter().find(|cipher| cipher.id == cipher_id)
                    });
                match cipher.map(|cipher| (cipher, CipherView::decrypt(cipher, &unlocked.keys))) {
                    None => unlocked.error = Some(ItemError::NotFound),
                    Some((_, Err(error))) => unlocked.error = Some(ItemError::Unreadable(error)),
                    Some((cipher, Ok(view))) => {
                        unlocked.open = Some(OpenItem {
                            view,
                            base: cipher.clone(),
                            stale: false,
                            conflict: ConflictStage::None,
                        });
                        unlocked.error = None;
                    }
                }
                effects.push(Effect::Rerender);
                State::Unlocked(unlocked)
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::Close), None) => {
                unlocked.open = None;
                effects.push(Effect::Rerender);
                State::Unlocked(unlocked)
            }
            (State::Unlocked(mut unlocked), Input::User(UserAction::Save(view)), None) => {
                effects.push(Effect::Rerender);
                let user_id = unlocked.account.user_id;
                let busy = view
                    .id
                    .is_some_and(|cipher_id| writing(requests, &unlocked.account, cipher_id));
                match view.id {
                    _ if busy => unlocked.error = Some(ItemError::Busy),
                    None => match unlocked.keys.outer_key(view.organization_id) {
                        Err(error) => unlocked.error = Some(ItemError::Unreadable(error)),
                        Ok(key) => {
                            let mut edits = view.encrypt(key, &Map::new(), rng);
                            let (path, body) = match view.organization_id {
                                None => (vec!["ciphers"], create_body(edits, user_id, None)),
                                Some(organization_id) => {
                                    edits.insert(
                                        "organizationId".into(),
                                        Value::String(organization_id.to_string()),
                                    );
                                    let body =
                                        create_body(edits, user_id, Some(&view.collection_ids));
                                    (vec!["ciphers", "create"], body)
                                }
                            };
                            let call = ApiCall {
                                method: Method::Post,
                                path: path.into_iter().map(String::from).collect(),
                                query: &[],
                                body: Body::Json(body),
                                kind: ApiKind::Write(Write {
                                    cipher_id: None,
                                    action: WriteAction::Create,
                                    view: Some(view),
                                    passkey: None,
                                }),
                                retried: false,
                            };
                            unlocked.error = None;
                            send_api(
                                config,
                                requests,
                                &mut unlocked.account,
                                now_ms,
                                call,
                                &mut effects,
                            );
                        }
                    },
                    Some(cipher_id) => {
                        let current = unlocked.account.cache.as_ref().and_then(|cache| {
                            cache.ciphers.iter().find(|cipher| cipher.id == cipher_id)
                        });
                        let open = unlocked
                            .open
                            .as_mut()
                            .filter(|open| open.base.id == cipher_id);
                        match (open, current) {
                            (_, None) => unlocked.error = Some(ItemError::NotFound),
                            (Some(open), Some(current)) if open.stale => {
                                // The server copy changed while the user edited:
                                // show both without a round trip.
                                match CipherView::decrypt(current, &unlocked.keys) {
                                    Ok(theirs) => {
                                        open.conflict = ConflictStage::Ready {
                                            mine: view,
                                            theirs: Box::new(theirs),
                                        };
                                    }
                                    Err(error) => {
                                        unlocked.error = Some(ItemError::Unreadable(error));
                                    }
                                }
                            }
                            (open, Some(current)) => {
                                let base = open.map_or(current, |open| &open.base);
                                let mut view = view;
                                let key = unlocked.keys.cipher_key(base).and_then(|key| {
                                    view.record_password_change(
                                        base,
                                        &key,
                                        &passkey::iso_date(now_ms),
                                    )?;
                                    Ok(key)
                                });
                                match key {
                                    Err(error) => {
                                        unlocked.error = Some(ItemError::Unreadable(error));
                                    }
                                    Ok(key) => {
                                        let edits = view.encrypt(&key, &base.raw, rng);
                                        let call = ApiCall {
                                            method: Method::Put,
                                            path: vec!["ciphers".into(), cipher_id.to_string()],
                                            query: &[],
                                            body: Body::Json(base.update_body(edits, user_id)),
                                            kind: ApiKind::Write(Write {
                                                cipher_id: Some(cipher_id),
                                                action: WriteAction::Update,
                                                view: Some(view),
                                                passkey: None,
                                            }),
                                            retried: false,
                                        };
                                        unlocked.error = None;
                                        send_api(
                                            config,
                                            requests,
                                            &mut unlocked.account,
                                            now_ms,
                                            call,
                                            &mut effects,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                State::Unlocked(unlocked)
            }
            (
                State::Unlocked(mut unlocked),
                Input::User(
                    action @ (UserAction::MoveToFolder { .. } | UserAction::SetReprompt { .. }),
                ),
                None,
            ) => {
                effects.push(Effect::Rerender);
                let (id, edit) = match action {
                    UserAction::MoveToFolder { id, folder } => (id, ItemEdit::Folder(folder)),
                    UserAction::SetReprompt { id, on } => (id, ItemEdit::Reprompt(on)),
                    _ => unreachable!("the pattern above admits only these two"),
                };
                match writing(requests, &unlocked.account, id) {
                    // Sent once the pending write lands, from the copy it
                    // brings back, so it carries that copy's revision date. A
                    // second edit of the same kind replaces the first.
                    true => {
                        let same = |(queued_id, queued): &(Uuid, ItemEdit)| {
                            *queued_id == id && mem::discriminant(queued) == mem::discriminant(&edit)
                        };
                        match unlocked.edits.iter_mut().find(|queued| same(queued)) {
                            Some(queued) => queued.1 = edit,
                            None => unlocked.edits.push((id, edit)),
                        }
                    }
                    false => send_edit(config, requests, &mut unlocked, now_ms, id, edit, &mut effects),
                }                State::Unlocked(unlocked)
            }
            (
                State::Unlocked(mut unlocked),
                Input::User(
                    action
                    @ (UserAction::Trash(_) | UserAction::Restore(_) | UserAction::Delete(_)),
                ),
                None,
            ) => {
                let (method, path, action, cipher_id) = match action {
                    UserAction::Trash(id) => (
                        Method::Put,
                        vec!["ciphers".into(), id.to_string(), "delete".into()],
                        WriteAction::Trash,
                        id,
                    ),
                    UserAction::Restore(id) => (
                        Method::Put,
                        vec!["ciphers".into(), id.to_string(), "restore".into()],
                        WriteAction::Restore,
                        id,
                    ),
                    UserAction::Delete(id) => (
                        Method::Delete,
                        vec!["ciphers".into(), id.to_string()],
                        WriteAction::Delete,
                        id,
                    ),
                    _ => unreachable!("the pattern above admits only these three"),
                };
                let call = ApiCall {
                    method,
                    path,
                    query: &[],
                    body: Body::None,
                    kind: ApiKind::Write(Write {
                        cipher_id: Some(cipher_id),
                        action,
                        view: None,
                        passkey: None,
                    }),
                    retried: false,
                };
                send_api(
                    config,
                    requests,
                    &mut unlocked.account,
                    now_ms,
                    call,
                    &mut effects,
                );
                State::Unlocked(unlocked)
            }
            (
                State::Unlocked(mut unlocked),
                Input::User(
                    action @ (UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)),
                ),
                None,
            ) => {
                let mut encrypt = |name: &SecretString| {
                    let name = EncString::encrypt(name.as_bytes(), unlocked.keys.user(), rng);
                    Body::Json(folder_body(&name))
                };
                let (method, path, body, folder) = match action {
                    UserAction::CreateFolder { name } => (
                        Method::Post,
                        vec!["folders".into()],
                        encrypt(&name),
                        FolderWrite::Create,
                    ),
                    UserAction::RenameFolder { id, name } => (
                        Method::Put,
                        vec!["folders".into(), id.to_string()],
                        encrypt(&name),
                        FolderWrite::Rename(id),
                    ),
                    UserAction::DeleteFolder(id) => (
                        Method::Delete,
                        vec!["folders".into(), id.to_string()],
                        Body::None,
                        FolderWrite::Delete(id),
                    ),
                    _ => unreachable!("the pattern above admits only these three"),
                };
                let call = ApiCall {
                    method,
                    path,
                    query: &[],
                    body,
                    kind: ApiKind::Folder(folder),
                    retried: false,
                };
                unlocked.error = None;
                send_api(
                    config,
                    requests,
                    &mut unlocked.account,
                    now_ms,
                    call,
                    &mut effects,
                );
                State::Unlocked(unlocked)
            }
            (
                State::Unlocked(mut unlocked),
                Input::User(UserAction::ResolveConflict { keep_mine }),
                None,
            ) => {
                let current = unlocked.account.cache.as_ref().and_then(|cache| {
                    let open = unlocked.open.as_ref()?;
                    cache
                        .ciphers
                        .iter()
                        .find(|cipher| cipher.id == open.base.id)
                });
                if let (Some(open), Some(current)) = (unlocked.open.as_mut(), current)
                    && let ConflictStage::Ready { mine, theirs } =
                        mem::replace(&mut open.conflict, ConflictStage::None)
                {
                    open.view = if keep_mine { *mine } else { *theirs };
                    open.base = current.clone();
                    open.stale = false;
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (
                State::Unlocked(mut unlocked),
                Input::Http { response, .. },
                Some(
                    purpose @ (Purpose::Prelogin
                    | Purpose::Derive
                    | Purpose::Token
                    | Purpose::SendEmailCode
                    | Purpose::Refresh
                    | Purpose::Api(_)
                    | Purpose::Store
                    | Purpose::DeviceEnroll { .. }
                    | Purpose::DeviceUnlock),
                ),
            ) => {
                let reply = account_answer(
                    config,
                    requests,
                    &mut unlocked.account,
                    now_ms,
                    purpose,
                    response,
                    &mut effects,
                );
                let state = match reply {
                    Err(SessionEnded) => logout(
                        requests,
                        unlocked.account,
                        Some(LoginError::SessionExpired),
                        &mut effects,
                    ),
                    Ok(None) => State::Unlocked(unlocked),
                    Ok(Some(ApiReply::Synced)) => {
                        unlocked.sync = SyncState::Idle;
                        rebuild(&mut unlocked, index_revision, &mut effects);
                        for bump in mem::take(&mut unlocked.retries) {
                            counter_write(
                                config,
                                requests,
                                &mut unlocked,
                                now_ms,
                                rng,
                                bump,
                                &mut effects,
                            );
                        }
                        effects.push(Effect::Rerender);
                        State::Unlocked(unlocked)
                    }
                    Ok(Some(ApiReply::Failed { message, write })) => {
                        match write.map(|write| write.passkey) {
                            Some(Some(PasskeyWrite::Created { id, .. })) => {
                                effects.push(Effect::PasskeyReply {
                                    id,
                                    reply: Err(PasskeyError::NotAllowed),
                                });
                            }
                            // A lost counter bump only lets the counter lag;
                            // the next assertion writes a higher one.
                            Some(Some(PasskeyWrite::Counter(_))) => {}
                            Some(None) => unlocked.error = Some(ItemError::Rejected(message)),
                            None => unlocked.sync = SyncState::Failed(message),
                        }
                        effects.push(Effect::Rerender);
                        State::Unlocked(unlocked)
                    }
                    Ok(Some(ApiReply::Folder { folder, response })) => {
                        effects.push(Effect::Rerender);
                        match response {
                            Err(error) => unlocked.error = Some(ItemError::Rejected(error.message)),
                            Ok(answer) if (200..300).contains(&answer.status) => {
                                let saved = match folder {
                                    FolderWrite::Delete(_) => None,
                                    FolderWrite::Create | FolderWrite::Rename(_) => {
                                        match serde_json::from_slice::<Value>(&answer.body) {
                                            Ok(Value::Object(object)) => {
                                                proto::from_value::<FolderResponse>(Value::Object(
                                                    object.clone(),
                                                ))
                                                .ok()
                                                .map(|typed| (typed, object))
                                            }
                                            _ => None,
                                        }
                                    }
                                };
                                match (folder, saved, unlocked.account.cache.as_mut()) {
                                    (FolderWrite::Delete(id), _, Some(cache)) => {
                                        cache.set_folder(id, None);
                                    }
                                    (
                                        FolderWrite::Create | FolderWrite::Rename(_),
                                        Some((typed, object)),
                                        Some(cache),
                                    ) => {
                                        let revision = typed
                                            .revision_date
                                            .as_deref()
                                            .and_then(epoch_millis)
                                            .unwrap_or(0);
                                        cache.revision = cache.revision.max(revision);
                                        cache.set_folder(typed.id, Some((typed, object)));
                                    }
                                    // No cache yet, or the server did not send
                                    // the folder back: fetch it.
                                    (FolderWrite::Delete(_), _, None)
                                    | (FolderWrite::Create | FolderWrite::Rename(_), _, _) => {
                                        unlocked.sync = SyncState::Running;
                                        send_api(
                                            config,
                                            requests,
                                            &mut unlocked.account,
                                            now_ms,
                                            sync_call(true),
                                            &mut effects,
                                        );
                                    }
                                }
                                store_cache(requests, &unlocked.account, &mut effects);
                                rebuild(&mut unlocked, index_revision, &mut effects);
                            }
                            Ok(answer) => {
                                let error = proto::from_slice::<ErrorResponse>(&answer.body)
                                    .unwrap_or_default();
                                unlocked.error = Some(ItemError::Rejected(match error.message() {
                                    Some(message) => message.to_owned(),
                                    None => format!("HTTP {}", answer.status),
                                }));
                            }
                        }
                        State::Unlocked(unlocked)
                    }
                    Ok(Some(ApiReply::Written { write, answer })) => {
                        effects.push(Effect::Rerender);
                        let error =
                            proto::from_slice::<ErrorResponse>(&answer.body).unwrap_or_default();
                        let saved = match write.action {
                            WriteAction::Delete => None,
                            WriteAction::Create
                            | WriteAction::Update
                            | WriteAction::Partial
                            | WriteAction::Trash
                            | WriteAction::Restore => CipherResponse::from_slice(&answer.body).ok(),
                        };
                        let success = (200..300).contains(&answer.status);
                        match (write.action, saved) {
                            (WriteAction::Delete, _) if success => {
                                let cipher_id = write.cipher_id;
                                if let Some(cache) = unlocked.account.cache.as_mut() {
                                    cache.ciphers.retain(|cipher| Some(cipher.id) != cipher_id);
                                }
                                if unlocked
                                    .open
                                    .as_ref()
                                    .is_some_and(|open| Some(open.base.id) == cipher_id)
                                {
                                    unlocked.open = None;
                                }
                                store_cache(requests, &unlocked.account, &mut effects);
                                rebuild(&mut unlocked, index_revision, &mut effects);
                            }
                            (_, Some(saved)) if success && unlocked.account.cache.is_some() => {
                                let user_create =
                                    write.action == WriteAction::Create && write.passkey.is_none();
                                if let Some(open) = unlocked
                                    .open
                                    .as_mut()
                                    .filter(|open| open.base.id == saved.id || user_create)
                                    && let Ok(view) = CipherView::decrypt(&saved, &unlocked.keys)
                                {
                                    open.view = view;
                                    open.base = saved.clone();
                                    open.stale = false;
                                    open.conflict = ConflictStage::None;
                                }
                                let cache = unlocked
                                    .account
                                    .cache
                                    .as_mut()
                                    .expect("the guard checked the cache");
                                let revision = epoch_millis(&saved.revision_date).unwrap_or(0);
                                cache.revision = cache.revision.max(revision);
                                match cache
                                    .ciphers
                                    .iter_mut()
                                    .find(|cipher| cipher.id == saved.id)
                                {
                                    Some(cipher) => *cipher = saved,
                                    None => cache.ciphers.push(saved),
                                }
                                if write.passkey.is_none() {
                                    unlocked.error = None;
                                }
                                store_cache(requests, &unlocked.account, &mut effects);
                                rebuild(&mut unlocked, index_revision, &mut effects);
                                if let Some(PasskeyWrite::Created { id, reply }) = write.passkey {
                                    effects.push(Effect::PasskeyReply {
                                        id,
                                        reply: Ok(*reply),
                                    });
                                }
                            }
                            (_, _) if success => {
                                // The server did not send the new copy back, so fetch it.
                                unlocked.sync = SyncState::Running;
                                send_api(
                                    config,
                                    requests,
                                    &mut unlocked.account,
                                    now_ms,
                                    sync_call(true),
                                    &mut effects,
                                );
                                if let Some(PasskeyWrite::Created { id, reply }) = write.passkey {
                                    effects.push(Effect::PasskeyReply {
                                        id,
                                        reply: Ok(*reply),
                                    });
                                }
                            }
                            (_, _) if write.passkey.is_some() => {
                                let out_of_date = is_out_of_date(answer.status, &error);
                                match write.passkey {
                                    Some(PasskeyWrite::Created { id, .. }) => {
                                        effects.push(Effect::PasskeyReply {
                                            id,
                                            reply: Err(PasskeyError::NotAllowed),
                                        });
                                    }
                                    Some(PasskeyWrite::Counter(bump)) if out_of_date => {
                                        // Another writer got there first: fetch
                                        // its copy, then keep the higher counter.
                                        unlocked.retries.push(bump);
                                        if !matches!(unlocked.sync, SyncState::Running) {
                                            unlocked.sync = SyncState::Running;
                                            send_api(
                                                config,
                                                requests,
                                                &mut unlocked.account,
                                                now_ms,
                                                sync_call(true),
                                                &mut effects,
                                            );
                                        }
                                    }
                                    // A refused counter bump only lets the
                                    // counter lag.
                                    Some(PasskeyWrite::Counter(_)) | None => {}
                                }
                            }
                            (WriteAction::Update, _) if is_out_of_date(answer.status, &error) => {
                                match (unlocked.open.as_mut(), write.view) {
                                    (Some(open), Some(mine))
                                        if Some(open.base.id) == write.cipher_id =>
                                    {
                                        open.conflict = ConflictStage::Syncing { mine };
                                        unlocked.sync = SyncState::Running;
                                        send_api(
                                            config,
                                            requests,
                                            &mut unlocked.account,
                                            now_ms,
                                            sync_call(true),
                                            &mut effects,
                                        );
                                    }
                                    _ => {
                                        unlocked.error = Some(ItemError::Rejected(
                                            error.message().unwrap_or_default().to_owned(),
                                        ));
                                    }
                                }
                            }
                            (
                                WriteAction::Create
                                | WriteAction::Update
                                | WriteAction::Partial
                                | WriteAction::Trash
                                | WriteAction::Restore
                                | WriteAction::Delete,
                                _,
                            ) => {
                                unlocked.error = Some(ItemError::Rejected(match error.message() {
                                    Some(message) => message.to_owned(),
                                    None => format!("HTTP {}", answer.status),
                                }));
                            }
                        }
                        State::Unlocked(unlocked)
                    }
                };
                // An answer may end the write that queued edits wait for,
                // whether it succeeded or not.
                match state {
                    State::Unlocked(mut unlocked) => {
                        while let Some(position) = unlocked
                            .edits
                            .iter()
                            .position(|(id, _)| !writing(requests, &unlocked.account, *id))
                        {
                            let (id, edit) = unlocked.edits.remove(position);
                            send_edit(config, requests, &mut unlocked, now_ms, id, edit, &mut effects);
                        }
                        State::Unlocked(unlocked)
                    }
                    other => other,
                }
            }
            (State::Unlocked(mut unlocked), Input::Tick, None) => {
                let expired = unlocked
                    .account
                    .passkey
                    .take_if(|pending| now_ms >= pending.deadline);
                if let Some(pending) = expired {
                    refuse_passkey(pending.id, &mut effects);
                    effects.push(Effect::Rerender);
                }
                expire_ssh(&mut unlocked.account.ssh, now_ms, &mut effects);
                if now_ms.saturating_sub(unlocked.last_activity) >= config.lock_after {
                    lock(requests, unlocked, &mut effects)
                } else {
                    let checking = |kind: &ApiKind| {
                        matches!(kind, ApiKind::RevisionDate { .. } | ApiKind::Sync { .. })
                    };
                    let busy = requests.any(
                        |purpose| matches!(purpose, Purpose::Api(call) if checking(&call.kind)),
                    ) || unlocked
                        .account
                        .waiting
                        .iter()
                        .any(|call| checking(&call.kind));
                    if !busy && now_ms.saturating_sub(unlocked.last_check) >= REVISION_POLL {
                        unlocked.last_check = now_ms;
                        send_api(
                            config,
                            requests,
                            &mut unlocked.account,
                            now_ms,
                            sync_call(false),
                            &mut effects,
                        );
                    }
                    State::Unlocked(unlocked)
                }
            }
            (
                State::Locked { mut account, error },
                Input::StorageChanged {
                    slot: StoreSlot::VaultCache,
                    bytes: Some(bytes),
                },
                None,
            ) => {
                if let Some(cache) = newer_cache(&account, &bytes) {
                    account.cache = Some(Box::new(cache));
                }
                State::Locked { account, error }
            }
            (
                State::Unlocked(mut unlocked),
                Input::StorageChanged {
                    slot: StoreSlot::VaultCache,
                    bytes: Some(bytes),
                },
                None,
            ) => {
                if let Some(cache) = newer_cache(&unlocked.account, &bytes) {
                    unlocked.account.cache = Some(Box::new(cache));
                    rebuild(&mut unlocked, index_revision, &mut effects);
                    effects.push(Effect::Rerender);
                }
                State::Unlocked(unlocked)
            }
            (
                State::Locked { account, .. },
                Input::StorageChanged {
                    slot: StoreSlot::RefreshToken,
                    bytes: None,
                },
                None,
            ) => logout(
                requests,
                account,
                Some(LoginError::SessionExpired),
                &mut effects,
            ),
            (
                State::Unlocked(unlocked),
                Input::StorageChanged {
                    slot: StoreSlot::RefreshToken,
                    bytes: None,
                },
                None,
            ) => logout(
                requests,
                unlocked.account,
                Some(LoginError::SessionExpired),
                &mut effects,
            ),
            (
                State::Locked { mut account, error },
                Input::StorageChanged {
                    slot: StoreSlot::DeviceWrap,
                    bytes,
                },
                None,
            ) => {
                account.device_wrap = bytes
                    .as_deref()
                    .and_then(|bytes| DeviceWrap::parse(bytes, account.user_id));
                effects.push(Effect::Rerender);
                State::Locked { account, error }
            }
            (
                State::Unlocked(mut unlocked),
                Input::StorageChanged {
                    slot: StoreSlot::DeviceWrap,
                    bytes,
                },
                None,
            ) => {
                // Another process changed device unlock, so this session's
                // trust in it is gone until the user unlocks again.
                unlocked.account.device_wrap = bytes
                    .as_deref()
                    .and_then(|bytes| DeviceWrap::parse(bytes, unlocked.account.user_id));
                lock(requests, unlocked, &mut effects)
            }
            // Inputs that do not fit the state.
            (
                state @ State::Starting,
                Input::User(
                    UserAction::Login { .. }
                    | UserAction::LoginApiKey { .. }
                    | UserAction::SubmitCode { .. }
                    | UserAction::SubmitNewDeviceOtp { .. }
                    | UserAction::SendEmailCode
                    | UserAction::Cancel
                    | UserAction::Unlock { .. }
                    | UserAction::DeviceUnlock
                    | UserAction::EnableDeviceUnlock
                    | UserAction::DisableDeviceUnlock
                    | UserAction::Lock
                    | UserAction::Logout
                    | UserAction::Sync
                    | UserAction::Open(_)
                    | UserAction::Close
                    | UserAction::Save(_)
                    | UserAction::Trash(_)
                    | UserAction::MoveToFolder { .. }
                    | UserAction::SetReprompt { .. }
                    | UserAction::Restore(_)
                    | UserAction::Delete(_)
                    | UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)
                    | UserAction::ResolveConflict { .. }
                    | UserAction::ChoosePasskey(_)
                    | UserAction::ApproveSsh
                    | UserAction::DenySsh,
                )
                | Input::Tick
                | Input::StorageChanged { .. },
                None,
            ) => state,
            (
                state @ State::LoggedOut { .. },
                Input::User(
                    UserAction::SubmitCode { .. }
                    | UserAction::SubmitNewDeviceOtp { .. }
                    | UserAction::SendEmailCode
                    | UserAction::Cancel
                    | UserAction::Unlock { .. }
                    | UserAction::DeviceUnlock
                    | UserAction::EnableDeviceUnlock
                    | UserAction::DisableDeviceUnlock
                    | UserAction::Lock
                    | UserAction::Logout
                    | UserAction::Sync
                    | UserAction::Open(_)
                    | UserAction::Close
                    | UserAction::Save(_)
                    | UserAction::Trash(_)
                    | UserAction::MoveToFolder { .. }
                    | UserAction::SetReprompt { .. }
                    | UserAction::Restore(_)
                    | UserAction::Delete(_)
                    | UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)
                    | UserAction::ResolveConflict { .. }
                    | UserAction::ChoosePasskey(_)
                    | UserAction::ApproveSsh
                    | UserAction::DenySsh,
                )
                | Input::Started { .. }
                | Input::Tick
                | Input::StorageChanged { .. },
                None,
            ) => state,
            (
                state @ State::Prelogin { .. },
                Input::User(
                    UserAction::Login { .. }
                    | UserAction::LoginApiKey { .. }
                    | UserAction::SubmitCode { .. }
                    | UserAction::SubmitNewDeviceOtp { .. }
                    | UserAction::SendEmailCode
                    | UserAction::Unlock { .. }
                    | UserAction::DeviceUnlock
                    | UserAction::EnableDeviceUnlock
                    | UserAction::DisableDeviceUnlock
                    | UserAction::Lock
                    | UserAction::Logout
                    | UserAction::Sync
                    | UserAction::Open(_)
                    | UserAction::Close
                    | UserAction::Save(_)
                    | UserAction::Trash(_)
                    | UserAction::MoveToFolder { .. }
                    | UserAction::SetReprompt { .. }
                    | UserAction::Restore(_)
                    | UserAction::Delete(_)
                    | UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)
                    | UserAction::ResolveConflict { .. }
                    | UserAction::ChoosePasskey(_)
                    | UserAction::ApproveSsh
                    | UserAction::DenySsh,
                )
                | Input::Started { .. }
                | Input::Tick
                | Input::StorageChanged { .. },
                None,
            ) => state,
            (
                state @ State::Deriving {
                    purpose: DerivePurpose::Login(_) | DerivePurpose::Unlock(_),
                    ..
                },
                Input::User(
                    UserAction::Login { .. }
                    | UserAction::LoginApiKey { .. }
                    | UserAction::SubmitCode { .. }
                    | UserAction::SubmitNewDeviceOtp { .. }
                    | UserAction::SendEmailCode
                    | UserAction::Cancel
                    | UserAction::Unlock { .. }
                    | UserAction::DeviceUnlock
                    | UserAction::EnableDeviceUnlock
                    | UserAction::DisableDeviceUnlock
                    | UserAction::Lock
                    | UserAction::Logout
                    | UserAction::Sync
                    | UserAction::Open(_)
                    | UserAction::Close
                    | UserAction::Save(_)
                    | UserAction::Trash(_)
                    | UserAction::MoveToFolder { .. }
                    | UserAction::SetReprompt { .. }
                    | UserAction::Restore(_)
                    | UserAction::Delete(_)
                    | UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)
                    | UserAction::ResolveConflict { .. }
                    | UserAction::ChoosePasskey(_)
                    | UserAction::ApproveSsh
                    | UserAction::DenySsh,
                )
                | Input::Started { .. }
                | Input::Tick
                | Input::StorageChanged { .. },
                None,
            ) => state,
            (
                state @ State::TokenRequest { .. },
                Input::User(
                    UserAction::Login { .. }
                    | UserAction::LoginApiKey { .. }
                    | UserAction::SubmitCode { .. }
                    | UserAction::SubmitNewDeviceOtp { .. }
                    | UserAction::SendEmailCode
                    | UserAction::Unlock { .. }
                    | UserAction::DeviceUnlock
                    | UserAction::EnableDeviceUnlock
                    | UserAction::DisableDeviceUnlock
                    | UserAction::Lock
                    | UserAction::Logout
                    | UserAction::Sync
                    | UserAction::Open(_)
                    | UserAction::Close
                    | UserAction::Save(_)
                    | UserAction::Trash(_)
                    | UserAction::MoveToFolder { .. }
                    | UserAction::SetReprompt { .. }
                    | UserAction::Restore(_)
                    | UserAction::Delete(_)
                    | UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)
                    | UserAction::ResolveConflict { .. }
                    | UserAction::ChoosePasskey(_)
                    | UserAction::ApproveSsh
                    | UserAction::DenySsh,
                )
                | Input::Started { .. }
                | Input::Tick
                | Input::StorageChanged { .. },
                None,
            ) => state,
            (
                state @ State::SecondFactor { .. },
                Input::User(
                    UserAction::Login { .. }
                    | UserAction::LoginApiKey { .. }
                    | UserAction::Unlock { .. }
                    | UserAction::DeviceUnlock
                    | UserAction::EnableDeviceUnlock
                    | UserAction::DisableDeviceUnlock
                    | UserAction::Lock
                    | UserAction::Logout
                    | UserAction::Sync
                    | UserAction::Open(_)
                    | UserAction::Close
                    | UserAction::Save(_)
                    | UserAction::Trash(_)
                    | UserAction::MoveToFolder { .. }
                    | UserAction::SetReprompt { .. }
                    | UserAction::Restore(_)
                    | UserAction::Delete(_)
                    | UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)
                    | UserAction::ResolveConflict { .. }
                    | UserAction::ChoosePasskey(_)
                    | UserAction::ApproveSsh
                    | UserAction::DenySsh,
                )
                | Input::Started { .. }
                | Input::Tick
                | Input::StorageChanged { .. },
                None,
            ) => state,
            (
                state @ State::Locked { .. },
                Input::User(
                    UserAction::Login { .. }
                    | UserAction::LoginApiKey { .. }
                    | UserAction::SubmitCode { .. }
                    | UserAction::SubmitNewDeviceOtp { .. }
                    | UserAction::SendEmailCode
                    | UserAction::EnableDeviceUnlock
                    | UserAction::DisableDeviceUnlock
                    | UserAction::Lock
                    | UserAction::Sync
                    | UserAction::Open(_)
                    | UserAction::Close
                    | UserAction::Save(_)
                    | UserAction::Trash(_)
                    | UserAction::MoveToFolder { .. }
                    | UserAction::SetReprompt { .. }
                    | UserAction::Restore(_)
                    | UserAction::Delete(_)
                    | UserAction::CreateFolder { .. }
                    | UserAction::RenameFolder { .. }
                    | UserAction::DeleteFolder(_)
                    | UserAction::ResolveConflict { .. }
                    | UserAction::ChoosePasskey(_)
                    | UserAction::ApproveSsh
                    | UserAction::DenySsh,
                )
                | Input::Started { .. }
                | Input::StorageChanged {
                    slot: StoreSlot::Account,
                    ..
                }
                | Input::StorageChanged {
                    slot: StoreSlot::VaultCache,
                    bytes: None,
                }
                | Input::StorageChanged {
                    slot: StoreSlot::RefreshToken,
                    bytes: Some(_),
                }
                // Read before this match.
                | Input::StorageChanged {
                    slot: StoreSlot::TwoFactorToken,
                    ..
                },
                None,
            ) => state,
            (
                state @ State::Unlocked(_),
                Input::User(
                    UserAction::Login { .. }
                    | UserAction::LoginApiKey { .. }
                    | UserAction::SubmitCode { .. }
                    | UserAction::SubmitNewDeviceOtp { .. }
                    | UserAction::SendEmailCode
                    | UserAction::Unlock { .. }
                    | UserAction::DeviceUnlock,
                )
                | Input::Started { .. }
                | Input::StorageChanged {
                    slot: StoreSlot::Account,
                    ..
                }
                | Input::StorageChanged {
                    slot: StoreSlot::VaultCache,
                    bytes: None,
                }
                | Input::StorageChanged {
                    slot: StoreSlot::RefreshToken,
                    bytes: Some(_),
                }
                // Read before this match.
                | Input::StorageChanged {
                    slot: StoreSlot::TwoFactorToken,
                    ..
                },
                None,
            ) => state,
            // An answer the state did not ask for, in any state. The second
            // half never matches, because only answers carry a purpose.
            (
                state @ (State::Starting
                | State::LoggedOut { .. }
                | State::Prelogin { .. }
                | State::Deriving { .. }
                | State::TokenRequest { .. }
                | State::SecondFactor { .. }
                | State::Locked { .. }
                | State::Unlocked(_)),
                Input::Http { .. } | Input::Derived { .. } | Input::DeviceSecret { .. },
                None
                | Some(
                    Purpose::Prelogin
                    | Purpose::Derive
                    | Purpose::Token
                    | Purpose::SendEmailCode
                    | Purpose::Refresh
                    | Purpose::Api(_)
                    | Purpose::Store
                    | Purpose::DeviceEnroll { .. }
                    | Purpose::DeviceUnlock,
                ),
            )
            | (
                state @ (State::Starting
                | State::LoggedOut { .. }
                | State::Prelogin { .. }
                | State::Deriving { .. }
                | State::TokenRequest { .. }
                | State::SecondFactor { .. }
                | State::Locked { .. }
                | State::Unlocked(_)),
                Input::Started { .. }
                | Input::User(_)
                | Input::Tick
                | Input::Passkey { .. }
                | Input::SshAgent { .. }
                | Input::StorageChanged { .. },
                Some(_),
            ) => state,
            (_, Input::Settings(_), _) => unreachable!("settings return before the match"),
        };
        effects
    }

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

    fn sending_email_code(&self) -> bool {
        self.requests
            .any(|purpose| matches!(purpose, Purpose::SendEmailCode))
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
                index: &unlocked.index,
                index_revision: self.index_revision,
                open: unlocked.open.as_ref(),
                syncing: matches!(unlocked.sync, SyncState::Running),
                sync_error: match &unlocked.sync {
                    SyncState::Failed(message) => Some(message),
                    _ => None,
                },
                error: unlocked.error.as_ref(),
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

/// What the shell sets up once, before [`Input::Started`].
#[derive(Clone, Debug)]
pub struct Config {
    /// The OAuth `client_id` of this app, such as `desktop`.
    pub client_id: String,
    /// Sent as `Bitwarden-Client-Name`.
    pub client_name: String,
    /// Sent as `Bitwarden-Client-Version`.
    pub client_version: String,
    /// Bitwarden's device type number, sent as `deviceType` and
    /// `Device-Type`.
    pub device_type: u8,
    /// Stable per install; the shell creates and keeps it.
    pub device_id: Uuid,
    pub device_name: String,
    /// Milliseconds with no user action before a [`Input::Tick`] locks.
    pub lock_after: u64,
}

impl Config {
    /// The base URLs for the server the user typed, such as
    /// `https://vault.example.com`. Bitwarden cloud (`vault.bitwarden.com`
    /// or `vault.bitwarden.eu`, with or without `vault.`) has its own
    /// identity and API hosts. Any other server serves both under the one
    /// base, at `/identity` and `/api`. A trailing slash is ignored. Fails
    /// for a URL that is not `http` or `https`.
    pub fn server_urls(server: &Url) -> Result<ServerUrls, ServerUrlError> {
        if !matches!(server.scheme(), "http" | "https") || server.cannot_be_a_base() {
            return Err(ServerUrlError);
        }
        let cloud = match server
            .host_str()
            .map(|host| host.strip_prefix("vault.").unwrap_or(host))
        {
            Some("bitwarden.com") => Some((
                "https://identity.bitwarden.com",
                "https://api.bitwarden.com",
            )),
            Some("bitwarden.eu") => {
                Some(("https://identity.bitwarden.eu", "https://api.bitwarden.eu"))
            }
            _ => None,
        };
        Ok(match cloud {
            Some((identity, api)) => ServerUrls {
                identity: Url::parse(identity).expect("a fixed, valid URL"),
                api: Url::parse(api).expect("a fixed, valid URL"),
            },
            None => ServerUrls {
                identity: endpoint(server, &["identity"], &[]),
                api: endpoint(server, &["api"], &[]),
            },
        })
    }
}

/// The identity and API base URLs of one server, from
/// [`Config::server_urls`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerUrls {
    pub identity: Url,
    pub api: Url,
}

/// The server URL is not an `http` or `https` URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerUrlError;

impl std::fmt::Display for ServerUrlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the server URL must start with http:// or https://")
    }
}

impl std::error::Error for ServerUrlError {}

/// `base` with `segments` appended to its path, each percent-encoded, and
/// `query` as its form-encoded query. A trailing slash on `base` is ignored.
/// `base` must be an `http` or `https` URL.
fn endpoint<S: AsRef<str>>(base: &Url, segments: &[S], query: &[(&str, &str)]) -> Url {
    let mut url = base.clone();
    url.path_segments_mut()
        .expect("Config and server_urls admit only http and https URLs")
        .pop_if_empty()
        .extend(segments);
    url.set_query(None);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    url
}

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
    /// one [`Effect::SshAgentReply`] with the same `id`.
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
    /// `https://vault.bitwarden.com`; see [`Config::server_urls`]. A URL that
    /// is not `http` or `https` gives [`LoginError::ServerUrl`].
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
    /// [`PasskeyPrompt`].
    Cancel,
    Unlock {
        password: SecretString,
    },
    /// Unlock with the device key, when [`Screen::Locked`] offers it.
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
    CreateFolder {
        name: SecretString,
    },
    RenameFolder {
        id: Uuid,
        name: SecretString,
    },
    /// Delete a folder; its items stay, outside any folder.
    DeleteFolder(Uuid),
    /// Close the conflict screen, keeping the user's copy (to save again)
    /// or the server's.
    ResolveConflict {
        keep_mine: bool,
    },
    /// Answer the [`PasskeyPrompt`] with the choice at this index.
    /// [`Cancel`](Self::Cancel) refuses it.
    ChoosePasskey(usize),
    /// Sign the request in [`VaultScreen::ssh_approval`].
    ApproveSsh,
    /// Refuse the request in [`VaultScreen::ssh_approval`].
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

pub enum Effect {
    Http {
        id: RequestId,
        request: HttpRequest,
    },
    /// Run [`derive_master_key`](crate::crypto::derive_master_key) off the
    /// UI thread and answer with [`Input::Derived`].
    Derive {
        id: RequestId,
        password: SecretString,
        email: String,
        kdf: Kdf,
    },
    /// Write `bytes` over the slot, with OS protection when
    /// [`StoreSlot::protected`], and answer with [`Input::Stored`].
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
    /// The reply frame for [`Input::SshAgent`], without its length prefix.
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
    /// A [`CacheDocument`].
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

/// What the UI shows.
pub enum Screen<'a> {
    /// Waiting for [`Input::Started`].
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
    /// [`UserAction::ApproveSsh`] or [`UserAction::DenySsh`].
    pub ssh_approval: Option<SshApproval<'a>>,
}

/// The item being viewed or edited.
pub struct OpenItem {
    pub view: CipherView,
    /// The cipher as it was opened. A save sends its revision date, so a
    /// newer server copy fails as a conflict.
    base: CipherResponse,
    /// A sync brought a newer server copy; the next save shows a conflict.
    pub stale: bool,
    conflict: ConflictStage,
}

impl OpenItem {
    /// Both versions, once a conflict is ready for the user to pick one.
    pub fn conflict(&self) -> Option<Conflict<'_>> {
        match &self.conflict {
            ConflictStage::Ready { mine, theirs } => Some(Conflict { mine, theirs }),
            _ => None,
        }
    }
}

pub struct Conflict<'a> {
    pub mine: &'a CipherView,
    pub theirs: &'a CipherView,
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

/// Login progress, and the account once a token arrived.
enum State {
    Starting,
    LoggedOut {
        form: LoginForm,
        error: Option<LoginError>,
    },
    Prelogin {
        server: Server,
        email: String,
        password: SecretString,
        request: RequestId,
    },
    Deriving {
        email: String,
        password: SecretString,
        kdf: Kdf,
        purpose: DerivePurpose,
        request: RequestId,
    },
    TokenRequest {
        server: Server,
        email: String,
        /// From prelogin; [`None`] for an API key, whose token response
        /// carries it.
        kdf: Option<Kdf>,
        credentials: Credentials,
        /// The challenge the request answers with a code. While set, the UI
        /// stays on the second-factor screen.
        challenge: Option<Challenge>,
        request: RequestId,
    },
    SecondFactor {
        server: Server,
        email: String,
        kdf: Option<Kdf>,
        master: Box<MasterKey>,
        password_hash: SecretString,
        challenge: Challenge,
        error: Option<LoginError>,
    },
    Locked {
        account: Account,
        error: Option<UnlockError>,
    },
    Unlocked(Box<Unlocked>),
}

#[derive(Default)]
struct LoginForm {
    server: Option<Url>,
    email: String,
}

/// One server: the URL the user gave, and the bases requests go to.
#[derive(Clone, Debug)]
struct Server {
    url: Url,
    urls: ServerUrls,
}

impl Server {
    fn new(url: Url) -> Result<Self, ServerUrlError> {
        Ok(Server {
            urls: Config::server_urls(&url)?,
            url,
        })
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "the other login states hold a Server inline too, so boxing it here saves nothing"
)]
enum DerivePurpose {
    Login(Server),
    Unlock(Box<Account>),
}

enum Credentials {
    /// `master` stays until the user key opens; it is boxed so moves
    /// between states copy no key bytes.
    Password {
        master: Box<MasterKey>,
        password_hash: SecretString,
        answer: Option<Answer>,
    },
    ApiKey,
}

enum Answer {
    TwoFactor {
        provider: u8,
        code: SecretString,
        remember: bool,
    },
    NewDevice {
        code: SecretString,
    },
    /// The stored token, sent before the server asks.
    Remembered {
        token: SecretString,
    },
}

/// The record in [`StoreSlot::TwoFactorToken`]. The server accepts `token`
/// only for `email`, and only `server` may see it.
struct RememberedLogin {
    server: Url,
    email: String,
    token: SecretString,
}

#[derive(Serialize, Deserialize)]
struct RememberedRecord<'a> {
    server: Url,
    email: &'a str,
    token: &'a str,
}

impl RememberedLogin {
    /// [`None`] for a malformed record, or one written before records named
    /// their server.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let record = serde_json::from_slice::<RememberedRecord<'_>>(bytes).ok()?;
        Some(Self {
            server: record.server,
            email: record.email.to_owned(),
            token: secret(record.token),
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&RememberedRecord {
            server: self.server.clone(),
            email: &self.email,
            token: &self.token,
        })
        .expect("a remembered login always serializes")
    }

    /// Whether this token may go with a login of `email` (in any ASCII
    /// case) to `server`.
    fn fits(&self, server: &Url, email: &str) -> bool {
        self.server == *server && self.email.eq_ignore_ascii_case(email)
    }
}

/// The signed-in account. `cache` is still encrypted, so it stays while
/// locked.
struct Account {
    server: Server,
    email: String,
    user_id: Uuid,
    kdf: Kdf,
    tokens: Tokens,
    /// Boxed, because it is far larger than the rest of the account.
    cache: Option<Box<Cache>>,
    /// The stored device-unlock record, if any.
    device_wrap: Option<DeviceWrap>,
    /// API calls held until a refresh brings a fresh access token.
    waiting: Vec<ApiCall>,
    /// A refresh is in flight.
    refreshing: bool,
    /// The passkey request that waits for unlock or a pick. It outlives a
    /// lock, but its choices do not.
    passkey: Option<Box<Pending>>,
    /// The catalog last sent in [`Effect::PasskeyCatalog`].
    catalog: Vec<PasskeyCatalogEntry>,
    /// The highest counter signed per credential id this session, so two
    /// assertions sign rising counters before the first write lands.
    signed: Vec<(Vec<u8>, u32)>,
    /// SSH sign requests that wait for unlock or approval, oldest first.
    /// Lock and logout fail them all.
    ssh: Vec<ssh_agent::Pending>,
}

struct Tokens {
    access: Option<SecretString>,
    /// [`None`] after an API key login on servers that send none.
    refresh: Option<SecretString>,
    /// Epoch ms.
    expires_at: u64,
}

struct Unlocked {
    account: Account,
    keys: VaultKeys,
    index: VaultIndex,
    open: Option<OpenItem>,
    sync: SyncState,
    error: Option<ItemError>,
    last_activity: u64,
    /// When the last revision check or sync started.
    last_check: u64,
    /// Counter writes that came back out of date, sent again after the
    /// next sync when they are still higher.
    retries: Vec<CounterBump>,
    device_error: Option<DeviceKeyError>,
    /// Public keys in [`SshAsk::UntilLock`] mode that the user approved this
    /// session. Lock drops them with the rest of `Unlocked`.
    ssh_grants: Vec<Vec<u8>>,
    /// Item edits waiting for the pending write to their item, oldest
    /// first, at most one of each kind per item.
    edits: Vec<(Uuid, ItemEdit)>,
}

/// An edit of an item by id, which needs nothing decrypted; see
/// [`UserAction::MoveToFolder`] and [`UserAction::SetReprompt`].
enum ItemEdit {
    Folder(Option<Uuid>),
    Reprompt(bool),
}

/// The record in [`StoreSlot::DeviceWrap`], stored as JSON. It holds no
/// secret in the clear, so it stays while locked.
#[derive(Serialize, Deserialize)]
struct DeviceWrap {
    user_id: Uuid,
    challenge: [u8; 32],
    /// Type 2 EncString of the 64-byte user key under
    /// [`device_key`]`(secret)`.
    sealed_user_key: String,
}

impl DeviceWrap {
    /// [`None`] for a malformed record or one of another user.
    fn parse(bytes: &[u8], user_id: Uuid) -> Option<Self> {
        serde_json::from_slice::<Self>(bytes)
            .ok()
            .filter(|wrap| wrap.user_id == user_id)
    }
}

enum SyncState {
    Idle,
    Running,
    Failed(String),
}

enum ConflictStage {
    None,
    /// A write came back out of date; waiting for the sync that brings the
    /// server copy.
    Syncing {
        mine: Box<CipherView>,
    },
    Ready {
        mine: Box<CipherView>,
        theirs: Box<CipherView>,
    },
}

/// The cached sync in memory: each cipher is its own record, and
/// [`to_document`](Self::to_document) rebuilds the stored document from
/// them.
struct Cache {
    /// See [`CacheDocument::revision`].
    revision: u64,
    synced_at: u64,
    /// The sync body without its `ciphers` key, as received.
    rest: Map<String, Value>,
    profile: Profile,
    folders: Vec<FolderResponse>,
    ciphers: Vec<CipherResponse>,
}

impl Cache {
    fn from_document(document: CacheDocument) -> Option<Self> {
        Self::from_sync(document.sync, document.revision, document.synced_at)
    }

    /// [`None`] when `body` is not a sync response.
    fn from_sync(body: Value, revision: u64, synced_at: u64) -> Option<Self> {
        let sync = SyncResponse::from_json(body.clone()).ok()?;
        let Value::Object(mut rest) = body else {
            return None;
        };
        rest.retain(|key, _| !key.eq_ignore_ascii_case("ciphers"));
        Some(Self {
            revision,
            synced_at,
            rest,
            profile: sync.profile,
            folders: sync.folders,
            ciphers: sync.ciphers,
        })
    }

    /// Puts `folder` (typed, and as the server sent it) in place of the
    /// folder `id`, or removes that folder for [`None`], in both
    /// [`folders`](Self::folders) and the stored sync body. Removing a folder
    /// also takes every cipher out of it, as the server does.
    fn set_folder(&mut self, id: Uuid, folder: Option<(FolderResponse, Map<String, Value>)>) {
        let key = self
            .rest
            .keys()
            .find(|key| key.eq_ignore_ascii_case("folders"))
            .cloned()
            .unwrap_or_else(|| "folders".to_owned());
        let stored = self
            .rest
            .entry(key)
            .or_insert_with(|| Value::Array(Vec::new()));
        if !stored.is_array() {
            *stored = Value::Array(Vec::new());
        }
        let stored = stored
            .as_array_mut()
            .expect("the value was just made a list");
        let same_id = |value: &Value| {
            value.as_object().is_some_and(|object| {
                object.iter().any(|(key, value)| {
                    key.eq_ignore_ascii_case("id")
                        && value.as_str().and_then(|text| text.parse().ok()) == Some(id)
                })
            })
        };
        stored.retain(|value| !same_id(value));
        self.folders.retain(|existing| existing.id != id);
        match folder {
            Some((typed, object)) => {
                stored.push(Value::Object(object));
                self.folders.push(typed);
            }
            None => {
                for cipher in &mut self.ciphers {
                    if cipher.folder_id == Some(id) {
                        cipher.folder_id = None;
                        cipher
                            .raw
                            .retain(|key, _| !key.eq_ignore_ascii_case("folderid"));
                        cipher.raw.insert("folderId".to_owned(), Value::Null);
                    }
                }
            }
        }
    }

    fn to_document(&self, user_id: Uuid) -> CacheDocument {
        let mut sync = self.rest.clone();
        sync.insert(
            "ciphers".to_owned(),
            Value::Array(
                self.ciphers
                    .iter()
                    .map(|cipher| Value::Object(cipher.raw.clone()))
                    .collect(),
            ),
        );
        CacheDocument {
            format: CacheDocument::FORMAT,
            user_id,
            revision: self.revision,
            synced_at: self.synced_at,
            sync: Value::Object(sync),
        }
    }
}

/// The `Account` slot.
#[derive(Serialize, Deserialize)]
struct AccountRecord {
    /// Absent in records written before the core took the server per
    /// login; such an account boots logged out.
    #[serde(default)]
    server: Option<Url>,
    email: String,
    user_id: Uuid,
    kdf: KdfRecord,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum KdfRecord {
    Pbkdf2 {
        iterations: u32,
    },
    Argon2id {
        iterations: u32,
        memory_mebibytes: u32,
        parallelism: u32,
    },
}

impl From<Kdf> for KdfRecord {
    fn from(kdf: Kdf) -> Self {
        match kdf {
            Kdf::Pbkdf2 { iterations } => Self::Pbkdf2 { iterations },
            Kdf::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            } => Self::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            },
        }
    }
}

impl From<KdfRecord> for Kdf {
    fn from(record: KdfRecord) -> Self {
        match record {
            KdfRecord::Pbkdf2 { iterations } => Self::Pbkdf2 { iterations },
            KdfRecord::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            } => Self::Argon2id {
                iterations,
                memory_mebibytes,
                parallelism,
            },
        }
    }
}

/// How long (ms) the vault stays unlocked between revision checks.
const REVISION_POLL: u64 = 5 * 60 * 1000;

/// Refresh this long (ms) before the access token expires.
const REFRESH_MARGIN: u64 = 60 * 1000;

/// What [`account_answer`] leaves to the unlocked state.
enum ApiReply {
    /// A sync answer arrived; the cache may have changed.
    Synced,
    Failed {
        message: String,
        /// The write that failed; [`None`] for a sync or revision check.
        write: Option<Write>,
    },
    Written {
        write: Write,
        answer: HttpResponse,
    },
    /// A folder write was answered, or got no response.
    Folder {
        folder: FolderWrite,
        response: Result<HttpResponse, HttpError>,
    },
}

/// The refresh token was refused, or a request failed with `401` twice.
struct SessionEnded;

/// Handles an API or refresh answer the same way in every state that holds
/// `account`: refreshes and retries, the revision check and storing a sync.
/// Leaves to the caller what only the unlocked state does.
fn account_answer(
    config: &Config,
    requests: &mut Table,
    account: &mut Account,
    now_ms: u64,
    purpose: Purpose,
    response: Result<HttpResponse, HttpError>,
    effects: &mut Vec<Effect>,
) -> Result<Option<ApiReply>, SessionEnded> {
    match (purpose, response) {
        (Purpose::Refresh, Err(error)) => {
            account.refreshing = false;
            // Offline: the held calls fail like any offline request, and
            // the user retries them.
            account.waiting.clear();
            Ok(Some(ApiReply::Failed {
                message: error.message,
                write: None,
            }))
        }
        (Purpose::Refresh, Ok(answer)) => {
            account.refreshing = false;
            let token = match answer.status {
                200 => proto::from_slice::<TokenResponse>(&answer.body).ok(),
                _ => None,
            };
            let token = token.ok_or(SessionEnded)?;
            account.tokens.access = Some(Zeroizing::new(token.access_token));
            account.tokens.expires_at =
                now_ms.saturating_add(token.expires_in.saturating_mul(1000));
            if let Some(refresh) = token.refresh_token {
                effects.push(Effect::Store {
                    id: requests.start(Purpose::Store),
                    slot: StoreSlot::RefreshToken,
                    bytes: refresh.as_bytes().to_vec(),
                });
                account.tokens.refresh = Some(Zeroizing::new(refresh));
            }
            for call in mem::take(&mut account.waiting) {
                send_api(config, requests, account, now_ms, call, effects);
            }
            Ok(None)
        }
        (Purpose::Api(mut call), Ok(answer)) if answer.status == 401 => {
            if call.retried {
                return Err(SessionEnded);
            }
            call.retried = true;
            account.tokens.access = None;
            send_api(config, requests, account, now_ms, call, effects);
            Ok(None)
        }
        (Purpose::Api(call), response) => match (call.kind, response) {
            (ApiKind::Write(write), Ok(answer)) => Ok(Some(ApiReply::Written { write, answer })),
            (ApiKind::Folder(folder), response) => Ok(Some(ApiReply::Folder { folder, response })),
            (kind, Err(error)) => Ok(Some(ApiReply::Failed {
                message: error.message,
                write: match kind {
                    ApiKind::Write(write) => Some(write),
                    _ => None,
                },
            })),
            (ApiKind::RevisionDate { force }, Ok(answer)) => {
                let revision = match answer.status {
                    200 => serde_json::from_slice::<u64>(&answer.body).ok(),
                    _ => None,
                };
                match revision {
                    None => Ok(Some(ApiReply::Failed {
                        message: format!("revision check failed: HTTP {}", answer.status),
                        write: None,
                    })),
                    Some(revision) => {
                        let current = account.cache.as_ref().map(|cache| cache.revision);
                        if force || current.is_none_or(|current| revision > current) {
                            let call = ApiCall {
                                method: Method::Get,
                                path: vec!["sync".into()],
                                query: &[("excludeDomains", "true")],
                                body: Body::None,
                                kind: ApiKind::Sync { revision },
                                retried: false,
                            };
                            send_api(config, requests, account, now_ms, call, effects);
                        }
                        Ok(None)
                    }
                }
            }
            (ApiKind::Sync { revision }, Ok(answer)) => {
                let body = match answer.status {
                    200 => serde_json::from_slice::<Value>(&answer.body).ok(),
                    _ => None,
                };
                match body.and_then(|body| Cache::from_sync(body, revision, now_ms)) {
                    None => Ok(Some(ApiReply::Failed {
                        message: format!("sync failed: HTTP {}", answer.status),
                        write: None,
                    })),
                    Some(cache) => {
                        // Never put an older account revision over a newer one,
                        // which another process may have stored meanwhile.
                        let newer = account
                            .cache
                            .as_ref()
                            .is_none_or(|current| current.revision <= cache.revision);
                        if newer {
                            account.cache = Some(Box::new(cache));
                            store_cache(requests, account, effects);
                        }
                        Ok(Some(ApiReply::Synced))
                    }
                }
            }
        },
        (_, _) => Ok(None),
    }
}

/// Sends `call` with the current access token, or holds it in
/// `account.waiting` and starts a refresh when the token is missing or
/// expires within [`REFRESH_MARGIN`].
fn send_api(
    config: &Config,
    requests: &mut Table,
    account: &mut Account,
    now_ms: u64,
    call: ApiCall,
    effects: &mut Vec<Effect>,
) {
    let fresh = account.tokens.access.is_some()
        && account.tokens.expires_at > now_ms.saturating_add(REFRESH_MARGIN);
    match (&account.tokens.refresh, fresh) {
        (Some(refresh), false) => {
            account.waiting.push(call);
            if !account.refreshing {
                account.refreshing = true;
                let mut form = device_form(config);
                form.extend([
                    ("grant_type", secret("refresh_token")),
                    ("client_id", secret(&config.client_id)),
                    ("refresh_token", refresh.clone()),
                ]);
                effects.push(Effect::Http {
                    id: requests.start(Purpose::Refresh),
                    request: token_request(&account.server.urls.identity, config, form),
                });
            }
        }
        // With no refresh token, send it anyway: a `401` then ends the session.
        _ => {
            let mut headers = client_headers(config);
            if let Some(access) = &account.tokens.access {
                headers.push(("Authorization", format!("Bearer {}", access.as_str())));
            }
            let request = HttpRequest {
                method: call.method,
                url: endpoint(&account.server.urls.api, &call.path, call.query),
                headers,
                body: call.body.clone(),
            };
            effects.push(Effect::Http {
                id: requests.start(Purpose::Api(call)),
                request,
            });
        }
    }
}

/// A revision check that syncs when the account changed, or always when
/// `force`.
fn sync_call(force: bool) -> ApiCall {
    ApiCall {
        method: Method::Get,
        path: vec!["accounts".into(), "revision-date".into()],
        query: &[],
        body: Body::None,
        kind: ApiKind::RevisionDate { force },
        retried: false,
    }
}

/// Sends `edit` of the cipher `id`, built from its cached copy. Sets
/// [`ItemError::NotFound`] when the cache has no such cipher.
fn send_edit(
    config: &Config,
    requests: &mut Table,
    unlocked: &mut Unlocked,
    now_ms: u64,
    id: Uuid,
    edit: ItemEdit,
    effects: &mut Vec<Effect>,
) {
    let current = unlocked
        .account
        .cache
        .as_ref()
        .and_then(|cache| cache.ciphers.iter().find(|cipher| cipher.id == id));
    let Some(current) = current else {
        unlocked.error = Some(ItemError::NotFound);
        return;
    };
    let (path, body, action) = match edit {
        // The partial update takes the favorite flag too, so it goes back as
        // it is.
        ItemEdit::Folder(folder) => (
            vec!["ciphers".into(), id.to_string(), "partial".into()],
            serde_json::json!({
                "folderId": folder.map(|folder| folder.to_string()),
                "favorite": current.favorite,
            }),
            WriteAction::Partial,
        ),
        ItemEdit::Reprompt(on) => {
            let mut edits = Map::new();
            edits.insert("reprompt".into(), Value::from(u8::from(on)));
            (
                vec!["ciphers".into(), id.to_string()],
                current.update_body(edits, unlocked.account.user_id),
                WriteAction::Update,
            )
        }
    };
    let call = ApiCall {
        method: Method::Put,
        path,
        query: &[],
        body: Body::Json(body),
        kind: ApiKind::Write(Write {
            cipher_id: Some(id),
            action,
            view: None,
            passkey: None,
        }),
        retried: false,
    };
    unlocked.error = None;
    send_api(
        config,
        requests,
        &mut unlocked.account,
        now_ms,
        call,
        effects,
    );
}

/// Whether a write to the cipher `cipher_id` is in flight or waits for a
/// token.
fn writing(requests: &Table, account: &Account, cipher_id: Uuid) -> bool {
    let write_to = |kind: &ApiKind| matches!(kind, ApiKind::Write(write) if write.cipher_id == Some(cipher_id));
    requests.any(|purpose| matches!(purpose, Purpose::Api(call) if write_to(&call.kind)))
        || account.waiting.iter().any(|call| write_to(&call.kind))
}

fn store_cache(requests: &mut Table, account: &Account, effects: &mut Vec<Effect>) {
    if let Some(cache) = &account.cache {
        effects.push(Effect::Store {
            id: requests.start(Purpose::Store),
            slot: StoreSlot::VaultCache,
            bytes: cache.to_document(account.user_id).to_vec(),
        });
    }
}

/// The stored document in `bytes` when it is for this account, in a known
/// format, and of a higher revision than the cache in memory.
fn newer_cache(account: &Account, bytes: &[u8]) -> Option<Cache> {
    let document = CacheDocument::from_slice(bytes)?;
    let current = account.cache.as_ref().map(|cache| cache.revision);
    if document.user_id != account.user_id
        || current.is_some_and(|current| document.revision <= current)
    {
        return None;
    }
    Cache::from_document(document)
}

/// Rebuilds the keys and the index from the cache, bumping `index_revision`
/// only when the index changed. Sends the passkey catalog when it changed,
/// marks the open item stale when the server copy changed, and readies a
/// conflict that waited for this sync.
fn rebuild(unlocked: &mut Unlocked, index_revision: &mut u64, effects: &mut Vec<Effect>) {
    let Some(cache) = &unlocked.account.cache else {
        return;
    };
    unlocked.keys = VaultKeys::new(unlocked.keys.user().clone(), &cache.profile);
    let index = VaultIndex::decrypt(&cache.folders, &cache.ciphers, &unlocked.keys);
    if index != unlocked.index {
        unlocked.index = index;
        *index_revision += 1;
    }
    emit_catalog(&mut unlocked.account, &unlocked.keys, effects);
    let Some(cache) = &unlocked.account.cache else {
        return;
    };
    let Some(open) = unlocked.open.as_mut() else {
        return;
    };
    let current = cache
        .ciphers
        .iter()
        .find(|cipher| cipher.id == open.base.id);
    open.stale = current.is_none_or(|current| current.revision_date != open.base.revision_date);
    if let ConflictStage::Syncing { .. } = open.conflict
        && let ConflictStage::Syncing { mine } =
            mem::replace(&mut open.conflict, ConflictStage::None)
    {
        match current.map(|current| CipherView::decrypt(current, &unlocked.keys)) {
            Some(Ok(theirs)) => {
                open.conflict = ConflictStage::Ready {
                    mine,
                    theirs: Box::new(theirs),
                };
            }
            Some(Err(error)) => unlocked.error = Some(ItemError::Unreadable(error)),
            None => unlocked.error = Some(ItemError::NotFound),
        }
    }
}

/// Unlocks `account` with its user key: decrypts the cached index, starts a
/// sync, sends the passkey catalog and serves a passkey request that waited
/// for unlock. With no cache it stays locked with
/// [`UnlockError::NoVaultData`].
#[expect(clippy::too_many_arguments, reason = "the parts of one handle call")]
fn open_vault(
    config: &Config,
    settings: &Settings,
    requests: &mut Table,
    index_revision: &mut u64,
    mut account: Account,
    user_key: SymmetricKey,
    now_ms: u64,
    rng: &mut impl CryptoRngCore,
    effects: &mut Vec<Effect>,
) -> State {
    let Some(cache) = &account.cache else {
        return State::Locked {
            account,
            error: Some(UnlockError::NoVaultData),
        };
    };
    let keys = VaultKeys::new(user_key, &cache.profile);
    let index = VaultIndex::decrypt(&cache.folders, &cache.ciphers, &keys);
    *index_revision += 1;
    send_api(
        config,
        requests,
        &mut account,
        now_ms,
        sync_call(true),
        effects,
    );
    let waiting = account.passkey.take();
    // SSH requests that waited keep waiting for approval, unless they ran
    // out of time or name a key the vault lacks.
    for pending in account.ssh.extract_if(.., |pending| {
        now_ms >= pending.deadline
            || !index
                .ssh_keys
                .iter()
                .any(|key| key.public_key == pending.public_key)
    }) {
        fail_ssh(pending.id, effects);
    }
    let mut unlocked = Box::new(Unlocked {
        account,
        keys,
        index,
        open: None,
        sync: SyncState::Running,
        error: None,
        last_activity: now_ms,
        last_check: now_ms,
        retries: Vec::new(),
        device_error: None,
        ssh_grants: Vec::new(),
        edits: Vec::new(),
    });
    release_ssh(&mut unlocked, settings, effects);
    emit_catalog(&mut unlocked.account, &unlocked.keys, effects);
    if let Some(pending) = waiting {
        serve_passkey(
            config,
            requests,
            &mut unlocked,
            now_ms,
            rng,
            *pending,
            effects,
        );
    }
    State::Unlocked(unlocked)
}

/// Drops every key, the index and the open item, and forgets pending
/// requests so their answers arrive as stale. A passkey save in flight is
/// refused; a passkey request that waits for a pick waits for unlock again.
fn lock(requests: &mut Table, unlocked: Box<Unlocked>, effects: &mut Vec<Effect>) -> State {
    let Unlocked { mut account, .. } = *unlocked;
    abandon_writes(requests, &mut account, effects);
    account.refreshing = false;
    if let Some(pending) = account.passkey.as_mut() {
        pending.choices.clear();
    }
    for pending in account.ssh.drain(..) {
        fail_ssh(pending.id, effects);
    }
    effects.push(Effect::Rerender);
    State::Locked {
        account,
        error: None,
    }
}

/// Clears the session's slots and the passkey catalog, keeping the email
/// for the login form. Every passkey request still open is refused.
fn logout(
    requests: &mut Table,
    mut account: Account,
    error: Option<LoginError>,
    effects: &mut Vec<Effect>,
) -> State {
    abandon_writes(requests, &mut account, effects);
    if let Some(pending) = account.passkey.take() {
        refuse_passkey(pending.id, effects);
    }
    for pending in account.ssh.drain(..) {
        fail_ssh(pending.id, effects);
    }
    let email = account.email;
    effects.extend([
        Effect::Clear {
            slot: StoreSlot::RefreshToken,
        },
        Effect::Clear {
            slot: StoreSlot::VaultCache,
        },
        Effect::Clear {
            slot: StoreSlot::DeviceWrap,
        },
        Effect::PasskeyCatalog {
            entries: Vec::new(),
        },
        Effect::Rerender,
    ]);
    State::LoggedOut {
        form: LoginForm {
            server: Some(account.server.url),
            email,
        },
        error,
    }
}

/// Holds `pending` in `slot`, refusing the request it replaces.
fn hold_passkey(slot: &mut Option<Box<Pending>>, pending: Pending, effects: &mut Vec<Effect>) {
    if let Some(older) = slot.replace(Box::new(pending)) {
        refuse_passkey(older.id, effects);
    }
}

/// Answers an agent frame that arrives while locked: an empty list, or a
/// sign request held in `ssh` until unlock or its deadline.
fn hold_ssh(
    ssh: &mut Vec<ssh_agent::Pending>,
    now_ms: u64,
    id: AgentRequestId,
    frame: &[u8],
    effects: &mut Vec<Effect>,
) {
    match ssh_agent::parse_request(frame) {
        Ok(ssh_agent::AgentRequest::List) => effects.push(Effect::SshAgentReply {
            id,
            frame: ssh_agent::identities_reply(&[]),
        }),
        Ok(ssh_agent::AgentRequest::Sign {
            public_key,
            data,
            flags,
        }) => {
            ssh.push(ssh_agent::Pending {
                id,
                public_key,
                data,
                flags,
                deadline: now_ms.saturating_add(ssh_agent::WAIT),
            });
            effects.push(Effect::Rerender);
        }
        Ok(ssh_agent::AgentRequest::Other(_)) | Err(ssh_agent::AgentParseError) => {
            fail_ssh(id, effects)
        }
    }
}

/// Fails every request in `ssh` whose deadline passed.
fn expire_ssh(ssh: &mut Vec<ssh_agent::Pending>, now_ms: u64, effects: &mut Vec<Effect>) {
    let expired: Vec<_> = ssh
        .extract_if(.., |pending| now_ms >= pending.deadline)
        .collect();
    if !expired.is_empty() {
        for pending in expired {
            fail_ssh(pending.id, effects);
        }
        effects.push(Effect::Rerender);
    }
}

fn fail_ssh(id: AgentRequestId, effects: &mut Vec<Effect>) {
    effects.push(Effect::SshAgentReply {
        id,
        frame: ssh_agent::failure_reply(),
    });
}

/// Signs every waiting request whose key need not ask: its mode is
/// [`SshAsk::Never`], or [`SshAsk::UntilLock`] with a grant. Returns whether
/// any was signed, after pushing [`Effect::Rerender`] for them.
fn release_ssh(unlocked: &mut Unlocked, settings: &Settings, effects: &mut Vec<Effect>) -> bool {
    let free: Vec<_> = unlocked
        .account
        .ssh
        .extract_if(.., |pending| {
            match ssh_ask(&unlocked.index, settings, &pending.public_key) {
                SshAsk::Always => false,
                SshAsk::Never => true,
                SshAsk::UntilLock => unlocked.ssh_grants.contains(&pending.public_key),
            }
        })
        .collect();
    for pending in &free {
        let frame = match &unlocked.account.cache {
            None => ssh_agent::failure_reply(),
            Some(cache) => ssh_agent::approved_reply(
                pending,
                &unlocked.index.ssh_keys,
                &cache.ciphers,
                &unlocked.keys,
            ),
        };
        effects.push(Effect::SshAgentReply {
            id: pending.id,
            frame,
        });
    }
    if !free.is_empty() {
        effects.push(Effect::Rerender);
    }
    !free.is_empty()
}

/// The mode of the vault key `public_key`: its own `ssh-ask` field, else the
/// global mode. A key the vault lacks gets the global mode.
fn ssh_ask(index: &VaultIndex, settings: &Settings, public_key: &[u8]) -> SshAsk {
    index
        .ssh_keys
        .iter()
        .find(|key| key.public_key == public_key)
        .and_then(|key| key.ask)
        .unwrap_or(settings.ssh_ask)
}

/// The name of the cipher that holds an SSH key; empty when it is gone.
fn ssh_key_name(index: &VaultIndex, cipher_id: Uuid) -> &str {
    index
        .ciphers
        .iter()
        .find(|cipher| cipher.id == cipher_id)
        .map_or("", |cipher| cipher.name.as_str())
}

fn refuse_passkey(id: RequestId, effects: &mut Vec<Effect>) {
    effects.push(Effect::PasskeyReply {
        id,
        reply: Err(PasskeyError::NotAllowed),
    });
}

/// Answers `pending` at once when it is an assertion that only one passkey
/// fits. Otherwise holds it with its choices for the user to pick, or
/// refuses it when nothing fits.
fn serve_passkey(
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
        Some(cache) => passkey::choices(&pending.request, &cache.ciphers, &unlocked.keys),
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
fn answer_passkey(
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
                    passkey::credential_id_bytes(&credential.credential_id).as_ref()
                        == Some(&credential_id)
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
                reply: Ok(passkey::assert(credential, client_data_hash, counter)),
            });
            let bump = CounterBump {
                cipher_id,
                credential_id: credential.credential_id.clone(),
                counter,
            };
            return counter_write(config, requests, unlocked, now_ms, rng, bump, effects);
        }
        (Target::NewLogin, PasskeyRequest::MakeCredential { .. }) => {
            let Some((credential, reply)) = passkey::create(request, now_ms, rng) else {
                return refuse_passkey(id, effects);
            };
            let mut view = passkey::new_login(request);
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
                (opened, passkey::create(request, now_ms, rng))
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
fn counter_write(
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

/// Sends [`Effect::PasskeyCatalog`] when the passkeys in the cache differ
/// from the catalog last sent.
fn emit_catalog(account: &mut Account, keys: &VaultKeys, effects: &mut Vec<Effect>) {
    let Some(cache) = &account.cache else {
        return;
    };
    let entries = passkey::catalog(&cache.ciphers, keys);
    if entries != account.catalog {
        account.catalog = entries.clone();
        effects.push(Effect::PasskeyCatalog { entries });
    }
}

/// Forgets every pending request and held call, refusing the passkey saves
/// among them.
fn abandon_writes(requests: &mut Table, account: &mut Account, effects: &mut Vec<Effect>) {
    let held = mem::take(&mut account.waiting);
    let pending = requests
        .drain()
        .into_iter()
        .filter_map(|purpose| match purpose {
            Purpose::Api(call) => Some(call),
            _ => None,
        });
    for call in pending.chain(held) {
        if let ApiKind::Write(Write {
            passkey: Some(PasskeyWrite::Created { id, .. }),
            ..
        }) = call.kind
        {
            refuse_passkey(id, effects);
        }
    }
}

/// The request that asks the server to email a second-factor code.
fn send_email_code(
    server: &Server,
    config: &Config,
    email: &str,
    password_hash: &SecretString,
) -> HttpRequest {
    HttpRequest {
        method: Method::Post,
        url: endpoint(&server.urls.api, &["two-factor", "send-email-login"], &[]),
        headers: client_headers(config),
        body: Body::Json(serde_json::json!({
            "email": email,
            "masterPasswordHash": password_hash.as_str(),
        })),
    }
}

/// The `grant_type=password` token request, with the second factor or
/// new-device code when the user gave one.
/// A token request with the password hash, to the `identity` base.
fn password_token(
    identity: &Url,
    config: &Config,
    email: &str,
    password_hash: &SecretString,
    answer: Option<&Answer>,
) -> HttpRequest {
    let mut form = device_form(config);
    form.extend([
        ("grant_type", secret("password")),
        ("scope", secret("api offline_access")),
        ("client_id", secret(&config.client_id)),
        ("username", secret(email)),
        ("password", password_hash.clone()),
    ]);
    match answer {
        None => {}
        Some(Answer::TwoFactor {
            provider,
            code,
            remember,
        }) => form.extend([
            ("twoFactorToken", code.clone()),
            ("twoFactorProvider", secret(&provider.to_string())),
            (
                "twoFactorRemember",
                secret(if *remember { "1" } else { "0" }),
            ),
        ]),
        Some(Answer::Remembered { token }) => form.extend([
            ("twoFactorToken", token.clone()),
            ("twoFactorProvider", secret(&REMEMBER_PROVIDER.to_string())),
            ("twoFactorRemember", secret("0")),
        ]),
        Some(Answer::NewDevice { code }) => form.push(("newDeviceOtp", code.clone())),
    }
    token_request(identity, config, form)
}

/// The `twoFactorProvider` number for a remembered token.
const REMEMBER_PROVIDER: u8 = 5;

/// A request to the token endpoint under the `identity` base.
fn token_request(
    identity: &Url,
    config: &Config,
    form: Vec<(&'static str, SecretString)>,
) -> HttpRequest {
    HttpRequest {
        method: Method::Post,
        url: endpoint(identity, &["connect", "token"], &[]),
        headers: client_headers(config),
        body: Body::Form(form),
    }
}

fn device_form(config: &Config) -> Vec<(&'static str, SecretString)> {
    vec![
        ("deviceType", secret(&config.device_type.to_string())),
        ("deviceIdentifier", secret(&config.device_id.to_string())),
        ("deviceName", secret(&config.device_name)),
    ]
}

fn client_headers(config: &Config) -> Vec<(&'static str, String)> {
    vec![
        ("Device-Type", config.device_type.to_string()),
        ("Bitwarden-Client-Name", config.client_name.clone()),
        ("Bitwarden-Client-Version", config.client_version.clone()),
    ]
}

fn secret(text: &str) -> SecretString {
    Zeroizing::new(text.to_owned())
}

/// The error to show for a failed identity request.
fn login_error(response: &Result<HttpResponse, HttpError>) -> LoginError {
    match response {
        Err(error) => LoginError::Network(error.message.clone()),
        Ok(answer) => match proto::from_slice::<ErrorResponse>(&answer.body)
            .ok()
            .as_ref()
            .and_then(ErrorResponse::message)
        {
            Some(message) if (400..500).contains(&answer.status) => {
                LoginError::Rejected(message.to_owned())
            }
            _ => LoginError::Server {
                status: answer.status,
            },
        },
    }
}

/// The user id in the `sub` claim of a JWT access token. The signature is
/// not checked: the server that issued the token is the only one that
/// reads it.
fn jwt_subject(token: &str) -> Option<Uuid> {
    #[derive(Deserialize)]
    struct Claims {
        sub: Uuid,
    }
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Claims>(&bytes)
        .ok()
        .map(|claims| claims.sub)
}

/// Epoch milliseconds of an RFC 3339 date such as
/// `2026-11-20T17:16:20.743050Z`. A missing offset reads as UTC, and digits
/// past milliseconds are cut.
fn epoch_millis(text: &str) -> Option<u64> {
    let (date, time) = text.split_once('T')?;
    let mut date_parts = date.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (
        date_parts.next()??,
        date_parts.next()??,
        date_parts.next()??,
    );
    let (time, offset_minutes) = match time.strip_suffix('Z') {
        Some(time) => (time, 0),
        None => match time.rfind(['+', '-']) {
            None => (time, 0),
            Some(index) => {
                let (hours, minutes) = time[index + 1..].split_once(':')?;
                let minutes = hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?;
                let sign = if time.as_bytes()[index] == b'-' {
                    -1
                } else {
                    1
                };
                (&time[..index], sign * minutes)
            }
        },
    };
    let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut clock_parts = clock.splitn(3, ':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (
        clock_parts.next()??,
        clock_parts.next()??,
        clock_parts.next()??,
    );
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let millis = fraction
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .fold(0, |total, digit| total * 10 + i64::from(digit - b'0'));
    // Days from 1970-01-01 to the date, by Howard Hinnant's `days_from_civil`.
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = ((days * 24 + hour) * 60 + minute) * 60 + second - offset_minutes * 60;
    u64::try_from(seconds * 1000 + millis).ok()
}
