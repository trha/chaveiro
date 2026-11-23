//! The ItemEdit form from the design, for a new item: it takes the details
//! pane's place until the item saves or the user cancels.
//!
//! The form keeps no copy of a secret the user types (password, card
//! security code, passport and tax numbers): each stays in its hidden box
//! until Save reads it. A new SSH key is generated when the form opens, so
//! the form holds that private key.

use chaveiro_core::{
    core::ItemError,
    crypto::SecretString,
    filter::Filter,
    vault::{
        CardView, CipherContent, CipherKind, CipherView, IdentityView, LoginView, SshKeyError,
        SshKeyView, UriView, VaultIndex,
    },
};
use rand_core::OsRng;
use windows_core::Result;
use winui_immediate::{Align, Length, Mask, Orientation, Overflow, PasswordHandle, Severity, Ui};
use zeroize::Zeroizing;

/// A new item the user is filling in.
pub struct Draft {
    name: SecretString,
    notes: SecretString,
    /// 0 for no folder, else 1 + the folder's position in
    /// [`VaultIndex::folders`].
    folder: usize,
    favorite: bool,
    reprompt: bool,
    /// The folder and favorite the draft started with, which count as no
    /// change.
    start: (usize, bool),
    fields: Fields,
    /// The user typed in a hidden box, which holds the only copy.
    secret_typed: bool,
    /// Save was clicked with an empty name.
    name_missing: bool,
    /// Save sent the item, and the form waits for the core to say how it
    /// went.
    saving: bool,
}

/// The fields of the item's kind, besides the hidden boxes.
enum Fields {
    Login {
        username: SecretString,
        totp: SecretString,
        website: SecretString,
    },
    Note,
    Card {
        cardholder: SecretString,
        /// A position in [`BRANDS`].
        brand: usize,
        // A plain box, so the user can check the digits against the card.
        number: SecretString,
        /// As typed; [`expiration`] reads it.
        expiration: SecretString,
        /// Save was clicked with an expiration it cannot read.
        expiration_invalid: bool,
    },
    Identity(Box<IdentityDraft>),
    SshKey(Box<SshDraft>),
}

/// The SSH key of a new item: generated when the form opens, or one the
/// user enters or imports.
///
/// Contract: `key` is the key that saves. For a generated key it was
/// made here; otherwise it is `text` read as a key, and `problem` says why
/// it is [`None`].
struct SshDraft {
    input: KeyInput,
    /// The private key as entered, or as the imported file holds it, which
    /// may have a passphrase. Empty for a generated key.
    text: SecretString,
    key: Option<SshKeyView>,
    problem: Option<KeyProblem>,
    /// Text the app should read for the form, asked for once.
    asked: Option<KeySource>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyInput {
    Generated,
    Entered,
    Imported,
}

/// Why an SSH form has no key to save.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyProblem {
    /// Save was clicked with no key.
    Missing,
    Key(SshKeyError),
    File(KeyTextError),
}

/// Where the app reads text for an SSH key form; see [`EditPass::ReadKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// The clipboard. With `only_key`, text that is not a private key is
    /// dropped, as when the form fills itself on Enter key.
    Clipboard { only_key: bool },
    /// A file the user picks.
    File,
}

/// Why the app read no text for a [`KeySource`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyTextError {
    /// The user closed the file picker.
    Cancelled,
    /// The file did not open, or is too large to be a key.
    Unreadable,
    /// The file is not text.
    NotText,
    /// The clipboard holds no text, or another app has it open.
    NoText,
}

struct IdentityDraft {
    /// A position in [`TITLES`].
    title: usize,
    first_name: SecretString,
    middle_name: SecretString,
    last_name: SecretString,
    username: SecretString,
    company: SecretString,
    license_number: SecretString,
    email: SecretString,
    phone: SecretString,
    address1: SecretString,
    address2: SecretString,
    address3: SecretString,
    city: SecretString,
    state: SecretString,
    postal_code: SecretString,
    country: SecretString,
}

/// Card brands as the ComboBox shows them, and as Bitwarden stores them.
/// The first is no brand.
const BRANDS: [(&str, &str); 11] = [
    ("No brand", ""),
    ("Visa", "Visa"),
    ("Mastercard", "Mastercard"),
    ("American Express", "Amex"),
    ("Discover", "Discover"),
    ("Diners Club", "Diners Club"),
    ("JCB", "JCB"),
    ("Maestro", "Maestro"),
    ("UnionPay", "UnionPay"),
    ("RuPay", "RuPay"),
    ("Other", "Other"),
];
/// Identity titles; the first is none.
const TITLES: [&str; 6] = ["No title", "Mr", "Mrs", "Ms", "Mx", "Dr"];

/// Room up front, so typing never grows a buffer: a grown buffer frees the
/// old one unwiped, and growing allocates on a keystroke.
fn field() -> SecretString {
    SecretString::new(String::with_capacity(FIELD_CAPACITY))
}

impl Draft {
    /// An empty item of `kind`, which starts in the folder `filter` shows,
    /// or as a favorite from Favorites, as the design's New says. An SSH key
    /// item starts with a new Ed25519 key pair.
    pub fn new(kind: CipherKind, filter: Filter, index: &VaultIndex) -> Self {
        let folder = match filter {
            Filter::Folder(id) => index
                .folders
                .iter()
                .position(|folder| folder.id == id)
                .map_or(0, |position| position + 1),
            _ => 0,
        };
        Draft {
            name: field(),
            notes: SecretString::new(String::with_capacity(NOTES_CAPACITY)),
            folder,
            favorite: filter == Filter::Favorites,
            reprompt: false,
            start: (folder, filter == Filter::Favorites),
            fields: match kind {
                CipherKind::Login => Fields::Login {
                    username: field(),
                    totp: field(),
                    website: field(),
                },
                CipherKind::Card => Fields::Card {
                    cardholder: field(),
                    brand: 0,
                    number: field(),
                    expiration: field(),
                    expiration_invalid: false,
                },
                CipherKind::Identity => Fields::Identity(Box::new(IdentityDraft {
                    title: 0,
                    first_name: field(),
                    middle_name: field(),
                    last_name: field(),
                    username: field(),
                    company: field(),
                    license_number: field(),
                    email: field(),
                    phone: field(),
                    address1: field(),
                    address2: field(),
                    address3: field(),
                    city: field(),
                    state: field(),
                    postal_code: field(),
                    country: field(),
                })),
                CipherKind::SshKey => Fields::SshKey(Box::new(SshDraft {
                    input: KeyInput::Generated,
                    text: SecretString::new(String::with_capacity(KEY_CAPACITY)),
                    key: Some(SshKeyView::generate_ed25519(&mut OsRng)),
                    problem: None,
                    asked: None,
                })),
                CipherKind::SecureNote | CipherKind::Unknown(_) => Fields::Note,
            },
            secret_typed: false,
            name_missing: false,
            saving: false,
        }
    }

    fn kind(&self) -> CipherKind {
        match self.fields {
            Fields::Login { .. } => CipherKind::Login,
            Fields::Note => CipherKind::SecureNote,
            Fields::Card { .. } => CipherKind::Card,
            Fields::Identity(_) => CipherKind::Identity,
            Fields::SshKey(_) => CipherKind::SshKey,
        }
    }

    /// Whether the user entered anything, so leaving the form would lose it.
    /// The generated SSH key alone is no change.
    pub fn changed(&self) -> bool {
        let typed = |texts: &[&SecretString]| texts.iter().any(|text| !text.is_empty());
        let fields = match &self.fields {
            Fields::Login {
                username,
                totp,
                website,
            } => typed(&[username, totp, website]),
            Fields::Note => false,
            Fields::SshKey(ssh) => !ssh.text.is_empty(),
            Fields::Card {
                cardholder,
                brand,
                number,
                expiration,
                ..
            } => typed(&[cardholder, number, expiration]) || *brand != 0,
            Fields::Identity(identity) => identity.title != 0 || typed(&identity.texts()),
        };
        fields
            || typed(&[&self.name, &self.notes])
            || self.secret_typed
            || self.reprompt
            || (self.folder, self.favorite) != self.start
    }

    /// The title of the dialog that asks before the draft is dropped.
    pub fn discard_title(&self) -> String {
        match self.name.trim() {
            "" => format!("Discard the new {}?", noun(self.kind())),
            name => format!("Discard changes to {name}?"),
        }
    }

    /// The item as the core takes it, with `secrets` read from the hidden
    /// boxes in the order [`edit_pane`] declares them.
    fn view(&self, index: &VaultIndex, secrets: [SecretString; 2]) -> CipherView {
        let [first, second] = secrets;
        let content = match &self.fields {
            Fields::Login {
                username,
                totp,
                website,
            } => CipherContent::Login(LoginView {
                username: some(username),
                password: some(&first),
                totp: some(totp),
                uris: some(website)
                    .map(|uri| UriView {
                        uri: Some(uri),
                        match_rule: None,
                    })
                    .into_iter()
                    .collect(),
                ..LoginView::default()
            }),
            Fields::Note => CipherContent::SecureNote,
            Fields::Card {
                cardholder,
                brand,
                number,
                expiration: typed,
                ..
            } => {
                let (exp_month, exp_year) = expiration(typed)
                    .flatten()
                    .map(|(month, year)| {
                        (
                            Zeroizing::new(month.to_string()),
                            Zeroizing::new(year.to_string()),
                        )
                    })
                    .unzip();
                CipherContent::Card(CardView {
                    cardholder_name: some(cardholder),
                    brand: (*brand != 0).then(|| Zeroizing::new(BRANDS[*brand].1.to_owned())),
                    number: some(number),
                    exp_month,
                    exp_year,
                    code: some(&first),
                })
            }
            Fields::Identity(identity) => CipherContent::Identity(Box::new(IdentityView {
                title: (identity.title != 0)
                    .then(|| Zeroizing::new(TITLES[identity.title].to_owned())),
                first_name: some(&identity.first_name),
                middle_name: some(&identity.middle_name),
                last_name: some(&identity.last_name),
                address1: some(&identity.address1),
                address2: some(&identity.address2),
                address3: some(&identity.address3),
                city: some(&identity.city),
                state: some(&identity.state),
                postal_code: some(&identity.postal_code),
                country: some(&identity.country),
                company: some(&identity.company),
                email: some(&identity.email),
                phone: some(&identity.phone),
                ssn: some(&second),
                username: some(&identity.username),
                passport_number: some(&first),
                license_number: some(&identity.license_number),
            })),
            Fields::SshKey(ssh) => CipherContent::SshKey(
                ssh.key
                    .clone()
                    .expect("Save checks that the form has a key"),
            ),
        };

        CipherView {
            id: None,
            organization_id: None,
            collection_ids: Vec::new(),
            kind: self.kind(),
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

impl IdentityDraft {
    fn texts(&self) -> [&SecretString; 15] {
        [
            &self.first_name,
            &self.middle_name,
            &self.last_name,
            &self.username,
            &self.company,
            &self.license_number,
            &self.email,
            &self.phone,
            &self.address1,
            &self.address2,
            &self.address3,
            &self.city,
            &self.state,
            &self.postal_code,
            &self.country,
        ]
    }
}

/// `text` trimmed, or [`None`] when that is empty.
fn some(text: &SecretString) -> Option<SecretString> {
    match text.trim() {
        "" => None,
        trimmed => Some(Zeroizing::new(trimmed.to_owned())),
    }
}

/// The month and four-digit year of a card expiration typed as `MM/YY`,
/// `MM/YYYY`, `MMYY` or with another separator. `Some(None)` for empty text,
/// [`None`] for text it cannot read.
fn expiration(text: &str) -> Option<Option<(u8, u16)>> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let groups: Vec<&str> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|group| !group.is_empty())
        .collect();

    let (month, year) = match groups.as_slice() {
        [] => return Some(None),
        [month, year] => (*month, *year),
        // One run of digits: the first two are the month.
        [_] if digits.len() == 4 || digits.len() == 6 => digits.split_at(2),
        _ => return None,
    };

    let month: u8 = month
        .parse()
        .ok()
        .filter(|month| (1..=12).contains(month))?;
    let year: u16 = match year.len() {
        2 => 2000 + year.parse::<u16>().ok()?,
        4 => year.parse().ok()?,
        _ => return None,
    };
    Some(Some((month, year)))
}

/// The kind in a sentence: "login", "SSH key".
fn noun(kind: CipherKind) -> &'static str {
    match kind {
        CipherKind::Login => "login",
        CipherKind::Card => "card",
        CipherKind::Identity => "identity",
        CipherKind::SshKey => "SSH key",
        CipherKind::SecureNote | CipherKind::Unknown(_) => "note",
    }
}

/// Bytes reserved for each one-line field of a [`Draft`]; longer text still
/// works, but its buffer grows.
const FIELD_CAPACITY: usize = 512;
/// Bytes reserved for a [`Draft`]'s notes.
const NOTES_CAPACITY: usize = 8 * 1024;
/// The height of a text box's header and the gap under it, in pixels, so a
/// control drawn over the box clears the header.
const HEADER_HEIGHT: f64 = 28.0;
/// Bytes reserved for an entered SSH private key, which fits an RSA 4096
/// key.
const KEY_CAPACITY: usize = 4 * 1024;

/// What the user did in the form in a pass.
pub enum EditPass {
    /// Nothing that leaves the form.
    Stay,
    /// Save the item.
    Save(Box<CipherView>),
    /// Close the form: the user cancelled, or the item saved and the core
    /// opened it.
    Close,
    /// Read text for the SSH key, and give it to [`Draft::key_read`].
    ReadKey(KeySource),
}

impl Draft {
    /// Takes the text the app read for [`EditPass::ReadKey`] from `source`.
    /// A draft of another kind ignores it.
    pub fn key_read(
        &mut self,
        source: KeySource,
        text: std::result::Result<SecretString, KeyTextError>,
    ) {
        let Fields::SshKey(ssh) = &mut self.fields else {
            return;
        };
        match (source, text) {
            // The user closed the picker, so the form stays as it was.
            (KeySource::File, Err(KeyTextError::Cancelled)) => {}
            (KeySource::File, Err(error)) => {
                ssh.problem = Some(KeyProblem::File(error));
            }
            (KeySource::File, Ok(text)) => {
                ssh.input = KeyInput::Imported;
                ssh.text = text;
                ssh.reread(None);
            }
            // Only a private key, unlocked or not, fills the box.
            (KeySource::Clipboard { only_key: true }, Ok(text))
                if matches!(
                    SshKeyView::from_openssh(&text, None),
                    Err(SshKeyError::NotAKey)
                ) => {}
            (KeySource::Clipboard { only_key: true }, Err(_)) => {}
            (KeySource::Clipboard { .. }, Ok(text)) => {
                ssh.text.clear();
                ssh.text.push_str(text.trim());
                ssh.reread(None);
            }
            (KeySource::Clipboard { only_key: false }, Err(error)) => {
                ssh.problem = Some(KeyProblem::File(error));
            }
        }
    }
}

impl SshDraft {
    /// Reads `text` as the key, unlocked with `passphrase`. Empty text is no
    /// key and no problem yet.
    fn reread(&mut self, passphrase: Option<&str>) {
        (self.key, self.problem) = match self.text.trim() {
            "" => (None, None),
            text => match SshKeyView::from_openssh(text, passphrase) {
                Ok(key) => (Some(key), None),
                Err(error) => (None, Some(KeyProblem::Key(error))),
            },
        };
    }
}

impl KeyProblem {
    fn message(self) -> &'static str {
        match self {
            KeyProblem::Missing => "Enter or import a private key.",
            KeyProblem::Key(SshKeyError::NotAKey) => "This is not an OpenSSH private key.",
            KeyProblem::Key(SshKeyError::NeedsPassphrase) => "The key has a passphrase.",
            KeyProblem::Key(SshKeyError::WrongPassphrase) => "Wrong passphrase.",
            KeyProblem::File(KeyTextError::Unreadable) => "Chaveiro couldn't read the file.",
            KeyProblem::File(KeyTextError::NotText) => "The file is not a text key file.",
            KeyProblem::File(KeyTextError::NoText) => "The clipboard has no text.",
            KeyProblem::File(KeyTextError::Cancelled) => {
                unreachable!("a closed picker leaves the form as it was")
            }
        }
    }
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

    // The hidden boxes of this kind, in the order `Draft::view` reads them.
    let mut secrets: [Option<PasswordHandle>; 2] = [None, None];
    // The boxes a failed Save can focus, in form order: the name, the card
    // expiration, and the SSH key's box or its passphrase box.
    let mut name_field = None;
    let mut expiration_field = None;
    let mut key_field = None;

    // Save and Cancel sit in a row above the scroll area, so they stay in
    // view however far the form scrolls.
    ui.grid("edit", &[Length::Auto, Length::Star(1.0)], |ui| {
        // Save, the one accent action, then the command bar with the rest.
        // Save sits beside the bar, not in it, so it never moves into the
        // bar's overflow menu, where WinUI draws it as a stretched block. A
        // new item has nothing to trash, so the bar has no More.
        ui.columns("commands", &[Length::Auto, Length::Star(1.0)], 0.0, |ui| {
            let mut button = ui.button("save", if saving { "Saving…" } else { "Save" })?;
            button
                .accent(true)?
                .icon('\u{E74E}')?
                .tooltip("Save (Ctrl+S)")?
                .busy(saving)?;

            // Centered on the bar's buttons, with the bar's spacing.
            button.valign(Align::Center)?.margin([4.0, 0.0, 4.0, 0.0])?;
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

                ui.label("form title", format_args!("New {}", noun(draft.kind())))?
                    .style("SubtitleTextBlockStyle")?;

                let mut name = ui.text_box("name", "", &mut draft.name)?;
                name_field = Some(name.field());
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

                let mut form = Form {
                    saving,
                    secret_typed: &mut draft.secret_typed,
                };
                match &mut draft.fields {
                    Fields::Login {
                        username,
                        totp,
                        website,
                    } => {
                        heading(ui, "login", "Login")?;
                        form.text(ui, "username", "Username", "", username)?;
                        secrets[0] = Some(form.secret(ui, "password", "Password")?);
                        form.text(
                            ui,
                            "totp",
                            "Authenticator key",
                            "Setup key or otpauth:// link",
                            totp,
                        )?;
                        form.text(ui, "website", "Website", "https://", website)?;
                    }
                    Fields::Note => {}
                    Fields::Card {
                        cardholder,
                        brand,
                        number,
                        expiration,
                        expiration_invalid,
                    } => {
                        heading(ui, "card", "Card")?;
                        form.text(ui, "cardholder", "Cardholder name", "", cardholder)?;
                        let brands = BRANDS.iter().map(|(label, _)| *label);
                        ui.combo_box("brand", brands, brand)?
                            .header("Brand")?
                            .enabled(!saving)?;

                        ui.text_box("number", "", number)?
                            .mask(Mask::CARD_NUMBER)?
                            .header("Number")?
                            .enabled(!saving)?;

                        ui.columns(
                            "expiration and code",
                            &[Length::Star(1.0), Length::Star(1.0)],
                            12.0,
                            |ui| {
                                let mut field = ui.text_box("expiration", "MM/YY", expiration)?;
                                expiration_field = Some(field.field());
                                if field.changed() {
                                    *expiration_invalid = false;
                                }
                                field
                                    .mask(Mask::MONTH_YEAR)?
                                    .header("Expiration")?
                                    .error(expiration_invalid.then_some("Invalid date"))?
                                    .enabled(!saving)?;

                                secrets[0] = Some(form.secret(ui, "code", "Security code")?);
                                Ok(())
                            },
                        )?;
                    }
                    Fields::Identity(identity) => {
                        heading(ui, "personal", "Personal")?;
                        ui.combo_box("title", TITLES, &mut identity.title)?
                            .header("Title")?
                            .enabled(!saving)?;
                        form.text(ui, "first", "First name", "", &mut identity.first_name)?;
                        form.text(ui, "middle", "Middle name", "", &mut identity.middle_name)?;
                        form.text(ui, "last", "Last name", "", &mut identity.last_name)?;
                        form.text(ui, "username", "Username", "", &mut identity.username)?;
                        form.text(ui, "company", "Company", "", &mut identity.company)?;

                        heading(ui, "identification", "Identification")?;
                        secrets[0] = Some(form.secret(ui, "passport", "Passport number")?);
                        secrets[1] = Some(form.secret(ui, "ssn", "Tax number")?);
                        form.text(
                            ui,
                            "license",
                            "Driver's license",
                            "",
                            &mut identity.license_number,
                        )?;

                        heading(ui, "contact", "Contact")?;
                        form.text(ui, "email", "Email", "", &mut identity.email)?;
                        form.text(ui, "phone", "Phone", "", &mut identity.phone)?;
                        form.text(ui, "address1", "Address", "", &mut identity.address1)?;
                        form.text(ui, "address2", "", "", &mut identity.address2)?;
                        form.text(ui, "address3", "", "", &mut identity.address3)?;
                        form.text(ui, "city", "City", "", &mut identity.city)?;
                        form.text(ui, "state", "State or region", "", &mut identity.state)?;
                        form.text(ui, "postal", "Postal code", "", &mut identity.postal_code)?;
                        form.text(ui, "country", "Country", "", &mut identity.country)?;
                    }
                    Fields::SshKey(ssh) => {
                        heading(ui, "ssh key", "SSH key")?;
                        ui.stack("sources", Orientation::Horizontal, 8.0, |ui| {
                            let mut generate = ui.button("generate", "Generate new")?;
                            generate.icon('\u{E72C}')?.enabled(!saving)?;
                            if generate.clicked()? {
                                ssh.input = KeyInput::Generated;
                                ssh.text.clear();
                                ssh.key = Some(SshKeyView::generate_ed25519(&mut OsRng));
                                ssh.problem = None;
                                ui.request_pass();
                            }

                            let mut enter = ui.button("enter", "Enter key")?;
                            enter.icon('\u{E765}')?.enabled(!saving)?;
                            if enter.clicked()? && ssh.input != KeyInput::Entered {
                                ssh.input = KeyInput::Entered;
                                ssh.text.clear();
                                ssh.key = None;
                                ssh.problem = None;
                                // A private key on the clipboard fills the
                                // box at once.
                                ssh.asked = Some(KeySource::Clipboard { only_key: true });
                            }

                            let mut import = ui.button("import", "Import key")?;
                            import.icon('\u{E8E5}')?.enabled(!saving)?;
                            if import.clicked()? {
                                ssh.asked = Some(KeySource::File);
                            }
                            Ok(())
                        })?;

                        match ssh.input {
                            KeyInput::Generated => {}
                            KeyInput::Imported => {
                                ui.label("source", format_args!("Imported from a file."))?
                                    .secondary(true)?;
                            }
                            KeyInput::Entered => {
                                // A key with a passphrase asks for it below
                                // instead.
                                let problem_line = match ssh.problem {
                                    Some(KeyProblem::Key(
                                        SshKeyError::NeedsPassphrase | SshKeyError::WrongPassphrase,
                                    ))
                                    | None => None,
                                    Some(problem) => Some(problem.message()),
                                };

                                // Paste sits in the box's top right corner,
                                // under the header.
                                ui.overlay("private key", |ui| {
                                    let mut area =
                                        ui.text_area("private key", "", &mut ssh.text)?;
                                    let changed = area.changed();
                                    key_field = Some(area.field());
                                    area.header("Private key")?
                                        .error(problem_line)?
                                        .enabled(!saving)?;
                                    if changed {
                                        ssh.reread(None);
                                    }

                                    let mut paste = ui.icon_button("paste", '\u{E77F}', "Paste")?;
                                    paste
                                        .align(Align::End)?
                                        .valign(Align::Start)?
                                        .margin([0.0, HEADER_HEIGHT + 4.0, 4.0, 0.0])?
                                        .enabled(!saving)?;
                                    if paste.clicked()? {
                                        ssh.asked = Some(KeySource::Clipboard { only_key: false });
                                    }
                                    Ok(())
                                })?;
                            }
                        }

                        if let Some(KeyProblem::Key(
                            error @ (SshKeyError::NeedsPassphrase | SshKeyError::WrongPassphrase),
                        )) = ssh.problem
                        {
                            let mut unlock = false;
                            let mut passphrase = None;
                            ui.columns(
                                "passphrase",
                                &[Length::Star(1.0), Length::Auto],
                                8.0,
                                |ui| {
                                    let mut field = ui.password_box("passphrase", "")?;
                                    unlock |= field.enter_pressed()?;
                                    field
                                        .header("The key has a passphrase")?
                                        .error(
                                            (error == SshKeyError::WrongPassphrase)
                                                .then_some("Wrong passphrase"),
                                        )?
                                        .enabled(!saving)?;
                                    passphrase = Some(field.handle());
                                    key_field = Some(field.handle().field());

                                    let mut button = ui.button("unlock", "Unlock")?;
                                    button.valign(Align::Center)?.enabled(!saving)?;
                                    unlock |= button.clicked()?;
                                    Ok(())
                                },
                            )?;

                            if unlock && let Some(handle) = passphrase {
                                let passphrase = ui.take_password(handle)?;
                                ssh.reread(Some(&passphrase));
                                ui.request_pass();
                            }
                        } else if let Some(problem) = ssh.problem
                            && ssh.input != KeyInput::Entered
                        {
                            ui.info_bar(
                                "key problem",
                                Severity::Error,
                                "No key to save.",
                                problem.message(),
                            )?;
                        }

                        if let Some(key) = &ssh.key {
                            for (id, caption, value) in [
                                ("public key", "Public key", &key.public_key),
                                ("fingerprint", "Fingerprint", &key.key_fingerprint),
                            ] {
                                ui.stack(id, Orientation::Vertical, 4.0, |ui| {
                                    ui.label("caption", format_args!("{caption}"))?
                                        .secondary(true)?
                                        .style("CaptionTextBlockStyle")?;
                                    ui.label(
                                        "value",
                                        format_args!("{}", value.as_deref().map_or("", |v| v)),
                                    )?
                                    .monospace(true)?
                                    .overflow(Overflow::Wrap)?;
                                    Ok(())
                                })?;
                            }
                        }

                        if let Some(source) = ssh.asked.take() {
                            pass = EditPass::ReadKey(source);
                        }
                    }
                }

                heading(ui, "more", "More")?;
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
        // The kind's own invalid box, if any: `Some(None)` when the problem
        // has no box to focus, as for a key file that did not read.
        let kind_invalid = match &mut draft.fields {
            Fields::SshKey(ssh) if ssh.key.is_none() => {
                ssh.problem = ssh.problem.or(Some(KeyProblem::Missing));
                Some(key_field)
            }
            Fields::Card {
                expiration: typed,
                expiration_invalid,
                ..
            } => {
                *expiration_invalid = expiration(typed).is_none();
                expiration_invalid.then_some(expiration_field)
            }
            _ => None,
        };

        draft.name_missing = draft.name.trim().is_empty();
        let first_invalid = match draft.name_missing {
            true => Some(name_field),
            false => kind_invalid,
        };

        match first_invalid {
            Some(field) => {
                // The box is in the window, so it takes focus at once, and
                // its scroll area brings it into view.
                if let Some(field) = field {
                    ui.focus(field)?;
                }
                ui.request_pass();
            }
            None => {
                let mut read = |handle: Option<PasswordHandle>| match handle {
                    Some(handle) => ui.take_password(handle),
                    None => Ok(SecretString::default()),
                };

                let secrets = [read(secrets[0])?, read(secrets[1])?];
                draft.saving = true;
                pass = EditPass::Save(Box::new(draft.view(index, secrets)));
            }
        }
    }
    Ok(pass)
}

/// What every box of the kind's fields shares in a pass.
struct Form<'a> {
    saving: bool,
    secret_typed: &'a mut bool,
}

impl Form<'_> {
    /// A text box with `header` over it, or none when `header` is empty.
    fn text(
        &mut self,
        ui: &mut Ui,
        id: &str,
        header: &str,
        placeholder: &str,
        value: &mut SecretString,
    ) -> Result<()> {
        let mut text_box = ui.text_box(id, placeholder, value)?;
        if !header.is_empty() {
            text_box.header(header)?;
        }
        text_box.enabled(!self.saving)?;
        Ok(())
    }

    /// A hidden box, whose text only Save reads.
    fn secret(&mut self, ui: &mut Ui, id: &str, header: &str) -> Result<PasswordHandle> {
        let mut field = ui.password_box(id, "")?;
        *self.secret_typed |= field.changed()?;
        // At the top of a row whose other box is taller, as under its error
        // line, so it keeps its own height.
        field
            .header(header)?
            .enabled(!self.saving)?
            .valign(Align::Start)?;
        Ok(field.handle())
    }
}

/// A caption over a group of fields, as the design's form groups have.
fn heading(ui: &mut Ui, id: &str, text: &str) -> Result<()> {
    ui.label(("heading", id), format_args!("{text}"))?
        .style("BodyStrongTextBlockStyle")?
        .margin([0.0, 8.0, 0.0, 0.0])?;
    Ok(())
}

/// The kind the New button's main part creates for `filter`: the filter's
/// own kind, else a login.
pub fn default_kind(filter: Filter) -> CipherKind {
    match filter {
        Filter::Kind(kind) => kind,
        _ => CipherKind::Login,
    }
}

#[cfg(test)]
mod tests {
    use super::expiration;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn every_written_form_of_a_date_reads_as_it(month in 1u8..=12, year in 2000u16..=2099, form in 0..5) {
            let short = year % 100;
            let text = match form {
                0 => format!("{month:02}/{short:02}"),
                1 => format!("{month}/{short:02}"),
                2 => format!("{month:02}{short:02}"),
                3 => format!("{month:02}/{year}"),
                _ => format!(" {month:02} - {year} "),
            };
            prop_assert_eq!(expiration(&text), Some(Some((month, year))));
        }

        #[test]
        fn a_month_past_12_or_0_is_unreadable(month in prop_oneof![Just(0u8), 13u8..=99], year in 0u16..100) {
            prop_assert_eq!(expiration(&format!("{month:02}/{year:02}")), None);
        }

        #[test]
        fn blank_text_is_no_date(spaces in " {0,4}") {
            prop_assert_eq!(expiration(&spaces), Some(None));
        }
    }

    #[test]
    fn a_three_digit_year_is_unreadable() {
        assert_eq!(expiration("01/202"), None);
    }
}
