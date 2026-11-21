//! The vault window, from the VaultScreen design: the navigation pane of
//! filters with their item counts. The list and details panes come later,
//! so the content names the filter and offers Lock.
//!
//! The window stays open for most of the app's life, so a pass that finds
//! the index unchanged allocates nothing: the counts and the pane's entries
//! are rebuilt only when [`VaultScreen::index_revision`] changes.

use chaveiro_core::{
    core::{UserAction, VaultScreen},
    filter::{Counts, Filter, KINDS},
    vault::{CipherKind, FolderSummary, VaultIndex},
};
use windows_core::Result;
use winui_immediate::{Length, NavigationEntry, NavigationSelection, Orientation, Ui};

/// What the vault window keeps between passes.
pub struct VaultForm {
    /// The pane's selection. A folder that leaves the vault falls back to
    /// [`Filter::All`].
    pub filter: Filter,
    /// Settings is selected in the pane, over `filter`.
    pub settings: bool,
    sidebar: Sidebar,
}

impl Default for VaultForm {
    fn default() -> Self {
        VaultForm {
            filter: Filter::All,
            settings: false,
            sidebar: Sidebar::default(),
        }
    }
}

impl VaultForm {
    /// Selects All items again and empties the cached counts and filters,
    /// keeping their capacity, so the next pass builds them from the index
    /// it is given.
    pub fn reset(&mut self) {
        self.filter = Filter::All;
        self.settings = false;
        let sidebar = &mut self.sidebar;
        sidebar.revision = None;
        sidebar.counts.clear();
        sidebar.filters.clear();
    }
}

/// What the pane shows, derived from one revision of the index.
///
/// Contract: `filters` has one slot per pane entry, in the order [`pane`]
/// pushes them, so a selected entry maps back to its filter; both come
/// from the index of `revision`.
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

/// What one pass of the vault window gives back.
pub struct VaultPass {
    pub action: Option<VaultAction>,
    /// The window's `AppWindow.Id`, to own system prompts.
    pub window: u64,
}

/// Draws the vault window, a window of its own beside the login one, which
/// a pass that shows the vault does not declare.
pub fn view(ui: &mut Ui, form: &mut VaultForm, screen: &VaultScreen<'_>) -> Result<VaultPass> {
    let index = screen.index;
    let revision = screen.index_revision;
    let sidebar = &mut form.sidebar;
    if sidebar.revision != Some(revision) {
        sidebar.counts.recount(index);
        sidebar.filters.clear();
        pane(index, &sidebar.counts, |filter, _| {
            sidebar.filters.push(filter)
        });
        sidebar.revision = Some(revision);
    }
    let sidebar = &form.sidebar;
    let position = |filter| {
        sidebar
            .filters
            .iter()
            .position(|slot| *slot == Some(filter))
    };
    if !form.settings && position(form.filter).is_none() {
        form.filter = Filter::All;
    }
    let mut selected = match form.settings {
        true => Some(NavigationSelection::Settings),
        false => position(form.filter).map(NavigationSelection::Entry),
    };

    let mut action = None;
    let mut window = ui.window("vault", "Chaveiro", |ui| {
        ui.grid("window", &[Length::Auto, Length::Star(1.0)], |ui| {
            ui.title_bar("title bar", "Chaveiro", false)?;
            let push = |entries: &mut Vec<_>| {
                pane(index, &sidebar.counts, |_, entry| entries.push(entry));
            };
            ui.navigation_view("pane", revision, push, &mut selected, |ui, selected| {
                ui.stack("content", Orientation::Vertical, 8.0, |ui| {
                    let shown = match selected {
                        Some(NavigationSelection::Settings) => None,
                        Some(NavigationSelection::Entry(entry)) => sidebar.filters[entry],
                        None => None,
                    };
                    match shown {
                        Some(filter) => {
                            title(ui, label(filter, index))?;
                            let count = sidebar.counts.get(filter);
                            let plural = if count == 1 { "" } else { "s" };
                            ui.label("count", format_args!("{count} item{plural}"))?;
                        }
                        None if selected == Some(NavigationSelection::Settings) => {
                            title(ui, "Settings")?;
                            ui.label("note", format_args!("Settings are not built yet."))?;
                        }
                        None => {}
                    }
                    if ui.button("lock", "Lock")?.clicked()? {
                        action = Some(VaultAction::User(UserAction::Lock));
                    }
                    Ok(())
                })?
                .padding([24.0, 16.0, 24.0, 24.0])?;
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    })?;
    window
        .mica()?
        .extends_into_title_bar()?
        .initial_size(WIDTH, HEIGHT)?;
    if window.close_requested() {
        action = Some(VaultAction::Close);
    }
    let window = window.id()?;
    match selected {
        Some(NavigationSelection::Settings) => form.settings = true,
        Some(NavigationSelection::Entry(entry)) => {
            if let Some(filter) = sidebar.filters[entry] {
                form.filter = filter;
                form.settings = false;
            }
        }
        None => {}
    }
    Ok(VaultPass { action, window })
}

/// Pushes the pane's entries in order, each with its filter, or `None`
/// for a header or separator. The one source of the pane's order, so
/// [`Sidebar::filters`] and the widget's entries agree.
fn pane<'a>(
    index: &'a VaultIndex,
    counts: &Counts,
    mut push: impl FnMut(Option<Filter>, NavigationEntry<'a>),
) {
    let item = |filter: Filter, label: &'a str| {
        let entry = NavigationEntry::Item {
            glyph: glyph(filter),
            label,
            count: Some(counts.get(filter)),
        };
        (Some(filter), entry)
    };
    let mut emit = |(filter, entry)| push(filter, entry);
    emit(item(Filter::All, label(Filter::All, index)));
    emit(item(Filter::Favorites, label(Filter::Favorites, index)));
    emit((None, NavigationEntry::Separator));
    for kind in KINDS {
        emit(item(Filter::Kind(kind), label(Filter::Kind(kind), index)));
    }
    emit((None, NavigationEntry::Header("Folders")));
    for folder in &index.folders {
        emit(item(Filter::Folder(folder.id), folder_label(folder)));
    }
    emit(item(Filter::NoFolder, label(Filter::NoFolder, index)));
    emit((None, NavigationEntry::Separator));
    emit(item(Filter::Trash, label(Filter::Trash, index)));
}

/// The label of `filter`'s pane entry.
fn label(filter: Filter, index: &VaultIndex) -> &str {
    match filter {
        Filter::All => "All items",
        Filter::Favorites => "Favorites",
        Filter::Kind(CipherKind::Login) => "Logins",
        Filter::Kind(CipherKind::Card) => "Cards",
        Filter::Kind(CipherKind::Identity) => "Identities",
        Filter::Kind(CipherKind::SecureNote) => "Notes",
        Filter::Kind(CipherKind::SshKey) => "SSH keys",
        Filter::Kind(CipherKind::Unknown(_)) => unreachable!("the pane lists only KINDS"),
        Filter::Folder(id) => index
            .folders
            .iter()
            .find(|folder| folder.id == id)
            .map_or(UNREADABLE_FOLDER, folder_label),
        Filter::NoFolder => "No folder",
        Filter::Trash => "Trash",
    }
}

fn folder_label(folder: &FolderSummary) -> &str {
    match folder.readable {
        true => folder.name.as_str(),
        false => UNREADABLE_FOLDER,
    }
}

/// The Segoe Fluent Icons glyph of `filter`'s pane entry, from the design.
fn glyph(filter: Filter) -> char {
    match filter {
        Filter::All => '\u{E8A9}',
        Filter::Favorites => '\u{E734}',
        Filter::Kind(CipherKind::Login) => '\u{E774}',
        Filter::Kind(CipherKind::Card) => '\u{E8C7}',
        Filter::Kind(CipherKind::Identity) => '\u{E77B}',
        Filter::Kind(CipherKind::SecureNote) => '\u{E8A5}',
        Filter::Kind(CipherKind::SshKey) => '\u{E8D7}',
        Filter::Kind(CipherKind::Unknown(_)) => unreachable!("the pane lists only KINDS"),
        Filter::Folder(_) | Filter::NoFolder => '\u{E8B7}',
        Filter::Trash => '\u{E74D}',
    }
}

fn title(ui: &mut Ui, text: &str) -> Result<()> {
    ui.label("title", format_args!("{text}"))?
        .style("SubtitleTextBlockStyle")?;
    Ok(())
}

/// The label of a folder whose name did not decrypt.
const UNREADABLE_FOLDER: &str = "Unreadable folder";

/// The window's first size, wide enough for the full pane, which needs
/// 1000px.
const WIDTH: f64 = 1200.0;
const HEIGHT: f64 = 760.0;
