use super::{Control, Response, must_write};
use crate::{
    events::{EventKind, Target},
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject,
};
use objc2_app_kit::{
    NSColor, NSControlTextEditingDelegate, NSImage, NSImageView, NSLayoutConstraint,
    NSLayoutConstraintOrientation, NSScrollView, NSTableCellView, NSTableColumn, NSTableView,
    NSTableViewColumnAutoresizingStyle, NSTableViewDataSource, NSTableViewDelegate,
    NSTableViewRowSizeStyle, NSTableViewStyle, NSTextField, NSView,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSString,
};
use std::{
    cell::{Cell, RefCell},
    hash::Hash,
    ops::{Deref, DerefMut},
};

impl Ui {
    /// A sidebar source list: group headers, and items with a symbol, a
    /// label and an optional count, that fills the space its container
    /// gives it. Its background is clear, so it shows the sidebar
    /// material of a [`split_view`](Self::split_view)'s first pane.
    ///
    /// `entries` runs only when `revision` differs from the last pass's,
    /// so a pass with the same revision builds nothing. `revision` must
    /// change whenever any entry does.
    ///
    /// `selected` is the position of the selected entry among all the
    /// entries `entries` pushes. The user's pick is copied into it, and a
    /// pick runs a pass. The pass writes `selected` into the list only when
    /// the app changed it, or the entries changed. A header, a spacer or a
    /// position past the end shows no selection.
    pub fn source_list(
        &mut self,
        id: impl Hash,
        revision: u64,
        selected: &mut Option<usize>,
        entries: impl FnOnce(&mut SourceListEntries<'_>),
    ) -> SourceListResponse<'_> {
        // A retain and release per pass; the source keeps it when made.
        let target = self.events.target().retain();
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let (scroll, table) = views(mtm);
            let source = Source::alloc(mtm).set_ivars(SourceIvars {
                entries: RefCell::new(Vec::new()),
                selecting: Cell::new(false),
                target,
            });
            let source: Retained<Source> = unsafe { msg_send![super(source), init] };
            ControlNode::payload(
                Retained::clone(&scroll).into_super(),
                Control::SourceList {
                    table,
                    source,
                    revision: None,
                    selected: None,
                },
            )
        });
        if visit == Visit::Created {
            let Control::SourceList { table, source, .. } = &self.control(node).control else {
                unreachable!("source list keys hold source lists")
            };
            // AppKit holds both weakly; the control keeps the source.
            unsafe {
                table.setDataSource(Some(ProtocolObject::from_ref(&**source)));
                table.setDelegate(Some(ProtocolObject::from_ref(&**source)));
            }
            self.events.register(table, node);
        }
        let picked = self.events.happened(node, EventKind::SelectionDidChange);
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::SourceList {
            table,
            source,
            revision: written_revision,
            selected: written_selected,
        } = &mut control.control
        else {
            unreachable!("source list keys hold source lists")
        };
        let reload = must_write(node, stale, written_revision, &Some(revision));
        if reload {
            {
                let mut stored = source.ivars().entries.borrow_mut();
                stored.clear();
                entries(&mut SourceListEntries {
                    entries: &mut stored,
                });
            }
            // The source holds no borrow of the `Ui`, so AppKit may ask it
            // for rows inside `reloadData`.
            source.ivars().selecting.set(true);
            table.reloadData();
            source.ivars().selecting.set(false);
            *written_revision = Some(revision);
        }
        let selectable = |position: Option<usize>| {
            position.filter(|&position| {
                matches!(
                    source.ivars().entries.borrow().get(position),
                    Some(Entry::Item { .. })
                )
            })
        };
        if picked && !reload {
            // Only code can select a row that is no item; that shows none.
            let shown = selectable(usize::try_from(table.selectedRow()).ok());
            *selected = shown;
        }
        let declared = selectable(*selected);
        if must_write(node, reload, written_selected, &declared) {
            source.ivars().selecting.set(true);
            match declared {
                Some(position) => table.selectRowIndexes_byExtendingSelection(
                    &NSIndexSet::indexSetWithIndex(position),
                    false,
                ),
                None => unsafe { table.deselectAll(None) },
            }
            source.ivars().selecting.set(false);
            *written_selected = declared;
        }
        SourceListResponse {
            response: Response { ui: self, node },
            picked: picked && !reload,
        }
    }
}

/// Where [`Ui::source_list`]'s `entries` pushes the list, top to bottom.
pub struct SourceListEntries<'e> {
    entries: &'e mut Vec<Entry>,
}

impl SourceListEntries<'_> {
    /// A group header, such as "Folders", which the user cannot select.
    pub fn header(&mut self, label: &str) {
        self.entries.push(Entry::Header(label.to_owned()));
    }

    /// An item the user can select. `symbol` is an SF Symbols name, such as
    /// `"folder"`; a name the system does not have shows no image.
    pub fn item(&mut self, symbol: &str, label: &str, count: Option<usize>) {
        self.entries.push(Entry::Item {
            symbol: symbol.to_owned(),
            label: label.to_owned(),
            count,
        });
    }

    /// An empty row the user cannot select, to set a group apart.
    pub fn spacer(&mut self) {
        self.entries.push(Entry::Spacer);
    }
}

pub(crate) enum Entry {
    Header(String),
    Item {
        symbol: String,
        label: String,
        count: Option<usize>,
    },
    Spacer,
}

/// What [`Ui::source_list`] returns.
pub struct SourceListResponse<'u> {
    response: Response<'u>,
    picked: bool,
}

impl SourceListResponse<'_> {
    /// Whether the user changed the selection since the last pass.
    pub fn picked(&self) -> bool {
        self.picked
    }
}

impl<'u> Deref for SourceListResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for SourceListResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}

pub(crate) struct SourceIvars {
    /// The entries as last pushed, which the rows show.
    entries: RefCell<Vec<Entry>>,
    /// Set while the widget changes the selection or reloads, so only the
    /// user's picks are reported.
    selecting: Cell<bool>,
    /// Where picks are reported.
    target: Retained<Target>,
}

define_class!(
    /// The data source and delegate of one source list. It answers AppKit
    /// from its own copy of the entries, so it needs no access to the `Ui`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SourceIvars]
    pub(crate) struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSControlTextEditingDelegate for Source {}

    unsafe impl NSTableViewDataSource for Source {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().entries.borrow().len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for Source {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            // The macro wraps the body, so `?` is not available here.
            let entries = self.ivars().entries.borrow();
            let entry = usize::try_from(row).ok().and_then(|row| entries.get(row));
            entry.map(|entry| cell(self.mtm(), entry))
        }

        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(self.entry_kind(row), Some(Kind::Header))
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(self.entry_kind(row), Some(Kind::Item))
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, notification: &NSNotification) {
            let ivars = self.ivars();
            if ivars.selecting.get() {
                return;
            }
            if let Some(table) = notification.object() {
                ivars.target.push(&table, EventKind::SelectionDidChange);
            }
        }
    }
);

#[derive(Clone, Copy)]
enum Kind {
    Header,
    Item,
    Spacer,
}

impl Source {
    fn entry_kind(&self, row: NSInteger) -> Option<Kind> {
        let entries = self.ivars().entries.borrow();
        Some(match entries.get(usize::try_from(row).ok()?)? {
            Entry::Header(_) => Kind::Header,
            Entry::Item { .. } => Kind::Item,
            Entry::Spacer => Kind::Spacer,
        })
    }
}

/// A one-column, headerless source list table with a clear background, in
/// a scroll view that draws none either.
fn views(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<NSTableView>) {
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    let table = NSTableView::new(mtm);
    let column =
        NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str("rows"));
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setStyle(NSTableViewStyle::SourceList);
    table.setRowSizeStyle(NSTableViewRowSizeStyle::Default);
    table.setFloatsGroupRows(false);
    table.setBackgroundColor(&NSColor::clearColor());
    table.setColumnAutoresizingStyle(
        NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle,
    );
    scroll.setDocumentView(Some(&table));
    (scroll, table)
}

/// The cell view of one entry. The table sizes it with its autoresizing
/// mask; constraints place its subviews.
fn cell(mtm: MainThreadMarker, entry: &Entry) -> Retained<NSView> {
    let cell = NSTableCellView::new(mtm);
    let label = |text: &str| {
        let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        field.setTranslatesAutoresizingMaskIntoConstraints(false);
        cell.addSubview(&field);
        field
    };
    let mut constraints = Vec::new();
    match entry {
        Entry::Spacer => {}
        Entry::Header(text) => {
            let field = label(text);
            constraints.extend([
                field
                    .leadingAnchor()
                    .constraintEqualToAnchor_constant(&cell.leadingAnchor(), 4.0),
                field
                    .trailingAnchor()
                    .constraintLessThanOrEqualToAnchor(&cell.trailingAnchor()),
                field
                    .centerYAnchor()
                    .constraintEqualToAnchor(&cell.centerYAnchor()),
            ]);
            unsafe { cell.setTextField(Some(&field)) };
        }
        Entry::Item {
            symbol,
            label: text,
            count,
        } => {
            let image = NSImageView::new(mtm);
            image.setTranslatesAutoresizingMaskIntoConstraints(false);
            image.setImage(
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(symbol),
                    None,
                )
                .as_deref(),
            );
            cell.addSubview(&image);
            let field = label(text);
            // The label shortens before the count does.
            field.setContentCompressionResistancePriority_forOrientation(
                250.0,
                NSLayoutConstraintOrientation::Horizontal,
            );
            constraints.extend([
                image
                    .leadingAnchor()
                    .constraintEqualToAnchor_constant(&cell.leadingAnchor(), 2.0),
                image
                    .centerYAnchor()
                    .constraintEqualToAnchor(&cell.centerYAnchor()),
                image.widthAnchor().constraintEqualToConstant(18.0),
                field
                    .leadingAnchor()
                    .constraintEqualToAnchor_constant(&image.trailingAnchor(), 6.0),
                field
                    .centerYAnchor()
                    .constraintEqualToAnchor(&cell.centerYAnchor()),
            ]);
            let end = match count {
                Some(count) => {
                    let badge = label(&count.to_string());
                    badge.setTextColor(Some(&NSColor::secondaryLabelColor()));
                    constraints.extend([
                        badge
                            .trailingAnchor()
                            .constraintEqualToAnchor_constant(&cell.trailingAnchor(), -6.0),
                        badge
                            .centerYAnchor()
                            .constraintEqualToAnchor(&cell.centerYAnchor()),
                    ]);
                    badge.leadingAnchor()
                }
                None => cell.trailingAnchor(),
            };
            constraints.push(
                field
                    .trailingAnchor()
                    .constraintLessThanOrEqualToAnchor_constant(&end, -6.0),
            );
            unsafe {
                cell.setImageView(Some(&image));
                cell.setTextField(Some(&field));
            }
        }
    }
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    cell.into_super()
}
