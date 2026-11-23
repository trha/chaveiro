//! One build pass over the declared UI: finding nodes, opening and closing
//! containers, and applying their WinUI children.

use crate::{
    bindings::*,
    events::{EventKind, EventSet, Events},
    widget::Control,
};
use immediate_tree::{Core, Edit, Node, NodeId, Shell, Tree, Visit, edits};
use std::{hash::Hash, rc::Rc};
use windows_core::{IInspectable, Interface, Result};

/// Builds the UI. The app gets it in [`App::view`](crate::App::view).
pub struct Ui {
    pub(crate) core: Core<NativeNode>,
    pub(crate) events: Events,
    /// Reused buffer for formatting labels before comparing them.
    pub(crate) scratch: String,
    /// Lists declared this pass, whose rows are built after the view.
    pub(crate) lists: Vec<NodeId>,
    /// Item sources to set after the pass, with no borrow held, because
    /// WinUI may ask for rows inside `SetItemsSource`.
    pub(crate) sources: Vec<(ItemsRepeater, usize)>,
    /// Rows to scroll into view after the sources are set, and whether to
    /// focus them, for the same reason: WinUI builds a row that does not
    /// exist yet.
    pub(crate) reveals: Vec<(ItemsRepeater, usize, bool)>,
}

impl Ui {
    /// A UI whose events call `wake`, and whose woken tasks call `post`
    /// from any thread to ask for a turn.
    pub(crate) fn with_task_post(
        wake: Rc<dyn Fn()>,
        post: impl 'static + Send + Sync + Fn() -> bool,
    ) -> Self {
        Ui {
            core: Core::new(post),
            events: Events::new(wake),
            scratch: String::new(),
            lists: Vec::new(),
            sources: Vec::new(),
            reveals: Vec::new(),
        }
    }

    /// Asks for another pass soon after this one, for app state that
    /// changed after the widgets that show it were declared, such as an
    /// action a list row found that the next view applies. Without it, the
    /// window shows the old state until the next event or woken task.
    pub fn request_pass(&mut self) {
        self.core.request_pass();
    }

    /// Whether this pass asked for another one, and forgets it.
    pub(crate) fn take_pass_request(&mut self) -> bool {
        self.core.take_pass_request()
    }

    /// Polls every task and stream woken since the last turn. Called by the
    /// runner at the start of a turn.
    pub(crate) fn poll_woken_tasks(&mut self) {
        // Woken or not, the runner runs the turn's passes.
        self.core.poll_woken_tasks();
    }

    /// Starts a pass. Returns whether events arrived since the last one.
    pub(crate) fn begin(&mut self) -> bool {
        debug_assert!(self.core.depth() == 0, "the last pass closed its frames");
        self.lists.clear();
        self.core.tree.begin_pass();
        let consumed = self.events.begin_pass(&mut self.core.tree);
        let root = self.core.tree.root();
        self.core.open(root);
        consumed
    }

    /// Ends a pass, closing every window and task it did not declare.
    /// Returns the number of open windows; tasks at the root do not count.
    pub(crate) fn end(&mut self) -> Result<usize> {
        self.close()?;
        let tree = &mut self.core.tree;
        let root = tree.root();
        for index in 0..tree.children(root).len() {
            let node = tree.children(root)[index];
            if let Node::Native(NativeNode(Payload::Window(window))) =
                &mut tree.node_mut(node).payload
            {
                // A window shows after its first pass, or after its first fit
                // when it fits its content, so no resize is seen.
                if window.cloaked && !(window.fit && !window.fitted_once) {
                    crate::widget::cloak(&window.window, false)?;
                    window.cloaked = false;
                }
            }
        }
        let windows = tree
            .children(root)
            .iter()
            .filter(|node| {
                matches!(
                    tree.node(**node).payload,
                    Node::Native(NativeNode(Payload::Window(_)))
                )
            })
            .count();
        Ok(windows)
    }

    /// Closes every window, as a pass that declares none would. Used after a
    /// failed pass, which may have left frames open.
    pub(crate) fn close_all(&mut self) {
        immediate_tree::close_all(self);
    }

    /// The number of open containers, for [`unwind`](Self::unwind).
    pub(crate) fn depth(&self) -> usize {
        self.core.depth()
    }

    /// Closes the containers opened after `depth`, innermost first, after a
    /// build failed inside them.
    pub(crate) fn unwind(&mut self, depth: usize) {
        immediate_tree::unwind(self, depth);
    }

    /// Opens `node` as the container for the widgets declared next.
    pub(crate) fn open(&mut self, node: NodeId) {
        self.core.open(node);
    }

    /// Opens `node` like [`open`](Self::open), but the children's keys
    /// derive from `scope` as well, so a new `scope` makes every child new.
    pub(crate) fn open_scoped(&mut self, node: NodeId, scope: impl Hash) {
        self.core.open_scoped(node, scope);
    }

    /// Finds or creates the node `id` in the open container and records it
    /// as a child of the kind `kind` says.
    pub(crate) fn visit(
        &mut self,
        id: impl Hash,
        kind: ChildKind,
        create: impl FnOnce() -> Result<Payload>,
    ) -> Result<(NodeId, Visit)> {
        self.core.try_visit(id, kind, || {
            create().map(|payload| Node::Native(NativeNode(payload)))
        })
    }

    /// The window the open containers are inside.
    pub(crate) fn enclosing_window(&self) -> NodeId {
        self.core
            .open_nodes()
            .rev()
            .find(|&node| {
                matches!(
                    self.core.tree.node(node).payload,
                    Node::Native(NativeNode(Payload::Window(_)))
                )
            })
            .expect("controls are declared inside a window")
    }

    /// The element of the innermost open control.
    pub(crate) fn enclosing_control(&self) -> UIElement {
        self.core
            .open_nodes()
            .rev()
            .find_map(|node| match &self.core.tree.node(node).payload {
                Node::Native(NativeNode(Payload::Control(control))) => {
                    Some(control.element.clone())
                }
                _ => None,
            })
            .expect("a shortcut is declared inside a control")
    }

    /// Closes the innermost container: applies its WinUI children, then drops
    /// the children it no longer declares.
    pub(crate) fn close(&mut self) -> Result<()> {
        immediate_tree::close(self)
    }

    pub(crate) fn control(&self, node: NodeId) -> &ControlNode {
        match self.core.tree.native(node) {
            Payload::Control(control) => control,
            _ => unreachable!("visual children and widget keys are controls"),
        }
    }

    /// Subscribes the control `node` to each of `kinds` it is not subscribed
    /// to yet, on the object `source` picks from it. A node subscribed to
    /// every kind costs a bit test per kind and no COM call.
    pub(crate) fn subscribe<I: Interface>(
        &mut self,
        node: NodeId,
        kinds: &[EventKind],
        source: impl Fn(&ControlNode) -> &I,
    ) -> Result<()> {
        // `self.core` and `self.events` are separate fields, so the control
        // stays borrowed while the events subscribe its object: no clone,
        // which would be a COM AddRef and Release on every pass.
        let Payload::Control(control) = self.core.tree.native_mut(node) else {
            unreachable!("widget keys hold controls")
        };
        let mut subscribed = control.subscribed;
        for &kind in kinds {
            if !subscribed.contains(kind) {
                self.events
                    .subscribe(node, &mut subscribed, source(control), kind)?;
            }
        }
        control.subscribed = subscribed;
        Ok(())
    }

    /// [`subscribe`](Self::subscribe) for any payload: `pick` returns the
    /// object to subscribe and the payload's record of subscribed kinds.
    pub(crate) fn subscribe_native<I: Interface>(
        &mut self,
        node: NodeId,
        kinds: &[EventKind],
        pick: impl FnOnce(&mut Payload) -> (&I, &mut EventSet),
    ) -> Result<()> {
        let (source, subscribed) = pick(self.core.tree.native_mut(node));
        for &kind in kinds {
            self.events.subscribe(node, subscribed, source, kind)?;
        }
        Ok(())
    }

    pub(crate) fn control_mut(&mut self, node: NodeId) -> &mut ControlNode {
        match self.core.tree.native_mut(node) {
            Payload::Control(control) => control,
            _ => unreachable!("widget keys hold controls"),
        }
    }

    /// Edits `items`, a menu's item list, from the items `old` to `new`.
    fn apply_menu_items(
        &self,
        items: &windows_collections::IVector<MenuFlyoutItemBase>,
        old: &[NodeId],
        new: &[NodeId],
    ) -> Result<()> {
        for edit in edits(old, new) {
            match edit {
                Edit::RemoveAt(index) => items.RemoveAt(index)?,
                Edit::InsertAt(index, child) => items.InsertAt(
                    index,
                    &self.control(child).element.cast::<MenuFlyoutItemBase>()?,
                )?,
            }
        }
        Ok(())
    }

    /// Makes `declared` the WinUI children of `node`. Leaves the previous
    /// list in `declared`.
    fn apply_native(&mut self, node: NodeId, declared: &mut Vec<NodeId>) -> Result<()> {
        match self.core.tree.native_mut(node) {
            Payload::Window(window) => {
                assert!(
                    declared.len() <= 1,
                    "a window's body declares at most one control"
                );
                let content = declared.first().copied();
                if window.content == content {
                    return Ok(());
                }
                window.content = content;
                let window = window.window.clone();
                match content {
                    Some(content) => window.SetContent(&self.control(content).element),
                    None => window.SetContent(None::<&UIElement>),
                }
            }
            Payload::Control(ControlNode {
                control:
                    control @ (Control::Row { .. }
                    | Control::NavigationView(_)
                    | Control::Scroll { .. }
                    | Control::ContentButton { .. }),
                element,
                ..
            }) => {
                assert!(
                    declared.len() <= 1,
                    "a list row, navigation view, scroll area or content button declares one control"
                );
                let content = control.content_slot().expect("matched above");
                let declared_content = declared.first().copied();
                if *content == declared_content {
                    return Ok(());
                }
                *content = declared_content;
                let view = element.cast::<ContentControl>()?;
                match declared_content {
                    Some(declared_content) => {
                        view.SetContent(&self.control(declared_content).element)
                    }
                    None => view.SetContent(None::<&IInspectable>),
                }
            }
            Payload::Control(ControlNode {
                control:
                    Control::TitleBar {
                        title_bar,
                        content,
                        right,
                        ..
                    },
                ..
            }) => {
                assert!(
                    declared.len() <= 2,
                    "a title bar declares a middle control and a right one"
                );
                let declared_content = declared.first().copied();
                let declared_right = declared.get(1).copied();
                let content_changed = *content != declared_content;
                let right_changed = *right != declared_right;
                *content = declared_content;
                *right = declared_right;
                let title_bar = title_bar.clone();
                let element =
                    |node: Option<NodeId>| node.map(|node| self.control(node).element.clone());
                if content_changed {
                    title_bar.SetContent(element(declared_content).as_ref())?;
                }
                if right_changed {
                    title_bar.SetRightHeader(element(declared_right).as_ref())?;
                }
                Ok(())
            }
            Payload::Control(ControlNode {
                control: Control::Pressable(parts),
                ..
            }) => {
                assert!(declared.len() <= 1, "a pressable declares one control");
                let declared_content = declared.first().copied();
                if parts.content == declared_content {
                    return Ok(());
                }
                let old = std::mem::replace(&mut parts.content, declared_content);
                let children = parts.children.clone();
                // The pill is child 0, so the content is child 1.
                if old.is_some() {
                    children.RemoveAt(1)?;
                }
                if let Some(declared_content) = declared_content {
                    let element = &self.control(declared_content).element;
                    Grid::SetColumn(&element.cast::<FrameworkElement>()?, 1)?;
                    children.Append(element)?;
                }
                Ok(())
            }
            Payload::Control(ControlNode {
                control: Control::CommandBar { .. },
                visual,
                ..
            }) => {
                // Afterwards `declared` holds the old list.
                std::mem::swap(visual, declared);
                // Borrowed again shared, so the commands list needs no clone.
                let Payload::Control(control) = self.core.tree.native(node) else {
                    unreachable!("matched above")
                };
                let Control::CommandBar { commands } = &control.control else {
                    unreachable!("matched above")
                };
                for edit in edits(declared, &control.visual) {
                    match edit {
                        Edit::RemoveAt(index) => commands.RemoveAt(index)?,
                        Edit::InsertAt(index, child) => commands.InsertAt(
                            index,
                            &self.control(child).element.cast::<ICommandBarElement>()?,
                        )?,
                    }
                }
                Ok(())
            }
            Payload::Control(ControlNode {
                control: Control::SubMenu { .. } | Control::SplitButton(_),
                visual,
                ..
            }) => {
                std::mem::swap(visual, declared);
                // Borrowed again shared, so the items list needs no clone.
                let Payload::Control(control) = self.core.tree.native(node) else {
                    unreachable!("matched above")
                };
                let items = match &control.control {
                    Control::SubMenu { items, .. } => items,
                    Control::SplitButton(parts) => &parts.items,
                    _ => unreachable!("matched above"),
                };
                self.apply_menu_items(items, declared, &control.visual)
            }
            Payload::Flyout(popup) => {
                assert!(declared.len() <= 1, "a flyout declares one control");
                let declared_content = declared.first().copied();
                if popup.content == declared_content {
                    return Ok(());
                }
                popup.content = declared_content;
                let flyout = popup.flyout.clone();
                match declared_content {
                    Some(content) => flyout.SetContent(&self.control(content).element),
                    None => flyout.SetContent(None::<&UIElement>),
                }
            }
            Payload::Menu(menu) => {
                let items = menu.items.clone();
                std::mem::swap(&mut menu.visual, declared);
                let Payload::Menu(menu) = self.core.tree.native(node) else {
                    unreachable!("matched above")
                };
                self.apply_menu_items(&items, declared, &menu.visual)
            }
            Payload::Control(ControlNode {
                control: Control::Navigator(navigator),
                ..
            }) => {
                assert!(declared.len() <= 1, "a navigator declares one screen");
                let declared_content = declared.first().copied();
                if navigator.content == declared_content {
                    return Ok(());
                }
                let old = std::mem::replace(&mut navigator.content, declared_content);
                let old = old.map(|old| self.control(old).element.clone());
                let entering = declared_content.map(|new| self.control(new).element.clone());
                let Payload::Control(ControlNode {
                    control: Control::Navigator(navigator),
                    ..
                }) = self.core.tree.native_mut(node)
                else {
                    unreachable!("matched above")
                };
                navigator.swap(old, entering)
            }
            Payload::Control(control) => {
                if control.control.children().is_none() {
                    assert!(declared.is_empty(), "only panels have children");
                    return Ok(());
                }
                let in_grid = matches!(control.control, Control::Grid { .. });
                // Afterwards `declared` holds the old list.
                std::mem::swap(&mut control.visual, declared);
                // Borrowed, not cloned: a clone is a COM AddRef and Release on
                // every pass, edits or not.
                let control = self.control(node);
                let children = control.control.children().expect("checked above");
                for edit in edits(declared, control.visual.as_slice()) {
                    match edit {
                        Edit::RemoveAt(index) => children.RemoveAt(index)?,
                        Edit::InsertAt(index, child) => {
                            children.InsertAt(index, &self.control(child).element)?
                        }
                    }
                }
                if in_grid {
                    self.place_in_rows(node)?;
                }
                Ok(())
            }
            Payload::Shortcut(_) => unreachable!("shortcut nodes are never opened"),
            Payload::Dialog(dialog) => {
                assert!(declared.len() <= 1, "a dialog declares one control");
                let declared_content = declared.first().copied();
                if dialog.content == declared_content {
                    return Ok(());
                }
                dialog.content = declared_content;
                let dialog = dialog.dialog.cast::<crate::bindings::ContentControl>()?;
                match declared_content {
                    Some(content) => {
                        dialog.SetContent(&self.control(content).element.cast::<IInspectable>()?)
                    }
                    None => dialog.SetContent(None::<&IInspectable>),
                }
            }
        }
    }

    /// Puts each child of the grid `node` in the row, or for a grid of
    /// columns the column, of its position.
    fn place_in_rows(&mut self, node: NodeId) -> Result<()> {
        let across = matches!(
            self.control(node).control,
            Control::Grid { across: true, .. }
        );
        for position in 0..self.control(node).visual.len() {
            let child = self.control(node).visual[position];
            let cell = i32::try_from(position).expect("a grid has fewer than 2^31 children");
            let child = self.control_mut(child);
            let written = match across {
                true => &mut child.grid_column,
                false => &mut child.grid_row,
            };
            if *written != cell {
                let frame = child.element.cast::<FrameworkElement>()?;
                match across {
                    true => Grid::SetColumn(&frame, cell)?,
                    false => Grid::SetRow(&frame, cell)?,
                }
                *written = cell;
            }
        }
        Ok(())
    }
}

pub(crate) use immediate_tree::ChildKind;

impl Shell for Ui {
    type Native = NativeNode;
    type Error = windows_core::Error;

    fn core(&mut self) -> &mut Core<NativeNode> {
        &mut self.core
    }

    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>) -> Result<()> {
        self.apply_native(node, declared)
    }

    fn release(&mut self, native: NativeNode) {
        release(&mut self.events, native.0);
    }
}

/// Reads the WinUI payload of a node this crate made.
pub(crate) trait NativeTree {
    /// Panics when `node` holds a node of `immediate-tree`, such as a cache.
    fn native(&self, node: NodeId) -> &Payload;
    fn native_mut(&mut self, node: NodeId) -> &mut Payload;
}

impl NativeTree for Tree<Node<NativeNode>> {
    fn native(&self, node: NodeId) -> &Payload {
        match &self.node(node).payload {
            Node::Native(NativeNode(native)) => native,
            _ => unreachable!("widget keys hold WinUI payloads"),
        }
    }

    fn native_mut(&mut self, node: NodeId) -> &mut Payload {
        match &mut self.node_mut(node).payload {
            Node::Native(NativeNode(native)) => native,
            _ => unreachable!("widget keys hold WinUI payloads"),
        }
    }
}

/// A WinUI payload, public only because [`Shell`] names it, so apps can
/// neither name nor read it.
pub struct NativeNode(pub(crate) Payload);

/// The WinUI nodes. The nodes every platform shares, such as caches and
/// tasks, are the other variants of [`Node`].
pub(crate) enum Payload {
    Window(WindowNode),
    Control(ControlNode),
    /// From [`Ui::menu`].
    Menu(Box<MenuNode>),
    /// From [`Ui::shortcut`].
    Shortcut(ShortcutNode),
    /// From [`Ui::flyout`].
    Flyout(Box<FlyoutNode>),
    /// From [`Ui::dialog`].
    Dialog(Box<DialogNode>),
}

/// A dialog, shown on its first pass and hidden when its node drops.
pub(crate) struct DialogNode {
    pub(crate) dialog: ContentDialog,
    /// The control set as its content.
    pub(crate) content: Option<NodeId>,
    pub(crate) subscribed: EventSet,
}

/// A flyout, shown on its first pass and hidden when its node drops.
pub(crate) struct FlyoutNode {
    pub(crate) flyout: Flyout,
    /// The control set as its content.
    pub(crate) content: Option<NodeId>,
    pub(crate) shown: bool,
    pub(crate) subscribed: EventSet,
}

/// A key held by `owner` from the first pass until the node drops.
pub(crate) struct ShortcutNode {
    pub(crate) accelerator: KeyboardAccelerator,
    /// [`None`] until the first pass attaches the key.
    pub(crate) owner: Option<UIElement>,
    pub(crate) subscribed: EventSet,
}

/// A menu, shown on its first pass and hidden when its node drops.
pub(crate) struct MenuNode {
    pub(crate) flyout: MenuFlyout,
    pub(crate) items: windows_collections::IVector<MenuFlyoutItemBase>,
    /// The items, in order, as last applied to `items`.
    pub(crate) visual: Vec<NodeId>,
    pub(crate) shown: bool,
    pub(crate) subscribed: EventSet,
}

pub(crate) struct WindowNode {
    pub(crate) window: Window,
    pub(crate) app_window: AppWindow,
    /// The control set as `Window.Content`.
    pub(crate) content: Option<NodeId>,
    /// The title last written.
    pub(crate) title: String,
    pub(crate) activated: bool,
    /// The Mica backdrop is set.
    pub(crate) mica: bool,
    /// The content draws the title bar, and only the close button is left.
    pub(crate) compact: bool,
    /// The content extends into the title bar, from `extends_into_title_bar`.
    pub(crate) extends: bool,
    /// `initial_size` has resized the window.
    pub(crate) sized: bool,
    /// The client size last written by `fit_to_content`, in pixels.
    pub(crate) fitted: (i32, i32),
    /// The content node whose alignment `fit_to_content` last set.
    pub(crate) fitted_content: Option<NodeId>,
    /// The app called `fit_to_content`, so the window waits for its first
    /// fit before it shows.
    pub(crate) fit: bool,
    pub(crate) fitted_once: bool,
    /// DWM hides the window until its first pass, or first fit, is done.
    pub(crate) cloaked: bool,
    /// Resizes made by `fit_to_content`.
    pub(crate) fit_tries: u32,
    /// The control set with `Window.SetTitleBar`.
    pub(crate) title_bar: Option<NodeId>,
    pub(crate) subscribed: EventSet,
}

pub(crate) struct ControlNode {
    /// The control as a `UIElement`, for child collections and imperative
    /// access.
    pub(crate) element: UIElement,
    pub(crate) control: Control,
    /// WinUI children as last applied (panels only).
    pub(crate) visual: Vec<NodeId>,
    pub(crate) subscribed: EventSet,
    /// Set by [`Response::invalidate`](crate::Response::invalidate): the next
    /// pass writes every managed property, then clears it.
    pub(crate) stale: bool,
    /// The `Grid.Row` last written; -1 until the control is placed in a grid.
    pub(crate) grid_row: i32,
    /// The `Grid.Column` last written; -1 until the control is placed in a
    /// grid of columns.
    pub(crate) grid_column: i32,
    /// `Control.IsEnabled` as last written; [`None`] until first written.
    pub(crate) enabled: Option<bool>,
    /// The header text last written, without the required mark; empty
    /// until first written.
    pub(crate) header: String,
    /// The field is marked required: its header ends in ` *`.
    pub(crate) required: bool,
    /// The access key last written; empty until first written.
    pub(crate) access_key: String,
    /// The style key last written; empty until first written.
    pub(crate) style: String,
    /// The horizontal alignment last written.
    pub(crate) align: Option<crate::widget::Align>,
    /// The vertical alignment last written.
    pub(crate) valign: Option<crate::widget::Align>,
    /// The width last written; `NaN` until first written.
    pub(crate) width: f64,
    /// The padding last written.
    pub(crate) padding: Option<[f64; 4]>,
    /// The margin last written.
    pub(crate) margin: Option<[f64; 4]>,
    /// The edges that show a divider line, as last written by
    /// [`Response::dividers`](crate::Response::dividers).
    pub(crate) dividers: Option<[bool; 4]>,
    /// The line under a field for its error, made by the first
    /// [`error`](crate::Response) call, and the message it shows.
    pub(crate) error: Option<crate::widget::ErrorLine>,
}

impl ControlNode {
    pub(crate) fn new(element: UIElement, control: Control) -> Payload {
        Payload::Control(ControlNode {
            element,
            control,
            visual: Vec::new(),
            subscribed: EventSet::default(),
            stale: false,
            grid_row: -1,
            grid_column: -1,
            enabled: None,
            header: String::new(),
            required: false,
            access_key: String::new(),
            style: String::new(),
            align: None,
            valign: None,
            width: f64::NAN,
            padding: None,
            margin: None,
            dividers: None,
            error: None,
        })
    }
}

/// Releases what a dropped node holds outside the tree.
fn release(events: &mut Events, payload: Payload) {
    match payload {
        Payload::Shortcut(shortcut) => {
            events.forget(&shortcut.accelerator);
            crate::widget::shortcut::release(&shortcut);
        }
        Payload::Flyout(popup) => {
            events.forget(&popup.flyout);
            // As for a menu below.
            let _ = popup
                .flyout
                .cast::<FlyoutBase>()
                .and_then(|flyout| flyout.Hide());
        }
        Payload::Dialog(dialog) => {
            events.forget(&dialog.dialog);
            // Hiding fails only for a dialog WinUI already closed, which is
            // the state this asks for.
            let _ = dialog.dialog.Hide();
        }
        Payload::Menu(menu) => {
            events.forget(&menu.flyout);
            // Hiding fails only for a menu WinUI already closed, which is
            // the state this asks for.
            let _ = menu
                .flyout
                .cast::<FlyoutBase>()
                .and_then(|flyout| flyout.Hide());
        }
        Payload::Window(window) => {
            events.forget(&window.app_window);
            // Closing fails only for a window the user already closed, which
            // is the state this asks for.
            let _ = window.window.Close();
        }
        Payload::Control(control) => {
            if control.subscribed != EventSet::default() {
                events.forget(&control.element);
            }
            if let Control::Navigator(navigator) = &control.control {
                // Stopping fails only when the dispatcher queue is shutting
                // down, and then no timer fires anyway.
                let _ = navigator.stop();
            }
        }
    }
}
