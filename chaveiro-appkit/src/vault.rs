//! The vault window, from the "Vault" boards of the Chaveiro macOS design:
//! a sidebar of filters with their item counts over the sync status, beside
//! the content. The toolbar, list and details come later, so the content
//! names the filter and offers Lock.
//!
//! The window stays open for most of the app's life, so a pass that finds
//! the index unchanged allocates nothing: the counts and the sidebar's
//! entries are rebuilt only when [`VaultScreen::index_revision`] changes.

use appkit_immediate::{Length, Orientation, SourceListEntries, Style, TextColor, Ui};
use chaveiro_core::{
    core::{UserAction, VaultScreen},
    filter::{Counts, Filter, KINDS},
    vault::{CipherKind, FolderSummary, VaultIndex},
};

/// What the vault window keeps between passes.
pub struct VaultForm {
    /// The sidebar's selection. A folder that leaves the vault falls back
    /// to [`Filter::All`].
    pub filter: Filter,
    sidebar: Sidebar,
}

impl Default for VaultForm {
    fn default() -> Self {
        VaultForm {
            filter: Filter::All,
            sidebar: Sidebar::default(),
        }
    }
}

impl VaultForm {
    /// Selects All Items again and empties the cached counts and filters,
    /// keeping their capacity, so the next pass builds them from the index
    /// it is given.
    pub fn reset(&mut self) {
        self.filter = Filter::All;
        let sidebar = &mut self.sidebar;
        sidebar.revision = None;
        sidebar.counts.clear();
        sidebar.filters.clear();
    }
}

/// What the sidebar shows, derived from one revision of the index.
///
/// Contract: `filters` has one slot per sidebar entry, in the order
/// [`sidebar`] pushes them, so a selected entry maps back to its filter;
/// both come from the index of `revision`.
#[derive(Default)]
struct Sidebar {
    /// [`None`] until the first pass, and after [`VaultForm::reset`].
    revision: Option<u64>,
    counts: Counts,
    filters: Vec<Option<Filter>>,
}

/// What the user asked for in a pass.
pub enum VaultAction {
    User(UserAction),
    /// The window's close button.
    Close,
}

/// Draws the vault window. `now_ms` is our clock (epoch ms), for how long
/// ago the last sync was.
pub fn view(
    ui: &mut Ui,
    form: &mut VaultForm,
    screen: &VaultScreen<'_>,
    now_ms: u64,
) -> Option<VaultAction> {
    let index = screen.index;
    let revision = screen.index_revision;
    let sidebar_state = &mut form.sidebar;
    if sidebar_state.revision != Some(revision) {
        sidebar_state.counts.recount(index);
        sidebar_state.filters.clear();
        sidebar(index, &sidebar_state.counts, |filter, _| {
            sidebar_state.filters.push(filter)
        });
        sidebar_state.revision = Some(revision);
    }
    let sidebar_state = &form.sidebar;
    let position = |filter| {
        sidebar_state
            .filters
            .iter()
            .position(|slot| *slot == Some(filter))
    };
    if position(form.filter).is_none() {
        form.filter = Filter::All;
    }
    let mut selected = position(form.filter);

    let mut action = None;
    let mut window = ui.window("vault", "Chaveiro", |ui| {
        ui.split_view("split", |ui| {
            ui.grid("sidebar", &[Length::Star(1.0), Length::Auto], |ui| {
                ui.source_list("filters", revision, &mut selected, |list| {
                    sidebar(index, &sidebar_state.counts, |_, entry| entry.push(list));
                });
                ui.stack_view("status", Orientation::Vertical, 0.0, |ui| {
                    sync_status(ui, screen, now_ms);
                })
                .padding([16.0, 8.0, 16.0, 12.0]);
            });
            ui.stack_view("content", Orientation::Vertical, 8.0, |ui| {
                // The filter shown is the one picked this pass.
                let shown = selected
                    .and_then(|entry| sidebar_state.filters[entry])
                    .unwrap_or(form.filter);
                ui.label("title", format_args!("{}", label(shown, index)))
                    .style(Style::Title2);
                let count = sidebar_state.counts.get(shown);
                let plural = if count == 1 { "" } else { "s" };
                ui.label(
                    "count",
                    format_args!("{count} item{plural} · {}", screen.email),
                )
                .text_color(TextColor::Secondary);
                if ui.button("lock", "Lock").clicked() {
                    action = Some(VaultAction::User(UserAction::Lock));
                }
            })
            .padding([24.0, 20.0, 24.0, 24.0]);
        });
    });
    window.full_size_content().initial_size(WIDTH, HEIGHT);
    if window.should_close() {
        action = Some(VaultAction::Close);
    }
    if let Some(filter) = selected.and_then(|entry| sidebar_state.filters[entry]) {
        form.filter = filter;
    }
    action
}

/// The line under the sidebar: syncing, or when the last sync was, and
/// whether it failed.
fn sync_status(ui: &mut Ui, screen: &VaultScreen<'_>, now_ms: u64) {
    let ago = screen
        .synced_at
        .map(|synced_at| Ago(now_ms.saturating_sub(synced_at) / 1000));
    ui.label(
        "sync",
        format_args!(
            "{}",
            match (screen.syncing, screen.sync_error, ago) {
                (true, _, _) => Status::Syncing,
                (false, Some(_), Some(ago)) => Status::Failed(ago),
                (false, Some(_), None) => Status::FailedNever,
                (false, None, Some(ago)) => Status::Synced(ago),
                (false, None, None) => Status::Never,
            }
        ),
    )
    .style(Style::Caption)
    .text_color(TextColor::Secondary);
}

/// The text of the sync line, written straight into the label, so a pass
/// allocates nothing.
enum Status {
    Syncing,
    Synced(Ago),
    Failed(Ago),
    FailedNever,
    Never,
}

impl std::fmt::Display for Status {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Syncing => formatter.write_str("Syncing…"),
            Status::Synced(ago) => write!(formatter, "Synced {ago}"),
            Status::Failed(ago) => write!(formatter, "Couldn’t sync · last synced {ago}"),
            Status::FailedNever => formatter.write_str("Couldn’t sync"),
            Status::Never => formatter.write_str("Not synced yet"),
        }
    }
}

/// A time span in seconds, shown as "2 minutes ago".
struct Ago(u64);

impl std::fmt::Display for Ago {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Ago(seconds) = *self;
        let (count, unit) = match seconds {
            0..60 => return formatter.write_str("just now"),
            60..3600 => (seconds / 60, "minute"),
            3600..86_400 => (seconds / 3600, "hour"),
            _ => (seconds / 86_400, "day"),
        };
        let plural = if count == 1 { "" } else { "s" };
        write!(formatter, "{count} {unit}{plural} ago")
    }
}

/// One sidebar entry, as [`sidebar`] gives it.
enum Entry<'a> {
    Header(&'a str),
    Item {
        symbol: &'a str,
        label: &'a str,
        count: usize,
    },
    Spacer,
}

impl Entry<'_> {
    fn push(&self, list: &mut SourceListEntries<'_>) {
        match *self {
            Entry::Header(label) => list.header(label),
            Entry::Item {
                symbol,
                label,
                count,
            } => list.item(symbol, label, Some(count)),
            Entry::Spacer => list.spacer(),
        }
    }
}

/// Pushes the sidebar's entries in order, each with its filter, or `None`
/// for a header or spacer. The one source of the sidebar's order, so
/// [`Sidebar::filters`] and the list's entries agree.
fn sidebar<'a>(
    index: &'a VaultIndex,
    counts: &Counts,
    mut push: impl FnMut(Option<Filter>, Entry<'a>),
) {
    let item = |filter: Filter, label: &'a str| {
        let entry = Entry::Item {
            symbol: symbol(filter),
            label,
            count: counts.get(filter),
        };
        (Some(filter), entry)
    };
    let mut emit = |(filter, entry)| push(filter, entry);
    emit((None, Entry::Header("Vault")));
    emit(item(Filter::All, label(Filter::All, index)));
    emit(item(Filter::Favorites, label(Filter::Favorites, index)));
    emit((None, Entry::Header("Types")));
    for kind in KINDS {
        emit(item(Filter::Kind(kind), label(Filter::Kind(kind), index)));
    }
    emit((None, Entry::Header("Folders")));
    for folder in &index.folders {
        emit(item(Filter::Folder(folder.id), folder_label(folder)));
    }
    emit(item(Filter::NoFolder, label(Filter::NoFolder, index)));
    emit((None, Entry::Spacer));
    emit(item(Filter::Trash, label(Filter::Trash, index)));
}

/// The label of `filter`'s sidebar entry, from the design.
fn label(filter: Filter, index: &VaultIndex) -> &str {
    match filter {
        Filter::All => "All Items",
        Filter::Favorites => "Favorites",
        Filter::Kind(CipherKind::Login) => "Logins",
        Filter::Kind(CipherKind::Card) => "Cards",
        Filter::Kind(CipherKind::Identity) => "Identities",
        Filter::Kind(CipherKind::SecureNote) => "Secure Notes",
        Filter::Kind(CipherKind::SshKey) => "SSH Keys",
        Filter::Kind(CipherKind::Unknown(_)) => unreachable!("the sidebar lists only KINDS"),
        Filter::Folder(id) => index
            .folders
            .iter()
            .find(|folder| folder.id == id)
            .map_or(UNREADABLE_FOLDER, folder_label),
        Filter::NoFolder => "No Folder",
        Filter::Trash => "Trash",
    }
}

fn folder_label(folder: &FolderSummary) -> &str {
    match folder.readable {
        true => folder.name.as_str(),
        false => UNREADABLE_FOLDER,
    }
}

/// The SF Symbols name of `filter`'s sidebar entry.
fn symbol(filter: Filter) -> &'static str {
    match filter {
        Filter::All => "square.grid.2x2",
        Filter::Favorites => "star",
        Filter::Kind(CipherKind::Login) => "globe",
        Filter::Kind(CipherKind::Card) => "creditcard",
        Filter::Kind(CipherKind::Identity) => "person.text.rectangle",
        Filter::Kind(CipherKind::SecureNote) => "note.text",
        Filter::Kind(CipherKind::SshKey) => "key",
        Filter::Kind(CipherKind::Unknown(_)) => unreachable!("the sidebar lists only KINDS"),
        Filter::Folder(_) => "folder",
        Filter::NoFolder => "folder.badge.questionmark",
        Filter::Trash => "trash",
    }
}

/// The label of a folder whose name did not decrypt.
const UNREADABLE_FOLDER: &str = "Unreadable folder";

/// The window's first content size, from the design's 1440×900 boards less
/// the menu bar.
const WIDTH: f64 = 1200.0;
const HEIGHT: f64 = 760.0;
