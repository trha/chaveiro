//! Menus: an `NSMenu` the app declares only while it is open, and its
//! items.
//!
//! A menu opens at a [`MenuAnchor`], and the app keeps declaring it until
//! [`MenuResponse::closed`]. Its items are nodes like any control, so a
//! click reads like a button's.

use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, MenuItemNode, MenuNode, Payload, Ui},
    tree::{NodeId, Visit},
};
use objc2::{MainThreadOnly, rc::Retained, runtime::ProtocolObject, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSView};
use objc2_foundation::{NSPoint, NSString};
use std::hash::Hash;

impl Ui {
    /// A menu of the items `body` declares, in order:
    /// [`menu_item`](Self::menu_item)s and
    /// [`menu_separator`](Self::menu_separator)s. Keep declaring it, with
    /// the same `id`, until [`MenuResponse::closed`]; a pass that stops
    /// declaring it closes it. A menu with no items does not open.
    ///
    /// At an anchor from [`Response::context_requested`], declare it in the
    /// pass that reported the request, after the control: AppKit shows the
    /// menu when that pass ends. At an anchor from [`Response::anchor`], it
    /// opens under the control when the first pass that declares it ends.
    ///
    /// A chosen item's click arrives in the same pass as `closed`.
    pub fn menu(
        &mut self,
        id: impl Hash,
        anchor: MenuAnchor,
        body: impl FnOnce(&mut Ui),
    ) -> MenuResponse {
        let (node, visit) = self.visit(id, ChildKind::Logical, |mtm| {
            let (menu, popup) = match &anchor.probe {
                Some(probe) => (probe.clone(), false),
                None => {
                    let menu = NSMenu::new(mtm);
                    menu.setAutoenablesItems(false);
                    (menu, true)
                }
            };
            Payload::Menu(MenuNode {
                menu,
                popup,
                items: Vec::new(),
                open: true,
            })
        });
        if visit == Visit::Created {
            let menu = self.menu_node(node);
            let (menu, popup) = (menu.menu.clone(), menu.popup);
            if popup {
                self.events.register(&menu, node);
                // AppKit holds the delegate weakly; `Events` keeps the target.
                menu.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
                self.popups.push((menu, anchor.view.clone()));
            }
        }
        self.open(node);
        body(self);
        self.close();
        let menu = self.menu_node(node);
        let events_of = if menu.popup { node } else { anchor.node };
        let closed =
            visit != Visit::Created && self.events.happened(events_of, EventKind::MenuDidClose);
        let menu = self.menu_node(node);
        if closed {
            menu.open = false;
        }
        MenuResponse {
            closed: closed || !menu.open,
        }
    }

    /// A menu item titled `title`. Declare it in a [`menu`](Self::menu)'s
    /// body.
    pub fn menu_item(&mut self, id: impl Hash, title: &str) -> MenuItemResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::new(),
                    Some(sel!(fire:)),
                    &NSString::new(),
                )
            };
            Payload::MenuItem(MenuItemNode {
                item,
                title: String::new(),
                enabled: None,
            })
        });
        let item = self.menu_item_node(node).item.clone();
        if visit == Visit::Created {
            self.events.register(&item, node);
            unsafe { item.setTarget(Some(self.events.target())) };
        }
        let item_node = self.menu_item_node(node);
        if must_write(node, false, item_node.title.as_str(), title) {
            item.setTitle(&NSString::from_str(title));
            item_node.title.clear();
            item_node.title.push_str(title);
        }
        MenuItemResponse { ui: self, node }
    }

    /// A line between groups of menu items.
    pub fn menu_separator(&mut self, id: impl Hash) {
        self.visit(id, ChildKind::Visual, |mtm| {
            Payload::MenuItem(MenuItemNode {
                item: NSMenuItem::separatorItem(mtm),
                title: String::new(),
                enabled: None,
            })
        });
    }

    fn menu_node(&mut self, node: NodeId) -> &mut MenuNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Menu(menu) => menu,
            _ => unreachable!("menu keys hold menus"),
        }
    }

    fn menu_item_node(&mut self, node: NodeId) -> &mut MenuItemNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::MenuItem(item) => item,
            _ => unreachable!("menu item keys hold menu items"),
        }
    }

    /// Makes the items `declared` the menu's, in order, for the menu node
    /// `node`. Leaves the previous list in `declared`.
    pub(crate) fn apply_menu(&mut self, node: NodeId, declared: &mut Vec<NodeId>) {
        let menu = self.menu_node(node);
        // Afterwards `declared` holds the old list.
        std::mem::swap(&mut menu.items, declared);
        if menu.items == *declared {
            return;
        }
        crate::write_check::record(node);
        let menu = menu.menu.clone();
        // Menus change rarely and hold few items, so all are set again. A
        // probe may still hold the items of the menu it showed before.
        menu.removeAllItems();
        let Payload::Menu(MenuNode { items, .. }) = &self.tree.node(node).payload else {
            unreachable!("matched above")
        };
        for &child in items {
            let Payload::MenuItem(item) = &self.tree.node(child).payload else {
                unreachable!("a menu holds menu items")
            };
            menu.addItem(&item.item);
        }
    }
}

impl Response<'_> {
    /// A request for the control's context menu since the last pass, by
    /// right-click or Control-click, as the anchor to open it at. AppKit
    /// runs that pass before it shows the menu, so declare the
    /// [`Ui::menu`] in the same pass. The first call lets the control
    /// report requests, so its first result is [`None`].
    ///
    /// On a table view, the anchor names the clicked row; see
    /// [`MenuAnchor::row`].
    pub fn context_requested(&mut self) -> Option<MenuAnchor> {
        let node = self.node;
        let ui = &mut *self.ui;
        let control = ui.control(node);
        let target: Retained<NSView> = match &control.control {
            Control::TableView { table, .. } => table.clone().into_super().into_super(),
            _ => control.view.clone(),
        };
        let Some(probe) = control.probe.clone() else {
            let probe = NSMenu::new(ui.mtm);
            // The app enables each item, so AppKit must not validate them.
            probe.setAutoenablesItems(false);
            ui.events.register(&probe, node);
            // AppKit holds the delegate weakly; `Events` keeps the target.
            probe.setDelegate(Some(ProtocolObject::from_ref(ui.events.target())));
            // SAFETY: the menu is a fresh NSMenu that the node keeps.
            unsafe { target.setMenu(Some(&probe)) };
            ui.control_mut(node).probe = Some(probe);
            return None;
        };
        if !ui.events.happened(node, EventKind::MenuNeedsUpdate) {
            return None;
        }
        let row = match &ui.control(node).control {
            Control::TableView { table, .. } => usize::try_from(table.clickedRow()).ok(),
            _ => None,
        };
        Some(MenuAnchor {
            node,
            view: target,
            probe: Some(probe),
            row,
        })
    }

    /// The anchor under the control, for a menu that a click on it opens,
    /// such as a More button's.
    pub fn anchor(&self) -> MenuAnchor {
        MenuAnchor {
            node: self.node,
            view: self.ui.control(self.node).view.clone(),
            probe: None,
            row: None,
        }
    }
}

/// Where a [`Ui::menu`] opens: from [`Response::context_requested`], where
/// the user clicked, or from [`Response::anchor`], under a control.
#[derive(Clone)]
pub struct MenuAnchor {
    node: NodeId,
    view: Retained<NSView>,
    /// The control's menu that AppKit is about to show, for a context
    /// request.
    probe: Option<Retained<NSMenu>>,
    row: Option<usize>,
}

impl MenuAnchor {
    /// For a context request on a table view, the clicked row: [`None`]
    /// for a click below the last row.
    pub fn row(&self) -> Option<usize> {
        self.row
    }
}

/// What [`Ui::menu`] returns.
pub struct MenuResponse {
    closed: bool,
}

impl MenuResponse {
    /// Whether the menu closed, so the app stops declaring it.
    pub fn closed(&self) -> bool {
        self.closed
    }
}

/// What [`Ui::menu_item`] returns.
pub struct MenuItemResponse<'u> {
    ui: &'u mut Ui,
    node: NodeId,
}

impl MenuItemResponse<'_> {
    /// Whether the user chose the item since the last pass.
    pub fn clicked(&self) -> bool {
        self.ui.events.happened(self.node, EventKind::Action)
    }

    /// Sets whether the user can choose the item. An item starts enabled.
    pub fn enabled(&mut self, on: bool) -> &mut Self {
        let node = self.node;
        let item = self.ui.menu_item_node(node);
        if must_write(node, false, &item.enabled, &Some(on)) {
            item.item.setEnabled(on);
            item.enabled = Some(on);
        }
        self
    }
}

/// Opens `menu` under `view`. Waits until the menu closes, so call it with
/// no borrow of the [`Ui`] held.
pub(crate) fn pop_up(menu: &NSMenu, view: &NSView) {
    let bounds = view.bounds();
    let below = match view.isFlipped() {
        true => bounds.size.height + 4.0,
        false => -4.0,
    };
    menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0, below), Some(view));
}
