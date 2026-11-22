use super::{Control, must_write};
use crate::{
    events::{Command, EventKind},
    pass::{ChildKind, NativeTree, Payload, ToolbarItemNode, ToolbarNode, Ui},
    tree::{NodeId, Visit},
};
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
    sel,
};
use objc2_app_kit::{
    NSImage, NSSearchToolbarItem, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode,
    NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem, NSToolbarItemGroup,
    NSToolbarItemGroupControlRepresentation, NSToolbarItemGroupSelectionMode,
    NSToolbarItemIdentifier, NSToolbarSidebarTrackingSeparatorItemIdentifier,
    NSToolbarToggleSidebarItemIdentifier, NSTrackingSeparatorToolbarItem, NSWindowToolbarStyle,
};
use objc2_foundation::{NSArray, NSCopying, NSInteger, NSObject, NSObjectProtocol, NSString};
use std::{cell::RefCell, hash::Hash};

impl Ui {
    /// The window's toolbar, holding the items `body` declares, in order:
    /// [`toolbar_item`](Self::toolbar_item)s and spaces. Declare it in a
    /// window's body, beside its one control. The toolbar shares the title
    /// bar, and the user cannot customize it.
    ///
    /// Panics outside a window's body.
    pub fn toolbar(&mut self, body: impl FnOnce(&mut Ui)) {
        let window = match self.core.tree.native(self.container()) {
            Payload::Window(window) => window.window.clone(),
            _ => panic!("a toolbar is declared in a window's body"),
        };
        let (node, visit) = self.visit("toolbar", ChildKind::Logical, |mtm| {
            let toolbar = NSToolbar::initWithIdentifier(
                NSToolbar::alloc(mtm),
                &NSString::from_str("toolbar"),
            );
            toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
            toolbar.setAllowsUserCustomization(false);
            let source = ToolbarSource::alloc(mtm).set_ivars(SourceIvars {
                items: RefCell::new(Vec::new()),
            });
            let source: Retained<ToolbarSource> = unsafe { msg_send![super(source), init] };
            Payload::Toolbar(ToolbarNode {
                window,
                toolbar,
                source,
                items: Vec::new(),
            })
        });
        if visit == Visit::Created {
            let Payload::Toolbar(toolbar) = self.core.tree.native(node) else {
                unreachable!("toolbar keys hold toolbars")
            };
            // AppKit holds the delegate weakly; the node keeps it.
            toolbar
                .toolbar
                .setDelegate(Some(ProtocolObject::from_ref(&*toolbar.source)));
            toolbar.window.setToolbar(Some(&toolbar.toolbar));
            toolbar
                .window
                .setToolbarStyle(NSWindowToolbarStyle::Unified);
        }
        self.open(node);
        body(self);
        self.close();
    }

    /// A toolbar button that shows the SF Symbols image `symbol`, with
    /// `label` as its tooltip and accessibility label. Declare it in a
    /// [`toolbar`](Self::toolbar)'s body.
    pub fn toolbar_item(
        &mut self,
        id: &'static str,
        symbol: &str,
        label: &str,
    ) -> ToolbarItemResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let identifier = NSString::from_str(id);
            let item =
                NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), &identifier);
            item.setBordered(true);
            // The app enables the item, so AppKit must not validate it.
            item.setAutovalidates(false);
            Payload::ToolbarItem(ToolbarItemNode {
                item: Some(item),
                identifier,
                symbol: String::new(),
                label: String::new(),
                enabled: None,
                subitems: Vec::new(),
                search: None,
            })
        });
        if visit == Visit::Created {
            let item = self
                .toolbar_item_node(node)
                .item
                .clone()
                .expect("made with an item");
            self.events.register(&item, node);
            unsafe {
                item.setTarget(Some(self.events.target()));
                item.setAction(Some(sel!(fire:)));
            }
        }
        let item_node = self.toolbar_item_node(node);
        let item = item_node.item.as_ref().expect("buttons have an item");
        if must_write(node, false, item_node.symbol.as_str(), symbol) {
            item.setImage(
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(symbol),
                    Some(&NSString::from_str(label)),
                )
                .as_deref(),
            );
            item_node.symbol.clear();
            item_node.symbol.push_str(symbol);
        }
        if must_write(node, false, item_node.label.as_str(), label) {
            let text = NSString::from_str(label);
            item.setLabel(&text);
            item.setToolTip(Some(&text));
            item_node.label.clear();
            item_node.label.push_str(label);
        }
        ToolbarItemResponse { ui: self, node }
    }

    /// Toolbar items drawn together, as one capsule of buttons: the
    /// [`toolbar_item`](Self::toolbar_item)s `body` declares, in order.
    /// Declare it in a [`toolbar`](Self::toolbar)'s body.
    pub fn toolbar_group(&mut self, id: &'static str, body: impl FnOnce(&mut Ui)) {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let identifier = NSString::from_str(id);
            let group = NSToolbarItemGroup::initWithItemIdentifier(
                NSToolbarItemGroup::alloc(mtm),
                &identifier,
            );
            group.setSelectionMode(NSToolbarItemGroupSelectionMode::Momentary);
            group.setControlRepresentation(NSToolbarItemGroupControlRepresentation::Expanded);
            Payload::ToolbarItem(ToolbarItemNode {
                item: Some(group.into_super()),
                identifier,
                symbol: String::new(),
                label: String::new(),
                enabled: None,
                subitems: Vec::new(),
                search: None,
            })
        });
        self.open(node);
        body(self);
        self.close();
    }

    /// Makes the items `declared` the subitems of the toolbar group `node`,
    /// in order. Leaves the previous list in `declared`.
    pub(crate) fn apply_toolbar_group(&mut self, node: NodeId, declared: &mut Vec<NodeId>) {
        let item = self.toolbar_item_node(node);
        // Afterwards `declared` holds the old list.
        std::mem::swap(&mut item.subitems, declared);
        if item.subitems == *declared {
            return;
        }
        crate::write_check::record(node);
        let group = item.item.clone().expect("groups have an item");
        let subitems: Vec<Retained<NSToolbarItem>> = self
            .toolbar_item_node(node)
            .subitems
            .clone()
            .into_iter()
            .map(|child| {
                self.toolbar_item_node(child)
                    .item
                    .clone()
                    .expect("a group holds toolbar buttons")
            })
            .collect();
        let group = group
            .downcast::<NSToolbarItemGroup>()
            .expect("toolbar group keys hold groups");
        // Groups change rarely, so all subitems are set again.
        group.setSubitems(&NSArray::from_retained_slice(&subitems));
    }

    /// The toolbar's search field, bound to `value`, `width` points wide.
    /// Declare it in a [`toolbar`](Self::toolbar)'s body.
    ///
    /// The field owns its text while the user types: the pass copies the
    /// field's text into `value` after an edit, and writes `value` into the
    /// field only when the app changed it since. The search runs on every
    /// keystroke; Return and the clear button need no handling.
    pub fn toolbar_search(
        &mut self,
        id: &'static str,
        placeholder: &str,
        width: f64,
        value: &mut String,
    ) -> ToolbarSearchResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let identifier = NSString::from_str(id);
            let item = NSSearchToolbarItem::initWithItemIdentifier(
                NSSearchToolbarItem::alloc(mtm),
                &identifier,
            );
            item.setPreferredWidthForSearchField(width);
            let field = item.searchField();
            Payload::ToolbarItem(ToolbarItemNode {
                item: Some(item.into_super()),
                identifier,
                symbol: String::new(),
                label: String::new(),
                enabled: None,
                subitems: Vec::new(),
                search: Some((field, String::new())),
            })
        });
        let item_node = self.toolbar_item_node(node);
        let (field, _) = item_node
            .search
            .as_mut()
            .expect("search keys hold search items");
        let field = field.clone();
        if visit == Visit::Created {
            self.events.register(&field, node);
            // AppKit holds the delegate weakly; `Events` keeps the target.
            unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self.events.target()))) };
        }
        let typed = self.events.happened(node, EventKind::TextDidChange);
        let item_node = self.toolbar_item_node(node);
        if must_write(node, false, item_node.label.as_str(), placeholder) {
            field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            item_node.label.clear();
            item_node.label.push_str(placeholder);
        }
        let (_, text) = item_node
            .search
            .as_mut()
            .expect("search keys hold search items");
        let mut changed = false;
        if typed {
            let current = field.stringValue().to_string();
            // An echo of this layer's own write reads back the stored text.
            if current != *text {
                value.clone_from(&current);
                *text = current;
                changed = true;
            }
        } else if must_write(node, false, text.as_str(), value.as_str()) {
            field.setStringValue(&NSString::from_str(value));
            text.clone_from(value);
        }
        ToolbarSearchResponse {
            ui: self,
            node,
            changed,
        }
    }

    /// Space that pushes the toolbar items after it to the trailing edge.
    pub fn toolbar_flexible_space(&mut self, id: impl Hash) {
        self.toolbar_space(id, unsafe { NSToolbarFlexibleSpaceItemIdentifier });
    }

    /// The system's button that hides and shows the window's
    /// [`split_view`](Self::split_view) sidebar. Put it before the
    /// [`toolbar_sidebar_separator`](Self::toolbar_sidebar_separator), so
    /// it sits over the sidebar.
    pub fn toolbar_toggle_sidebar(&mut self, id: impl Hash) {
        self.toolbar_space(id, unsafe { NSToolbarToggleSidebarItemIdentifier });
    }

    /// A divider that follows the edge of the window's
    /// [`split_view`](Self::split_view) sidebar, so the items after it sit
    /// over the other panes.
    pub fn toolbar_sidebar_separator(&mut self, id: impl Hash) {
        self.toolbar_space(id, unsafe {
            NSToolbarSidebarTrackingSeparatorItemIdentifier
        });
    }

    /// A divider that follows divider `divider` of the window's
    /// [`split_view`](Self::split_view), counted from the leading edge, the
    /// sidebar's included, so the items after it sit over the panes past
    /// that divider. The window's title stays before it and truncates
    /// there, so put it after a list pane that the title names.
    ///
    /// Panics when the window's body declared no split view before the
    /// toolbar.
    pub fn toolbar_divider_separator(&mut self, id: &'static str, divider: usize) {
        // The window's control was declared before the toolbar, and joins
        // the window only when the window's body ends.
        let split_view = self
            .window_controls()
            .iter()
            .find_map(|&node| match &self.control(node).control {
                Control::SplitView { controller, .. } => Some(controller.splitView()),
                _ => None,
            })
            .expect("the window's control is a split view, declared before the toolbar");
        self.visit(id, ChildKind::Visual, |_| {
            let identifier = NSString::from_str(id);
            let item = NSTrackingSeparatorToolbarItem::trackingSeparatorToolbarItemWithIdentifier_splitView_dividerIndex(
                &identifier,
                &split_view,
                divider as NSInteger,
            );
            Payload::ToolbarItem(ToolbarItemNode {
                item: Some(item.into_super()),
                identifier,
                symbol: String::new(),
                label: String::new(),
                enabled: None,
                subitems: Vec::new(),
                search: None,
            })
        });
    }

    /// A toolbar item that AppKit makes for a system identifier.
    fn toolbar_space(&mut self, id: impl Hash, identifier: &NSToolbarItemIdentifier) {
        self.visit(id, ChildKind::Visual, |_| {
            Payload::ToolbarItem(ToolbarItemNode {
                item: None,
                identifier: identifier.copy(),
                symbol: String::new(),
                label: String::new(),
                enabled: None,
                subitems: Vec::new(),
                search: None,
            })
        });
    }

    fn toolbar_item_node(&mut self, node: NodeId) -> &mut ToolbarItemNode {
        match self.core.tree.native_mut(node) {
            Payload::ToolbarItem(item) => item,
            _ => unreachable!("toolbar item keys hold toolbar items"),
        }
    }

    /// Makes the items `declared` the toolbar's, in order, for the toolbar
    /// node `node`. Leaves the previous list in `declared`.
    pub(crate) fn apply_toolbar(&mut self, node: NodeId, declared: &mut Vec<NodeId>) {
        let Payload::Toolbar(toolbar) = self.core.tree.native_mut(node) else {
            unreachable!("toolbar keys hold toolbars")
        };
        // Afterwards `declared` holds the old list.
        std::mem::swap(&mut toolbar.items, declared);
        let changes: Vec<_> = immediate_tree::edits(declared, &toolbar.items).collect();
        if changes.is_empty() {
            return;
        }
        crate::write_check::record(node);
        let bar = toolbar.toolbar.clone();
        let source = toolbar.source.clone();
        let items = toolbar.items.clone();
        let item_of = |ui: &Ui, child: NodeId| match ui.core.tree.native(child) {
            Payload::ToolbarItem(item) => (item.identifier.clone(), item.item.clone()),
            _ => unreachable!("a toolbar holds toolbar items"),
        };
        // AppKit asks the source for each item it inserts.
        *source.ivars().items.borrow_mut() = items
            .iter()
            .filter_map(|&child| {
                let (identifier, item) = item_of(self, child);
                item.map(|item| (identifier, item))
            })
            .collect();
        for change in changes {
            match change {
                immediate_tree::Edit::RemoveAt(index) => bar.removeItemAtIndex(index as _),
                immediate_tree::Edit::InsertAt(index, child) => {
                    let (identifier, _) = item_of(self, child);
                    bar.insertItemWithItemIdentifier_atIndex(&identifier, index as _);
                }
            }
        }
    }
}

/// A key that [`ToolbarSearchResponse::keys`] reports. The field runs each
/// as usual too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
}

/// What [`Ui::toolbar_search`] returns.
pub struct ToolbarSearchResponse<'u> {
    ui: &'u mut Ui,
    node: NodeId,
    changed: bool,
}

impl ToolbarSearchResponse<'_> {
    /// Whether the user changed the text since the last pass; `value` then
    /// holds the new text.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// The arrow keys the user pressed in the field since the last pass,
    /// in order.
    pub fn keys(&self) -> impl Iterator<Item = Key> {
        self.ui
            .events
            .commands(self.node)
            .filter_map(|command| match command {
                Command::Up => Some(Key::Up),
                Command::Down => Some(Key::Down),
                _ => None,
            })
    }
}

/// What [`Ui::toolbar_item`] returns.
pub struct ToolbarItemResponse<'u> {
    ui: &'u mut Ui,
    node: NodeId,
}

impl ToolbarItemResponse<'_> {
    /// Whether the item was clicked since the last pass.
    pub fn clicked(&self) -> bool {
        self.ui.events.happened(self.node, EventKind::Action)
    }

    /// Sets whether the user can click the item. An item starts enabled.
    pub fn enabled(&mut self, on: bool) -> &mut Self {
        let node = self.node;
        let item = self.ui.toolbar_item_node(node);
        if must_write(node, false, &item.enabled, &Some(on)) {
            item.item
                .as_ref()
                .expect("buttons have an item")
                .setEnabled(on);
            item.enabled = Some(on);
        }
        self
    }
}

pub(crate) struct SourceIvars {
    /// The items the toolbar may insert, by identifier.
    items: RefCell<Vec<(Retained<NSString>, Retained<NSToolbarItem>)>>,
}

define_class!(
    /// The delegate of one toolbar. It hands AppKit the items the last pass
    /// declared, from its own copy, so it needs no access to the `Ui`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SourceIvars]
    pub(crate) struct ToolbarSource;

    unsafe impl NSObjectProtocol for ToolbarSource {}

    unsafe impl NSToolbarDelegate for ToolbarSource {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn item_for_identifier(
            &self,
            _toolbar: &NSToolbar,
            identifier: &NSToolbarItemIdentifier,
            _inserted: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            self.ivars()
                .items
                .borrow()
                .iter()
                .find(|(known, _)| **known == *identifier)
                .map(|(_, item)| item.clone())
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_identifiers(
            &self,
            _toolbar: &NSToolbar,
        ) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            // The pass inserts every item itself.
            NSArray::new()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_identifiers(
            &self,
            _toolbar: &NSToolbar,
        ) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            let items = self.ivars().items.borrow();
            let mut identifiers: Vec<&NSString> =
                items.iter().map(|(identifier, _)| &**identifier).collect();
            identifiers.push(unsafe { NSToolbarFlexibleSpaceItemIdentifier });
            identifiers.push(unsafe { NSToolbarSidebarTrackingSeparatorItemIdentifier });
            identifiers.push(unsafe { NSToolbarToggleSidebarItemIdentifier });
            NSArray::from_slice(&identifiers)
        }
    }
);
