//! The ItemEdit form from the design, for a new item: it takes the details
//! pane's place until the item saves or the user cancels.
//!
//! Only logins and secure notes have a form so far. The form keeps no copy
//! of the password: it stays in its box until Save reads it.

use chaveiro_core::{
    core::ItemError,
    crypto::SecretString,
    filter::Filter,
    vault::{CipherContent, CipherKind, CipherView, LoginView, UriView, VaultIndex},
};
use windows_core::Result;
use winui_immediate::{Align, Length, Orientation, Severity, Ui};
use zeroize::Zeroizing;

/// A new item the user is filling in.
pub struct Draft {
    kind: CipherKind,
    name: SecretString,
    username: SecretString,
    totp: SecretString,
    website: SecretString,
    notes: SecretString,
    /// 0 for no folder, else 1 + the folder's position in
    /// [`VaultIndex::folders`].
    folder: usize,
    favorite: bool,
    reprompt: bool,
    /// The folder and favorite the draft started with, which count as no
    /// change.
    start: (usize, bool),
    /// The user typed in the password box, which holds the only copy.
    password_typed: bool,
    /// Save was clicked with an empty name.
    name_missing: bool,
    /// Save sent the item, and the form waits for the core to say how it
    /// went.
    saving: bool,
}

impl Draft {
    /// An empty item of `kind`, which starts in the folder `filter` shows,
    /// or as a favorite from Favorites, as the design's New says.
    pub fn new(kind: CipherKind, filter: Filter, index: &VaultIndex) -> Self {
        let folder = match filter {
            Filter::Folder(id) => index
                .folders
                .iter()
                .position(|folder| folder.id == id)
                .map_or(0, |position| position + 1),
            _ => 0,
        };
        // Room up front, so typing never grows a buffer: a grown buffer
        // frees the old one unwiped, and growing allocates on a keystroke.
        let field = || SecretString::new(String::with_capacity(FIELD_CAPACITY));
        Draft {
            kind,
            name: field(),
            username: field(),
            totp: field(),
            website: field(),
            notes: SecretString::new(String::with_capacity(NOTES_CAPACITY)),
            folder,
            favorite: filter == Filter::Favorites,
            reprompt: false,
            start: (folder, filter == Filter::Favorites),
            password_typed: false,
            name_missing: false,
            saving: false,
        }
    }

    /// Whether the user entered anything, so leaving the form would lose it.
    pub fn changed(&self) -> bool {
        [
            &self.name,
            &self.username,
            &self.totp,
            &self.website,
            &self.notes,
        ]
        .iter()
        .any(|text| !text.is_empty())
            || self.password_typed
            || self.reprompt
            || (self.folder, self.favorite) != self.start
    }

    /// The title of the dialog that asks before the draft is dropped.
    pub fn discard_title(&self) -> String {
        match (self.name.trim(), self.kind) {
            ("", CipherKind::Login) => "Discard the new login?".to_owned(),
            ("", _) => "Discard the new note?".to_owned(),
            (name, _) => format!("Discard changes to {name}?"),
        }
    }

    /// The item as the core takes it, with `password` from its box.
    fn view(&self, index: &VaultIndex, password: SecretString) -> CipherView {
        let some = |text: &SecretString| match text.trim().is_empty() {
            true => None,
            false => Some(Zeroizing::new(text.trim().to_owned())),
        };
        let content = match self.kind {
            CipherKind::Login => CipherContent::Login(LoginView {
                username: some(&self.username),
                password: some(&password),
                totp: some(&self.totp),
                uris: some(&self.website)
                    .map(|uri| UriView {
                        uri: Some(uri),
                        match_rule: None,
                    })
                    .into_iter()
                    .collect(),
                ..LoginView::default()
            }),
            _ => CipherContent::SecureNote,
        };
        CipherView {
            id: None,
            organization_id: None,
            collection_ids: Vec::new(),
            kind: self.kind,
            name: Zeroizing::new(self.name.trim().to_owned()),
            // A note keeps its spaces and blank lines as typed.
            notes: (!self.notes.is_empty()).then(|| self.notes.clone()),
            folder_id: self
                .folder
                .checked_sub(1)
                .and_then(|position| index.folders.get(position))
                .map(|folder| folder.id),
            favorite: self.favorite,
            reprompt: self.reprompt,
            content,
            fields: Vec::new(),
            password_history: Vec::new(),
        }
    }
}

/// Bytes reserved for each one-line field of a [`Draft`]; longer text still
/// works, but its buffer grows.
const FIELD_CAPACITY: usize = 512;
/// Bytes reserved for a [`Draft`]'s notes.
const NOTES_CAPACITY: usize = 8 * 1024;

/// What the user did in the form in a pass.
pub enum EditPass {
    /// Nothing that leaves the form.
    Stay,
    /// Save the item.
    Save(Box<CipherView>),
    /// Close the form: the user cancelled, or the item saved and the core
    /// opened it.
    Close,
}

/// The kinds that have a form. The New menu offers the others disabled.
pub fn has_form(kind: CipherKind) -> bool {
    matches!(kind, CipherKind::Login | CipherKind::SecureNote)
}

/// Draws the form for `draft` in the details pane. `creating` and `error`
/// are the core's, from the vault screen. Ctrl+S saves.
pub fn edit_pane(
    ui: &mut Ui,
    draft: &mut Draft,
    index: &VaultIndex,
    creating: bool,
    error: Option<&ItemError>,
) -> Result<EditPass> {
    // The save went out on an earlier pass; once the core no longer waits
    // for it, it either opened the new item or set an error.
    if draft.saving && !creating {
        draft.saving = false;
        if error.is_none() {
            return Ok(EditPass::Close);
        }
    }
    let saving = draft.saving;
    let mut pass = EditPass::Stay;
    let mut save = false;
    let mut password = None;
    // Save and Cancel sit in a row above the scroll area, so they stay in
    // view however far the form scrolls.
    ui.grid("edit", &[Length::Auto, Length::Star(1.0)], |ui| {
        // Save, the one accent action, then the command bar with the rest.
        // Save sits beside the bar, not in it, so it never moves into the
        // bar's overflow menu, where WinUI draws it as a stretched block. A
        // new item has nothing to trash, so the bar has no More.
        ui.columns("commands", &[Length::Auto, Length::Star(1.0)], 0.0, |ui| {
            let label = if saving { "Saving…" } else { "Save" };
            let mut button = ui.button("save", label)?;
            button
                .accent(true)?
                .icon('\u{E74E}')?
                .tooltip("Save (Ctrl+S)")?
                .busy(saving)?;
            // Centered on the bar's buttons, with the bar's spacing.
            button
                .valign(Align::Center)?
                .margin([4.0, 0.0, 4.0, 0.0])?;
            save |= button.clicked()?;
            ui.command_bar("commands", |ui| {
                let mut cancel = ui.app_bar_button("cancel", '\u{E711}', "Cancel")?;
                cancel.enabled(!saving)?;
                if cancel.clicked()? {
                    pass = EditPass::Close;
                }
                Ok(())
            })?;
            Ok(())
        })?
        .margin([24.0, 16.0, 24.0, 12.0])?;
        save |= ui.shortcut("save shortcut", false, 'S')? && !saving;
        ui.scroll("fields", |ui| {
            ui.stack("fields", Orientation::Vertical, 12.0, |ui| {
                if let Some(error) = error.filter(|_| !saving) {
                    let message = match error {
                        ItemError::Rejected(message) => message.as_str(),
                        ItemError::Busy => "The last save is still going. Try again in a moment.",
                        ItemError::NotFound | ItemError::Unreadable(_) => {
                            "Chaveiro couldn't save the item."
                        }
                    };
                    ui.info_bar("error", Severity::Error, "The item was not saved.", message)?;
                }
                let title = match draft.kind {
                    CipherKind::Login => "New login",
                    _ => "New note",
                };
                ui.label("title", format_args!("{title}"))?
                    .style("SubtitleTextBlockStyle")?;

                let mut name = ui.text_box("name", "", &mut draft.name)?;
                if name.changed() {
                    draft.name_missing = false;
                }
                name.header("Name")?
                    .required(true)?
                    .error(draft.name_missing.then_some("Enter a name"))?
                    .enabled(!saving)?;
                // Named, not collected, so a pass allocates nothing for them.
                let folders =
                    std::iter::once("No folder").chain(index.folders.iter().map(|folder| {
                        match folder.readable {
                            true => folder.name.as_str(),
                            false => "Unreadable folder",
                        }
                    }));
                ui.combo_box("folder", folders, &mut draft.folder)?
                    .header("Folder")?
                    .enabled(!saving)?;
                ui.check_box("favorite", "Favorite", &mut draft.favorite)?
                    .enabled(!saving)?;

                if draft.kind == CipherKind::Login {
                    ui.text_box("username", "", &mut draft.username)?
                        .header("Username")?
                        .enabled(!saving)?;
                    let mut field = ui.password_box("password", "")?;
                    draft.password_typed |= field.changed()?;
                    field.header("Password")?.enabled(!saving)?;
                    password = Some(field.handle());
                    ui.text_box("totp", "Setup key or otpauth:// link", &mut draft.totp)?
                        .header("Authenticator key")?
                        .enabled(!saving)?;
                    ui.text_box("website", "https://", &mut draft.website)?
                        .header("Website")?
                        .enabled(!saving)?;
                }
                ui.text_area("notes", "", &mut draft.notes)?
                    .header("Notes")?
                    .enabled(!saving)?;
                ui.check_box(
                    "reprompt",
                    "Ask for master password before this item opens",
                    &mut draft.reprompt,
                )?
                .enabled(!saving)?;
                Ok(())
            })?
            .padding([24.0, 16.0, 24.0, 24.0])?;
            Ok(())
        })?;
        Ok(())
    })?;
    if save && !saving {
        match draft.name.trim().is_empty() {
            true => {
                draft.name_missing = true;
                ui.request_pass();
            }
            false => {
                let password = match password {
                    Some(handle) => ui.take_password(handle)?,
                    None => SecretString::default(),
                };
                draft.saving = true;
                pass = EditPass::Save(Box::new(draft.view(index, password)));
            }
        }
    }
    Ok(pass)
}

/// The kind the New button's main part creates for `filter`: the filter's
/// own kind when it has a form, else a login.
pub fn default_kind(filter: Filter) -> CipherKind {
    match filter {
        Filter::Kind(kind) if has_form(kind) => kind,
        _ => CipherKind::Login,
    }
}
