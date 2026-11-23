use super::{
    config::{Config, endpoint},
    effect::{Body, Effect, HttpRequest, Method, StoreSlot},
    input::{HttpError, HttpResponse},
    login::{client_headers, device_form, secret, token_request},
    passkey::refuse_passkey,
    record::Cache,
    request::{ApiCall, ApiKind, FolderWrite, PasskeyWrite, Purpose, Table, Write, WriteAction},
    screen::ItemError,
    session::store_cache,
    state::{Account, ItemEdit, Unlocked},
};
use crate::{core::mem, proto, proto::identity::TokenResponse};
use serde_json::{Map, Value};
use uuid::Uuid;
use zeroize::Zeroizing;

/// How long (ms) the vault stays unlocked between revision checks.
pub(super) const REVISION_POLL: u64 = 5 * 60 * 1000;

/// Refresh this long (ms) before the access token expires.
pub(super) const REFRESH_MARGIN: u64 = 60 * 1000;

/// What [`account_answer`] leaves to the unlocked state.
pub(super) enum ApiReply {
    /// A sync answer arrived; the cache may have changed.
    Synced,
    /// Calls that got no answer: one request, or every call held for a
    /// refresh that failed.
    Failed {
        message: String,
        failed: Vec<ApiKind>,
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
pub(super) struct SessionEnded;

/// Handles an API or refresh answer the same way in every state that holds
/// `account`: refreshes and retries, the revision check and storing a sync.
/// Leaves to the caller what only the unlocked state does.
pub(super) fn account_answer(
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
            let failed = account.waiting.drain(..).map(|call| call.kind).collect();
            Ok(Some(ApiReply::Failed {
                message: error.message,
                failed,
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
                failed: vec![kind],
            })),
            (ApiKind::RevisionDate { force }, Ok(answer)) => {
                let revision = match answer.status {
                    200 => serde_json::from_slice::<u64>(&answer.body).ok(),
                    _ => None,
                };
                match revision {
                    None => Ok(Some(ApiReply::Failed {
                        message: format!("revision check failed: HTTP {}", answer.status),
                        failed: vec![ApiKind::RevisionDate { force }],
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
                        failed: vec![ApiKind::Sync { revision }],
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
pub(super) fn send_api(
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
pub(super) fn sync_call(force: bool) -> ApiCall {
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
pub(super) fn send_edit(
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
pub(super) fn writing(requests: &Table, account: &Account, cipher_id: Uuid) -> bool {
    pending_write(requests, account, |write| {
        write.cipher_id == Some(cipher_id)
    })
}

/// A create the user saved is in flight or waits to be sent. A passkey
/// create does not count, because the user did not save it from a form.
pub(super) fn creating(requests: &Table, account: &Account) -> bool {
    pending_write(requests, account, |write| {
        write.action == WriteAction::Create && write.passkey.is_none()
    })
}

/// A create or update the user saved from a form is in flight or waits to
/// be sent.
pub(super) fn saving(requests: &Table, account: &Account) -> bool {
    // Only `UserAction::Save` sends a write with a view.
    pending_write(requests, account, |write| write.view.is_some())
}

/// Whether a write that `matches` is in flight or waits for a token.
fn pending_write(requests: &Table, account: &Account, matches: impl Fn(&Write) -> bool) -> bool {
    let wanted = |kind: &ApiKind| matches!(kind, ApiKind::Write(write) if matches(write));
    requests.any(|purpose| matches!(purpose, Purpose::Api(call) if wanted(&call.kind)))
        || account.waiting.iter().any(|call| wanted(&call.kind))
}

/// Forgets every pending request and held call, refusing the passkey saves
/// among them.
pub(super) fn abandon_writes(
    requests: &mut Table,
    account: &mut Account,
    effects: &mut Vec<Effect>,
) {
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
