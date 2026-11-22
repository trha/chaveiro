//! The vault window on its own, over a made-up vault, so it can be seen and
//! driven without an account. It plays the core's part for the item
//! actions: Open, Close, Save of a favorite, Trash and Restore change the
//! made-up vault and print to stderr, and Copy prints the text it copies.
//! Sync now runs a made-up sync of 2 seconds that alternates between a
//! success and offline. Lock prints `lock` and closes the
//! window. `tests/ui/vault.ps1` drives it.
//!
//! `VAULT_BENCH=1` runs passes on a timer, counts the Rust allocations of
//! each steady pass on the UI thread, prints a summary and exits.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]
#![allow(dead_code, reason = "shares modules with the app")]

#[path = "../src/vault.rs"]
mod vault;

use chaveiro_core::{
    core::{UserAction, VaultScreen},
    crypto::SecretString,
    vault::{
        CardView, CipherContent, CipherKind, CipherSummary, CipherView, CopyField, CopyFields,
        CustomField, FolderSummary, IdentityView, LoginView, SshKeyView, UriView, VaultIndex,
    },
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
use uuid::Uuid;
use vault::{VaultAction, VaultApp, VaultForm};
use windows_core::Result;
use winui_immediate::{App, Handle, Ui, run};

fn main() -> Result<()> {
    let bench = std::env::var_os("VAULT_BENCH").map(|_| Bench::new());
    let ticking = bench.is_some();
    run(move |handle: Handle<Message>| {
        if ticking {
            let handle = handle.clone();
            std::thread::spawn(move || {
                while handle.send(Message::Tick) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            });
        }
        let mut index = sample();
        let views: Vec<CipherView> = index.ciphers.iter().map(view_of).collect();
        // The index holds a login's addresses for search and the reprompt
        // flag, as the core's does.
        for (summary, view) in index.ciphers.iter_mut().zip(&views) {
            summary.reprompt = view.reprompt;
            summary.copyable = copyable(view);
            if let CipherContent::Login(login) = &view.content {
                summary.uris = login
                    .uris
                    .iter()
                    .filter_map(|uri| uri.uri.clone())
                    .collect();
            }
        }
        Ok(Preview {
            index,
            views,
            revision: 1,
            opened: None,
            form: VaultForm::default(),
            open: true,
            bench,
            handle,
            syncs: 0,
            server: url::Url::parse("https://vault.bitwarden.com").expect("a valid URL"),
            sync: match std::env::var("VAULT_SYNC").as_deref() {
                Ok("syncing") => Some(SyncPreview::Syncing),
                Ok("offline") => Some(SyncPreview::Offline),
                _ => None,
            },
            synced_at: epoch_ms().saturating_sub(2 * 60_000),
        })
    })
}

/// The made-up vault and the item it has open, as the core would keep
/// them.
///
/// Contract: `views` holds one view per item of `index`, in its order,
/// and `revision` changes whenever `index` does.
struct Preview {
    index: VaultIndex,
    views: Vec<CipherView>,
    revision: u64,
    opened: Option<Uuid>,
    form: VaultForm,
    open: bool,
    bench: Option<Bench>,
    handle: Handle<Message>,
    /// Syncs finished, which alternate between success and offline.
    syncs: u32,
    server: url::Url,
    /// `VAULT_SYNC`: `syncing` or `offline` starts in that sync state.
    /// Sync now then runs a sync of 2 seconds.
    sync: Option<SyncPreview>,
    /// When the last sync succeeded, in epoch ms; two minutes before the
    /// preview started at first.
    synced_at: u64,
}

enum Message {
    /// A bench pass.
    Tick,
    /// The made-up sync ended.
    SyncDone,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SyncPreview {
    Syncing,
    Offline,
}

impl App for Preview {
    type Message = Message;

    fn update(&mut self, message: Message) {
        if let Message::SyncDone = message {
            // Every second sync fails, so both results show.
            self.syncs += 1;
            self.sync = match self.syncs % 2 {
                1 => None,
                _ => Some(SyncPreview::Offline),
            };
            if self.sync.is_none() {
                self.synced_at = epoch_ms();
            }
        }
    }

    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if let Some(bench) = &mut self.bench
            && bench.measure()
        {
            self.open = false;
        }
        if !self.open {
            return Ok(());
        }
        let screen = VaultScreen {
            email: "alex@example.com",
            server: &self.server,
            index: &self.index,
            index_revision: self.revision,
            open: None,
            syncing: self.sync == Some(SyncPreview::Syncing),
            sync_error: (self.sync == Some(SyncPreview::Offline))
                .then_some("The server could not be reached."),
            error: None,
            revision: None,
            synced_at: Some(self.synced_at),
            device_unlock: false,
            device_enrolling: false,
            device_error: None,
            ssh_approval: None,
        };
        let open = self
            .opened
            .and_then(|id| self.views.iter().find(|view| view.id == Some(id)));
        let pass = vault::view::<Self>(ui, &mut self.form, &screen, open)?;
        match pass.action {
            None => {}
            Some(VaultAction::Close) => self.open = false,
            Some(VaultAction::User(action)) => self.act(action),
            // The preview opens no browser, so a test can check the address.
            Some(VaultAction::OpenWebsite(url)) => eprintln!("website {url}"),
        }
        Ok(())
    }
}

impl VaultApp for Preview {
    fn vault(&mut self) -> Option<(&mut VaultForm, &VaultIndex)> {
        Some((&mut self.form, &self.index))
    }
}

impl Preview {
    /// Does what the core would do with `action`, and prints it.
    fn act(&mut self, action: UserAction) {
        match action {
            UserAction::Lock => {
                eprintln!("lock");
                self.open = false;
            }
            UserAction::Sync => {
                eprintln!("sync");
                if self.sync != Some(SyncPreview::Syncing) {
                    self.sync = Some(SyncPreview::Syncing);
                    let handle = self.handle.clone();
                    winui_executor::spawn(async move {
                        winui_executor::sleep(std::time::Duration::from_secs(2)).await;
                        handle.send(Message::SyncDone);
                    })
                    .detach();
                }
            }
            UserAction::Open(id) => {
                eprintln!("open {}", id.as_u128());
                self.opened = Some(id);
            }
            UserAction::Close => {
                eprintln!("close");
                self.opened = None;
            }
            UserAction::Save(view) => {
                let id = view.id.expect("the preview saves only existing items");
                eprintln!("save {} favorite={}", id.as_u128(), view.favorite);
                let position = self.position(id);
                self.index.ciphers[position].favorite = view.favorite;
                self.views[position] = *view;
                self.revision += 1;
            }
            UserAction::Trash(id) | UserAction::Restore(id) => {
                let deleted = matches!(action, UserAction::Trash(_));
                eprintln!(
                    "{} {}",
                    if deleted { "trash" } else { "restore" },
                    id.as_u128()
                );
                let position = self.position(id);
                self.index.ciphers[position].deleted = deleted;
                self.revision += 1;
            }
            UserAction::MoveToFolder { id, folder } => {
                eprintln!(
                    "move {} to {}",
                    id.as_u128(),
                    folder.map_or(0, |folder| folder.as_u128())
                );
                let position = self.position(id);
                self.index.ciphers[position].folder_id = folder;
                self.views[position].folder_id = folder;
                self.revision += 1;
            }
            // The preview has no clipboard, so a test can check the text.
            UserAction::Copy { id, field } => {
                let view = &self.views[self.position(id)];
                let guarded = field.guarded() && view.reprompt;
                if let Some(text) = copy_text(view, field).filter(|_| !guarded) {
                    eprintln!("copy {} {field:?} {text}", id.as_u128());
                }
            }
            UserAction::SetReprompt { id, on } => {
                eprintln!("reprompt {} {on}", id.as_u128());
                let position = self.position(id);
                self.index.ciphers[position].reprompt = on;
                self.views[position].reprompt = on;
                self.revision += 1;
            }
            _ => {}
        }
    }

    fn position(&self, id: Uuid) -> usize {
        self.index
            .ciphers
            .iter()
            .position(|cipher| cipher.id == id)
            .expect("actions name items of the list")
    }
}

/// Two folders, one that did not decrypt, and items of every type: 10 live
/// (3 favorites, 2 with no folder, 1 of an unknown type, 1 that did not
/// decrypt) and 2 in the trash.
fn sample() -> VaultIndex {
    let folder = |id, name: &str, readable| FolderSummary {
        id: Uuid::from_u128(id),
        name: secret(name),
        readable,
    };
    let item =
        |id, kind, name: &str, subtitle: &str, folder: u128, favorite, deleted| CipherSummary {
            id: Uuid::from_u128(id),
            kind,
            name: secret(name),
            subtitle: secret(subtitle),
            uris: Vec::new(),
            folder_id: (folder != 0).then(|| Uuid::from_u128(folder)),
            organization_id: None,
            favorite,
            reprompt: false,
            copyable: Default::default(),
            deleted,
            readable: true,
        };
    let mut unreadable = item(21, CipherKind::Login, "", "", 1, false, false);
    unreadable.readable = false;
    VaultIndex {
        // Sorted as the core sorts them: by name, the unreadable ones first.
        folders: vec![
            folder(3, "", false),
            folder(1, "Personal", true),
            folder(2, "Work", true),
        ],
        ciphers: vec![
            unreadable,
            item(
                14,
                CipherKind::Identity,
                "Alex Lima",
                "Alex Lima",
                0,
                false,
                false,
            ),
            item(
                10,
                CipherKind::Login,
                "Amazon",
                "alex@example.com",
                1,
                false,
                false,
            ),
            item(
                16,
                CipherKind::SshKey,
                "Deploy key",
                "SHA256:q3RvT1xYk0w2",
                2,
                true,
                false,
            ),
            item(19, CipherKind::Login, "Deleted login", "old", 1, true, true),
            item(
                20,
                CipherKind::Card,
                "Expired card",
                "Visa, *1111",
                0,
                false,
                true,
            ),
            item(11, CipherKind::Login, "GitHub", "alex-dev", 2, true, false),
            item(15, CipherKind::SecureNote, "Home Wi-Fi", "", 1, true, false),
            item(
                12,
                CipherKind::Login,
                "Mail server",
                "alex",
                2,
                false,
                false,
            ),
            item(18, CipherKind::Login, "Old forum", "alexl", 0, false, false),
            item(
                13,
                CipherKind::Card,
                "Personal Visa",
                "Visa, *4242",
                1,
                false,
                false,
            ),
            item(
                17,
                CipherKind::Unknown(7),
                "Shared drive",
                "",
                3,
                false,
                false,
            ),
        ],
        ssh_keys: Vec::new(),
    }
}

/// The opened form of `summary`: GitHub, Personal Visa, Alex Lima and
/// Deploy key have every field of their type, Home Wi-Fi asks for the
/// master password, and the rest have only a name.
fn view_of(summary: &CipherSummary) -> CipherView {
    let some = |text: &str| Some(secret(text));
    let content = match summary.id.as_u128() {
        11 => CipherContent::Login(LoginView {
            username: some("alex-dev"),
            password: some("Tr0ub4dor&3-xK9"),
            totp: some("JBSWY3DPEHPK3PXP"),
            password_revision_date: Some("2026-10-14T09:12:00.000Z".to_owned()),
            uris: vec![
                UriView {
                    uri: some("https://github.com/login"),
                    match_rule: None,
                },
                UriView {
                    uri: some("https://gist.github.com"),
                    match_rule: Some(2),
                },
            ],
            fido2_credentials: Vec::new(),
        }),
        13 => CipherContent::Card(CardView {
            cardholder_name: some("Alex M. Example"),
            brand: some("Visa"),
            number: some("4242424242424242"),
            exp_month: some("8"),
            exp_year: some("2029"),
            code: some("123"),
        }),
        14 => CipherContent::Identity(Box::new(IdentityView {
            title: some("Mx"),
            first_name: some("Alex"),
            middle_name: some("M."),
            last_name: some("Lima"),
            address1: some("Rua das Flores 12, 3.º Esq."),
            address2: None,
            address3: None,
            city: some("Lisboa"),
            state: None,
            postal_code: some("1200-195"),
            country: some("Portugal"),
            company: some("Example Lda."),
            email: some("alex@example.com"),
            phone: some("+351 912 345 678"),
            ssn: some("123 456 789"),
            username: some("alexexample"),
            passport_number: some("CB1234567"),
            license_number: some("L-1234567 8"),
        })),
        16 => CipherContent::SshKey(SshKeyView {
            private_key: some("-----BEGIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEAAAAA"),
            public_key: some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJq3RvT1xb8fKc build@chaveiro"),
            key_fingerprint: some("SHA256:q3RvT1xYk0w2pV7sDm4hN9eLa6uZc3Jg5Fb8fKc"),
        }),
        _ => match summary.kind {
            CipherKind::Login => CipherContent::Login(LoginView::default()),
            CipherKind::Card => CipherContent::Card(CardView {
                cardholder_name: None,
                brand: None,
                number: None,
                exp_month: None,
                exp_year: None,
                code: None,
            }),
            CipherKind::SecureNote => CipherContent::SecureNote,
            _ => CipherContent::Unknown,
        },
    };
    let fields = match summary.id.as_u128() {
        11 => vec![
            CustomField {
                kind: 0,
                name: some("Recovery email"),
                value: some("alex.backup@example.com"),
                linked_id: None,
            },
            CustomField {
                kind: 1,
                name: some("API token"),
                value: some("ghp_8fKc2Q"),
                linked_id: None,
            },
            CustomField {
                kind: 2,
                name: some("Admin account"),
                value: some("true"),
                linked_id: None,
            },
        ],
        _ => Vec::new(),
    };
    CipherView {
        id: Some(summary.id),
        organization_id: None,
        collection_ids: Vec::new(),
        kind: summary.kind,
        name: summary.name.clone(),
        notes: match summary.id.as_u128() {
            15 => some("Network: Lima-5G\nPassword: correct horse battery staple"),
            _ => None,
        },
        folder_id: summary.folder_id,
        favorite: summary.favorite,
        reprompt: summary.id.as_u128() == 15,
        content,
        fields,
        password_history: Vec::new(),
    }
}

/// The text a copy of `field` gives, as the core gives it: for a TOTP
/// secret, the code at this moment.
fn copy_text(view: &CipherView, field: CopyField) -> Option<String> {
    if field == CopyField::Totp {
        let CipherContent::Login(login) = &view.content else {
            return None;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let code = chaveiro_core::totp::code(login.totp.as_ref()?, now.as_secs())?;
        return Some(code.to_string());
    }
    let login = match &view.content {
        CipherContent::Login(login) => Some(login),
        _ => None,
    };
    let text = match field {
        CopyField::Username => login.and_then(|login| login.username.as_ref()),
        CopyField::Password => login.and_then(|login| login.password.as_ref()),
        CopyField::Totp => login.and_then(|login| login.totp.as_ref()),
        CopyField::Uri => {
            login.and_then(|login| login.uris.iter().find_map(|uri| uri.uri.as_ref()))
        }
        CopyField::Notes => view.notes.as_ref(),
    };
    text.map(|text| text.to_string())
}

fn copyable(view: &CipherView) -> CopyFields {
    [
        CopyField::Username,
        CopyField::Password,
        CopyField::Totp,
        CopyField::Uri,
        CopyField::Notes,
    ]
    .into_iter()
    .filter(|field| copy_text(view, *field).is_some())
    .collect()
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_millis() as u64)
}

fn secret(text: &str) -> SecretString {
    SecretString::new(text.to_owned())
}

/// Counts the UI thread's allocations between the starts of two passes.
struct Bench {
    passes: u32,
    /// The allocation count at the start of the last pass.
    last: usize,
    allocations: Vec<usize>,
}

impl Bench {
    /// Passes before measuring: the window opens and the rows are realized.
    const WARM_UP: u32 = 30;
    const MEASURED: usize = 200;

    /// Reserves every buffer up front, so the bench itself allocates nothing
    /// while it measures.
    fn new() -> Self {
        Bench {
            passes: 0,
            last: 0,
            allocations: Vec::with_capacity(Self::MEASURED),
        }
    }

    /// Records the pass that starts now. Returns true when the bench is done.
    fn measure(&mut self) -> bool {
        if self.allocations.len() == Self::MEASURED {
            return true;
        }
        let count = ALLOCATIONS.get();
        self.passes += 1;
        if self.passes > Self::WARM_UP {
            self.allocations.push(count - self.last);
        }
        self.last = count;
        if self.allocations.len() < Self::MEASURED {
            return false;
        }
        let total: usize = self.allocations.iter().sum();
        let most = self.allocations.iter().max().copied().unwrap_or(0);
        eprintln!(
            "bench: {} steady passes, {total} UI-thread allocations in all, at most {most} in one",
            self.allocations.len()
        );
        true
    }
}

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: forwards to `System` and only bumps a thread-local counter.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;
