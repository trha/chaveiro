//! The client state machine: inputs in, effects out.

mod api;
mod config;
mod effect;
mod input;
mod login;
pub mod passkey;
mod record;
mod request;
mod screen;
mod session;
pub mod ssh_agent;
mod state;

use api::{
    ApiReply, REVISION_POLL, SessionEnded, account_answer, creating, send_api, send_edit,
    sync_call, writing,
};
use config::{Server, endpoint};
use login::{
    client_headers, device_form, jwt_subject, login_error, password_token, secret, send_email_code,
    token_request,
};
use passkey::{answer_passkey, counter_write, hold_passkey, refuse_passkey, serve_passkey};
use record::{AccountRecord, Cache, RememberedLogin};
use session::{lock, logout, newer_cache, open_vault, rebuild, store_cache};
use ssh_agent::{expire_ssh, fail_ssh, hold_ssh, release_ssh, ssh_ask, ssh_key_name};
use state::{
    Account, Answer, ConflictStage, Credentials, DerivePurpose, DeviceWrap, ItemEdit, LoginForm,
    State, SyncState, Tokens, Unlocked,
};

use crate::{
    crypto::{EncString, SecretString, SymmetricKey, device_key},
    proto::{
        self,
        cache::CacheDocument,
        cipher::{CipherResponse, FolderResponse, create_body, folder_body, is_out_of_date},
        identity::{ErrorResponse, PreloginResponse, TokenFailure, TokenResponse, prelogin_body},
        sync::Profile,
    },
    vault::{self, CipherView, CopyField, VaultIndex, VaultKeys, decrypt_user_key},
};
pub use config::{Config, ServerUrlError, ServerUrls};
pub use effect::{Body, Effect, HttpRequest, Method, StoreSlot};
pub use input::{
    DeviceKeyError, HttpError, HttpResponse, Input, SecretBytes, Settings, StorageError,
    StoredAccount, UserAction,
};
use jiff::Timestamp;
use passkey::Pending;
pub use passkey::{
    PasskeyCatalogEntry, PasskeyChoice, PasskeyError, PasskeyPrompt, PasskeyReply, PasskeyRequest,
};
use rand_core::CryptoRngCore;
pub use request::RequestId;
use request::{ApiCall, ApiKind, FolderWrite, PasskeyWrite, Purpose, Table, Write, WriteAction};
pub use screen::{
    CODE_PROVIDERS, Challenge, Conflict, ItemError, LoginError, OpenItem, Screen, UnlockError,
    VaultScreen,
};
use serde_json::{Map, Value};
pub use ssh_agent::{AgentRequestId, SshApproval, SshAsk};
use std::mem;
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
                let busy = match view.id {
                    Some(cipher_id) => writing(requests, &unlocked.account, cipher_id),
                    None => creating(requests, &unlocked.account),
                };
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
                }
                State::Unlocked(unlocked)
            }
            (State::Unlocked(unlocked), Input::User(UserAction::Copy { id, field }), None) => {
                let text = unlocked
                    .account
                    .cache
                    .as_ref()
                    .and_then(|cache| cache.ciphers.iter().find(|cipher| cipher.id == id))
                    .filter(|cipher| !(field.guarded() && cipher.reprompt == 1))
                    .and_then(|cipher| {
                        // An unreadable field copies nothing, so the error goes.
                        vault::decrypt_field(cipher, &unlocked.keys, field).ok().flatten()
                    });
                let text = match field {
                    CopyField::Totp => {
                        text.and_then(|secret| crate::totp::code(&secret, now_ms / 1000))
                    }
                    _ => text,
                };
                if let Some(text) = text {
                    effects.push(Effect::Copy { text });
                }
                State::Unlocked(unlocked)
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
                                            .and_then(|date| date.parse::<Timestamp>().ok())
                                            .and_then(|time| u64::try_from(time.as_millisecond()).ok())
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
                                // A user create opens the new item, whatever
                                // was open, so the shell can select it.
                                let reopen = user_create
                                    || unlocked
                                        .open
                                        .as_ref()
                                        .is_some_and(|open| open.base.id == saved.id);
                                if reopen
                                    && let Ok(view) = CipherView::decrypt(&saved, &unlocked.keys)
                                {
                                    unlocked.open = Some(OpenItem {
                                        view,
                                        base: saved.clone(),
                                        stale: false,
                                        conflict: ConflictStage::None,
                                    });
                                }
                                let cache = unlocked
                                    .account
                                    .cache
                                    .as_mut()
                                    .expect("the guard checked the cache");
                                let revision = saved
                                    .revision_date
                                    .parse::<Timestamp>()
                                    .ok()
                                    .and_then(|time| u64::try_from(time.as_millisecond()).ok())
                                    .unwrap_or(0);
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
                    | UserAction::Copy { .. }
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
                    | UserAction::Copy { .. }
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
                    | UserAction::Copy { .. }
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
                    | UserAction::Copy { .. }
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
                    | UserAction::Copy { .. }
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
                    | UserAction::Copy { .. }
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
                    | UserAction::Copy { .. }
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

    fn sending_email_code(&self) -> bool {
        self.requests
            .any(|purpose| matches!(purpose, Purpose::SendEmailCode))
    }
}
