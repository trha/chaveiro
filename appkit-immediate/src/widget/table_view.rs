use super::{Control, Response, must_write};
use crate::{
    App,
    events::EventKind,
    pass::{ChildKind, ControlNode, Payload, Ui},
    tree::{Key, NodeId, Visit},
};
use ahash::AHashMap;
use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_app_kit::{
    NSBackgroundStyle, NSControl, NSScrollView, NSTableCellView, NSTableColumn, NSTableView,
    NSTableViewColumnAutoresizingStyle, NSTableViewStyle, NSView,
};
use objc2_foundation::{NSObjectProtocol, NSString};
use std::{
    any::Any,
    hash::Hash,
    ops::{Deref, DerefMut},
    rc::Rc,
};

/// Builds row `index` of the table view `TableViewId` from the app's state.
/// Given to [`Ui::table_view`] as a plain function, because AppKit asks for
/// rows during layout, outside [`App::view`], and a function pointer can
/// capture no borrowed data.
pub type RowFn<A> = fn(&mut A, &mut Ui, TableViewId, usize);

impl Ui {
    /// A scrolling table view of `len` rows, one column wide, that fills the
    /// space its container gives it; put it in a
    /// [`Length::Star`](crate::Length::Star) grid row.
    ///
    /// AppKit decides which rows exist. For a row it shows, the crate calls
    /// `row` during layout, and again after every [`App::view`] so the row
    /// shows current state. `row` must declare exactly one control, whose
    /// constraints give the row its height. A row that scrolls out of view
    /// keeps its controls for the next row that scrolls in, so UI state for
    /// an item belongs in [`Ui::table_view_state`], not in [`Ui::state`]
    /// inside the row.
    ///
    /// `revision` must change whenever the order or identity of the items
    /// changes; AppKit then reloads every row.
    ///
    /// The user can select one row; see [`TableViewResponse::selection`].
    pub fn table_view<A: App>(
        &mut self,
        id: impl Hash,
        len: usize,
        revision: u64,
        row: RowFn<A>,
    ) -> TableViewResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let (scroll, table) = table_views(mtm);
            ControlNode::payload(
                Retained::clone(&scroll).into_super(),
                Control::TableView {
                    table,
                    data: Box::new(TableData {
                        row: erase(row),
                        row_address: row as usize,
                        realized: Vec::new(),
                        free: Vec::new(),
                        next_slot: 0,
                        len: 0,
                        revision: 0,
                        loaded: false,
                        reloading: false,
                        selected: None,
                        inset: false,
                        item_state: AHashMap::new(),
                    }),
                },
            )
        });
        let Control::TableView { table, data } = &mut self.control_mut(node).control else {
            unreachable!("table view keys hold table views")
        };
        let table = table.clone();
        if data.row_address != row as usize {
            data.row = erase(row);
            data.row_address = row as usize;
        }
        let reload = !data.loaded || data.len != len || data.revision != revision;
        data.loaded = true;
        data.len = len;
        data.revision = revision;
        data.reloading = reload;
        if visit == Visit::Created {
            self.events.register(&table, node);
            // AppKit holds both weakly; `Events` keeps the target alive.
            unsafe {
                table.setDataSource(Some(ProtocolObject::from_ref(self.events.target())));
                table.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
            }
        }
        if reload {
            self.events.set_row_count(&table, len);
            // AppKit asks for rows inside `reloadData`, so the runner calls
            // it after the pass, with no borrow held.
            self.reloads.push(table);
        }
        // Keep every row alive: AppKit shows the realized ones, and the free
        // ones wait for reuse.
        self.open(node);
        for position in 0..self.table_data(node).slots() {
            let slot = self.table_data(node).slot_at(position);
            self.visit(("row", slot), ChildKind::Logical, |_| {
                unreachable!("every listed row is live")
            });
        }
        self.close();
        self.tables.push(node);
        TableViewResponse(Response { ui: self, node })
    }

    /// UI state for the item `item` of `table`, such as whether it is
    /// expanded. `item` is the app's stable key for the item, not its row
    /// index. The state follows the item whichever row shows it, and lives
    /// until the table view is dropped or
    /// [`retain_table_view_state`](Self::retain_table_view_state) removes
    /// it. `init` runs the first time an item asks.
    ///
    /// Panics when the item held state of another type.
    pub fn table_view_state<T: 'static>(
        &mut self,
        table: TableViewId,
        item: u64,
        init: impl FnOnce() -> T,
    ) -> &mut T {
        self.table_data_mut(table.0)
            .item_state
            .entry(item)
            .or_insert_with(|| Box::new(init()))
            .downcast_mut::<T>()
            .expect("an item keeps one state type")
    }

    /// Drops the [`table_view_state`](Self::table_view_state) of every item
    /// for which `keep` returns false.
    pub fn retain_table_view_state(
        &mut self,
        table: TableViewId,
        mut keep: impl FnMut(u64) -> bool,
    ) {
        self.table_data_mut(table.0)
            .item_state
            .retain(|item, _| keep(*item));
    }

    /// The number of row views `table` keeps, shown or free for reuse: about
    /// one screen of rows, however long the table is.
    pub fn table_view_slots(&self, table: TableViewId) -> usize {
        self.table_data(table.0).slots()
    }

    /// Builds every realized row of every table view declared this pass.
    /// Runs after [`App::view`], with the app the row functions expect.
    pub(crate) fn build_rows(&mut self, app: &mut dyn Any) {
        for table_position in 0..self.tables.len() {
            let table = self.tables[table_position];
            for row_position in 0..self.table_data(table).realized.len() {
                let row = self.table_data(table).realized[row_position];
                if row.index < self.table_data(table).len {
                    self.build_row(table, row.node, row.index, app);
                }
            }
        }
    }

    /// Builds row `index` of `table` for AppKit, reusing a free row when
    /// there is one, and returns its view. Runs during layout, between
    /// passes.
    pub(crate) fn realize(
        &mut self,
        table: NodeId,
        index: usize,
        app: &mut dyn Any,
    ) -> Retained<NSView> {
        // AppKit asks between passes, so the last pass's stamps are still
        // current; a reused row's controls carry them. A fresh pass number
        // lets the row be built again.
        self.tree.begin_pass();
        let (node, slot) = match self.table_data_mut(table).free.pop() {
            Some(free) => free,
            None => {
                let data = self.table_data_mut(table);
                let slot = data.next_slot;
                data.next_slot += 1;
                let shell: Retained<RowShell> =
                    unsafe { msg_send![RowShell::alloc(self.mtm), init] };
                let shell = shell.into_super().into_super();
                let key = Key::child(self.tree.node(table).key(), ("row", slot));
                let Payload::Control(row) = ControlNode::payload(
                    shell.clone(),
                    Control::Row {
                        shell,
                        content: None,
                        pins: Vec::new(),
                    },
                ) else {
                    unreachable!("payload makes control payloads")
                };
                // The table sizes the cell with its autoresizing mask.
                row.view.setTranslatesAutoresizingMaskIntoConstraints(true);
                (self.tree.adopt(table, key, Payload::Control(row)), slot)
            }
        };
        self.build_row(table, node, index, app);
        self.table_data_mut(table)
            .realized
            .push(RealizedRow { node, slot, index });
        self.control(node).view.clone()
    }

    /// Moves the row whose view AppKit removed back to the free rows. A view
    /// this table never realized, such as a placeholder, is ignored.
    pub(crate) fn recycle(&mut self, table: NodeId, view: usize) {
        let position = (0..self.table_data(table).realized.len()).find(|&position| {
            let node = self.table_data(table).realized[position].node;
            Retained::as_ptr(&self.control(node).view).addr() == view
        });
        if let Some(position) = position {
            let data = self.table_data_mut(table);
            let row = data.realized.swap_remove(position);
            data.free.push((row.node, row.slot));
        }
    }

    /// Whether `table` is still a live table view, for calls AppKit makes
    /// after a pass dropped it, and the row index it may show.
    pub(crate) fn table_len(&self, table: NodeId) -> Option<usize> {
        match self.tree.get(table).map(|node| &node.payload) {
            Some(Payload::Control(ControlNode {
                control: Control::TableView { data, .. },
                ..
            })) => Some(data.len),
            _ => None,
        }
    }

    /// The table view of the node `table`, while it is live.
    pub(crate) fn table_of(&self, table: NodeId) -> Option<Retained<NSTableView>> {
        match self.tree.get(table).map(|node| &node.payload) {
            Some(Payload::Control(ControlNode {
                control: Control::TableView { table, .. },
                ..
            })) => Some(table.clone()),
            _ => None,
        }
    }

    fn build_row(&mut self, table: NodeId, row: NodeId, index: usize, app: &mut dyn Any) {
        let build = Rc::clone(&self.table_data(table).row);
        self.open(row);
        build(app, self, TableViewId(table), index);
        self.close();
    }

    fn table_data(&self, table: NodeId) -> &TableData {
        match &self.control(table).control {
            Control::TableView { data, .. } => data,
            _ => unreachable!("table view ids name table views"),
        }
    }

    fn table_data_mut(&mut self, table: NodeId) -> &mut TableData {
        match &mut self.control_mut(table).control {
            Control::TableView { data, .. } => data,
            _ => unreachable!("table view ids name table views"),
        }
    }
}

/// What [`Ui::table_view`] returns.
pub struct TableViewResponse<'u>(Response<'u>);

impl TableViewResponse<'_> {
    /// Names the table view for [`Ui::table_view_state`] and
    /// [`Ui::table_view_slots`].
    pub fn id(&self) -> TableViewId {
        TableViewId(self.0.node)
    }

    /// Shows the rows inset from the table's edges, with rounded selection,
    /// as a list beside a sidebar does. A table starts plain.
    pub fn inset(&mut self, on: bool) -> &mut Self {
        let node = self.0.node;
        let Control::TableView { table, data } = &mut self.0.ui.control_mut(node).control else {
            unreachable!("table view keys hold table views")
        };
        if must_write(node, false, &data.inset, &on) {
            table.setStyle(match on {
                true => NSTableViewStyle::Inset,
                false => NSTableViewStyle::Plain,
            });
            data.inset = on;
        }
        self
    }

    /// The selected row. The user's pick, by click or arrow key, is copied
    /// into `selected`, and a pick runs a pass. The table shows `selected`
    /// once the pass ends, scrolled into view when the app changed it. A
    /// row past the end shows no selection. Returns whether the user picked
    /// since the last pass.
    ///
    /// A pick in the pass that changes the rows is dropped, because the row
    /// it named may now show another item.
    pub fn selection(&mut self, selected: &mut Option<usize>) -> bool {
        let node = self.0.node;
        let ui = &mut *self.0.ui;
        let event = ui.events.happened(node, EventKind::SelectionDidChange);
        let Control::TableView { table, data } = &mut ui.control_mut(node).control else {
            unreachable!("table view keys hold table views")
        };
        // The crate's own changes post the event too: a written selection,
        // and a reload that dropped the selected row, which the selection
        // written after it restores. Either way the table then shows what
        // was written last, so only a differing row is the user's pick.
        let shown = usize::try_from(table.selectedRow()).ok();
        let picked = event && !data.reloading && data.selected != Some(shown);
        if picked {
            *selected = shown;
            data.selected = Some(shown);
        }
        let declared = selected.filter(|&row| row < data.len);
        // A reload may keep a row selected that now shows another item, so
        // the selection is written again after each one.
        if must_write(node, data.reloading, &data.selected, &Some(declared)) {
            data.selected = Some(declared);
            let table = table.clone();
            ui.selections.push((table, declared));
        }
        picked
    }
}

impl<'u> Deref for TableViewResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for TableViewResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}

/// Names a table view for [`Ui::table_view_state`].
#[derive(Clone, Copy, Debug)]
pub struct TableViewId(NodeId);

/// The rows of one table view.
///
/// Contract: every row node of the table is in exactly one of
/// [`realized`](Self::realized) and [`free`](Self::free), and is a child of
/// the table node in the tree.
pub(crate) struct TableData {
    /// The app's row function, with the app's type erased.
    row: ErasedRow,
    /// The address of the function `row` wraps, to notice a new one.
    row_address: usize,
    /// Rows AppKit shows. About one screen of rows, so a `Vec` beats a map.
    realized: Vec<RealizedRow>,
    /// Rows AppKit removed, with their slots, kept for reuse.
    free: Vec<(NodeId, u32)>,
    next_slot: u32,
    len: usize,
    revision: u64,
    /// Whether the table was loaded yet.
    loaded: bool,
    /// This pass reloads the table.
    reloading: bool,
    /// The selection last written or read back; [`None`] until the app
    /// first declares one.
    selected: Option<Option<usize>>,
    inset: bool,
    item_state: AHashMap<u64, Box<dyn Any>>,
}

impl TableData {
    fn slots(&self) -> usize {
        self.realized.len() + self.free.len()
    }

    /// The slot of the row at `position` in realized-then-free order.
    fn slot_at(&self, position: usize) -> u32 {
        match self.realized.get(position) {
            Some(row) => row.slot,
            None => self.free[position - self.realized.len()].1,
        }
    }
}

#[derive(Clone, Copy)]
struct RealizedRow {
    node: NodeId,
    /// Fixes the row's key: `Key::child(table key, ("row", slot))`.
    slot: u32,
    /// The item it shows.
    index: usize,
}

define_class!(
    /// The cell view of one row. The row view tells it the background
    /// style, emphasized on a selected row of a focused table, and it
    /// passes the style to every control inside it, so system colors such
    /// as the label color turn light on the selection. AppKit's own cell
    /// view tells only its text field and image view.
    #[unsafe(super(NSTableCellView, NSView))]
    #[thread_kind = MainThreadOnly]
    struct RowShell;

    unsafe impl NSObjectProtocol for RowShell {}

    impl RowShell {
        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, style: NSBackgroundStyle) {
            let _: () = unsafe { msg_send![super(self), setBackgroundStyle: style] };
            pass_background_style(self, style);
        }
    }
);

/// Sets `style` on the cell of every control inside `view`.
fn pass_background_style(view: &NSView, style: NSBackgroundStyle) {
    for child in view.subviews().iter() {
        if let Some(cell) = child
            .downcast_ref::<NSControl>()
            .and_then(|control| control.cell())
        {
            cell.setBackgroundStyle(style);
        }
        pass_background_style(&child, style);
    }
}

/// A one-column, headerless, plain table view with automatic row heights
/// in a scroll view.
fn table_views(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<NSTableView>) {
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    let table = NSTableView::new(mtm);
    let column =
        NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str("rows"));
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setStyle(NSTableViewStyle::Plain);
    table.setUsesAutomaticRowHeights(true);
    table.setColumnAutoresizingStyle(
        NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle,
    );
    scroll.setDocumentView(Some(&table));
    (scroll, table)
}

/// A [`RowFn`] that takes the app as [`Any`], so [`TableData`] needs no app
/// type.
type ErasedRow = Rc<dyn Fn(&mut dyn Any, &mut Ui, TableViewId, usize)>;

/// Wraps `row` for an app seen as [`Any`].
fn erase<A: App>(row: RowFn<A>) -> ErasedRow {
    Rc::new(move |app: &mut dyn Any, ui: &mut Ui, table, index| {
        let app = app
            .downcast_mut::<A>()
            .expect("a table's rows are built with the app that declared it");
        row(app, ui, table, index);
    })
}
