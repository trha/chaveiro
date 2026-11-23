//! The ItemEdit form from the design: it takes the details pane's place for
//! a new item, or for the open item the user edits, until the item saves or
//! the user cancels.
//!
//! The form keeps no copy of the secret in its hidden box (a login's
//! password or a card's security code): it stays in the box until Save
//! reads it, which leaves the box as it is, so a failed save keeps it. An
//! edit writes the stored secret into the box when the form opens. A new
//! SSH key is
//! generated when the form opens, so the form holds that private key.

use chaveiro_core::{
    core::{Conflict, ItemError},
    crypto::SecretString,
    filter::Filter,
    vault::{
        CardView, CipherContent, CipherKind, CipherView, IdentityView, LoginView, SshKeyError,
        SshKeyView, UriView, VaultIndex,
    },
};
use rand_core::OsRng;
use std::fmt::Write as _;
use windows_core::Result;
use winui_immediate::{Align, Length, Mask, Orientation, Overflow, PasswordHandle, Severity, Ui};
use zeroize::Zeroizing;

/// An item the user is filling in: a new one, or the open item.
pub struct Draft {
    now: Values,
    /// What the form opened with. A field still equal to its start saves as
    /// the item stored it, so an edit changes only what the user changed.
    start: Box<Values>,
    /// The item as stored, or an empty one of the kind for a new item. Save
    /// builds on it, so what the form does not show stays: custom fields,
    /// passkeys, more websites and the password history.
    base: Box<CipherView>,
    /// The stored secret for the hidden box, written into it as the box is
    /// made, before it reports changes.
    fill: Option<SecretString>,
    /// The user typed in the hidden box, which holds the only copy.
    secret_typed: bool,
    /// Save was clicked with an empty name.
    name_missing: bool,
    /// Save was clicked with a card expiration it cannot read.
    expiration_invalid: bool,
    /// Save sent the item, and the form waits for the core to say how it
    /// went.
    saving: bool,
}

/// What the user can change in the form, besides the hidden boxes.
#[derive(Clone, PartialEq)]
struct Values {
    name: SecretString,
    notes: SecretString,
    /// 0 for no folder, else 1 + the folder's position in
    /// [`VaultIndex::folders`].
    folder: usize,
    favorite: bool,
    reprompt: bool,
    fields: Fields,
}

/// The fields of the item's kind, besides the hidden boxes.
#[derive(Clone, PartialEq)]
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
    },
    Identity(Box<IdentityDraft>),
    SshKey(Box<SshDraft>),
}

/// The SSH key of the item: the stored one, one generated here, or one the
/// user enters or imports.
///
/// Contract: `key` is the key that saves. For a stored or generated key it
/// came from there; otherwise it is `text` read as a key, and `problem` says
/// why it is [`None`].
#[derive(Clone)]
struct SshDraft {
    input: KeyInput,
    /// The private key as entered, or as the imported file holds it, which
    /// may have a passphrase. Empty for a stored or generated key.
    text: SecretString,
    key: Option<SshKeyView>,
    problem: Option<KeyProblem>,
    /// Text the app should read for the form, asked for once.
    asked: Option<KeySource>,
}

/// Equal when the same key would save from the same input; the problem and
/// the pending ask are how the form got there.
impl PartialEq for SshDraft {
    fn eq(&self, other: &Self) -> bool {
        fn private(ssh: &SshDraft) -> Option<&Option<SecretString>> {
            ssh.key.as_ref().map(|key| &key.private_key)
        }
        self.input == other.input && self.text == other.text && private(self) == private(other)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyInput {
    /// The key the item had when the form opened.
    Stored,
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

#[derive(Clone, PartialEq)]
struct IdentityDraft {
    /// A position in [`TITLES`].
    title: usize,
    first_name: SecretString,
    middle_name: SecretString,
    last_name: SecretString,
    username: SecretString,
    company: SecretString,
    passport_number: SecretString,
    ssn: SecretString,
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
/// The first is no brand, and the last stands for any brand not listed.
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
    buffer(FIELD_CAPACITY, None)
}

/// A [`field`] holding `value`, or empty for [`None`].
fn field_of(value: Option<&SecretString>) -> SecretString {
    buffer(FIELD_CAPACITY, value)
}

/// Text that holds `value` and has room for `capacity` bytes.
fn buffer(capacity: usize, value: Option<&SecretString>) -> SecretString {
    let mut text = SecretString::new(String::with_capacity(capacity));
    if let Some(value) = value {
        text.push_str(value);
    }
    text
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
        let (fields, content) = match kind {
            CipherKind::Login => (
                Fields::Login {
                    username: field(),
                    totp: field(),
                    website: field(),
                },
                CipherContent::Login(LoginView::default()),
            ),
            CipherKind::Card => (
                Fields::Card {
                    cardholder: field(),
                    brand: 0,
                    number: field(),
                    expiration: field(),
                },
                CipherContent::Card(CardView::default()),
            ),
            CipherKind::Identity => (
                Fields::Identity(Box::new(IdentityDraft {
                    title: 0,
                    first_name: field(),
                    middle_name: field(),
                    last_name: field(),
                    username: field(),
                    company: field(),
                    passport_number: field(),
                    ssn: field(),
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
                CipherContent::Identity(Box::default()),
            ),
            CipherKind::SshKey => {
                // The base holds the key too, so an untouched key saves.
                let key = SshKeyView::generate_ed25519(&mut OsRng);
                (
                    Fields::SshKey(Box::new(SshDraft {
                        input: KeyInput::Generated,
                        text: buffer(KEY_CAPACITY, None),
                        key: Some(key.clone()),
                        problem: None,
                        asked: None,
                    })),
                    CipherContent::SshKey(key),
                )
            }
            CipherKind::SecureNote | CipherKind::Unknown(_) => {
                (Fields::Note, CipherContent::SecureNote)
            }
        };
        let values = Values {
            name: field(),
            notes: buffer(NOTES_CAPACITY, None),
            folder,
            favorite: filter == Filter::Favorites,
            reprompt: false,
            fields,
        };
        let base = CipherView::new(values.fields.kind(), SecretString::default(), content);
        Draft::of(values, base)
    }

    /// The form for `view`, an item the core opened, filled with what it
    /// stores. A brand or title the lists lack shows as Other or no title,
    /// and saves unchanged unless the user picks another.
    pub fn edit(view: &CipherView, index: &VaultIndex) -> Self {
        let text = |value: &Option<SecretString>| field_of(value.as_ref());
        let fields = match &view.content {
            CipherContent::Login(login) => Fields::Login {
                username: text(&login.username),
                totp: text(&login.totp),
                website: field_of(login.uris.first().and_then(|uri| uri.uri.as_ref())),
            },
            CipherContent::Card(card) => Fields::Card {
                cardholder: text(&card.cardholder_name),
                brand: match card.brand.as_deref().map(String::as_str) {
                    None | Some("") => 0,
                    Some(stored) => BRANDS
                        .iter()
                        .position(|(_, brand)| *brand == stored)
                        .unwrap_or(BRANDS.len() - 1),
                },
                // In the box's own form, so the mask finds nothing to change
                // and an untouched number is no change.
                number: {
                    let mut number = field();
                    if let Some(stored) = &card.number {
                        Mask::CARD_NUMBER.write(stored, &mut number);
                    }
                    number
                },
                expiration: expiration_text(card),
            },
            CipherContent::Identity(identity) => Fields::Identity(Box::new(IdentityDraft {
                title: identity
                    .title
                    .as_deref()
                    .and_then(|stored| TITLES.iter().position(|title| title == stored))
                    .unwrap_or(0),
                first_name: text(&identity.first_name),
                middle_name: text(&identity.middle_name),
                last_name: text(&identity.last_name),
                username: text(&identity.username),
                company: text(&identity.company),
                passport_number: text(&identity.passport_number),
                ssn: text(&identity.ssn),
                license_number: text(&identity.license_number),
                email: text(&identity.email),
                phone: text(&identity.phone),
                address1: text(&identity.address1),
                address2: text(&identity.address2),
                address3: text(&identity.address3),
                city: text(&identity.city),
                state: text(&identity.state),
                postal_code: text(&identity.postal_code),
                country: text(&identity.country),
            })),
            CipherContent::SshKey(key) => Fields::SshKey(Box::new(SshDraft {
                input: KeyInput::Stored,
                text: buffer(KEY_CAPACITY, None),
                key: Some(key.clone()),
                problem: None,
                asked: None,
            })),
            // A note's text is its notes; an unknown kind keeps its content.
            _ => Fields::Note,
        };
        let values = Values {
            name: field_of(Some(&view.name)),
            notes: buffer(NOTES_CAPACITY, view.notes.as_ref()),
            folder: view
                .folder_id
                .and_then(|id| index.folders.iter().position(|folder| folder.id == id))
                .map_or(0, |position| position + 1),
            favorite: view.favorite,
            reprompt: view.reprompt,
            fields,
        };
        Draft::of(values, view.clone())
    }

    fn of(values: Values, base: CipherView) -> Self {
        Draft {
            start: Box::new(values.clone()),
            now: values,
            fill: hidden(&base),
            base: Box::new(base),
            secret_typed: false,
            name_missing: false,
            expiration_invalid: false,
            saving: false,
        }
    }

    /// Whether the user changed anything, so leaving the form would lose
    /// it. A generated SSH key the user kept is no change.
    pub fn changed(&self) -> bool {
        self.secret_typed || self.now != *self.start
    }

    /// The title of the dialog that asks before the draft is dropped.
    pub fn discard_title(&self) -> String {
        match (self.base.id, self.now.name.trim()) {
            (Some(_), _) => format!("Discard changes to {}?", self.base.name.as_str()),
            (None, "") => format!("Discard the new {}?", noun(self.base.kind)),
            (None, name) => format!("Discard changes to {name}?"),
        }
    }

    /// The item as the core takes it, with `secret` read from the hidden
    /// box, empty for a kind without one.
    fn view(&self, index: &VaultIndex, secret: SecretString) -> CipherView {
        let (now, start) = (&self.now, &*self.start);
        let mut view = (*self.base).clone();
        if now.name != start.name {
            view.name = Zeroizing::new(now.name.trim().to_owned());
        }
        if now.notes != start.notes {
            // A note keeps its spaces and blank lines as typed.
            view.notes = (!now.notes.is_empty()).then(|| now.notes.clone());
        }
        view.folder_id = now
            .folder
            .checked_sub(1)
            .and_then(|position| index.folders.get(position))
            .map(|folder| folder.id);
        view.favorite = now.favorite;
        view.reprompt = now.reprompt;

        match (&now.fields, &start.fields, &mut view.content) {
            (
                Fields::Login {
                    username,
                    totp,
                    website,
                },
                Fields::Login {
                    username: start_username,
                    totp: start_totp,
                    website: start_website,
                },
                CipherContent::Login(login),
            ) => {
                pick(&mut login.username, username, start_username);
                pick_secret(&mut login.password, &secret);
                pick(&mut login.totp, totp, start_totp);
                // The box shows the first website; the others stay.
                if website != start_website {
                    match (some(website), login.uris.is_empty()) {
                        (Some(uri), true) => login.uris.push(UriView {
                            uri: Some(uri),
                            match_rule: None,
                        }),
                        (Some(uri), false) => login.uris[0].uri = Some(uri),
                        (None, true) => {}
                        (None, false) => {
                            login.uris.remove(0);
                        }
                    }
                }
            }
            (
                Fields::Card {
                    cardholder,
                    brand,
                    number,
                    expiration: typed,
                },
                Fields::Card {
                    cardholder: start_cardholder,
                    brand: start_brand,
                    number: start_number,
                    expiration: start_typed,
                },
                CipherContent::Card(card),
            ) => {
                pick(&mut card.cardholder_name, cardholder, start_cardholder);
                if brand != start_brand {
                    card.brand = (*brand != 0).then(|| Zeroizing::new(BRANDS[*brand].1.to_owned()));
                }
                // Stored as digits only, as the box shows them grouped.
                if number != start_number {
                    let mut digits = field();
                    digits.extend(number.chars().filter(char::is_ascii_digit));
                    card.number = some(&digits);
                }
                if typed != start_typed {
                    (card.exp_month, card.exp_year) = expiration(typed)
                        .flatten()
                        .map(|(month, year)| {
                            (
                                Zeroizing::new(month.to_string()),
                                Zeroizing::new(year.to_string()),
                            )
                        })
                        .unzip();
                }
                pick_secret(&mut card.code, &secret);
            }
            (
                Fields::Identity(identity),
                Fields::Identity(start_identity),
                CipherContent::Identity(stored),
            ) => {
                let stored: &mut IdentityView = stored;
                if identity.title != start_identity.title {
                    stored.title = (identity.title != 0)
                        .then(|| Zeroizing::new(TITLES[identity.title].to_owned()));
                }
                let targets = [
                    &mut stored.first_name,
                    &mut stored.middle_name,
                    &mut stored.last_name,
                    &mut stored.username,
                    &mut stored.company,
                    &mut stored.passport_number,
                    &mut stored.ssn,
                    &mut stored.license_number,
                    &mut stored.email,
                    &mut stored.phone,
                    &mut stored.address1,
                    &mut stored.address2,
                    &mut stored.address3,
                    &mut stored.city,
                    &mut stored.state,
                    &mut stored.postal_code,
                    &mut stored.country,
                ];
                for ((target, text), start_text) in targets
                    .into_iter()
                    .zip(identity.texts())
                    .zip(start_identity.texts())
                {
                    pick(target, text, start_text);
                }
            }
            (Fields::SshKey(ssh), Fields::SshKey(start_ssh), CipherContent::SshKey(key)) => {
                if ssh != start_ssh {
                    *key = ssh
                        .key
                        .clone()
                        .expect("Save checks that the form has a key");
                }
            }
            (Fields::Note, ..) => {}
            _ => unreachable!("a draft's fields, their start and its base share one kind"),
        }
        view
    }
}

impl Fields {
    fn kind(&self) -> CipherKind {
        match self {
            Fields::Login { .. } => CipherKind::Login,
            Fields::Note => CipherKind::SecureNote,
            Fields::Card { .. } => CipherKind::Card,
            Fields::Identity(_) => CipherKind::Identity,
            Fields::SshKey(_) => CipherKind::SshKey,
        }
    }
}

impl IdentityDraft {
    /// The text boxes, in the order [`Draft::view`] writes them back.
    fn texts(&self) -> [&SecretString; 17] {
        [
            &self.first_name,
            &self.middle_name,
            &self.last_name,
            &self.username,
            &self.company,
            &self.passport_number,
            &self.ssn,
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

/// The secret of `view` for its hidden box, or [`None`] when it has none
/// to show.
fn hidden(view: &CipherView) -> Option<SecretString> {
    match &view.content {
        CipherContent::Login(login) => login.password.clone(),
        CipherContent::Card(card) => card.code.clone(),
        _ => None,
    }
    .filter(|secret| !secret.is_empty())
}

/// Writes `text` trimmed into `stored`, unless the box still shows `start`,
/// so an untouched field keeps its stored text as it was.
fn pick(stored: &mut Option<SecretString>, text: &SecretString, start: &SecretString) {
    if text != start {
        *stored = some(text);
    }
}

/// Writes `read`, from a hidden box, into `stored`, unless it equals the
/// stored text.
fn pick_secret(stored: &mut Option<SecretString>, read: &SecretString) {
    if read.as_str() != stored.as_deref().map_or("", String::as_str) {
        *stored = some(read);
    }
}

/// `text` trimmed, or [`None`] when that is empty.
fn some(text: &SecretString) -> Option<SecretString> {
    match text.trim() {
        "" => None,
        trimmed => Some(Zeroizing::new(trimmed.to_owned())),
    }
}

/// The card's expiration as the form shows it, `MM/YY`, or empty when the
/// stored month or year does not read as a number.
fn expiration_text(card: &CardView) -> SecretString {
    let mut text = field();
    let number = |value: &Option<SecretString>| value.as_deref()?.trim().parse::<u16>().ok();
    if let (Some(month), Some(year)) = (number(&card.exp_month), number(&card.exp_year)) {
        write!(*text, "{month:02}/{:02}", year % 100).expect("writing to a String does not fail");
    }
    text
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
    /// Settle the open item's conflict. Keeping theirs also closes the form.
    Resolve { keep_mine: bool },
}

impl Draft {
    /// Takes the text the app read for [`EditPass::ReadKey`] from `source`.
    /// A draft of another kind ignores it.
    pub fn key_read(
        &mut self,
        source: KeySource,
        text: std::result::Result<SecretString, KeyTextError>,
    ) {
        let Fields::SshKey(ssh) = &mut self.now.fields else {
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
            KeyProblem::File(KeyTextError::Unreadable) => "Couldn't read the file.",
            KeyProblem::File(KeyTextError::NotText) => "The file is not a text key file.",
            KeyProblem::File(KeyTextError::NoText) => "The clipboard has no text.",
            KeyProblem::File(KeyTextError::Cancelled) => {
                unreachable!("a closed picker leaves the form as it was")
            }
        }
    }
}

/// What the core says about the form's save, from the vault screen.
pub struct SaveState<'a> {
    pub saving: bool,
    pub error: Option<&'a ItemError>,
    /// The open item's conflict, once ready for the user to settle.
    pub conflict: Option<Conflict<'a>>,
}

/// Draws the form for `draft` in the details pane. Ctrl+S saves.
pub fn edit_pane(
    ui: &mut Ui,
    draft: &mut Draft,
    index: &VaultIndex,
    state: SaveState<'_>,
) -> Result<EditPass> {
    // Only an edit of the open item can meet its conflict.
    let conflict = state
        .conflict
        .filter(|conflict| draft.base.id.is_some() && conflict.mine.id == draft.base.id);

    // The save went out on an earlier pass; once the core no longer waits
    // for it, it either opened the item, set an error or found a conflict.
    if draft.saving && !state.saving {
        draft.saving = false;
        if state.error.is_none() && conflict.is_none() {
            return Ok(EditPass::Close);
        }
    }

    let saving = draft.saving;
    let mut pass = EditPass::Stay;
    let mut save = false;
    // Taken on the form's first pass, as the hidden box is made.
    let fill = draft.fill.take();

    // The hidden box of this kind, if it has one.
    let mut secret = None;
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
        // bar's overflow menu, where WinUI draws it as a stretched block. The
        // form has no More: the details pane's has the item's other
        // commands.
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
                if let Some(error) = state.error.filter(|_| !saving) {
                    let message = match error {
                        ItemError::Rejected(message) => message.as_str(),
                        ItemError::Busy => "The last save is still going. Try again in a moment.",
                        ItemError::NotFound | ItemError::Unreadable(_) => {
                            "Couldn't save the item."
                        }
                    };
                    ui.info_bar("error", Severity::Error, "The item was not saved.", message)?;
                }

                if conflict.is_some() && !saving {
                    ui.info_bar(
                        "conflict",
                        Severity::Warning,
                        "This item changed on another device.",
                        "Keep your version, then save it over theirs, or take theirs and \
                         drop your changes.",
                    )?;
                    ui.stack("conflict choices", Orientation::Horizontal, 8.0, |ui| {
                        if ui.button("keep mine", "Keep mine")?.clicked()? {
                            pass = EditPass::Resolve { keep_mine: true };
                        }
                        if ui.button("take theirs", "Take theirs")?.clicked()? {
                            pass = EditPass::Resolve { keep_mine: false };
                        }
                        Ok(())
                    })?;
                }

                let verb = match draft.base.id {
                    Some(_) => "Edit",
                    None => "New",
                };
                ui.label(
                    "form title",
                    format_args!("{verb} {}", noun(draft.base.kind)),
                )?
                .style("SubtitleTextBlockStyle")?;

                let mut name = ui.text_box("name", "", &mut draft.now.name)?;
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
                ui.combo_box("folder", folders, &mut draft.now.folder)?
                    .header("Folder")?
                    .enabled(!saving)?;
                ui.check_box("favorite", "Favorite", &mut draft.now.favorite)?
                    .enabled(!saving)?;

                let mut form = Form {
                    saving,
                    secret_typed: &mut draft.secret_typed,
                    fill: fill.as_ref(),
                };
                match &mut draft.now.fields {
                    Fields::Login {
                        username,
                        totp,
                        website,
                    } => {
                        heading(ui, "login", "Login")?;
                        form.text(ui, "username", "Username", "", username)?;
                        secret = Some(form.secret(ui, "password", "Password")?);
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

                        let expiration_invalid = &mut draft.expiration_invalid;
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

                                secret = Some(form.secret(ui, "code", "Security code")?);
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
                        form.text(
                            ui,
                            "passport",
                            "Passport number",
                            "",
                            &mut identity.passport_number,
                        )?;
                        form.text(ui, "ssn", "Tax number", "", &mut identity.ssn)?;
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
                            KeyInput::Stored | KeyInput::Generated => {}
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
                ui.text_area("notes", "", &mut draft.now.notes)?
                    .header("Notes")?
                    .enabled(!saving)?;
                ui.check_box(
                    "reprompt",
                    "Ask for master password before this item opens",
                    &mut draft.now.reprompt,
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
        let kind_invalid = match (&mut draft.now.fields, &draft.start.fields) {
            (Fields::SshKey(ssh), _) if ssh.key.is_none() => {
                ssh.problem = ssh.problem.or(Some(KeyProblem::Missing));
                Some(key_field)
            }
            // An expiration the user left as stored saves as stored.
            (
                Fields::Card {
                    expiration: typed, ..
                },
                Fields::Card {
                    expiration: start, ..
                },
            ) => {
                draft.expiration_invalid = typed != start && expiration(typed).is_none();
                draft.expiration_invalid.then_some(expiration_field)
            }
            _ => None,
        };

        draft.name_missing = draft.now.name.trim().is_empty();
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
                let secret = match secret {
                    // Read, not taken: a failed save leaves the box as the
                    // user left it.
                    Some(handle) => ui.read_password(handle)?,
                    None => SecretString::default(),
                };
                draft.saving = true;
                pass = EditPass::Save(Box::new(draft.view(index, secret)));
            }
        }
    }
    Ok(pass)
}

/// What every box of the kind's fields shares in a pass.
struct Form<'a> {
    saving: bool,
    secret_typed: &'a mut bool,
    /// Text to write into the hidden box this pass.
    fill: Option<&'a SecretString>,
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
        // Written before `changed` subscribes the box, so a new box reports
        // no change for it.
        if let Some(fill) = self.fill {
            field.set(fill)?;
        }
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
mod tests;
