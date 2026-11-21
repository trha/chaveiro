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
    vault::{
        CardView, CipherContent, CipherKind, CipherSummary, CipherView, FolderSummary,
        IdentityView, LoginView, SshKeyView, VaultIndex,
    },
};
use uuid::Uuid;
use windows_core::Result;
use winui_immediate::{
    App, Length, ListId, NavigationEntry, NavigationSelection, Orientation, Overflow, Severity, Ui,
};

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
}

impl Default for VaultForm {
    fn default() -> Self {
        VaultForm {
            filter: Filter::All,
            settings: false,
            sidebar: Sidebar::default(),
            list: ItemList::default(),
            selected: None,
            opened: None,
            clicked: None,
            revealed: Vec::new(),
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
        self.list.shows = None;
        self.list.rows.clear();
        self.selected = None;
        self.opened = None;
        self.clicked = None;
        self.revealed.clear();
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

/// The rows of the item list.
///
/// Contract: `rows` holds the position in [`VaultIndex::ciphers`] of each
/// item the filter of `shows` matches, in index order, for the index of
/// the revision in `shows`; `revision` changes whenever `rows` does.
#[derive(Default)]
struct ItemList {
    /// The index revision and filter `rows` were built for; [`None`]
    /// until the first build and after [`VaultForm::reset`].
    shows: Option<(u64, Filter)>,
    rows: Vec<usize>,
    revision: u64,
}

impl ItemList {
    /// Builds the rows for `filter` over `index`, unless they are built
    /// for that revision and filter already.
    fn refresh(&mut self, index: &VaultIndex, revision: u64, filter: Filter) {
        if self.shows == Some((revision, filter)) {
            return;
        }
        self.rows.clear();
        self.rows.extend(
            index
                .ciphers
                .iter()
                .enumerate()
                .filter(|(_, cipher)| filter.matches(cipher, &index.folders))
                .map(|(position, _)| position),
        );
        self.shows = Some((revision, filter));
        self.revision += 1;
    }
}

/// Names one field of the selected item: a static name, and the position
/// of a website or custom field, or 0.
type FieldId = (&'static str, usize);

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
    let mut action = None;
    // A row clicked last pass: open it, or for an item that cannot be
    // read, close the open one, because its details come from the list.
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
    // The core opened another item, such as a reopen after a sync.
    let opened = open.and_then(|view| view.id);
    if opened != form.opened {
        form.opened = opened;
        if opened.is_some() {
            form.select(opened);
        }
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

    let list = &mut form.list;
    let revealed = &mut form.revealed;
    let selected_item = form.selected;
    let mut window = ui.window("vault", "Chaveiro", |ui| {
        ui.grid("window", &[Length::Auto, Length::Star(1.0)], |ui| {
            ui.title_bar("title bar", "Chaveiro", false)?;
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
                list.refresh(index, revision, filter);
                ui.columns(
                    "panes",
                    &[Length::Pixels(LIST_WIDTH), Length::Star(1.0)],
                    0.0,
                    |ui| {
                        let heading = ListHeading {
                            title: label(filter, index),
                            count: sidebar.counts.get(filter),
                            empty: match filter {
                                Filter::Trash => "Trash is empty.",
                                _ => "No items here yet.",
                            },
                        };
                        if list_pane::<A>(ui, list, heading)? {
                            action = Some(VaultAction::User(UserAction::Lock));
                        }
                        let details = Details {
                            index,
                            selected: selected_item,
                            open,
                            error: screen.error,
                        };
                        if let Some(asked) = details_pane(ui, details, revealed)? {
                            action = Some(VaultAction::User(asked));
                        }
                        Ok(())
                    },
                )?;
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

/// Draws the list pane: the filter's name and count over its items.
/// Returns whether Lock was clicked.
fn list_pane<A: VaultApp>(ui: &mut Ui, list: &ItemList, heading: ListHeading<'_>) -> Result<bool> {
    let mut lock = false;
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
                ui.list("items", list.rows.len(), list.revision, row::<A>)?;
            }
        }
        Ok(())
    })?
    // The line between the list and the details.
    .dividers([false, false, true, false])?;
    Ok(lock)
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
    Ok(())
}

/// What the details pane shows.
struct Details<'a> {
    index: &'a VaultIndex,
    selected: Option<Uuid>,
    open: Option<&'a CipherView>,
    error: Option<&'a ItemError>,
}

/// Draws the selected item's details in a scroll area. Returns what the
/// user asked for with the command bar.
fn details_pane(
    ui: &mut Ui,
    details: Details<'_>,
    revealed: &mut Vec<FieldId>,
) -> Result<Option<UserAction>> {
    let mut asked = None;
    let summary = details
        .selected
        .and_then(|id| details.index.ciphers.iter().find(|cipher| cipher.id == id));
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
            asked = commands(ui, summary, view)?;
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
            fields(ui, view, revealed)?;
            footer(ui, details.index, view)
        })?
        .padding([24.0, 16.0, 24.0, 24.0])?;
        Ok(())
    })?;
    Ok(asked)
}

/// The command bar over the details: Favorite and Move to trash, or
/// Restore for an item in the trash.
fn commands(ui: &mut Ui, summary: &CipherSummary, view: &CipherView) -> Result<Option<UserAction>> {
    let mut asked = None;
    ui.command_bar("commands", |ui| {
        if summary.deleted {
            if ui
                .app_bar_button("restore", '\u{E777}', "Restore")?
                .clicked()?
            {
                asked = Some(UserAction::Restore(summary.id));
            }
            return Ok(());
        }
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
        Ok(())
    })?;
    Ok(asked)
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
/// type", then its custom fields and notes.
fn fields(ui: &mut Ui, view: &CipherView, revealed: &mut Vec<FieldId>) -> Result<()> {
    match &view.content {
        CipherContent::Login(login) => login_fields(ui, login, revealed)?,
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
    let custom: Vec<Field<'_>> = view
        .fields
        .iter()
        .enumerate()
        .map(|(position, field)| {
            let caption = field.name.as_ref().map_or("", |name| name.as_str());
            let value = field.value.as_ref().map(|value| value.as_str());
            match field.kind {
                CUSTOM_HIDDEN => Field::secret(("custom", position), caption, value),
                CUSTOM_BOOLEAN => Field::text(
                    ("custom", position),
                    caption,
                    Some(match value {
                        Some("true") => "Yes",
                        _ => "No",
                    }),
                ),
                CUSTOM_LINKED => match field.linked_id.and_then(|id| linked(view, id)) {
                    Some((value, true)) => Field::secret(("custom", position), caption, value),
                    Some((value, false)) => Field::text(("custom", position), caption, value),
                    None => Field::text(("custom", position), caption, Some("Linked field")),
                },
                _ => Field::text(("custom", position), caption, value),
            }
        })
        .collect();
    group(
        ui,
        "custom fields",
        Some("Custom fields"),
        &custom,
        revealed,
    )?;
    let note = view.notes.as_ref().map(|notes| notes.as_str());
    let caption = match view.kind {
        CipherKind::SecureNote => "Note",
        _ => "Notes",
    };
    group(
        ui,
        "notes",
        None,
        &[Field::lines(("notes", 0), caption, note)],
        revealed,
    )
}

fn login_fields(ui: &mut Ui, login: &LoginView, revealed: &mut Vec<FieldId>) -> Result<()> {
    let passkeys: Vec<SecretString> = login
        .fido2_credentials
        .iter()
        .map(|passkey| {
            let mut shown = SecretString::default();
            shown.push_str(&passkey.rp_id);
            if let Some(user) = &passkey.user_name {
                shown.push_str(" · ");
                shown.push_str(user);
            }
            shown
        })
        .collect();
    let mut account = vec![
        Field::text(("username", 0), "Username", text(&login.username)),
        Field::secret(("password", 0), "Password", text(&login.password)),
    ];
    account.extend(
        passkeys
            .iter()
            .enumerate()
            .map(|(position, shown)| Field::text(("passkey", position), "Passkey", Some(shown))),
    );
    group(ui, "account", None, &account, revealed)?;
    let websites: Vec<Field<'_>> = login
        .uris
        .iter()
        .enumerate()
        .map(|(position, uri)| {
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
        })
        .collect();
    group(ui, "websites", None, &websites, revealed)
}

fn card_fields(ui: &mut Ui, card: &CardView, revealed: &mut Vec<FieldId>) -> Result<()> {
    // The masked number keeps its last 4 digits.
    let masked = card
        .number
        .as_ref()
        .filter(|number| number.len() > 4)
        .and_then(|number| number.get(number.len() - 4..))
        .map(|last| {
            let mut masked = SecretString::new(String::from("•••• •••• •••• "));
            masked.push_str(last);
            masked
        });
    let expiration = match (text(&card.exp_month), text(&card.exp_year)) {
        (None, None) => None,
        (month, year) => {
            let mut shown = SecretString::default();
            let month = month.unwrap_or("");
            if month.len() == 1 {
                shown.push('0');
            }
            shown.push_str(month);
            shown.push_str(" / ");
            shown.push_str(year.unwrap_or(""));
            Some(shown)
        }
    };
    let mut number = Field::secret(("number", 0), "Number", text(&card.number));
    if let Some(masked) = &masked {
        number.kind = Kind::Secret { hidden: masked };
    }
    group(
        ui,
        "card",
        None,
        &[
            Field::text(
                ("cardholder", 0),
                "Cardholder name",
                text(&card.cardholder_name),
            ),
            Field::text(("brand", 0), "Brand", text(&card.brand)),
            number,
            Field::monospace(
                ("expiration", 0),
                "Expiration",
                expiration.as_ref().map(|shown| shown.as_str()),
            ),
            Field::secret(("code", 0), "Security code", text(&card.code)),
        ],
        revealed,
    )
}

fn identity_fields(
    ui: &mut Ui,
    identity: &IdentityView,
    revealed: &mut Vec<FieldId>,
) -> Result<()> {
    let name = joined(
        [
            &identity.title,
            &identity.first_name,
            &identity.middle_name,
            &identity.last_name,
        ],
        " ",
    );
    let place = joined(
        [&identity.city, &identity.state, &identity.postal_code],
        " ",
    );
    let mut address = SecretString::default();
    let lines = [
        text(&identity.address1),
        text(&identity.address2),
        text(&identity.address3),
        place.as_ref().map(|place| place.as_str()),
        text(&identity.country),
    ];
    for line in lines.into_iter().flatten().filter(|line| !line.is_empty()) {
        if !address.is_empty() {
            address.push('\n');
        }
        address.push_str(line);
    }
    group(
        ui,
        "personal",
        Some("Personal"),
        &[
            Field::text(("name", 0), "Name", name.as_ref().map(|name| name.as_str())),
            Field::text(("username", 0), "Username", text(&identity.username)),
            Field::text(("company", 0), "Company", text(&identity.company)),
        ],
        revealed,
    )?;
    group(
        ui,
        "identification",
        Some("Identification"),
        &[
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
        ],
        revealed,
    )?;
    group(
        ui,
        "contact",
        Some("Contact"),
        &[
            Field::text(("email", 0), "Email", text(&identity.email)),
            Field::text(("phone", 0), "Phone", text(&identity.phone)),
            Field::lines(("address", 0), "Address", Some(&address)),
        ],
        revealed,
    )
}

fn ssh_key_fields(ui: &mut Ui, ssh_key: &SshKeyView, revealed: &mut Vec<FieldId>) -> Result<()> {
    group(
        ui,
        "ssh key",
        None,
        &[
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
        ],
        revealed,
    )?;
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
struct Field<'a> {
    id: FieldId,
    caption: &'a str,
    /// [`None`] or empty for a field the item leaves empty, which is
    /// hidden.
    value: Option<&'a str>,
    kind: Kind<'a>,
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
        hidden: &'a str,
    },
}

impl<'a> Field<'a> {
    fn text(id: FieldId, caption: &'a str, value: Option<&'a str>) -> Self {
        Field {
            id,
            caption,
            value,
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
            kind: Kind::Secret { hidden: DOTS },
            ..Field::text(id, caption, value)
        }
    }
}

/// Draws the fields of `fields` that have a value, in a card group under
/// `header`. Draws nothing when none has a value.
fn group(
    ui: &mut Ui,
    id: &'static str,
    header: Option<&str>,
    fields: &[Field<'_>],
    revealed: &mut Vec<FieldId>,
) -> Result<()> {
    let has_value = |field: &&Field<'_>| field.value.is_some_and(|value| !value.is_empty());
    if !fields.iter().any(|field| has_value(&field)) {
        return Ok(());
    }
    ui.stack(id, Orientation::Vertical, 8.0, |ui| {
        if let Some(header) = header {
            ui.label("header", format_args!("{header}"))?
                .style("BodyStrongTextBlockStyle")?;
        }
        ui.card_group("fields", |ui| {
            for (position, field) in fields.iter().filter(has_value).enumerate() {
                field_row(ui, field, position > 0, revealed)?;
            }
            Ok(())
        })?;
        Ok(())
    })?;
    Ok(())
}

/// One row: the caption over the value, and a Show button for a secret.
fn field_row(
    ui: &mut Ui,
    field: &Field<'_>,
    divider: bool,
    revealed: &mut Vec<FieldId>,
) -> Result<()> {
    let value = field.value.unwrap_or("");
    let shown = revealed.contains(&field.id);
    ui.card_row(
        field.id,
        divider,
        &[Length::Star(1.0), Length::Auto],
        |ui| {
            ui.stack("text", Orientation::Vertical, 0.0, |ui| {
                ui.label("caption", format_args!("{}", field.caption))?
                    .secondary(true)?
                    .style("CaptionTextBlockStyle")?;
                let (text, monospace, overflow) = match field.kind {
                    Kind::Text => (value, false, Overflow::Ellipsis),
                    Kind::Monospace => (value, true, Overflow::Ellipsis),
                    Kind::Lines => (value, false, Overflow::Wrap),
                    Kind::Secret { .. } if shown => (value, true, Overflow::Ellipsis),
                    Kind::Secret { hidden } => (hidden, true, Overflow::Ellipsis),
                };
                ui.label("value", format_args!("{text}"))?
                    .monospace(monospace)?
                    .overflow(overflow)?;
                Ok(())
            })?;
            if let Kind::Secret { .. } = field.kind {
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
            Ok(())
        },
    )?;
    Ok(())
}

fn text(secret: &Option<SecretString>) -> Option<&str> {
    secret.as_ref().map(|secret| secret.as_str())
}

/// The parts of `parts` that have text, joined by `separator`; [`None`]
/// when none has.
fn joined<const N: usize>(
    parts: [&Option<SecretString>; N],
    separator: &str,
) -> Option<SecretString> {
    let mut text = SecretString::default();
    for part in parts.into_iter().flatten().filter(|part| !part.is_empty()) {
        if !text.is_empty() {
            text.push_str(separator);
        }
        text.push_str(part);
    }
    (!text.is_empty()).then_some(text)
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

/// The list pane's width, from the design.
const LIST_WIDTH: f64 = 320.0;

/// The window's first size, wide enough for the full pane, which needs
/// 1000px.
const WIDTH: f64 = 1200.0;
const HEIGHT: f64 = 760.0;
