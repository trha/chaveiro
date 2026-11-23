use super::{
    api::{abandon_writes, send_api, sync_call},
    config::Config,
    effect::{Effect, StoreSlot},
    input::Settings,
    passkey::{refuse_passkey, serve_passkey},
    record::Cache,
    request::{Purpose, Table},
    screen::{ItemError, LoginError, UnlockError},
    ssh_agent::{fail_ssh, release_ssh},
    state::{Account, ConflictStage, LoginForm, State, SyncState, Unlocked},
};
use crate::{
    core::{mem, passkey},
    crypto::SymmetricKey,
    proto::cache::CacheDocument,
    vault::{CipherView, VaultIndex, VaultKeys},
};
use rand_core::CryptoRngCore;

pub(super) fn store_cache(requests: &mut Table, account: &Account, effects: &mut Vec<Effect>) {
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
pub(super) fn newer_cache(account: &Account, bytes: &[u8]) -> Option<Cache> {
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
pub(super) fn rebuild(
    unlocked: &mut Unlocked,
    index_revision: &mut u64,
    effects: &mut Vec<Effect>,
) {
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
pub(super) fn open_vault(
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
pub(super) fn lock(
    requests: &mut Table,
    unlocked: Box<Unlocked>,
    effects: &mut Vec<Effect>,
) -> State {
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
pub(super) fn logout(
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

/// Sends [`Effect::PasskeyCatalog`] when the passkeys in the cache differ
/// from the catalog last sent.
pub(super) fn emit_catalog(account: &mut Account, keys: &VaultKeys, effects: &mut Vec<Effect>) {
    let Some(cache) = &account.cache else {
        return;
    };
    let entries = passkey::catalog(&cache.ciphers, keys);
    if entries != account.catalog {
        account.catalog = entries.clone();
        effects.push(Effect::PasskeyCatalog { entries });
    }
}
