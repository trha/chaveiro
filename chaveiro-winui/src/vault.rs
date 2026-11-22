//! The vault window, from the VaultScreen design: the navigation pane of
//! filters with their item counts, the list of the chosen filter's items,
//! and the open item's details. Editing comes later.
//!
//! The window stays open for most of the app's life, so a pass that finds
//! the index and filter unchanged allocates nothing for the pane or the
//! list: the counts, the pane's entries and the list's rows are rebuilt
//! only when [`VaultScreen::index_revision`] or the filter changes.

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
use std::{fmt::Write as _, time::Duration};
use url::Url;
use uuid::Uuid;
use windows_core::Result;
use winui_immediate::{
    App, Key, Length, ListId, MenuAnchor, NavigationEntry, NavigationSelection, Orientation,
    Overflow, Severity, Ui,
};
use zeroize::{Zeroize, Zeroizing};

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
    /// The pane's selection. A folder that leaves the vault falls back to
    /// [`Filter::All`].
    pub filter: Filter,
    /// Settings is selected in the pane, over `filter`.
    pub settings: bool,
    sidebar: Sidebar,
    search: Search,
    list: ItemList,
    /// The item the list shows as selected, whose details show. It
    /// follows the core's open item, and a click on a row.
    selected: Option<Uuid>,
    /// The id of the core's open item on the last pass, to notice a new
    /// one.
    opened: Option<Uuid>,
    /// The row clicked since the last pass, which the next pass opens.
    clicked: Option<Uuid>,
    /// The secrets of the selected item shown in clear. Emptied when
    /// another item is selected.
    revealed: Vec<FieldId>,
    menus: ItemMenus,
}

impl Default for VaultForm {
    fn default() -> Self {
        VaultForm {
            filter: Filter::All,
            settings: false,
            sidebar: Sidebar::default(),
            search: Search::default(),
            list: ItemList::default(),
            selected: None,
            opened: None,
            clicked: None,
            revealed: Vec::new(),
            menus: ItemMenus::default(),
        }
    }
}

impl VaultForm {
    /// Selects All items and no item again, and empties the cached counts,
    /// filters and rows, keeping their capacity, so the next pass builds
    /// them from the index it is given.
    pub fn reset(&mut self) {
        self.filter = Filter::All;
        self.settings = false;
        let sidebar = &mut self.sidebar;
        sidebar.revision = None;
        sidebar.counts.clear();
        sidebar.filters.clear();
        self.search.text.zeroize();
        self.search.query.set("");
        self.search.revision += 1;
        self.list.shows = None;
        self.list.rows.clear();
        self.list.ranked.clear();
        self.selected = None;
        self.opened = None;
        self.clicked = None;
        self.revealed.clear();
        self.menus.open = None;
    }

    /// Selects `id`, hiding the secrets of the item selected before.
    fn select(&mut self, id: Option<Uuid>) {
        if self.selected != id {
            self.selected = id;
            self.revealed.clear();
        }
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

/// The search box in the title bar.
///
/// Contract: `query` is `text` parsed, and `revision` changes whenever
/// `query` does.
#[derive(Default)]
struct Search {
    text: Zeroizing<String>,
    query: Query,
    revision: u64,
    /// The box's placeholder: "Search" and the filter's name.
    placeholder: String,
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
        // The keys are unique, so an unstable sort keeps index order
        // inside a tier, and it does not allocate.
        self.ranked.sort_unstable();
        self.rows.clear();
        self.rows
            .extend(self.ranked.iter().map(|(_, position)| *position));
        self.shows = shows;
        self.revision += 1;
    }
}

/// The item menu, the design's ItemMenu, opened by a right-click on a row
/// or by the details pane's More button.
#[derive(Default)]
struct ItemMenus {
    /// The menu while it is open.
    open: Option<ItemMenu>,
    /// How many menus opened, so each one is a menu of its own, which
    /// opens at its own anchor.
    opened: u64,
}

impl ItemMenus {
    /// Opens the menu of the item `id` at `anchor`, closing any other.
    fn open(&mut self, id: Uuid, anchor: MenuAnchor) {
        self.opened += 1;
        self.open = Some(ItemMenu {
            item: id,
            anchor,
            serial: self.opened,
        });
    }
}

#[derive(Clone, Copy)]
struct ItemMenu {
    item: Uuid,
    anchor: MenuAnchor,
    /// The value of [`ItemMenus::opened`] when this menu opened.
    serial: u64,
}

/// Names one field of the selected item: a static name, and the position
/// of a website or custom field, or 0.
type FieldId = (&'static str, usize);

/// What the user asked for in a pass.
pub enum VaultAction {
    User(UserAction),
    /// The window's close button.
    Close,
    /// Open this `http` or `https` address in the default browser.
    OpenWebsite(Url),
}

/// What one pass of the vault window gives back.
pub struct VaultPass {
    pub action: Option<VaultAction>,
    /// The window's `AppWindow.Id`, to own system prompts.
    pub window: u64,
}

/// Draws the vault window, a window of its own beside the login one, which
/// a pass that shows the vault does not declare. `open` is the core's open
/// item, from [`VaultScreen::open`].
pub fn view<A: VaultApp>(
    ui: &mut Ui,
    form: &mut VaultForm,
    screen: &VaultScreen<'_>,
    open: Option<&CipherView>,
) -> Result<VaultPass> {
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
    // The core opened another item, such as a reopen after a sync. A row
    // clicked since wins, so this comes first.
    let opened = open.and_then(|view| view.id);
    if opened != form.opened {
        form.opened = opened;
        if opened.is_some() {
            form.select(opened);
        }
    }
    let mut action = None;
    // A row clicked or moved to last pass: open it, or for an item that
    // cannot be read, close the open one, because its details come from
    // the list.
    if let Some(id) = form.clicked.take() {
        form.select(Some(id));
        let readable = index
            .ciphers
            .iter()
            .any(|cipher| cipher.id == id && cipher.readable);
        let opened = open.and_then(|view| view.id);
        action = match (readable, opened) {
            (true, opened) if opened != Some(id) => Some(VaultAction::User(UserAction::Open(id))),
            (false, Some(_)) => Some(VaultAction::User(UserAction::Close)),
            _ => None,
        };
    }

    // This pass asks the core to open the selected item, which it does
    // after the pass draws, so `open` is still the item before.
    let opening = matches!(action, Some(VaultAction::User(UserAction::Open(_))));

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

    let search = &mut form.search;
    search.placeholder.clear();
    match form.filter {
        Filter::Folder(_) => {
            let name = label(form.filter, index);
            write!(search.placeholder, "Search {name}")
        }
        filter => search.placeholder.write_str(search_label(filter)),
    }
    .expect("writing to a String does not fail");
    search.placeholder.push_str(" (Ctrl+F)");
    let list = &mut form.list;
    let clicked = &mut form.clicked;
    let revealed = &mut form.revealed;
    let menus = &mut form.menus;
    let selected_item = form.selected;
    let mut window = ui.window("vault", "Chaveiro", |ui| {
        ui.grid("window", &[Length::Auto, Length::Star(1.0)], |ui| {
            // Rows that Up and Down in the search box move the selection by.
            let mut step = 0isize;
            ui.title_bar("title bar", "Chaveiro", false, |ui| {
                let mut field = ui.search_box("search", &search.placeholder, &mut search.text)?;
                let changed = field.changed();
                for key in field.keys()? {
                    match key {
                        Key::Down => step += 1,
                        Key::Up => step -= 1,
                        _ => {}
                    }
                }
                // The title bar sizes its middle to the content, so the
                // box gets the design's widest as its width.
                field.shortcut('F')?.width(SEARCH_WIDTH)?;
                if changed {
                    search.query.set(&search.text);
                    search.revision += 1;
                }
                Ok(())
            })?;
            let push = |entries: &mut Vec<_>| {
                pane(index, &sidebar.counts, |_, entry| entries.push(entry));
            };
            ui.navigation_view("pane", revision, push, &mut selected, |ui, selected| {
                let shown = match selected {
                    Some(NavigationSelection::Entry(entry)) => sidebar.filters[entry],
                    Some(NavigationSelection::Settings) | None => None,
                };
                let Some(filter) = shown else {
                    ui.stack("settings", Orientation::Vertical, 8.0, |ui| {
                        if selected == Some(NavigationSelection::Settings) {
                            title(ui, "Settings")?;
                            ui.label("note", format_args!("Settings are not built yet."))?;
                        }
                        Ok(())
                    })?
                    .padding([24.0, 16.0, 24.0, 24.0])?;
                    return Ok(());
                };
                list.refresh(index, revision, filter, search);
                let searching = !search.query.is_empty();
                ui.columns(
                    "panes",
                    &[Length::Pixels(LIST_WIDTH), Length::Star(1.0)],
                    0.0,
                    |ui| {
                        let heading = ListHeading {
                            title: label(filter, index),
                            count: match searching {
                                true => list.rows.len(),
                                false => sidebar.counts.get(filter),
                            },
                            empty: match filter {
                                _ if searching => "No items match your search.",
                                Filter::Trash => "Trash is empty.",
                                _ => "No items here yet.",
                            },
                        };
                        let pane = list_pane::<A>(ui, list, heading)?;
                        if pane.lock {
                            action = Some(VaultAction::User(UserAction::Lock));
                        }
                        // Up and Down move the selection, from the search box
                        // or from the list. The next pass opens the row they
                        // moved to, as a click would. From the list, focus
                        // follows the selection.
                        if let Some(items) = pane.items {
                            for (step, focus) in [(step, false), (pane.step, true)] {
                                let Some(position) = moved(&list.rows, index, selected_item, step)
                                else {
                                    continue;
                                };
                                *clicked = Some(index.ciphers[list.rows[position]].id);
                                ui.request_pass();
                                match focus {
                                    true => ui.focus_row(items, position),
                                    false => ui.bring_into_view(items, position),
                                }
                            }
                        }
                        let details = Details {
                            index,
                            selected: selected_item,
                            open,
                            opening,
                            error: screen.error,
                        };
                        let pass = details_pane(ui, details, revealed)?;
                        if let Some(asked) = pass.asked {
                            action = Some(VaultAction::User(asked));
                        }
                        if let Some((id, anchor)) = pass.more {
                            menus.open(id, anchor);
                            ui.request_pass();
                        }
                        Ok(())
                    },
                )?;
                Ok(())
            })?;
            // The copy keys act on the selected item, as its menu would.
            if let Some(summary) =
                selected_item.and_then(|id| index.ciphers.iter().find(|cipher| cipher.id == id))
            {
                for &(field, .., key) in COPIES {
                    if let Some((letter, _)) = key
                        && copyable(summary, field)
                        && ui.shortcut(("copy", field as u8), false, letter)?
                    {
                        action = Some(VaultAction::User(UserAction::Copy {
                            id: summary.id,
                            field,
                        }));
                    }
                }
            }
            if let Some(open_menu) = menus.open {
                match index
                    .ciphers
                    .iter()
                    .find(|cipher| cipher.id == open_menu.item)
                {
                    // The item left the vault, as after a sync.
                    None => menus.open = None,
                    Some(summary) => {
                        let mut picked = None;
                        let id = ("item menu", open_menu.serial);
                        let menu = ui.menu(id, open_menu.anchor, |ui| {
                            item_menu(ui, index, summary, &mut picked)
                        })?;
                        if menu.closed() {
                            menus.open = None;
                        }
                        if picked.is_some() {
                            action = picked;
                        }
                    }
                }
            }
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
    // The caller applies the action after this pass drew the window, and
    // a row click reaches here a pass after its event, so ask for a pass
    // that shows the result.
    if action.is_some() {
        ui.request_pass();
    }
    match selected {
        Some(NavigationSelection::Settings) => form.settings = true,
        Some(NavigationSelection::Entry(entry)) => {
            if let Some(filter) = sidebar.filters[entry] {
                if filter != form.filter {
                    // The search box came before the pane in this pass, so
                    // its placeholder names the old filter until the next.
                    ui.request_pass();
                }
                form.filter = filter;
                form.settings = false;
            }
        }
        None => {}
    }
    Ok(VaultPass { action, window })
}

/// What the list pane's header shows.
struct ListHeading<'a> {
    title: &'a str,
    count: usize,
    /// The text in place of an empty list.
    empty: &'a str,
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

/// What the user did in the list pane in a pass.
struct ListPane {
    /// Lock was clicked.
    lock: bool,
    /// The item list; [`None`] while the list is empty and shows a note.
    items: Option<ListId>,
    /// Rows that Up and Down in the list move the selection by.
    step: isize,
}

/// Draws the list pane: the filter's name and count over its items.
fn list_pane<A: VaultApp>(
    ui: &mut Ui,
    list: &ItemList,
    heading: ListHeading<'_>,
) -> Result<ListPane> {
    let mut lock = false;
    let mut items = None;
    let mut step = 0;
    ui.grid("list pane", &[Length::Auto, Length::Star(1.0)], |ui| {
        ui.columns("header", &[Length::Star(1.0), Length::Auto], 8.0, |ui| {
            ui.stack("heading", Orientation::Vertical, 0.0, |ui| {
                title(ui, heading.title)?;
                let count = heading.count;
                let plural = if count == 1 { "" } else { "s" };
                ui.label("count", format_args!("{count} item{plural}"))?
                    .secondary(true)?
                    .style("CaptionTextBlockStyle")?;
                Ok(())
            })?;
            // Lock moves to the title bar once it takes controls.
            lock = ui.button("lock", "Lock")?.clicked()?;
            Ok(())
        })?
        .margin([16.0, 16.0, 16.0, 8.0])?;
        match list.rows.is_empty() {
            true => {
                ui.label("empty", format_args!("{}", heading.empty))?
                    .secondary(true)?
                    .margin([16.0, 24.0, 16.0, 0.0])?;
            }
            false => {
                let list = ui.list("items", list.rows.len(), list.revision, row::<A>)?;
                for key in ui.list_keys(list)? {
                    match key {
                        Key::Down => step += 1,
                        Key::Up => step -= 1,
                        _ => {}
                    }
                }
                items = Some(list);
            }
        }
        Ok(())
    })?
    // The line between the list and the details.
    .dividers([false, false, true, false])?;
    Ok(ListPane { lock, items, step })
}

/// One row of the item list: the type icon, the name over the subtitle,
/// and a star for a favorite. A click selects the item, and the next pass
/// opens it.
fn row<A: VaultApp>(app: &mut A, ui: &mut Ui, _: ListId, position: usize) -> Result<()> {
    // A row declares one control even when the vault just went away.
    let Some((form, index)) = app.vault() else {
        ui.pressable("row", |_| Ok(()))?;
        return Ok(());
    };
    let Some(cipher) = form.list.rows.get(position).map(|&row| &index.ciphers[row]) else {
        ui.pressable("row", |_| Ok(()))?;
        return Ok(());
    };
    let name = match cipher.readable {
        true => cipher.name.as_str(),
        false => UNREADABLE_ITEM,
    };
    let mut row = ui.pressable("row", |ui| {
        ui.columns(
            "row",
            &[Length::Auto, Length::Star(1.0), Length::Auto],
            12.0,
            |ui| {
                ui.font_icon("icon", kind_glyph(cipher.kind))?
                    .size(20.0)?
                    .secondary(true)?;
                ui.stack("text", Orientation::Vertical, 0.0, |ui| {
                    ui.label("name", format_args!("{name}"))?
                        .overflow(Overflow::Ellipsis)?
                        .italic(!cipher.readable)?;
                    ui.label("subtitle", format_args!("{}", cipher.subtitle.as_str()))?
                        .overflow(Overflow::Ellipsis)?
                        .secondary(true)?
                        .style("CaptionTextBlockStyle")?;
                    Ok(())
                })?;
                if cipher.favorite {
                    ui.font_icon("favorite", FILLED_STAR)?.size(12.0)?;
                }
                Ok(())
            },
        )?
        .margin([0.0, 8.0, 12.0, 8.0])?;
        Ok(())
    })?;
    row.name(name)?
        .selected(form.selected == Some(cipher.id))?
        .margin([4.0, 2.0, 4.0, 2.0])?;
    if row.clicked()? {
        form.clicked = Some(cipher.id);
    }
    // The menu acts on this row's item and leaves the selection alone.
    if let Some(anchor) = row.context_requested()? {
        form.menus.open(cipher.id, anchor);
        ui.request_pass();
    }
    Ok(())
}

/// What the details pane shows.
struct Details<'a> {
    index: &'a VaultIndex,
    selected: Option<Uuid>,
    open: Option<&'a CipherView>,
    /// The core opens `selected` after this pass, so the pane still shows
    /// `open` for this one pass.
    opening: bool,
    error: Option<&'a ItemError>,
}

/// What the user did in the details pane in a pass.
#[derive(Default)]
struct DetailsPass {
    /// What the command bar asked for.
    asked: Option<UserAction>,
    /// More was clicked: the item and the button to open its menu under.
    more: Option<(Uuid, MenuAnchor)>,
}

/// Draws the selected item's details in a scroll area.
fn details_pane(
    ui: &mut Ui,
    details: Details<'_>,
    revealed: &mut Vec<FieldId>,
) -> Result<DetailsPass> {
    let mut pass = DetailsPass::default();
    // Drawing the item before until the new one opens keeps the pane's
    // controls in place, so the pane does not blank for a pass.
    let shown = match details.open {
        Some(view) if details.opening => view.id,
        _ => details.selected,
    };
    let summary = shown.and_then(|id| details.index.ciphers.iter().find(|cipher| cipher.id == id));
    ui.scroll("details", |ui| {
        ui.stack("details", Orientation::Vertical, 16.0, |ui| {
            if let Some(error) = details.error {
                let (title, message) = item_error(error);
                ui.info_bar("error", Severity::Error, title, message)?;
            }
            let Some(summary) = summary else {
                ui.label("empty", format_args!("Select an item to see its details."))?
                    .secondary(true)?;
                return Ok(());
            };
            if !summary.readable {
                heading(ui, summary.kind, UNREADABLE_ITEM)?;
                ui.info_bar(
                    "unreadable",
                    Severity::Error,
                    "This item didn't decrypt.",
                    "Its key or data may be damaged. Other items are not affected. \
                     Try Sync; if it stays, check it in the web vault.",
                )?;
                return Ok(());
            }
            let Some(view) = details.open.filter(|view| view.id == Some(summary.id)) else {
                return Ok(());
            };
            pass = commands(ui, summary, view)?;
            heading(ui, view.kind, view.name.as_str())?;
            if view.reprompt {
                ui.info_bar(
                    "reprompt",
                    Severity::Informational,
                    "This item asks for your master password before it opens.",
                    "Chaveiro can't ask for it yet, so the item stays closed.",
                )?;
                return Ok(());
            }
            if let Some(field) = fields(ui, view, revealed)? {
                pass.asked = Some(UserAction::Copy {
                    id: summary.id,
                    field,
                });
            }
            footer(ui, details.index, view)
        })?
        .padding([24.0, 16.0, 24.0, 24.0])?;
        Ok(())
    })?;
    Ok(pass)
}

/// The command bar over the details: Favorite, Move to trash and More, or
/// Restore and More for an item in the trash.
fn commands(ui: &mut Ui, summary: &CipherSummary, view: &CipherView) -> Result<DetailsPass> {
    let mut asked = None;
    let mut more = None;
    ui.command_bar("commands", |ui| {
        if summary.deleted {
            if ui
                .app_bar_button("restore", '\u{E777}', "Restore")?
                .clicked()?
            {
                asked = Some(UserAction::Restore(summary.id));
            }
        } else {
            asked = favorite_and_trash(ui, summary, view)?;
        }
        let mut button = ui.app_bar_button("more", '\u{E712}', "More")?;
        if button.clicked()? {
            more = Some((summary.id, button.anchor()));
        }
        Ok(())
    })?;
    Ok(DetailsPass { asked, more })
}

/// Favorite and Move to trash, for an item outside the trash.
fn favorite_and_trash(
    ui: &mut Ui,
    summary: &CipherSummary,
    view: &CipherView,
) -> Result<Option<UserAction>> {
    let mut asked = None;
    let (glyph, label) = match view.favorite {
        true => (FILLED_STAR, "Unfavorite"),
        false => ('\u{E734}', "Favorite"),
    };
    if ui.app_bar_button("favorite", glyph, label)?.clicked()? {
        let mut edited = view.clone();
        edited.favorite = !edited.favorite;
        asked = Some(UserAction::Save(Box::new(edited)));
    }
    if ui
        .app_bar_button("trash", '\u{E74D}', "Move to trash")?
        .clicked()?
    {
        asked = Some(UserAction::Trash(summary.id));
    }
    Ok(asked)
}

/// The entries of the item menu for `summary`, from the design's ItemMenu,
/// with what the core can do today. Sets `picked` to what the user picked.
/// The entries act on the item by id, so it need not be open.
fn item_menu(
    ui: &mut Ui,
    index: &VaultIndex,
    summary: &CipherSummary,
    picked: &mut Option<VaultAction>,
) -> Result<()> {
    let id = summary.id;
    if summary.deleted {
        if ui.menu_item("restore", '\u{E777}', "Restore")?.clicked()? {
            *picked = Some(VaultAction::User(UserAction::Restore(id)));
        }
        return Ok(());
    }
    // An item that did not decrypt offers only Move to trash.
    if summary.readable {
        if let Some(url) = website(&summary.uris) {
            if ui
                .menu_item("website", '\u{E8A7}', "Open website")?
                .clicked()?
            {
                *picked = Some(VaultAction::OpenWebsite(url));
            }
            ui.menu_separator("links")?;
        }
        let mut any = false;
        for &(field, glyph, label, key) in COPIES {
            if !copyable(summary, field) {
                continue;
            }
            any = true;
            let mut item = ui.menu_item(("copy", field as u8), glyph, label)?;
            if let Some((_, hint)) = key {
                item.key_hint(hint)?;
            }
            if item.clicked()? {
                *picked = Some(VaultAction::User(UserAction::Copy { id, field }));
            }
        }
        if any {
            ui.menu_separator("copies")?;
        }
        ui.submenu("folder", '\u{E8B7}', "Move to folder", |ui| {
            // A folder the vault lacks counts as no folder, as in the pane.
            let current = summary
                .folder_id
                .filter(|folder| index.folders.iter().any(|known| known.id == *folder));
            let mut target = None;
            if ui
                .menu_toggle("none", None, "No folder", current.is_none())?
                .clicked()?
            {
                target = Some(None);
            }
            for folder in &index.folders {
                let here = current == Some(folder.id);
                if ui
                    .menu_toggle(folder.id, None, folder_label(folder), here)?
                    .clicked()?
                {
                    target = Some(Some(folder.id));
                }
            }
            if let Some(folder) = target.filter(|folder| *folder != current) {
                *picked = Some(VaultAction::User(UserAction::MoveToFolder { id, folder }));
            }
            Ok(())
        })?;
        let on = summary.reprompt;
        if ui
            .menu_toggle("reprompt", Some('\u{E72E}'), "Ask for master password", on)?
            .clicked()?
        {
            *picked = Some(VaultAction::User(UserAction::SetReprompt { id, on: !on }));
        }
        ui.menu_separator("end")?;
    }
    let mut trash = ui.menu_item("trash", '\u{E74D}', "Move to trash")?;
    trash.critical(true)?;
    if trash.clicked()? {
        *picked = Some(VaultAction::User(UserAction::Trash(id)));
    }
    Ok(())
}

/// The copy entries of the item menu, in order: the field, its glyph, its
/// label, and its Ctrl key with the key's name, as Bitwarden binds them.
const COPIES: &[(CopyField, char, &str, Option<(char, &str)>)] = &[
    (
        CopyField::Username,
        '\u{E77B}',
        "Copy username",
        Some(('U', "Ctrl+U")),
    ),
    (
        CopyField::Password,
        '\u{E8D7}',
        "Copy password",
        Some(('P', "Ctrl+P")),
    ),
    (
        CopyField::Totp,
        '\u{E916}',
        "Copy verification code",
        Some(('T', "Ctrl+T")),
    ),
    (CopyField::Uri, '\u{E71B}', "Copy website", None),
    (CopyField::Notes, '\u{E70B}', "Copy note", None),
];

/// Whether the item menu and keys offer to copy `field` of `summary`:
/// the item holds it, decrypted, outside the trash, and the master password
/// re-prompt, which Chaveiro cannot ask yet, does not guard it.
fn copyable(summary: &CipherSummary, field: CopyField) -> bool {
    summary.readable
        && !summary.deleted
        && summary.copyable.contains(field)
        && !(field.guarded() && summary.reprompt)
}

/// The first of `uris` the browser can open: an `http` or `https` address,
/// with `https://` put in front of one without a scheme, such as
/// `github.com`. Other schemes, such as an app link or `file:`, are left
/// out, because a shared item could name any program.
fn website(uris: &[SecretString]) -> Option<Url> {
    uris.iter().find_map(|uri| {
        let uri = uri.trim();
        let url = Url::parse(uri)
            .ok()
            .filter(Url::has_host)
            .or_else(|| Url::parse(&format!("https://{uri}")).ok())?;
        matches!(url.scheme(), "http" | "https").then_some(url)
    })
}

/// The item's type icon and name, in the subtitle style.
fn heading(ui: &mut Ui, kind: CipherKind, name: &str) -> Result<()> {
    ui.columns("heading", &[Length::Auto, Length::Star(1.0)], 12.0, |ui| {
        ui.font_icon("icon", kind_glyph(kind))?.size(20.0)?;
        ui.label("name", format_args!("{name}"))?
            .overflow(Overflow::Wrap)?
            .style("SubtitleTextBlockStyle")?;
        Ok(())
    })?;
    Ok(())
}

/// The field groups of `view`'s type, from the design's "Details per item
/// type", then its custom fields and notes. Gives the field whose Copy
/// button was clicked.
fn fields(
    ui: &mut Ui,
    view: &CipherView,
    revealed: &mut Vec<FieldId>,
) -> Result<Option<CopyField>> {
    let mut copied = None;
    match &view.content {
        CipherContent::Login(login) => copied = login_fields(ui, login, revealed)?,
        CipherContent::Card(card) => card_fields(ui, card, revealed)?,
        CipherContent::Identity(identity) => identity_fields(ui, identity, revealed)?,
        CipherContent::SshKey(ssh_key) => ssh_key_fields(ui, ssh_key, revealed)?,
        CipherContent::SecureNote => {}
        CipherContent::Unknown => {
            ui.info_bar(
                "unknown",
                Severity::Informational,
                "Chaveiro can't show this type of item yet.",
                "It stays safe in your vault; open it in another app to see or edit it.",
            )?;
        }
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
    group(ui, "custom fields", Some("Custom fields"), custom, revealed)?;
    let caption = match view.kind {
        CipherKind::SecureNote => "Note",
        _ => "Notes",
    };
    let notes = [Field::lines(("notes", 0), caption, text(&view.notes))];
    group(ui, "notes", None, notes.into_iter(), revealed)?;
    Ok(copied)
}

/// Gives [`CopyField::Totp`] when the code's Copy button was clicked.
fn login_fields(
    ui: &mut Ui,
    login: &LoginView,
    revealed: &mut Vec<FieldId>,
) -> Result<Option<CopyField>> {
    // The code changes with the clock, so the pane draws again on each
    // second while it shows.
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let code = login
        .totp
        .as_ref()
        .and_then(|secret| totp::now(secret, clock.as_secs()));
    if code.is_some() {
        let into_second = Duration::from_nanos(u64::from(clock.subsec_nanos()));
        ui.request_pass_after(Duration::from_secs(1) - into_second);
    }
    let account = [
        Field::text(("username", 0), "Username", text(&login.username)),
        Field::secret(("password", 0), "Password", text(&login.password)),
    ]
    .into_iter()
    .chain(code.as_ref().map(|code| Field {
        id: ("totp", 0),
        caption: "Authenticator code",
        value: Value::Code(code),
        kind: Kind::Code {
            seconds_left: code.seconds_left(clock.as_secs()),
        },
    }))
    .chain(
        login
            .fido2_credentials
            .iter()
            .enumerate()
            .map(|(position, passkey)| Field {
                value: Value::Passkey {
                    site: &passkey.rp_id,
                    user: passkey.user_name.as_deref().map(|user| user.as_str()),
                },
                ..Field::text(("passkey", position), "Passkey", None)
            }),
    );
    let copied = group(ui, "account", None, account, revealed)?;
    let websites = login.uris.iter().enumerate().map(|(position, uri)| {
        let caption = match uri.match_rule {
            None => "Website",
            Some(0) => "Website · match: base domain",
            Some(1) => "Website · match: host",
            Some(2) => "Website · match: starts with",
            Some(3) => "Website · match: exact",
            Some(4) => "Website · match: regular expression",
            Some(5) => "Website · match: never",
            Some(_) => "Website",
        };
        Field::text(("website", position), caption, text(&uri.uri))
    });
    group(ui, "websites", None, websites, revealed)?;
    Ok(copied
        .filter(|id| *id == ("totp", 0))
        .map(|_| CopyField::Totp))
}

fn card_fields(ui: &mut Ui, card: &CardView, revealed: &mut Vec<FieldId>) -> Result<()> {
    let mut number = Field::secret(("number", 0), "Number", text(&card.number));
    // The masked number keeps its last 4 digits.
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
    group(ui, "card", None, fields.into_iter(), revealed)?;
    Ok(())
}

fn identity_fields(
    ui: &mut Ui,
    identity: &IdentityView,
    revealed: &mut Vec<FieldId>,
) -> Result<()> {
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
        Field::text(("username", 0), "Username", text(&identity.username)),
        Field::text(("company", 0), "Company", text(&identity.company)),
    ];
    group(
        ui,
        "personal",
        Some("Personal"),
        personal.into_iter(),
        revealed,
    )?;
    let identification = [
        Field::secret(
            ("passport", 0),
            "Passport number",
            text(&identity.passport_number),
        ),
        Field::secret(("ssn", 0), "Tax number", text(&identity.ssn)),
        Field::monospace(
            ("license", 0),
            "Driver's license",
            text(&identity.license_number),
        ),
    ];
    group(
        ui,
        "identification",
        Some("Identification"),
        identification.into_iter(),
        revealed,
    )?;
    let contact = [
        Field::text(("email", 0), "Email", text(&identity.email)),
        Field::text(("phone", 0), "Phone", text(&identity.phone)),
        Field {
            value: Value::Joined(&address, "\n"),
            ..Field::lines(("address", 0), "Address", None)
        },
    ];
    group(
        ui,
        "contact",
        Some("Contact"),
        contact.into_iter(),
        revealed,
    )?;
    Ok(())
}

fn ssh_key_fields(ui: &mut Ui, ssh_key: &SshKeyView, revealed: &mut Vec<FieldId>) -> Result<()> {
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
    group(ui, "ssh key", None, fields.into_iter(), revealed)?;
    ui.label(
        "agent",
        format_args!("The SSH agent offers this key. Chaveiro asks you before each use."),
    )?
    .overflow(Overflow::Wrap)?
    .secondary(true)?;
    Ok(())
}
/// The line under the fields: the folder and when the password last
/// changed.
fn footer(ui: &mut Ui, index: &VaultIndex, view: &CipherView) -> Result<()> {
    let folder = view
        .folder_id
        .and_then(|id| index.folders.iter().find(|folder| folder.id == id))
        .map_or("No folder", folder_label);
    let changed = match &view.content {
        CipherContent::Login(login) => login.password_revision_date.as_deref(),
        _ => None,
    };
    // The dates are ISO 8601; the day is enough here.
    let day = changed.map(|date| date.get(..10).unwrap_or(date));
    let history = view.password_history.len();
    let mut footer = ui.label(
        "footer",
        format_args!(
            "{}{folder}{}{}{}",
            if view.folder_id.is_some() {
                "Folder: "
            } else {
                ""
            },
            day.map_or("", |_| " · Password changed "),
            day.unwrap_or(""),
            match history {
                0 => "",
                _ => " · Has password history",
            },
        ),
    )?;
    footer
        .overflow(Overflow::Wrap)?
        .secondary(true)?
        .style("CaptionTextBlockStyle")?;
    Ok(())
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

/// What a field shows, written straight into its label, so drawing it
/// copies no secret into a buffer of its own.
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
    /// `MM / YYYY`, padding a one-digit month.
    Expiration(&'a str, &'a str),
    /// The parts that are not empty, joined by the separator.
    Joined(&'a [Value<'a>], &'a str),
    Code(&'a totp::Code),
}

impl Value<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Value::Text(text) => text.is_none_or(str::is_empty),
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
            Value::Expiration(month, year) => {
                let pad = if month.len() == 1 { "0" } else { "" };
                write!(formatter, "{pad}{month} / {year}")
            }
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
            Value::Code(code) => write!(formatter, "{code}"),
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
    /// An authenticator code: large digits, the seconds it has left after
    /// the caption, and a Copy button.
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

/// Draws the fields of `fields` that have a value, in a card group under
/// `header`. Draws nothing when none has a value. Gives the field whose
/// Copy button was clicked.
fn group<'a>(
    ui: &mut Ui,
    id: &'static str,
    header: Option<&str>,
    fields: impl Clone + Iterator<Item = Field<'a>>,
    revealed: &mut Vec<FieldId>,
) -> Result<Option<FieldId>> {
    let mut shown = fields.filter(|field| !field.value.is_empty());
    if shown.clone().next().is_none() {
        return Ok(None);
    }
    let mut copied = None;
    ui.stack(id, Orientation::Vertical, 8.0, |ui| {
        if let Some(header) = header {
            ui.label("header", format_args!("{header}"))?
                .style("BodyStrongTextBlockStyle")?;
        }
        ui.card_group("fields", |ui| {
            let mut divider = false;
            for field in shown.by_ref() {
                if field_row(ui, &field, divider, revealed)? {
                    copied = Some(field.id);
                }
                divider = true;
            }
            Ok(())
        })?;
        Ok(())
    })?;
    Ok(copied)
}

/// One row: the caption over the value, a Show button for a secret and a
/// Copy button for a code. Gives whether Copy was clicked.
fn field_row(
    ui: &mut Ui,
    field: &Field<'_>,
    divider: bool,
    revealed: &mut Vec<FieldId>,
) -> Result<bool> {
    let shown = revealed.contains(&field.id);
    let mut copied = false;
    ui.card_row(
        field.id,
        divider,
        &[Length::Star(1.0), Length::Auto],
        |ui| {
            ui.stack("text", Orientation::Vertical, 0.0, |ui| {
                let caption = field.caption;
                let mut label = match field.kind {
                    Kind::Code { seconds_left } => {
                        ui.label("caption", format_args!("{caption} · {seconds_left} s"))?
                    }
                    _ => ui.label("caption", format_args!("{caption}"))?,
                };
                label.secondary(true)?.style("CaptionTextBlockStyle")?;
                let (value, monospace, overflow) = match field.kind {
                    Kind::Text | Kind::Code { .. } => (field.value, false, Overflow::Ellipsis),
                    Kind::Monospace => (field.value, true, Overflow::Ellipsis),
                    Kind::Lines => (field.value, false, Overflow::Wrap),
                    Kind::Secret { .. } if shown => (field.value, true, Overflow::Ellipsis),
                    Kind::Secret { hidden } => (hidden, true, Overflow::Ellipsis),
                };
                let mut label = ui.label("value", format_args!("{value}"))?;
                label.monospace(monospace)?.overflow(overflow)?;
                // The design's 20px digits; Segoe UI's figures are tabular.
                if let Kind::Code { .. } = field.kind {
                    label.style("SubtitleTextBlockStyle")?;
                }
                Ok(())
            })?;
            match field.kind {
                Kind::Secret { .. } => {
                    let (glyph, label) = match shown {
                        true => ('\u{ED1A}', "Hide"),
                        false => ('\u{E890}', "Show"),
                    };
                    if ui.icon_button("reveal", glyph, label)?.clicked()? {
                        match shown {
                            true => revealed.retain(|id| *id != field.id),
                            false => revealed.push(field.id),
                        }
                    }
                }
                Kind::Code { .. } => {
                    copied = ui.icon_button("copy", '\u{E8C8}', "Copy code")?.clicked()?;
                }
                Kind::Text | Kind::Monospace | Kind::Lines => {}
            }
            Ok(())
        },
    )?;
    Ok(copied)
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

/// The title and message of the info bar for `error`.
fn item_error(error: &ItemError) -> (&str, &str) {
    match error {
        ItemError::NotFound => ("This item is gone.", "It is no longer in the vault."),
        ItemError::Unreadable(_) => (
            "This item didn't decrypt.",
            "Its key or data may be damaged. Other items are not affected.",
        ),
        ItemError::Busy => (
            "A change to this item is still saving.",
            "Try again in a moment.",
        ),
        ItemError::Rejected(message) => ("The change was not saved.", message.as_str()),
    }
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

/// The search box placeholder for a filter other than a folder.
fn search_label(filter: Filter) -> &'static str {
    match filter {
        Filter::All => "Search all items",
        Filter::Favorites => "Search favorites",
        Filter::Kind(CipherKind::Login) => "Search logins",
        Filter::Kind(CipherKind::Card) => "Search cards",
        Filter::Kind(CipherKind::Identity) => "Search identities",
        Filter::Kind(CipherKind::SecureNote) => "Search notes",
        Filter::Kind(CipherKind::SshKey) => "Search SSH keys",
        Filter::Kind(CipherKind::Unknown(_)) => unreachable!("the pane lists only KINDS"),
        Filter::Folder(_) => unreachable!("a folder's placeholder has its name"),
        Filter::NoFolder => "Search items with no folder",
        Filter::Trash => "Search trash",
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
        Filter::Kind(kind) => kind_glyph(kind),
        Filter::Folder(_) | Filter::NoFolder => '\u{E8B7}',
        Filter::Trash => '\u{E74D}',
    }
}

/// The Segoe Fluent Icons glyph of an item type, from the design.
fn kind_glyph(kind: CipherKind) -> char {
    match kind {
        CipherKind::Login => '\u{E774}',
        CipherKind::Card => '\u{E8C7}',
        CipherKind::Identity => '\u{E77B}',
        CipherKind::SecureNote => '\u{E8A5}',
        CipherKind::SshKey => '\u{E8D7}',
        CipherKind::Unknown(_) => '\u{E9CE}',
    }
}

fn title(ui: &mut Ui, text: &str) -> Result<()> {
    ui.label("title", format_args!("{text}"))?
        .overflow(Overflow::Ellipsis)?
        .style("SubtitleTextBlockStyle")?;
    Ok(())
}

/// The label of a folder whose name did not decrypt.
const UNREADABLE_FOLDER: &str = "Unreadable folder";
/// The name of an item that did not decrypt, in its row and details.
const UNREADABLE_ITEM: &str = "Can't read this item";
/// A hidden secret.
const DOTS: &str = "••••••••••";
const FILLED_STAR: char = '\u{E735}';

/// The custom field types of [`CustomField::kind`](chaveiro_core::vault::CustomField::kind).
const CUSTOM_HIDDEN: u8 = 1;
const CUSTOM_BOOLEAN: u8 = 2;
const CUSTOM_LINKED: u8 = 3;

/// The search box's widest, from the design.
const SEARCH_WIDTH: f64 = 480.0;
/// The list pane's width, from the design.
const LIST_WIDTH: f64 = 320.0;

/// The window's first size, wide enough for the full pane, which needs
/// 1000px.
const WIDTH: f64 = 1200.0;
const HEIGHT: f64 = 760.0;
