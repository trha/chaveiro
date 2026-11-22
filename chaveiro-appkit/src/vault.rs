//! The vault window, from the "Vault" boards of the Chaveiro macOS design:
//! a sidebar of filters with their item counts over the sync status, the
//! list of the chosen filter's items, and the open item's details. The
//! toolbar holds the item commands, the search field, Sync and Lock.
//!
//! The window stays open for most of the app's life, so a pass that finds
//! the index, the filter and the open item unchanged allocates nothing:
//! the counts, the sidebar's entries and the list's rows are rebuilt only
//! when [`VaultScreen::index_revision`], the filter or the search changes,
//! and the
//! details are drawn from the open item in place.

use appkit_immediate::{
    Align, App, Key, Length, MenuAnchor, Orientation, SourceListEntries, Style, TableViewId,
    TextColor, Ui,
};
use chaveiro_core::{
    core::{ItemError, UserAction, VaultScreen},
    crypto::SecretString,
    filter::{Counts, Filter, KINDS},
    search::{Query, Tier},
    totp,
    vault::{
        CardView, CipherContent, CipherKind, CipherSummary, CipherView, CopyField, FolderSummary,
        IdentityView, LoginView, SshKeyView, VaultIndex,
    },
};
use std::fmt::Write as _;
use uuid::Uuid;

/// An app that shows the vault window. The real app and the vault example
/// are different types, so the list rows reach the vault through this trait
/// instead of naming one app.
pub trait VaultApp: App {
    /// The form and the index the vault window shows; [`None`] while it
    /// shows no vault.
    fn vault(&mut self) -> Option<(&mut VaultForm, &VaultIndex)>;
}

/// What the vault window keeps between passes.
pub struct VaultForm {
    /// The sidebar's selection. A folder that leaves the vault falls back
    /// to [`Filter::All`].
    pub filter: Filter,
    sidebar: Sidebar,
    search: Search,
    list: ItemList,
    /// The item the list shows as selected, whose details show. It
    /// follows the core's open item, and the user's pick in the list.
    selected: Option<Uuid>,
    /// The id of the core's open item on the last pass, to notice a new
    /// one.
    opened: Option<Uuid>,
    /// The item Up or Down in the search field moved to last pass, which
    /// this pass opens as a pick in the list would.
    moved: Option<Uuid>,
    /// A row's context menu while it is open.
    menu: Option<ItemMenu>,
    /// How many menus opened, so each one is a menu node of its own.
    menus_opened: u64,
    /// The secrets of the selected item shown in clear. Emptied when
    /// another item is selected.
    revealed: Vec<FieldId>,
    /// The toolbar's subtitle, written in place each pass, so a pass
    /// allocates nothing.
    subtitle: String,
}

impl Default for VaultForm {
    fn default() -> Self {
        VaultForm {
            filter: Filter::All,
            sidebar: Sidebar::default(),
            search: Search::default(),
            list: ItemList::default(),
            selected: None,
            opened: None,
            moved: None,
            menu: None,
            menus_opened: 0,
            revealed: Vec::new(),
            subtitle: String::new(),
        }
    }
}

impl VaultForm {
    /// Selects All Items and no item again, empties the search, and
    /// empties the cached counts, filters and rows, keeping their capacity,
    /// so the next pass builds them from the index it is given.
    pub fn reset(&mut self) {
        self.filter = Filter::All;
        let sidebar = &mut self.sidebar;
        sidebar.revision = None;
        sidebar.counts.clear();
        sidebar.filters.clear();
        self.search.text.clear();
        self.search.query.set("");
        self.search.revision += 1;
        self.list.shows = None;
        self.list.rows.clear();
        self.list.ranked.clear();
        self.selected = None;
        self.opened = None;
        self.moved = None;
        self.menu = None;
        self.revealed.clear();
    }
}

/// An open context menu of a row.
struct ItemMenu {
    /// The item it acts on, even when the rows change while it is open.
    item: Uuid,
    anchor: MenuAnchor,
    /// The value of [`VaultForm::menus_opened`] when it opened.
    serial: u64,
}

/// Selects `id` in `selected`, hiding the secrets of the item selected
/// before.
fn select(selected: &mut Option<Uuid>, revealed: &mut Vec<FieldId>, id: Option<Uuid>) {
    if *selected != id {
        *selected = id;
        revealed.clear();
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

/// The search field in the toolbar.
///
/// Contract: `query` is `text` parsed, and `revision` changes whenever
/// `query` does.
#[derive(Default)]
struct Search {
    text: String,
    query: Query,
    revision: u64,
}

/// The rows of the item list.
///
/// Contract: `rows` holds the position in [`VaultIndex::ciphers`] of each
/// item that the filter of `shows` and the search match, best [`Tier`]
/// first and in index order inside a tier, for the index and search of
/// the revisions in `shows`; `revision` changes whenever `rows` does.
#[derive(Default)]
struct ItemList {
    /// The index revision, filter and search revision `rows` were built
    /// for; [`None`] until the first build and after [`VaultForm::reset`].
    shows: Option<(u64, Filter, u64)>,
    rows: Vec<usize>,
    /// Scratch for sorting the rows, kept for its capacity.
    ranked: Vec<(Tier, usize)>,
    revision: u64,
}

impl ItemList {
    /// Builds the rows for `filter` and `search` over `index`, unless they
    /// are built for those revisions and filter already.
    fn refresh(&mut self, index: &VaultIndex, revision: u64, filter: Filter, search: &mut Search) {
        let shows = Some((revision, filter, search.revision));
        if self.shows == shows {
            return;
        }
        self.ranked.clear();
        for (position, cipher) in index.ciphers.iter().enumerate() {
            if filter.matches(cipher, &index.folders)
                && let Some(tier) = search.query.rank(cipher)
            {
                self.ranked.push((tier, position));
            }
        }
        // The keys are unique, so an unstable sort keeps index order inside
        // a tier, and it does not allocate.
        self.ranked.sort_unstable();
        self.rows.clear();
        self.rows
            .extend(self.ranked.iter().map(|(_, position)| *position));
        self.shows = shows;
        self.revision += 1;
    }
}

/// The row of `rows` that `step` rows of Up (negative) or Down (positive)
/// move to from the row of `selected`, stopping at the ends; from no
/// selected row, Down moves to the first and Up to the last. [`None`] when
/// nothing moves.
fn moved(rows: &[usize], index: &VaultIndex, selected: Option<Uuid>, step: isize) -> Option<usize> {
    let last = rows.len().checked_sub(1)?;
    let current = rows
        .iter()
        .position(|&row| Some(index.ciphers[row].id) == selected);
    let target = match (current, step.signum()) {
        (_, 0) => return None,
        (Some(current), _) => current.saturating_add_signed(step).min(last),
        (None, 1) => 0,
        (None, _) => last,
    };
    (current != Some(target)).then_some(target)
}

/// Names one field of the selected item: a static name, and the position
/// of a website, passkey or custom field, or 0.
type FieldId = (&'static str, usize);

/// What the user asked for in a pass.
pub enum VaultAction {
    User(UserAction),
    /// A Copy button in the details: the value it copies, as the details
    /// show it.
    Copy(SecretString),
    /// The window's close button.
    Close,
}

/// Draws the vault window. `open` is the core's open item, from
/// [`VaultScreen::open`]. `now_ms` is our clock (epoch ms), for how long
/// ago the last sync was and for the open login's verification code.
pub fn view<A: VaultApp>(
    ui: &mut Ui,
    form: &mut VaultForm,
    screen: &VaultScreen<'_>,
    open: Option<&CipherView>,
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

    // The core opened another item, such as a reopen after a sync. A pick
    // in this pass's list wins, because it comes later.
    let opened = open.and_then(|view| view.id);
    if opened != form.opened {
        form.opened = opened;
        if opened.is_some() {
            select(&mut form.selected, &mut form.revealed, opened);
        }
    }

    let VaultForm {
        filter,
        sidebar: sidebar_state,
        search,
        list,
        selected: selected_item,
        opened: _,
        moved: moved_item,
        menu,
        menus_opened,
        revealed,
        subtitle,
    } = form;

    let position = |filter| {
        sidebar_state
            .filters
            .iter()
            .position(|slot| *slot == Some(filter))
    };
    if position(*filter).is_none() {
        *filter = Filter::All;
    }
    let mut selected = position(*filter);

    let mut action = None;
    let mut drawn = None;
    // The title is the filter of the last pass, because the window takes
    // it before the sidebar reads a pick; a pick asks for another pass.
    let title = label(*filter, index);
    let mut window = ui.window("vault", title, |ui| {
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

            // The filter shown is the one picked this pass.
            let shown = selected
                .and_then(|entry| sidebar_state.filters[entry])
                .unwrap_or(*filter);
            list.refresh(index, revision, shown, search);

            let searching = !search.query.is_empty();
            let count = match searching {
                true => list.rows.len(),
                false => sidebar_state.counts.get(shown),
            };
            let plural = if count == 1 { "" } else { "s" };
            subtitle.clear();
            write!(subtitle, "{count} item{plural} · {}", screen.email)
                .expect("writing to a String does not fail");

            let empty = match shown {
                _ if searching => "No items match your search.",
                Filter::Trash => "Trash is empty.",
                _ => "No items here yet.",
            };
            let pane = list_pane::<A>(ui, list, index, *selected_item, (menu, menus_opened), empty);
            if let Some(asked) = pane.asked {
                action = Some(VaultAction::User(asked));
            }

            // A pick opens its item, or for an item that cannot be read,
            // closes the open one, because its details come from the list.
            let mut opening = false;
            // Up or Down in the search field last pass picks as a click
            // does.
            if let Some(id) = pane.picked.or(moved_item.take()) {
                select(selected_item, revealed, Some(id));
                let readable = index
                    .ciphers
                    .iter()
                    .any(|cipher| cipher.id == id && cipher.readable);
                let asked = match (readable, opened) {
                    (true, opened) if opened != Some(id) => Some(UserAction::Open(id)),
                    (false, Some(_)) => Some(UserAction::Close),
                    _ => None,
                };
                opening = matches!(asked, Some(UserAction::Open(_)));
                if let Some(asked) = asked {
                    action = Some(VaultAction::User(asked));
                }
            }

            let details = Details {
                index,
                selected: *selected_item,
                open,
                opening,
                error: screen.error,
            };
            if let Some(text) = details_pane(ui, details, revealed, now_ms) {
                action = Some(VaultAction::Copy(text));
            }
            drawn = Some(details);
        });

        // The split view came first, so the toolbar acts on the item the
        // details show this pass.
        ui.toolbar(|ui| {
            ui.toolbar_toggle_sidebar("toggle sidebar");
            ui.toolbar_sidebar_separator("sidebar");
            // The title names the list, so it stays over the list pane.
            ui.toolbar_divider_separator("list", 1);
            ui.toolbar_flexible_space("space");
            let (summary, view) =
                drawn.map_or((None, None), |details: Details<'_>| details.shown());
            ui.toolbar_group("item", |ui| {
                if let Some(asked) = commands(ui, summary, view) {
                    action = Some(VaultAction::User(asked));
                }
            });
            let field = ui.toolbar_search("search", "Search", SEARCH_WIDTH, &mut search.text);
            let changed = field.changed();
            // Up and Down move the list's selection from the field.
            let step: isize = field
                .keys()
                .map(|key| match key {
                    Key::Up => -1,
                    Key::Down => 1,
                })
                .sum();
            if changed {
                search.query.set(&search.text);
                search.revision += 1;
                // The list came before the field in this pass.
                ui.request_pass();
            }
            if let Some(row) = moved(&list.rows, index, *selected_item, step) {
                *moved_item = Some(index.ciphers[list.rows[row]].id);
                ui.request_pass();
            }

            ui.toolbar_group("vault", |ui| {
                let mut sync = ui.toolbar_item("sync", "arrow.clockwise", "Sync");
                sync.enabled(!screen.syncing);
                if sync.clicked() {
                    action = Some(VaultAction::User(UserAction::Sync));
                }
                if ui.toolbar_item("lock", "lock", "Lock").clicked() {
                    action = Some(VaultAction::User(UserAction::Lock));
                }
            });
        });
    });

    window
        .full_size_content()
        .subtitle(subtitle)
        .initial_size(WIDTH, HEIGHT);
    if window.should_close() {
        action = Some(VaultAction::Close);
    }

    if let Some(picked) = selected.and_then(|entry| sidebar_state.filters[entry]) {
        if picked != *filter {
            // The title named the filter before.
            ui.request_pass();
        }
        *filter = picked;
    }

    // The caller applies the action after this pass drew the window, so
    // ask for a pass that shows the result.
    if action.is_some() {
        ui.request_pass();
    }
    action
}

/// What the user did in the list pane in a pass.
struct ListPane {
    /// The item the user picked in the list.
    picked: Option<Uuid>,
    /// What the user chose in a row's context menu.
    asked: Option<UserAction>,
}

/// Draws the list pane: the filter's items, with the row of `selected`
/// selected, or `empty` when there are none. The toolbar names the filter.
/// `menus` is the open context menu and the count of menus opened.
fn list_pane<A: VaultApp>(
    ui: &mut Ui,
    list: &ItemList,
    index: &VaultIndex,
    selected: Option<Uuid>,
    (menu, opened): (&mut Option<ItemMenu>, &mut u64),
    empty: &str,
) -> ListPane {
    let mut picked = None;
    let mut asked = None;

    // The pane reaches under the toolbar; only scroll views inset their
    // content below it, so the note scrolls too.
    ui.grid("list pane", &[Length::Star(1.0)], |ui| {
        if list.rows.is_empty() {
            ui.scroll_view("empty", |ui| {
                ui.stack_view("empty", Orientation::Vertical, 0.0, |ui| {
                    ui.label("note", format_args!("{empty}"))
                        .text_color(TextColor::Secondary);
                })
                .padding([16.0, 8.0, 16.0, 8.0]);
            });
            return;
        }

        let rows = &list.rows;
        let mut row = rows
            .iter()
            .position(|&row| Some(index.ciphers[row].id) == selected);
        let mut table = ui.table_view("items", rows.len(), list.revision, row_view::<A>);
        table.inset(true);

        // Clicking below the last row picks none; the next pass selects
        // the item's row again.
        if table.selection(&mut row)
            && let Some(row) = row
        {
            picked = Some(index.ciphers[rows[row]].id);
        }

        if let Some(anchor) = table.context_requested() {
            // A click below the last row opens no menu.
            *menu = anchor.row().map(|row| {
                *opened += 1;
                ItemMenu {
                    item: index.ciphers[rows[row]].id,
                    anchor,
                    serial: *opened,
                }
            });
        }

        // An item that left the vault closes its menu.
        let shown = menu.as_ref().and_then(|open| {
            let summary = index.ciphers.iter().find(|cipher| cipher.id == open.item)?;
            Some((open, summary))
        });
        let closed = shown.is_none_or(|(open, summary)| {
            ui.menu(("item menu", open.serial), open.anchor.clone(), |ui| {
                asked = context_menu(ui, summary);
            })
            .closed()
        });
        if closed {
            *menu = None;
        }
    })
    .width(LIST_WIDTH);

    ListPane { picked, asked }
}

/// The items of a row's context menu, from the "Vault — list context
/// menu" board: the copies the item offers, then Move to Trash, or Put
/// Back for an item in the trash. Returns what the user chose.
fn context_menu(ui: &mut Ui, summary: &CipherSummary) -> Option<UserAction> {
    let id = summary.id;
    if summary.deleted {
        let restore = ui.menu_item("restore", "Put Back");
        return restore.clicked().then_some(UserAction::Restore(id));
    }

    let mut asked = None;
    let mut copies = false;
    for (field, title) in COPIES {
        if copyable(summary, field) {
            copies = true;
            if ui.menu_item(("copy", field as u8), title).clicked() {
                asked = Some(UserAction::Copy { id, field });
            }
        }
    }
    if copies {
        ui.menu_separator("copies");
    }
    if ui.menu_item("trash", "Move to Trash").clicked() {
        asked = Some(UserAction::Trash(id));
    }
    asked
}

/// The copy items of the context menu, in order.
const COPIES: [(CopyField, &str); 3] = [
    (CopyField::Username, "Copy User Name"),
    (CopyField::Password, "Copy Password"),
    (CopyField::Totp, "Copy Verification Code"),
];

/// Whether the context menu offers to copy `field` of `summary`: the item
/// holds it, decrypted, and the master password re-prompt, which Chaveiro
/// cannot ask yet, does not guard it.
fn copyable(summary: &CipherSummary, field: CopyField) -> bool {
    summary.readable && summary.copyable.contains(field) && !(field.guarded() && summary.reprompt)
}

/// One row of the item list: the type icon, the name over the subtitle,
/// and a star for a favorite.
fn row_view<A: VaultApp>(app: &mut A, ui: &mut Ui, _: TableViewId, position: usize) {
    let cipher = app.vault().and_then(|(form, index)| {
        let row = *form.list.rows.get(position)?;
        Some(&index.ciphers[row])
    });

    // A row declares one control even when the vault just went away.
    let Some(cipher) = cipher else {
        ui.stack_view("row", Orientation::Horizontal, 0.0, |_| {});
        return;
    };

    let name = match cipher.readable {
        true => cipher.name.as_str(),
        false => UNREADABLE_ITEM,
    };

    ui.stack_view("row", Orientation::Horizontal, 10.0, |ui| {
        // Symbols differ in width, so a fixed one lines up the names.
        ui.symbol("icon", kind_symbol(cipher.kind), 15.0)
            .text_color(TextColor::Secondary)
            .width(22.0)
            .align(Align::Center);

        ui.stack_view("text", Orientation::Vertical, 1.0, |ui| {
            ui.label("name", format_args!("{name}")).flexible_width();
            ui.label("subtitle", format_args!("{}", cipher.subtitle.as_str()))
                .style(Style::Caption)
                .text_color(TextColor::Secondary)
                .flexible_width();
        })
        .padding([0.0; 4])
        .flexible_width()
        .align(Align::Center);

        if cipher.favorite {
            ui.symbol("favorite", "star.fill", 11.0)
                .text_color(TextColor::Orange)
                .align(Align::Center);
        }
    })
    .padding([6.0, 7.0, 6.0, 7.0]);
}

/// What the details pane shows.
#[derive(Clone, Copy)]
struct Details<'a> {
    index: &'a VaultIndex,
    selected: Option<Uuid>,
    open: Option<&'a CipherView>,
    /// The core opens `selected` after this pass, so the pane still shows
    /// `open`, or no item, for this one pass.
    opening: bool,
    error: Option<&'a ItemError>,
}

impl<'a> Details<'a> {
    /// The item the pane shows, and its open form once the core opened it.
    fn shown(&self) -> (Option<&'a CipherSummary>, Option<&'a CipherView>) {
        // Drawing what showed before until the new item opens keeps the
        // pane's controls in place, so the pane does not blank for a pass.
        let shown = match self.opening {
            true => self.open.and_then(|view| view.id),
            false => self.selected,
        };
        let summary = shown.and_then(|id| self.index.ciphers.iter().find(|cipher| cipher.id == id));
        let view = summary.and_then(|summary| {
            self.open
                .filter(|view| summary.readable && view.id == Some(summary.id))
        });
        (summary, view)
    }
}

/// Draws the shown item's details in a scroll view. Returns the value of
/// the field whose Copy button was clicked.
fn details_pane(
    ui: &mut Ui,
    details: Details<'_>,
    revealed: &mut Vec<FieldId>,
    now_ms: u64,
) -> Option<SecretString> {
    let index = details.index;
    let (summary, view) = details.shown();
    let mut copied = None;

    ui.scroll_view("details", |ui| {
        ui.stack_view("details", Orientation::Vertical, 18.0, |ui| {
            if let Some(error) = details.error {
                let (title, message) = item_error(error);
                notice(ui, "error", Some(title), message);
            }

            let Some(summary) = summary else {
                ui.label("empty", format_args!("Select an item to see its details."))
                    .text_color(TextColor::Secondary);
                return;
            };

            if !summary.readable {
                heading(ui, summary.kind, UNREADABLE_ITEM, None);
                notice(
                    ui,
                    "unreadable",
                    Some("This item didn’t decrypt."),
                    "Its key or data may be damaged. Other items are not affected. \
                     Try Sync; if it stays, check it in the web vault.",
                );
                return;
            }

            let Some(view) = view else {
                return;
            };

            // A folder id the index does not list is one that did not
            // decrypt.
            let folder = view.folder_id.map(|id| {
                index
                    .folders
                    .iter()
                    .find(|folder| folder.id == id)
                    .map_or(UNREADABLE_FOLDER, folder_label)
            });

            heading(ui, view.kind, view.name.as_str(), folder);

            if view.reprompt {
                notice(
                    ui,
                    "reprompt",
                    None,
                    "This item asks for your master password before it opens. \
                     Chaveiro can’t ask for it yet, so the item stays closed.",
                );
                return;
            }

            copied = fields(ui, view, revealed, now_ms);
            footer(ui, folder, view);
        })
        .padding([28.0, 8.0, 28.0, 28.0]);
    });
    copied
}

/// A message in the details: an error line for `title` when there is one,
/// over `message` in the secondary color.
fn notice(ui: &mut Ui, id: &'static str, title: Option<&str>, message: &str) {
    ui.stack_view(id, Orientation::Vertical, 4.0, |ui| {
        if title.is_some() {
            ui.alert_line("title", title);
        }
        ui.label("message", format_args!("{message}"))
            .text_color(TextColor::Secondary)
            .wraps();
    })
    .padding([0.0; 4]);
}

/// The heading over the details: the item's type icon, and its name over
/// its type and folder.
fn heading(ui: &mut Ui, kind: CipherKind, name: &str, folder: Option<&str>) {
    ui.stack_view("heading", Orientation::Horizontal, 12.0, |ui| {
        ui.symbol("icon", kind_symbol(kind), 24.0)
            .text_color(TextColor::Secondary)
            .align(Align::Center);

        ui.stack_view("title", Orientation::Vertical, 2.0, |ui| {
            ui.label("name", format_args!("{name}"))
                .style(Style::Heading)
                .flexible_width();

            let kind = kind_label(kind);
            let mut line = match folder {
                Some(folder) => ui.label("kind", format_args!("{kind} · {folder}")),
                None => ui.label("kind", format_args!("{kind}")),
            };
            line.style(Style::Caption)
                .text_color(TextColor::Secondary)
                .flexible_width();
        })
        .padding([0.0; 4])
        .flexible_width()
        .align(Align::Center);
    })
    .padding([0.0; 4]);
}

/// The toolbar's item commands: Favorite and Move to Trash, or Restore for
/// an item in the trash. They act on the shown item, and are disabled until
/// it is open. Returns what the user asked for.
fn commands(
    ui: &mut Ui,
    summary: Option<&CipherSummary>,
    view: Option<&CipherView>,
) -> Option<UserAction> {
    if let Some(summary) = summary.filter(|summary| summary.deleted) {
        let mut restore = ui.toolbar_item("restore", "arrow.uturn.backward", "Restore");
        restore.enabled(view.is_some());
        return restore.clicked().then_some(UserAction::Restore(summary.id));
    }

    let mut asked = None;
    let (symbol, label) = match view.is_some_and(|view| view.favorite) {
        true => ("star.fill", "Unfavorite"),
        false => ("star", "Favorite"),
    };

    let mut favorite = ui.toolbar_item("favorite", symbol, label);
    favorite.enabled(view.is_some());
    if favorite.clicked()
        && let Some(view) = view
    {
        let mut edited = view.clone();
        edited.favorite = !edited.favorite;
        asked = Some(UserAction::Save(Box::new(edited)));
    }

    let mut trash = ui.toolbar_item("trash", "trash", "Move to Trash");
    trash.enabled(view.is_some());
    if trash.clicked()
        && let Some(summary) = summary
    {
        asked = Some(UserAction::Trash(summary.id));
    }

    asked
}

/// The field groups of `view`'s type, then its custom fields and notes.
/// Returns the value of the field whose Copy button was clicked.
fn fields(
    ui: &mut Ui,
    view: &CipherView,
    revealed: &mut Vec<FieldId>,
    now_ms: u64,
) -> Option<SecretString> {
    let mut copied = None;
    let mut copy = |text| copied = copied.take().or(text);
    match &view.content {
        CipherContent::Login(login) => copy(login_fields(ui, login, revealed, now_ms)),
        CipherContent::SecureNote => {}
        CipherContent::Unknown => notice(
            ui,
            "unknown",
            None,
            "Chaveiro can’t show this type of item yet. It stays safe in your vault; \
             open it in another app to see or edit it.",
        ),
        CipherContent::Card(card) => copy(card_fields(ui, card, revealed)),
        CipherContent::Identity(identity) => copy(identity_fields(ui, identity, revealed)),
        CipherContent::SshKey(ssh_key) => copy(ssh_key_fields(ui, ssh_key, revealed)),
    }

    let custom = view.fields.iter().enumerate().map(|(position, field)| {
        let id = ("custom", position);
        let caption = field.name.as_ref().map_or("", |name| name.as_str());
        let value = text(&field.value);
        match field.kind {
            CUSTOM_HIDDEN => Field::secret(id, caption, value),
            CUSTOM_BOOLEAN => Field::text(
                id,
                caption,
                Some(match value {
                    Some("true") => "Yes",
                    _ => "No",
                }),
            ),
            CUSTOM_LINKED => match field
                .linked_id
                .and_then(|linked_id| linked(view, linked_id))
            {
                Some((value, true)) => Field::secret(id, caption, value),
                Some((value, false)) => Field::text(id, caption, value),
                None => Field::text(id, caption, Some("Linked field")),
            },
            _ => Field::text(id, caption, value),
        }
    });
    copy(group(
        ui,
        "custom fields",
        Some("Custom fields"),
        custom,
        revealed,
    ));

    let caption = match view.kind {
        CipherKind::SecureNote => "Note",
        _ => "Notes",
    };
    let note = Field::lines(("notes", 0), caption, text(&view.notes));
    copy(group(ui, "notes", None, [note].into_iter(), revealed));
    copied
}

fn login_fields(
    ui: &mut Ui,
    login: &LoginView,
    revealed: &mut Vec<FieldId>,
    now_ms: u64,
) -> Option<SecretString> {
    let now_s = now_ms / 1000;
    let code = text(&login.totp).and_then(|secret| totp::now(secret, now_s));
    // The countdown changes each second, so the pane draws again then.
    if code.is_some() {
        let deadline = std::time::UNIX_EPOCH + std::time::Duration::from_secs(now_s + 1);
        ui.task(("next second", deadline), move || {
            appkit_executor::sleep_until_clock(deadline)
        });
    }
    let account = [
        Field::text(("username", 0), "User name", text(&login.username)),
        Field::secret(("password", 0), "Password", text(&login.password)),
    ]
    .into_iter()
    .chain(code.as_ref().map(|code| Field {
        id: ("totp", 0),
        caption: "Verification code",
        value: Value::Code(code),
        kind: Kind::Code {
            seconds_left: code.seconds_left(now_s),
        },
    }));
    let mut copied = group(ui, "account", None, account, revealed);

    let passkeys = login
        .fido2_credentials
        .iter()
        .enumerate()
        .map(|(position, passkey)| Field {
            value: Value::Passkey {
                site: passkey.rp_id.as_str(),
                user: text(&passkey.user_name),
            },
            ..Field::text(("passkey", position), "Passkey", None)
        });
    copied = copied.or(group(ui, "passkeys", Some("Passkeys"), passkeys, revealed));

    let websites = login.uris.iter().enumerate().map(|(position, uri)| {
        let caption = match uri.match_rule {
            Some(0) => "Website · base domain",
            Some(1) => "Website · host",
            Some(2) => "Website · starts with",
            Some(3) => "Website · exact",
            Some(4) => "Website · regular expression",
            Some(5) => "Website · never",
            None | Some(_) => "Website",
        };
        Field::text(("website", position), caption, text(&uri.uri))
    });
    copied.or(group(ui, "websites", Some("Websites"), websites, revealed))
}

fn card_fields(ui: &mut Ui, card: &CardView, revealed: &mut Vec<FieldId>) -> Option<SecretString> {
    let mut number = Field::secret(("number", 0), "Number", text(&card.number));
    if let Some(digits) = text(&card.number) {
        number.value = Value::CardNumber(digits);
    }

    // The hidden number keeps its last 4 digits.
    if let Some(last) = card
        .number
        .as_ref()
        .filter(|number| number.len() > 4)
        .and_then(|number| number.get(number.len() - 4..))
    {
        number.kind = Kind::Secret {
            hidden: Value::Masked(last),
        };
    }

    let expiration = match (text(&card.exp_month), text(&card.exp_year)) {
        (None, None) => Value::Text(None),
        (month, year) => Value::Expiration(month.unwrap_or(""), year.unwrap_or("")),
    };

    let fields = [
        Field::text(
            ("cardholder", 0),
            "Cardholder name",
            text(&card.cardholder_name),
        ),
        Field::text(("brand", 0), "Brand", text(&card.brand)),
        number,
        Field {
            value: expiration,
            ..Field::monospace(("expiration", 0), "Expiration", None)
        },
        Field::secret(("code", 0), "Security code", text(&card.code)),
    ];
    group(ui, "card", None, fields.into_iter(), revealed)
}

fn identity_fields(
    ui: &mut Ui,
    identity: &IdentityView,
    revealed: &mut Vec<FieldId>,
) -> Option<SecretString> {
    fn part(value: &Option<SecretString>) -> Value<'_> {
        Value::Text(text(value))
    }

    let name = [
        part(&identity.title),
        part(&identity.first_name),
        part(&identity.middle_name),
        part(&identity.last_name),
    ];
    let place = [
        part(&identity.city),
        part(&identity.state),
        part(&identity.postal_code),
    ];
    let address = [
        part(&identity.address1),
        part(&identity.address2),
        part(&identity.address3),
        Value::Joined(&place, " "),
        part(&identity.country),
    ];

    let personal = [
        Field {
            value: Value::Joined(&name, " "),
            ..Field::text(("name", 0), "Name", None)
        },
        Field::text(("username", 0), "User name", text(&identity.username)),
        Field::text(("company", 0), "Company", text(&identity.company)),
    ];
    let personal = group(
        ui,
        "personal",
        Some("Personal"),
        personal.into_iter(),
        revealed,
    );

    let identification = [
        Field::secret(
            ("passport", 0),
            "Passport number",
            text(&identity.passport_number),
        ),
        Field::secret(("ssn", 0), "Tax number", text(&identity.ssn)),
        Field::monospace(
            ("license", 0),
            "Driver’s license",
            text(&identity.license_number),
        ),
    ];
    let identification = group(
        ui,
        "identification",
        Some("Identification"),
        identification.into_iter(),
        revealed,
    );

    let contact = [
        Field::text(("email", 0), "Email", text(&identity.email)),
        Field::text(("phone", 0), "Phone", text(&identity.phone)),
        Field {
            value: Value::Joined(&address, "\n"),
            ..Field::lines(("address", 0), "Address", None)
        },
    ];
    let contact = group(
        ui,
        "contact",
        Some("Contact"),
        contact.into_iter(),
        revealed,
    );
    personal.or(identification).or(contact)
}

fn ssh_key_fields(
    ui: &mut Ui,
    ssh_key: &SshKeyView,
    revealed: &mut Vec<FieldId>,
) -> Option<SecretString> {
    let fields = [
        Field::monospace(("public key", 0), "Public key", text(&ssh_key.public_key)),
        Field::monospace(
            ("fingerprint", 0),
            "Fingerprint",
            text(&ssh_key.key_fingerprint),
        ),
        Field::secret(
            ("private key", 0),
            "Private key",
            text(&ssh_key.private_key),
        ),
    ];
    let copied = group(ui, "ssh key", None, fields.into_iter(), revealed);

    notice(
        ui,
        "agent",
        None,
        "The SSH agent offers this key. Chaveiro asks you before each use.",
    );
    copied
}

/// The line under the fields: the folder, when the password last changed,
/// and whether it has a history.
fn footer(ui: &mut Ui, folder: Option<&str>, view: &CipherView) {
    let changed = match &view.content {
        CipherContent::Login(login) => login.password_revision_date.as_deref(),
        _ => None,
    };

    // The dates are ISO 8601; the day is enough here.
    let day = changed.map(|date| date.get(..10).unwrap_or(date));

    let history = match view.password_history.len() {
        0 => "",
        _ => " · Has password history",
    };

    ui.label(
        "footer",
        format_args!(
            "{}{}{}{history}",
            match folder {
                Some(_) => "Folder: ",
                None => "No folder",
            },
            folder.unwrap_or(""),
            Changed(day),
        ),
    )
    .style(Style::Caption)
    .text_color(TextColor::Secondary)
    .wraps();
}

/// " · Password changed <day>", or nothing.
struct Changed<'a>(Option<&'a str>);

impl std::fmt::Display for Changed<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(day) => write!(formatter, " · Password changed {day}"),
            None => Ok(()),
        }
    }
}

/// One field row of a group, before it is drawn.
#[derive(Clone, Copy)]
struct Field<'a> {
    id: FieldId,
    caption: &'a str,
    /// Empty for a field the item leaves empty, which is hidden.
    value: Value<'a>,
    kind: Kind<'a>,
}

/// What a field shows, written straight into its label, so a pass
/// allocates nothing and copies no secret into a buffer of its own.
#[derive(Clone, Copy)]
enum Value<'a> {
    Text(Option<&'a str>),
    /// The site, then ` · ` and the user name when there is one.
    Passkey {
        site: &'a str,
        user: Option<&'a str>,
    },
    /// A card number's dots, then its last digits.
    Masked(&'a str),
    /// A card number in groups of 4, as printed on the card; spaces the
    /// item already has are dropped.
    CardNumber(&'a str),
    /// `MM / YYYY`, padding a one-digit month.
    Expiration(&'a str, &'a str),
    /// The parts that are not empty, joined by the separator.
    Joined(&'a [Value<'a>], &'a str),
    /// A verification code, in two halves.
    Code(&'a totp::Code),
}

impl Value<'_> {
    /// What a Copy button puts on the pasteboard: the text as the item
    /// holds it, so a card number or a code copies without its spaces.
    fn copied(&self) -> SecretString {
        let raw = match self {
            Value::Text(text) => text.unwrap_or(""),
            Value::Code(code) => code.as_str(),
            Value::CardNumber(number) => {
                return SecretString::new(
                    number
                        .chars()
                        .filter(|char| !char.is_whitespace())
                        .collect(),
                );
            }
            _ => return SecretString::new(self.to_string()),
        };
        SecretString::new(raw.to_owned())
    }

    fn is_empty(&self) -> bool {
        match self {
            Value::Text(text) => text.is_none_or(str::is_empty),
            Value::CardNumber(number) => number.trim().is_empty(),
            Value::Passkey { .. } | Value::Masked(_) | Value::Expiration(..) | Value::Code(_) => {
                false
            }
            Value::Joined(parts, _) => parts.iter().all(Value::is_empty),
        }
    }
}

impl std::fmt::Display for Value<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Text(text) => formatter.write_str(text.unwrap_or("")),
            Value::Passkey { site, user } => match user {
                Some(user) => write!(formatter, "{site} · {user}"),
                None => formatter.write_str(site),
            },
            Value::Masked(last) => write!(formatter, "•••• •••• •••• {last}"),
            Value::CardNumber(number) => {
                let digits = number.chars().filter(|char| !char.is_whitespace());
                for (position, char) in digits.enumerate() {
                    if position > 0 && position % 4 == 0 {
                        formatter.write_char(' ')?;
                    }
                    formatter.write_char(char)?;
                }
                Ok(())
            }
            Value::Expiration(month, year) => {
                let pad = if month.len() == 1 { "0" } else { "" };
                write!(formatter, "{pad}{month} / {year}")
            }
            Value::Code(code) => write!(formatter, "{code}"),
            Value::Joined(parts, separator) => {
                let mut first = true;
                for part in parts.iter().filter(|part| !part.is_empty()) {
                    if !first {
                        formatter.write_str(separator)?;
                    }
                    first = false;
                    write!(formatter, "{part}")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Kind<'a> {
    Text,
    Monospace,
    /// Text that keeps its line breaks, such as a note or an address.
    Lines,
    /// A monospace value that shows `hidden` until its Show button
    /// reveals it.
    Secret {
        hidden: Value<'a>,
    },
    /// A verification code, with the seconds until the next one.
    Code {
        seconds_left: u64,
    },
}

impl<'a> Field<'a> {
    fn text(id: FieldId, caption: &'a str, value: Option<&'a str>) -> Self {
        Field {
            id,
            caption,
            value: Value::Text(value),
            kind: Kind::Text,
        }
    }

    fn monospace(id: FieldId, caption: &'a str, value: Option<&'a str>) -> Self {
        Field {
            kind: Kind::Monospace,
            ..Field::text(id, caption, value)
        }
    }

    fn lines(id: FieldId, caption: &'a str, value: Option<&'a str>) -> Self {
        Field {
            kind: Kind::Lines,
            ..Field::text(id, caption, value)
        }
    }

    fn secret(id: FieldId, caption: &'a str, value: Option<&'a str>) -> Self {
        Field {
            kind: Kind::Secret {
                hidden: Value::Text(Some(DOTS)),
            },
            ..Field::text(id, caption, value)
        }
    }
}

/// Draws the fields of `fields` that have a value, in a group box under
/// `header`. Draws nothing when none has a value. Returns the value of the
/// field whose Copy button was clicked.
fn group<'a>(
    ui: &mut Ui,
    id: &'static str,
    header: Option<&str>,
    fields: impl Clone + Iterator<Item = Field<'a>>,
    revealed: &mut Vec<FieldId>,
) -> Option<SecretString> {
    let filled = fields.filter(|field| !field.value.is_empty());

    if filled.clone().next().is_none() {
        return None;
    }
    let mut copied = None;

    ui.stack_view(id, Orientation::Vertical, 6.0, |ui| {
        if let Some(header) = header {
            ui.label("header", format_args!("{header}"))
                .style(Style::Caption)
                .text_color(TextColor::Secondary);
        }

        ui.group_box("fields", |ui| {
            for (position, field) in filled.enumerate() {
                if position > 0 {
                    ui.separator(("separator", field.id));
                }
                if field_row(ui, &field, revealed) {
                    copied = Some(field.value.copied());
                }
            }
        });
    })
    .padding([0.0; 4]);
    copied
}

/// One row: the caption beside the value, a Show button for a secret, and
/// a Copy button for any value but a passkey. Returns whether Copy was
/// clicked.
fn field_row(ui: &mut Ui, field: &Field<'_>, revealed: &mut Vec<FieldId>) -> bool {
    let shown = revealed.contains(&field.id);
    let mut copied = false;

    // A note's caption stays at its first line.
    let align = match field.kind {
        Kind::Lines => Align::Start,
        Kind::Text | Kind::Monospace | Kind::Secret { .. } | Kind::Code { .. } => Align::Center,
    };

    ui.stack_view(field.id, Orientation::Horizontal, 12.0, |ui| {
        // The same style as the value, so both lines are as tall.
        ui.label("caption", format_args!("{}", field.caption))
            .style(Style::Body)
            .text_color(TextColor::Secondary)
            .width(CAPTION_WIDTH)
            .align(align);

        let value = match field.kind {
            Kind::Secret { hidden } if !shown => hidden,
            _ => field.value,
        };
        let mut label = ui.label("value", format_args!("{value}"));

        // Custom fields of one position may differ in kind between items,
        // so the style is set either way.
        label
            .style(match field.kind {
                Kind::Monospace | Kind::Secret { .. } => Style::Monospace,
                Kind::Code { .. } => Style::Title2,
                Kind::Text | Kind::Lines => Style::Body,
            })
            .flexible_width()
            .align(align);

        if let Kind::Lines = field.kind {
            label.wraps();
        }

        if let Kind::Code { seconds_left } = field.kind {
            ui.label("seconds left", format_args!("{seconds_left} s"))
                .style(Style::Caption)
                .text_color(TextColor::Secondary)
                .align(Align::Center);
        }

        if let Kind::Secret { .. } = field.kind {
            let (symbol, label) = match shown {
                true => ("eye.slash", "Hide"),
                false => ("eye", "Show"),
            };

            let mut reveal = ui.icon_button("reveal", symbol, label);
            reveal.align(Align::Center);

            if reveal.clicked() {
                match shown {
                    true => revealed.retain(|id| *id != field.id),
                    false => revealed.push(field.id),
                }

                // The value was drawn before the click was read.
                ui.request_pass();
            }
        }

        if !matches!(field.value, Value::Passkey { .. }) {
            let label = match field.kind {
                Kind::Code { .. } => "Copy verification code",
                _ => "Copy",
            };
            let mut copy = ui.icon_button("copy", "doc.on.doc", label);
            copy.align(Align::Center);
            copied = copy.clicked();
        }
    })
    .padding([12.0, 9.0, 12.0, 9.0]);
    copied
}

fn text(secret: &Option<SecretString>) -> Option<&str> {
    secret.as_ref().map(|secret| secret.as_str())
}

/// The value a linked custom field shows: the item field `id` names, and
/// whether it is a secret. [`None`] for an id this type does not have.
fn linked(view: &CipherView, id: u32) -> Option<(Option<&str>, bool)> {
    match &view.content {
        CipherContent::Login(login) => match id {
            100 => Some((text(&login.username), false)),
            101 => Some((text(&login.password), true)),
            _ => None,
        },
        CipherContent::Card(card) => match id {
            300 => Some((text(&card.cardholder_name), false)),
            301 => Some((text(&card.exp_month), false)),
            302 => Some((text(&card.exp_year), false)),
            303 => Some((text(&card.code), true)),
            304 => Some((text(&card.brand), false)),
            305 => Some((text(&card.number), true)),
            _ => None,
        },
        CipherContent::Identity(identity) => match id {
            400 => Some((text(&identity.title), false)),
            401 => Some((text(&identity.middle_name), false)),
            402 => Some((text(&identity.address1), false)),
            403 => Some((text(&identity.address2), false)),
            404 => Some((text(&identity.address3), false)),
            405 => Some((text(&identity.city), false)),
            406 => Some((text(&identity.state), false)),
            407 => Some((text(&identity.postal_code), false)),
            408 => Some((text(&identity.country), false)),
            409 => Some((text(&identity.company), false)),
            410 => Some((text(&identity.email), false)),
            411 => Some((text(&identity.phone), false)),
            412 => Some((text(&identity.ssn), true)),
            413 => Some((text(&identity.username), false)),
            414 => Some((text(&identity.passport_number), true)),
            415 => Some((text(&identity.license_number), false)),
            416 => Some((text(&identity.first_name), false)),
            417 => Some((text(&identity.last_name), false)),
            _ => None,
        },
        _ => None,
    }
}

/// The title and message shown for `error`.
fn item_error(error: &ItemError) -> (&str, &str) {
    match error {
        ItemError::NotFound => ("This item is gone.", "It is no longer in the vault."),
        ItemError::Unreadable(_) => (
            "This item didn’t decrypt.",
            "Its key or data may be damaged. Other items are not affected.",
        ),
        ItemError::Busy => (
            "A change to this item is still saving.",
            "Try again in a moment.",
        ),
        ItemError::Rejected(message) => ("The change was not saved.", message.as_str()),
    }
}

/// The line under the sidebar: syncing, or when the last sync was, and
/// whether it failed.
fn sync_status(ui: &mut Ui, screen: &VaultScreen<'_>, now_ms: u64) {
    let ago = screen
        .synced_at
        .map(|synced_at| Ago(now_ms.saturating_sub(synced_at) / 1000));
    // The text counts minutes, so the line draws again on the next one.
    // Counted from the sync, so every pass in a minute names one deadline.
    if let Some(synced_at) = screen.synced_at {
        let elapsed = now_ms.saturating_sub(synced_at);
        let next_minute = synced_at + (elapsed / 60_000 + 1) * 60_000;
        let deadline = std::time::UNIX_EPOCH + std::time::Duration::from_millis(next_minute);
        ui.task(("next minute", deadline), move || {
            appkit_executor::sleep_until_clock(deadline)
        });
    }
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
        Filter::Kind(kind) => kind_symbol(kind),
        Filter::Folder(_) => "folder",
        Filter::NoFolder => "folder.badge.questionmark",
        Filter::Trash => "trash",
    }
}

/// The SF Symbols name of an item type.
fn kind_symbol(kind: CipherKind) -> &'static str {
    match kind {
        CipherKind::Login => "globe",
        CipherKind::Card => "creditcard",
        CipherKind::Identity => "person.text.rectangle",
        CipherKind::SecureNote => "note.text",
        CipherKind::SshKey => "key",
        CipherKind::Unknown(_) => "questionmark.square",
    }
}

/// The name of an item type, under the item's name in the details.
fn kind_label(kind: CipherKind) -> &'static str {
    match kind {
        CipherKind::Login => "Login",
        CipherKind::Card => "Card",
        CipherKind::Identity => "Identity",
        CipherKind::SecureNote => "Secure Note",
        CipherKind::SshKey => "SSH Key",
        CipherKind::Unknown(_) => "Item",
    }
}

/// The label of a folder whose name did not decrypt.
const UNREADABLE_FOLDER: &str = "Unreadable folder";
/// The name of an item that did not decrypt, in its row and details.
const UNREADABLE_ITEM: &str = "Can’t read this item";
/// A hidden secret.
const DOTS: &str = "••••••••••";
/// The width of the toolbar's search field, from the design.
const SEARCH_WIDTH: f64 = 190.0;
/// The width of the list pane, from the design.
const LIST_WIDTH: f64 = 300.0;
/// The width of a field row's caption, from the design.
const CAPTION_WIDTH: f64 = 130.0;

/// Custom field types, as the server numbers them.
const CUSTOM_HIDDEN: u8 = 1;
const CUSTOM_BOOLEAN: u8 = 2;
const CUSTOM_LINKED: u8 = 3;

/// The window's first content size, from the design's 1440×900 boards less
/// the menu bar.
const WIDTH: f64 = 1200.0;
const HEIGHT: f64 = 760.0;
