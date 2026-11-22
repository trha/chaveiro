//! The vault window on its own, over a made-up vault, so it can be seen and
//! driven without an account. It plays the core's part for the item
//! actions: Open, Close, Save of a favorite, Trash and Restore change the
//! made-up vault and print to stderr. Lock prints `lock` and closes the
//! window.
//!
//! An interval stream plays a script at the start of the view, which works
//! where System Events cannot reach the window. It picks rows of the list through the
//! table as a click would, and after each pick prints the details heading
//! of every pass, panicking when one shows no heading: the details must not
//! blank while the core opens the picked item. It also clicks Show on the
//! password and the toolbar's Favorite, Move to Trash, Restore and Sync,
//! picks folders in the sidebar, searches and moves the selection with
//! Down from the search field, and counts the Rust allocations of steady
//! passes with an item open, which must be zero.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]
#![allow(dead_code, reason = "shares modules with the app")]

#[path = "../src/vault.rs"]
mod vault;

use appkit_immediate::{App, Ui, run};
use chaveiro_core::{
    core::{UserAction, VaultScreen},
    crypto::SecretString,
    vault::{
        CardView, CipherContent, CipherKind, CipherSummary, CipherView, CopyField, CustomField,
        FolderSummary, IdentityView, LoginView, SshKeyView, UriView, VaultIndex,
    },
};
use objc2::{MainThreadMarker, Message, rc::Retained, sel};
use objc2_app_kit::{
    NSApplication, NSButton, NSEvent, NSEventModifierFlags, NSEventType, NSMenuDelegate,
    NSSearchToolbarItem, NSSplitView, NSTableView, NSTableViewStyle, NSTextField,
    NSTextInputClient, NSTextView, NSToolbarItem, NSToolbarItemGroup, NSView,
};
use objc2_foundation::{NSIndexSet, NSNotFound, NSRange, NSString};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::Duration,
};
use uuid::Uuid;
use vault::{VaultAction, VaultApp, VaultForm};

fn main() {
    run(|| {
        let index = sample();
        let views = index.ciphers.iter().map(view_of).collect();
        Preview {
            index,
            views,
            revision: 1,
            opened: None,
            form: VaultForm::default(),
            open: true,
            played: 0,
            server: url::Url::parse("https://vault.bitwarden.com").expect("a valid URL"),
            watch: 0,
            steady: None,
        }
    });
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Dump,
    /// Selects the list row with this name through the table, then watches
    /// the details of the next passes.
    PickRow(&'static str),
    /// Selects the sidebar row with this label through the table.
    PickFilter(&'static str),
    /// Clicks the button with this tooltip in the details.
    Click(&'static str),
    /// Replaces the search field's text, as typing would.
    Search(&'static str),
    /// Presses Down in the search field.
    SearchDown,
    /// Right-clicks the list row with this name, prints the menu's items
    /// and chooses the one with this title.
    Menu(&'static str, &'static str),
    /// Starts counting allocations.
    Measure,
    /// A message that changes nothing, so its pass is a steady one.
    Tick,
    Report,
    Quit,
}

/// The time between steps.
const STEP_EVERY: Duration = Duration::from_millis(300);

const SCRIPT: &[Step] = &[
    Step::Dump,
    Step::PickRow("GitHub"),
    Step::Dump,
    Step::PickRow("Mail server"),
    Step::Dump,
    Step::PickRow("GitHub"),
    Step::Click("Show"),
    Step::Dump,
    Step::Measure,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Report,
    Step::Click("Favorite"),
    Step::Dump,
    Step::PickFilter("Work"),
    Step::Dump,
    Step::Click("Move to Trash"),
    Step::PickFilter("Trash"),
    Step::PickRow("GitHub"),
    Step::Dump,
    Step::Click("Restore"),
    Step::Click("Sync"),
    Step::Dump,
    Step::PickFilter("All Items"),
    Step::PickRow("Personal Visa"),
    Step::Dump,
    Step::Click("Show"),
    Step::Dump,
    Step::Measure,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Report,
    Step::PickRow("Alex Lima"),
    Step::Dump,
    Step::PickRow("Deploy key"),
    Step::Dump,
    Step::Search("hub"),
    Step::Dump,
    Step::Search("mail"),
    Step::SearchDown,
    Step::Dump,
    Step::Measure,
    Step::Tick,
    Step::Tick,
    Step::Tick,
    Step::Report,
    Step::Search(""),
    Step::Menu("GitHub", "Copy Verification Code"),
    Step::Menu("Mail server", "Copy Password"),
    Step::PickRow("GitHub"),
    Step::Click("Copy verification code"),
    Step::Menu("Personal Visa", "Move to Trash"),
    Step::PickFilter("Trash"),
    Step::Menu("Personal Visa", "Put Back"),
    Step::PickFilter("All Items"),
    Step::Search("zzz"),
    Step::Dump,
    Step::Search(""),
    Step::Dump,
    Step::Quit,
];

/// The made-up vault and the item it has open, as the core would keep
/// them.
///
/// Contract: `views` holds one view per item of `index`, in its order.
struct Preview {
    index: VaultIndex,
    views: Vec<CipherView>,
    /// Changes whenever `index` does.
    revision: u64,
    opened: Option<Uuid>,
    form: VaultForm,
    open: bool,
    /// Steps of [`SCRIPT`] played so far.
    played: usize,
    /// Parsed once, so steady passes allocate nothing.
    server: url::Url,
    /// Passes left whose details heading is printed and checked.
    watch: u32,
    /// The allocation count when measuring began.
    steady: Option<usize>,
}

impl Preview {
    /// Plays one step of the script.
    fn play(&mut self, step: Step) {
        let mtm = MainThreadMarker::new().expect("the view runs on the main thread");
        let content = content(mtm);
        match step {
            Step::Dump => dump(&content),
            Step::PickRow(name) => {
                let list = &tables(&content)[1];
                // A click focuses the list, which draws its selection
                // emphasized.
                if let Some(window) = list.window() {
                    window.makeFirstResponder(Some(list));
                }
                pick(list, name);
                println!("-- picked row {name}");
                self.watch = 4;
            }
            Step::PickFilter(label) => {
                let sidebar = &tables(&content)[0];
                pick(sidebar, label);
                println!("-- picked filter {label}");
            }
            Step::Click(tooltip) => {
                let button = buttons(&details(&content)).into_iter().find(|button| {
                    button
                        .toolTip()
                        .is_some_and(|tip| tip.to_string() == tooltip)
                });
                match button {
                    Some(button) => unsafe { button.performClick(None) },
                    None => {
                        // Not in the details, so a toolbar item, which
                        // sends its action as a click does.
                        let item = toolbar_items(&content)
                            .into_iter()
                            .find(|item| item.label().to_string() == tooltip)
                            .expect("the details or the toolbar have the button");
                        let action = item.action().expect("toolbar buttons have an action");
                        let target = item.target();
                        // SAFETY: the action is the crate's `fire:`, which
                        // takes the sender.
                        let sent = unsafe {
                            NSApplication::sharedApplication(mtm).sendAction_to_from(
                                action,
                                target.as_deref(),
                                Some(&item),
                            )
                        };
                        assert!(sent, "the toolbar item's action reached its target");
                    }
                }
                println!("-- clicked {tooltip}");
            }
            Step::Search(text) => {
                let editor = search_editor(&content);
                unsafe { editor.selectAll(None) };
                match text {
                    "" => unsafe { editor.doCommandBySelector(sel!(deleteBackward:)) },
                    text => {
                        // Typed text has no range to replace.
                        let nowhere = NSRange::new(NSNotFound as usize, 0);
                        unsafe {
                            editor.insertText_replacementRange(&NSString::from_str(text), nowhere)
                        };
                    }
                }
                println!("-- searched {text:?}");
            }
            // AppKit opens a menu between passes, and the step plays inside
            // one, which could not run the pass that fills the menu.
            Step::Menu(name, title) => {
                dispatch2::DispatchQueue::main().exec_async(move || open_menu(name, title));
            }
            Step::SearchDown => {
                unsafe { search_editor(&content).doCommandBySelector(sel!(moveDown:)) };
                println!("-- pressed Down in the search field");
            }
            Step::Measure => self.steady = Some(ALLOCATIONS.get()),
            Step::Tick => {}
            Step::Report => {
                let start = self.steady.take().expect("measuring began");
                println!("-- steady passes allocated {}", ALLOCATIONS.get() - start);
            }
            Step::Quit => self.open = std::env::var("QUIT").ok().as_deref() == Some("0"),
        }
    }
}

impl App for Preview {
    fn view(&mut self, ui: &mut Ui) {
        // Played before the view reads the state, so this pass shows the
        // step.
        let mut ticks = ui.stream("script", || appkit_executor::interval(STEP_EVERY));
        while ticks.next().is_some() {
            if let Some(step) = SCRIPT.get(self.played) {
                self.played += 1;
                self.play(*step);
            }
        }
        if ui.should_terminate() {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let now_ms = 1_800_000_000_000;
        let screen = VaultScreen {
            email: "alex@example.com",
            server: &self.server,
            index: &self.index,
            index_revision: self.revision,
            open: None,
            syncing: false,
            sync_error: None,
            error: None,
            revision: None,
            synced_at: Some(now_ms - 2 * 60 * 1000),
            device_unlock: false,
            device_enrolling: false,
            device_error: None,
            ssh_approval: None,
        };
        let open = self
            .opened
            .and_then(|id| self.views.iter().find(|view| view.id == Some(id)));
        let action = vault::view::<Self>(ui, &mut self.form, &screen, open, now_ms);
        if self.watch > 0 {
            self.watch -= 1;
            let mtm = MainThreadMarker::new().expect("view runs on the main thread");
            let shown = texts(&details(&content(mtm)));
            println!("   pass shows {:?}", &shown[..shown.len().min(2)]);
            assert!(
                !shown.is_empty(),
                "the details blanked while an item opened"
            );
        }
        match action {
            None => {}
            Some(VaultAction::Close) => self.open = false,
            // The preview leaves the pasteboard alone.
            Some(VaultAction::Copy(text)) => eprintln!("copy {}", text.as_str()),
            Some(VaultAction::User(action)) => self.act(action),
        }
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
            UserAction::Sync => eprintln!("sync"),
            UserAction::Copy { id, field } => eprintln!("copy {field:?} of {}", id.as_u128()),
            UserAction::Lock => {
                eprintln!("lock");
                self.open = false;
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
                let verb = if deleted { "trash" } else { "restore" };
                eprintln!("{verb} {}", id.as_u128());
                let position = self.position(id);
                self.index.ciphers[position].deleted = deleted;
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

/// Right-clicks the list row `name`, prints the menu's items and chooses
/// the one titled `title`, as [`Step::Menu`] says.
fn open_menu(name: &str, title: &str) {
    let mtm = MainThreadMarker::new().expect("blocks on the main queue run on the main thread");
    let content = content(mtm);
    let list = &tables(&content)[1];

    let row = row_named(list, name);
    let rect = list.rectOfRow(row);
    let point = list.convertPoint_toView(
        objc2_foundation::NSPoint::new(rect.origin.x + 20.0, rect.origin.y + 10.0),
        None,
    );
    let window = list.window().expect("the list is in the window");
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::RightMouseDown,
        point,
        NSEventModifierFlags::empty(),
        0.0,
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    )
    .expect("a mouse event");
    // Sets the clicked row, as a right click does.
    let menu = list.menuForEvent(&event).expect("the list has a menu");
    // Asks the delegate, as AppKit does each time before it shows the
    // menu. `NSMenu::update` asks only once.
    // The last menu's node is gone, and its items with it.
    assert_eq!(menu.numberOfItems(), 0, "a closed menu left its items");
    let delegate = menu.delegate().expect("the crate's menus have a delegate");
    delegate.menuNeedsUpdate(&menu);
    let titles: Vec<String> = menu
        .itemArray()
        .iter()
        .map(|item| item.title().to_string())
        .collect();
    println!("-- menu of {name}: {titles:?}");
    let position = titles
        .iter()
        .position(|item| item == title)
        .unwrap_or_else(|| panic!("the menu has {title}"));
    // AppKit closes the menu before it sends the item's action.
    delegate.menuDidClose(&menu);
    menu.performActionForItemAtIndex(position as isize);
    println!("-- chose {title}");
}

fn content(mtm: MainThreadMarker) -> Retained<NSView> {
    NSApplication::sharedApplication(mtm)
        .windows()
        .firstObject()
        .expect("the window is open")
        .contentView()
        .expect("a window has a content view")
}

/// Selects the row of `table` whose first text is `first`, as a click
/// would.
fn pick(table: &NSTableView, first: &str) {
    let row = row_named(table, first);
    table
        .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row as usize), false);
}

/// The row of `table` whose first text is `first`.
fn row_named(table: &NSTableView, first: &str) -> isize {
    (0..table.numberOfRows())
        .find(|&row| {
            table
                .viewAtColumn_row_makeIfNecessary(0, row, false)
                .is_some_and(|cell| texts(&cell).first().is_some_and(|text| text == first))
        })
        .unwrap_or_else(|| panic!("the table has a row {first}"))
}

/// The sidebar's table view and the list's, told apart by their style.
fn tables(view: &NSView) -> [Retained<NSTableView>; 2] {
    let found = all_tables(view);
    let sidebar = |table: &&Retained<NSTableView>| table.style() == NSTableViewStyle::SourceList;
    let pick = |want: bool| {
        found
            .iter()
            .find(|table| sidebar(table) == want)
            .expect("the window has a sidebar and a list")
            .clone()
    };
    [pick(true), pick(false)]
}

fn all_tables(view: &NSView) -> Vec<Retained<NSTableView>> {
    let mut found = Vec::new();
    if let Ok(table) = Retained::downcast::<NSTableView>(view.retain()) {
        found.push(table);
    }
    for child in view.subviews().iter() {
        found.extend(all_tables(&child));
    }
    found
}

fn buttons(view: &NSView) -> Vec<Retained<NSButton>> {
    let mut found = Vec::new();
    if let Ok(button) = Retained::downcast::<NSButton>(view.retain()) {
        found.push(button);
    }
    for child in view.subviews().iter() {
        found.extend(buttons(&child));
    }
    found
}

/// The window's toolbar items, in order, with each group's items in place
/// of the group.
fn toolbar_items(content: &NSView) -> Vec<Retained<NSToolbarItem>> {
    let toolbar = content
        .window()
        .and_then(|window| window.toolbar())
        .expect("the window has a toolbar");
    toolbar
        .items()
        .iter()
        .flat_map(|item| match item.downcast_ref::<NSToolbarItemGroup>() {
            Some(group) => group.subitems().iter().collect(),
            None => vec![item],
        })
        .collect()
}

/// The field editor of the toolbar's search field, which takes the focus.
fn search_editor(content: &NSView) -> Retained<NSTextView> {
    let window = content.window().expect("the content is in a window");
    let field = window
        .toolbar()
        .expect("the window has a toolbar")
        .items()
        .iter()
        .find_map(|item| Some(item.downcast::<NSSearchToolbarItem>().ok()?.searchField()))
        .expect("the toolbar has a search field");
    if field.currentEditor().is_none() {
        window.makeFirstResponder(Some(&field));
    }
    field
        .currentEditor()
        .expect("the focused field has an editor")
        .downcast::<NSTextView>()
        .expect("a field editor is a text view")
}

/// The labels of the toolbar's groups, one list per group.
fn toolbar_groups(content: &NSView) -> Vec<Vec<String>> {
    let toolbar = content
        .window()
        .and_then(|window| window.toolbar())
        .expect("the window has a toolbar");
    toolbar
        .items()
        .iter()
        .filter_map(|item| {
            let group = item.downcast_ref::<NSToolbarItemGroup>()?;
            Some(
                group
                    .subitems()
                    .iter()
                    .map(|sub| sub.label().to_string())
                    .collect(),
            )
        })
        .collect()
}

/// The details pane: the split view's third pane.
fn details(content: &NSView) -> Retained<NSView> {
    pane(content, 2)
}

/// Pane `position` of the window's split view.
fn pane(content: &NSView, position: usize) -> Retained<NSView> {
    fn split(view: &NSView) -> Option<Retained<NSSplitView>> {
        if let Ok(split) = Retained::downcast::<NSSplitView>(view.retain()) {
            return Some(split);
        }
        view.subviews().iter().find_map(|child| split(&child))
    }
    let split = split(content).expect("the window has a split view");
    split.arrangedSubviews().objectAtIndex(position)
}

/// The text of every text field in `view`, depth first.
fn texts(view: &NSView) -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(field) = Retained::downcast::<NSTextField>(view.retain()) {
        found.push(field.stringValue().to_string());
    }
    for child in view.subviews().iter() {
        found.extend(texts(&child));
    }
    found
}

/// Prints the list's rows, with the selected one marked, and the details'
/// text and buttons.
fn dump(content: &NSView) {
    println!("-- list");
    // An empty list shows a note in place of its table.
    let list = all_tables(content)
        .into_iter()
        .find(|table| table.style() != NSTableViewStyle::SourceList);
    match list {
        Some(list) => {
            for row in 0..list.numberOfRows() {
                let mark = if list.selectedRow() == row { ">" } else { " " };
                let cell = list.viewAtColumn_row_makeIfNecessary(0, row, false);
                let text = cell.map(|cell| texts(&cell)).unwrap_or_default();
                println!("{mark} {}", text.join(" | "));
            }
        }
        None => println!("  note {:?}", texts(&pane(content, 1))),
    }
    let details = details(content);
    println!("-- details {:?}", texts(&details));
    let tips: Vec<String> = buttons(&details)
        .iter()
        .filter_map(|button| button.toolTip().map(|tip| tip.to_string()))
        .collect();
    println!("   buttons {tips:?}");
    let toolbar: Vec<String> = toolbar_items(content)
        .iter()
        .map(|item| {
            let state = if item.isEnabled() { "" } else { " (disabled)" };
            format!("{}{state}", item.label())
        })
        .collect();
    println!(
        "   toolbar {toolbar:?} groups {:?}",
        toolbar_groups(content)
    );
}

/// Two folders, one that did not decrypt, and items of several types: 10
/// live (3 favorites, 1 of an unknown type, 1 that did not decrypt) and 2
/// in the trash.
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
            // The logins: GitHub has a verification code too.
            copyable: match (kind, id) {
                (_, 11) => [CopyField::Username, CopyField::Password, CopyField::Totp]
                    .into_iter()
                    .collect(),
                (CipherKind::Login, _) => [CopyField::Username, CopyField::Password]
                    .into_iter()
                    .collect(),
                _ => Default::default(),
            },
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
                "Lima Studio",
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
                "SHA256:q3RvT1xY",
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
            item(11, CipherKind::Login, "GitHub", "alex-dev", 2, false, false),
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

/// The opened form of `summary`: GitHub has every login field and custom
/// fields, Mail server a user name and password, Personal Visa, Alex Lima
/// and Deploy key every field of their type, Home Wi-Fi asks for the
/// master password, and the rest have only a name.
fn view_of(summary: &CipherSummary) -> CipherView {
    let some = |text: &str| Some(secret(text));
    let content = match (summary.id.as_u128(), summary.kind) {
        (11, _) => CipherContent::Login(LoginView {
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
        (12, _) => CipherContent::Login(LoginView {
            username: some("alex@mail.example"),
            password: some("hunter2-but-longer"),
            ..LoginView::default()
        }),
        (13, _) => CipherContent::Card(CardView {
            cardholder_name: some("Alex M. Example"),
            brand: some("Visa"),
            number: some("4242424242424242"),
            exp_month: some("8"),
            exp_year: some("2029"),
            code: some("123"),
        }),
        (14, _) => CipherContent::Identity(Box::new(IdentityView {
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
        (16, _) => CipherContent::SshKey(SshKeyView {
            private_key: some("-----BEGIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEAAAAA"),
            public_key: some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJq3RvT1xb8fKc build@chaveiro"),
            key_fingerprint: some("SHA256:q3RvT1xYk0w2pV7sDm4hN9eLa6uZc3Jg5Fb8fKc"),
        }),
        (_, CipherKind::Login) => CipherContent::Login(LoginView::default()),
        (_, CipherKind::SecureNote) => CipherContent::SecureNote,
        _ => CipherContent::Unknown,
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
                name: some("Org admin"),
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
            11 => some("Two-step is on.\nRecovery codes are in the safe."),
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

fn secret(text: &str) -> SecretString {
    SecretString::new(text.to_owned())
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
