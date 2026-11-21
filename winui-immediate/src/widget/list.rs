use super::Control;
use crate::{
    App,
    bindings::{
        ContentControl, Control as XamlControl, ElementFactoryGetArgs, ElementFactoryRecycleArgs,
        HorizontalAlignment, IElementFactory, IElementFactory_Impl, ItemsRepeater, ScrollViewer,
        UIElement,
    },
    pass::{ChildKind, ControlNode, Payload, Ui},
    runner,
    tree::{Key, NodeId, Visit},
};
use ahash::AHashMap;
use std::{any::Any, hash::Hash, rc::Rc};
use windows_core::{ComObject, IInspectable, Interface, Ref, Result, implement_decl};

/// Builds row `index` of the list `ListId` from the app's state. Given to [`Ui::list`]
/// as a plain function, because WinUI asks for rows during layout, outside
/// [`App::view`], and a function pointer can capture no borrowed data.
pub type RowFn<A> = fn(&mut A, &mut Ui, ListId, usize) -> Result<()>;

impl Ui {
    /// A scrolling list of `len` rows that fills the space its parent gives
    /// it; put it in a [`Length::Star`](crate::Length::Star) grid row.
    ///
    /// WinUI decides which rows exist. For a row it shows, the crate calls
    /// `row` at once, during layout, and again after every [`App::view`] so
    /// the row shows current state. `row` must declare exactly one control.
    /// A row that scrolls out of view keeps its controls for the next row that
    /// scrolls in, so UI state for an item belongs in [`Ui::list_state`], not
    /// in [`Ui::state`] inside the row.
    ///
    /// `revision` must change whenever the order or identity of the items
    /// changes; WinUI then rebuilds every row.
    pub fn list<A: App>(
        &mut self,
        id: impl Hash,
        len: usize,
        revision: u64,
        row: RowFn<A>,
    ) -> Result<ListId> {
        let (node, visit) = self.visit(id, ChildKind::Visual, || {
            let scroll = ScrollViewer::new()?;
            let repeater = ItemsRepeater::new()?;
            scroll.cast::<ContentControl>()?.SetContent(&repeater)?;
            Ok(ControlNode::new(
                scroll.cast()?,
                Control::List {
                    repeater,
                    data: Box::new(ListData {
                        row: erase(row),
                        row_address: row as usize,
                        realized: Vec::new(),
                        free: Vec::new(),
                        next_slot: 0,
                        len: 0,
                        revision: 0,
                        sourced: false,
                        item_state: AHashMap::new(),
                    }),
                },
            ))
        })?;
        let Control::List { repeater, data } = &mut self.control_mut(node).control else {
            unreachable!("list keys hold lists")
        };
        if visit == Visit::Created {
            let factory: IElementFactory =
                ComObject::new(RowFactory { list: node }).into_interface();
            repeater.SetItemTemplate(&factory.cast::<IInspectable>()?)?;
        }
        if data.row_address != row as usize {
            data.row = erase(row);
            data.row_address = row as usize;
        }
        let source_changed = !data.sourced || data.len != len || data.revision != revision;
        let repeater = repeater.clone();
        if source_changed {
            data.sourced = true;
            data.len = len;
            data.revision = revision;
            // WinUI may ask for rows inside `SetItemsSource`, so the runner
            // sets it after the pass, with no borrow held.
            self.sources.push((repeater, len));
        }
        // Keep every row alive: WinUI holds the realized ones, and the free
        // ones wait for reuse.
        self.open(node);
        for position in 0..self.list_data(node).slots() {
            let slot = self.list_data(node).slot_at(position);
            self.visit(("row", slot), ChildKind::Logical, || {
                unreachable!("every listed row is live")
            })?;
        }
        self.close()?;
        self.lists.push(node);
        Ok(ListId(node))
    }

    /// UI state for the item `item` of `list`, such as whether it is
    /// expanded. `item` is the app's stable key for the item, not its row
    /// index. The state follows the item whichever row shows it, and lives
    /// until the list is dropped or [`retain_list_state`](Self::retain_list_state)
    /// removes it. `init` runs the first time an item asks.
    ///
    /// Panics when the item held state of another type.
    pub fn list_state<T: 'static>(
        &mut self,
        list: ListId,
        item: u64,
        init: impl FnOnce() -> T,
    ) -> &mut T {
        self.list_data_mut(list.0)
            .item_state
            .entry(item)
            .or_insert_with(|| Box::new(init()))
            .downcast_mut::<T>()
            .expect("an item keeps one state type")
    }

    /// Scrolls `list` so row `index` shows, after this pass; WinUI builds
    /// the row first if it does not exist. An index past the end does
    /// nothing.
    pub fn bring_into_view(&mut self, list: ListId, index: usize) {
        let Control::List { repeater, data } = &self.control(list.0).control else {
            unreachable!("list ids name lists")
        };
        if index < data.len {
            self.reveals.push((repeater.clone(), index));
        }
    }

    /// Drops the [`list_state`](Self::list_state) of every item for which
    /// `keep` returns false. Call it when items leave the app's data;
    /// otherwise their state stays until the list is dropped.
    pub fn retain_list_state(&mut self, list: ListId, mut keep: impl FnMut(u64) -> bool) {
        self.list_data_mut(list.0)
            .item_state
            .retain(|item, _| keep(*item));
    }

    /// Builds every realized row of every list declared this pass. Runs after
    /// [`App::view`], with the app the row functions expect.
    pub(crate) fn build_rows(&mut self, app: &mut dyn Any) -> Result<()> {
        for list_position in 0..self.lists.len() {
            let list = self.lists[list_position];
            for row_position in 0..self.list_data(list).realized.len() {
                let row = self.list_data(list).realized[row_position];
                if row.index < self.list_data(list).len {
                    self.build_row(list, row.node, row.index, app)?;
                }
            }
        }
        Ok(())
    }

    /// Builds row `index` of `list` for WinUI, reusing a free row when there
    /// is one, and returns its element. Runs during layout.
    pub(crate) fn realize(
        &mut self,
        list: NodeId,
        index: usize,
        app: &mut dyn Any,
    ) -> Result<UIElement> {
        // WinUI asks between passes, so the last pass's stamps are still
        // current; a reused row's controls carry them. A fresh pass number
        // lets the row be built again. Nothing else reads stamps between
        // passes.
        self.tree.begin_pass();
        let (node, slot) = match self.list_data_mut(list).free.pop() {
            Some(free) => free,
            None => {
                let data = self.list_data_mut(list);
                let slot = data.next_slot;
                data.next_slot += 1;
                let shell = ContentControl::new()?;
                // A row's control spans the row, as in a WinUI list.
                shell
                    .cast::<XamlControl>()?
                    .SetHorizontalContentAlignment(HorizontalAlignment::Stretch)?;
                let key = Key::child(self.tree.node(list).key(), ("row", slot));
                let payload = ControlNode::new(shell.cast()?, Control::Row { content: None });
                (self.tree.adopt(list, key, payload), slot)
            }
        };
        let depth = self.depth();
        if let Err(error) = self.build_row(list, node, index, app) {
            // No pass runs now to clean up, so close what the row opened,
            // and keep the row free: WinUI never gets its element, so it
            // would never recycle it.
            self.unwind(depth);
            self.list_data_mut(list).free.push((node, slot));
            return Err(error);
        }
        self.list_data_mut(list)
            .realized
            .push(RealizedRow { node, slot, index });
        Ok(self.control(node).element.clone())
    }

    /// Moves the row whose element WinUI recycled back to the free rows.
    pub(crate) fn recycle(&mut self, list: NodeId, element: &UIElement) {
        let position = (0..self.list_data(list).realized.len()).find(|&position| {
            let node = self.list_data(list).realized[position].node;
            self.control(node).element == *element
        });
        // An element this list never realized has nothing to return.
        if let Some(position) = position {
            let data = self.list_data_mut(list);
            let row = data.realized.swap_remove(position);
            data.free.push((row.node, row.slot));
        }
    }

    /// Whether `list` is still a live list, for calls WinUI makes after a
    /// pass dropped it.
    pub(crate) fn is_list(&self, list: NodeId) -> bool {
        matches!(
            self.tree.get(list).map(|node| &node.payload),
            Some(Payload::Control(ControlNode {
                control: Control::List { .. },
                ..
            }))
        )
    }

    fn build_row(
        &mut self,
        list: NodeId,
        row: NodeId,
        index: usize,
        app: &mut dyn Any,
    ) -> Result<()> {
        let build = Rc::clone(&self.list_data(list).row);
        self.open(row);
        build(app, self, ListId(list), index)?;
        self.close()
    }

    fn list_data(&self, list: NodeId) -> &ListData {
        match &self.control(list).control {
            Control::List { data, .. } => data,
            _ => unreachable!("list ids name lists"),
        }
    }

    fn list_data_mut(&mut self, list: NodeId) -> &mut ListData {
        match &mut self.control_mut(list).control {
            Control::List { data, .. } => data,
            _ => unreachable!("list ids name lists"),
        }
    }
}

/// Names a list for [`Ui::list_state`].
#[derive(Clone, Copy, Debug)]
pub struct ListId(NodeId);

/// The rows of one list.
///
/// Contract: every row node of the list is in exactly one of
/// [`realized`](Self::realized) and [`free`](Self::free), and is a child of
/// the list node in the tree.
pub(crate) struct ListData {
    /// The app's row function, with the app's type erased.
    row: Rc<dyn Fn(&mut dyn Any, &mut Ui, ListId, usize) -> Result<()>>,
    /// The address of the function `row` wraps, to notice a new one.
    row_address: usize,
    /// Rows WinUI shows. About one screen of rows, so a `Vec` beats a map.
    realized: Vec<RealizedRow>,
    /// Rows WinUI recycled, with their slots, kept for reuse.
    free: Vec<(NodeId, u32)>,
    next_slot: u32,
    len: usize,
    revision: u64,
    /// Whether the repeater was given a source yet.
    sourced: bool,
    item_state: AHashMap<u64, Box<dyn Any>>,
}

impl ListData {
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
    /// Fixes the row's key: `Key::child(list key, ("row", slot))`.
    slot: u32,
    /// The item it shows.
    index: usize,
}

/// Wraps `row` for an app seen as [`Any`].
fn erase<A: App>(row: RowFn<A>) -> Rc<dyn Fn(&mut dyn Any, &mut Ui, ListId, usize) -> Result<()>> {
    Rc::new(move |app: &mut dyn Any, ui: &mut Ui, list, index| {
        let app = app
            .downcast_mut::<A>()
            .expect("a list's rows are built with the app that declared it");
        row(app, ui, list, index)
    })
}

implement_decl! {
    impl RowFactory as RowFactory_Impl: [IElementFactory]
}

/// The element factory WinUI asks for rows; it forwards to the runner.
struct RowFactory {
    list: NodeId,
}

impl IElementFactory_Impl for RowFactory_Impl {
    fn GetElement(&self, arguments: Ref<'_, ElementFactoryGetArgs>) -> Result<UIElement> {
        let index = arguments
            .ok()?
            .Data()?
            .cast::<windows_reference::IReference<i32>>()?
            .Value()?;
        let index = usize::try_from(index).expect("the source holds indexes from 0");
        runner::realize_row(self.list, index)
    }

    fn RecycleElement(&self, arguments: Ref<'_, ElementFactoryRecycleArgs>) -> Result<()> {
        runner::recycle_row(self.list, &arguments.ok()?.Element()?);
        Ok(())
    }
}

/// The `ItemsSource` for `len` rows: item `index` is `index`, boxed, because
/// the crate builds each row from its index.
pub(crate) fn index_source(len: usize) -> windows_collections::IObservableVector<IInspectable> {
    (0..len)
        .map(|index| {
            let index = i32::try_from(index).expect("a list has fewer than 2^31 rows");
            Some(windows_reference::IReference::<i32>::from(index).into())
        })
        .collect::<Vec<Option<IInspectable>>>()
        .into()
}
