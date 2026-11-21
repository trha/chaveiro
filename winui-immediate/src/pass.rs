//! One build pass over the declared UI: finding nodes, opening and closing
//! containers, and applying their WinUI children.

use crate::{
    bindings::*,
    children::{Edit, edits},
    events::{EventSet, Events},
    tree::{Key, NodeId, Tree, Visit},
    widget::Control,
};
use std::{any::Any, hash::Hash, rc::Rc};
use windows_core::{IInspectable, Interface, Result};

/// Builds the UI. The app gets it in [`App::view`](crate::App::view).
pub struct Ui {
    pub(crate) tree: Tree<Payload>,
    /// Mirrors the call stack of open containers: the root frame at the
    /// bottom during a pass, and no frame between passes.
    frames: Vec<Frame>,
    pub(crate) events: Events,
    /// Reused buffer for formatting labels before comparing them.
    pub(crate) scratch: String,
    /// Lists declared this pass, whose rows are built after the view.
    pub(crate) lists: Vec<NodeId>,
    /// Item sources to set after the pass, with no borrow held, because
    /// WinUI may ask for rows inside `SetItemsSource`.
    pub(crate) sources: Vec<(ItemsRepeater, usize)>,
    /// Set by [`request_pass`](Self::request_pass) during a pass.
    pass_requested: bool,
}

impl Ui {
    pub(crate) fn new(wake: Rc<dyn Fn()>) -> Self {
        Ui {
            tree: Tree::new(Payload::Root),
            frames: Vec::new(),
            events: Events::new(wake),
            scratch: String::new(),
            lists: Vec::new(),
            sources: Vec::new(),
            pass_requested: false,
        }
    }

    /// Asks for another pass soon after this one, for app state that
    /// changed after the widgets that show it were declared, such as an
    /// action a list row found that the next view applies. Without it, the
    /// window shows the old state until the next event or message.
    pub fn request_pass(&mut self) {
        self.pass_requested = true;
    }

    /// Whether this pass asked for another one, and forgets it.
    pub(crate) fn take_pass_request(&mut self) -> bool {
        std::mem::take(&mut self.pass_requested)
    }

    /// Starts a pass. Returns whether events arrived since the last one.
    pub(crate) fn begin(&mut self) -> bool {
        debug_assert!(self.frames.is_empty(), "the last pass closed its frames");
        self.lists.clear();
        self.tree.begin_pass();
        let consumed = self.events.begin_pass(&mut self.tree);
        let root = self.tree.root();
        self.open(root);
        consumed
    }

    /// Ends a pass, closing every window it did not declare. Returns the
    /// number of open windows.
    pub(crate) fn end(&mut self) -> Result<usize> {
        self.close()?;
        let root = self.tree.root();
        for index in 0..self.tree.children(root).len() {
            let node = self.tree.children(root)[index];
            if let Payload::Window(window) = &mut self.tree.node_mut(node).payload {
                // A window shows after its first pass, or after its first fit
                // when it fits its content, so no resize is seen.
                if window.cloaked && !(window.fit && !window.fitted_once) {
                    crate::widget::cloak(&window.window, false)?;
                    window.cloaked = false;
                }
            }
        }
        Ok(self.tree.children(root).len())
    }

    /// Closes every window, as a pass that declares none would. Used after a
    /// failed pass, which may have left frames open.
    pub(crate) fn close_all(&mut self) {
        self.unwind(0);
        self.tree.begin_pass();
        let root = self.tree.root();
        let mut nothing = self.tree.take_list();
        let events = &mut self.events;
        self.tree
            .close(root, &mut nothing, |payload| release(events, payload));
        self.tree.recycle_list(nothing);
    }

    /// The number of open containers, for [`unwind`](Self::unwind).
    pub(crate) fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Closes the containers opened after `depth`, innermost first, after a
    /// build failed inside them. Closing attaches every node they visited to
    /// its parent, so no node is left live but unreachable; a skipped
    /// discard would leave such orphans in the tree.
    pub(crate) fn unwind(&mut self, depth: usize) {
        while self.frames.len() > depth {
            // The build already failed with the error the caller returns; a
            // second WinUI error while closing changes nothing for it, and
            // the frame is popped either way.
            let _ = self.close();
        }
    }

    /// Opens `node` as the container for the widgets declared next.
    pub(crate) fn open(&mut self, node: NodeId) {
        let key = self.tree.node(node).key();
        self.open_with_key(node, key);
    }

    /// Opens `node` like [`open`](Self::open), but the children's keys
    /// derive from `scope` as well, so a new `scope` makes every child new.
    pub(crate) fn open_scoped(&mut self, node: NodeId, scope: impl Hash) {
        let key = Key::child(self.tree.node(node).key(), scope);
        self.open_with_key(node, key);
    }

    fn open_with_key(&mut self, node: NodeId, key: Key) {
        let logical = self.tree.take_list();
        let visual = self.tree.take_list();
        self.frames.push(Frame {
            node,
            key,
            logical,
            visual,
        });
    }

    /// Finds or creates the node `id` in the open container and records it
    /// as a child of the kind `kind` says.
    pub(crate) fn visit(
        &mut self,
        id: impl Hash,
        kind: ChildKind,
        create: impl FnOnce() -> Result<Payload>,
    ) -> Result<(NodeId, Visit)> {
        let frame = self
            .frames
            .last()
            .expect("widgets are declared inside a pass");
        let key = Key::child(frame.key, id);
        let parent = frame.node;
        let created = match self.tree.find(key) {
            Some(_) => None,
            None => Some(create()?),
        };
        let (node, visit) = self.tree.visit(parent, key, || {
            created.expect("a payload was created because the key was missing")
        });
        let frame = self
            .frames
            .last_mut()
            .expect("widgets are declared inside a pass");
        frame.logical.push(node);
        if kind == ChildKind::Visual {
            frame.visual.push(node);
        }
        Ok((node, visit))
    }

    /// The window the open containers are inside.
    pub(crate) fn enclosing_window(&self) -> NodeId {
        self.frames
            .iter()
            .rev()
            .map(|frame| frame.node)
            .find(|&node| matches!(self.tree.node(node).payload, Payload::Window(_)))
            .expect("controls are declared inside a window")
    }

    /// Closes the innermost container: applies its WinUI children, then drops
    /// the children it no longer declares.
    pub(crate) fn close(&mut self) -> Result<()> {
        let mut frame = self.frames.pop().expect("close matches open");
        let applied = self.apply_visual(frame.node, &mut frame.visual);
        let events = &mut self.events;
        self.tree.close(frame.node, &mut frame.logical, |payload| {
            release(events, payload)
        });
        frame.visual.clear();
        self.tree.recycle_list(frame.logical);
        self.tree.recycle_list(frame.visual);
        applied
    }

    pub(crate) fn control(&self, node: NodeId) -> &ControlNode {
        match &self.tree.node(node).payload {
            Payload::Control(control) => control,
            _ => unreachable!("visual children and widget keys are controls"),
        }
    }

    pub(crate) fn control_mut(&mut self, node: NodeId) -> &mut ControlNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Control(control) => control,
            _ => unreachable!("widget keys hold controls"),
        }
    }

    /// Makes `declared` the WinUI children of `node`. Leaves the previous
    /// list in `declared`.
    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>) -> Result<()> {
        match &mut self.tree.node_mut(node).payload {
            Payload::Root => Ok(()),
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
                    control
                    @ (Control::Row { .. } | Control::NavigationView(_) | Control::Scroll { .. }),
                element,
                ..
            }) => {
                assert!(
                    declared.len() <= 1,
                    "a list row, navigation view or scroll area declares one control"
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
                control: Control::CommandBar { commands },
                visual,
                ..
            }) => {
                let commands = commands.clone();
                // Afterwards `declared` holds the old list.
                std::mem::swap(visual, declared);
                let Payload::Control(control) = &self.tree.node(node).payload else {
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
                }) = &mut self.tree.node_mut(node).payload
                else {
                    unreachable!("matched above")
                };
                navigator.swap(old, entering)
            }
            Payload::Control(control) => {
                let Some(children) = control.control.children() else {
                    assert!(declared.is_empty(), "only panels have children");
                    return Ok(());
                };
                let children = children.clone();
                let in_grid = matches!(control.control, Control::Grid { .. });
                // Afterwards `declared` holds the old list.
                std::mem::swap(&mut control.visual, declared);
                for edit in edits(declared, self.control(node).visual.as_slice()) {
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
            Payload::Cache(cache) => {
                // A cache has no element: its controls belong to the parent.
                std::mem::swap(&mut cache.emitted, declared);
                let Payload::Cache(cache) = &self.tree.node(node).payload else {
                    unreachable!("matched above")
                };
                self.frames
                    .last_mut()
                    .expect("a cache is declared inside a container")
                    .visual
                    .extend_from_slice(&cache.emitted);
                Ok(())
            }
            Payload::State(_) => unreachable!("state nodes are never opened"),
        }
    }

    /// Adds the controls a cache emitted on its last run to the open
    /// container, for a pass that skips the cache's body.
    pub(crate) fn emit_cached(&mut self, node: NodeId) {
        let Payload::Cache(cache) = &self.tree.node(node).payload else {
            unreachable!("cache keys hold caches")
        };
        self.frames
            .last_mut()
            .expect("a cache is declared inside a container")
            .visual
            .extend_from_slice(&cache.emitted);
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

/// How a declared node joins its container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildKind {
    /// A control: it joins the container's WinUI children as well.
    Visual,
    /// A node without an element of its own (window, cache, state): it joins
    /// only the tree.
    Logical,
}

pub(crate) enum Payload {
    Root,
    Window(WindowNode),
    Control(ControlNode),
    Cache(CacheNode),
    /// UI-only data from [`Ui::state`].
    State(Box<dyn Any>),
}

pub(crate) struct CacheNode {
    /// The dependencies of the last run, reassigned in place while their type
    /// stays the same.
    pub(crate) deps: Option<Box<dyn Any>>,
    /// The controls the last run declared, in order, which a skipped run
    /// declares again for the parent.
    pub(crate) emitted: Vec<NodeId>,
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
            width: f64::NAN,
            padding: None,
            margin: None,
            dividers: None,
            error: None,
        })
    }
}

/// One open container during a pass.
struct Frame {
    node: NodeId,
    key: Key,
    /// Nodes declared in it so far, in order.
    logical: Vec<NodeId>,
    /// The subset of `logical` that are WinUI children.
    visual: Vec<NodeId>,
}

/// Releases what a dropped node holds outside the tree.
fn release(events: &mut Events, payload: Payload) {
    match payload {
        Payload::Root | Payload::Cache(_) | Payload::State(_) => {}
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
