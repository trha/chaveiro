//! One build pass over the declared UI: finding nodes, opening and closing
//! containers, and applying their AppKit children.

use crate::{
    children::{Edit, edits},
    events::{EventKind, Events},
    tree::{Key, NodeId, Tree, Visit},
    widget::Control,
};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSApplication, NSView, NSWindow};
use std::{hash::Hash, rc::Rc};

/// Builds the UI. The app gets it in [`App::view`](crate::App::view).
pub struct Ui {
    pub(crate) tree: Tree<Payload>,
    /// Mirrors the call stack of open containers: the root frame at the
    /// bottom during a pass, and no frame between passes.
    frames: Vec<Frame>,
    pub(crate) events: Events,
    pub(crate) mtm: MainThreadMarker,
    /// Reused buffer for formatting labels before comparing them.
    pub(crate) scratch: String,
}

impl Ui {
    pub(crate) fn new(mtm: MainThreadMarker, wake: Rc<dyn Fn()>) -> Self {
        Ui {
            tree: Tree::new(Payload::Root),
            frames: Vec::new(),
            events: Events::new(mtm, wake),
            mtm,
            scratch: String::new(),
        }
    }

    /// Starts a pass. Returns whether events arrived since the last one.
    pub(crate) fn begin(&mut self) -> bool {
        debug_assert!(self.frames.is_empty(), "the last pass closed its frames");
        self.tree.begin_pass();
        let consumed = self.events.begin_pass(&mut self.tree);
        let root = self.tree.root();
        self.open(root);
        consumed
    }

    /// Ends a pass, closing every window it did not declare. Returns the
    /// number of open windows.
    pub(crate) fn end(&mut self) -> usize {
        self.close();
        self.tree.children(self.tree.root()).len()
    }

    /// Closes every window, as a pass that declares none would. Used after a
    /// pass panicked, which may have left containers open: closing them
    /// first attaches every node they visited to its parent, so no node is
    /// left live but unreachable.
    pub(crate) fn close_all(&mut self) {
        while !self.frames.is_empty() {
            self.close();
        }
        self.tree.begin_pass();
        let root = self.tree.root();
        let mut nothing = self.tree.take_list();
        let events = &self.events;
        self.tree
            .close(root, &mut nothing, |payload| release(events, payload));
        self.tree.recycle_list(nothing);
    }

    /// Whether the user asked to quit (Cmd+Q, the Dock, logout) since the
    /// last pass. The app quits by declaring no window.
    pub fn should_terminate(&self) -> bool {
        self.events
            .happened(self.tree.root(), EventKind::ShouldTerminate)
    }

    /// Routes the application's delegate calls to the root node and makes
    /// the shared target the application's delegate.
    pub(crate) fn adopt_application(&self, application: &NSApplication) {
        self.events.register(application, self.tree.root());
        application.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
    }

    /// Opens `node` as the container for the widgets declared next.
    pub(crate) fn open(&mut self, node: NodeId) {
        let key = self.tree.node(node).key();
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
        create: impl FnOnce(MainThreadMarker) -> Payload,
    ) -> (NodeId, Visit) {
        let mtm = self.mtm;
        let frame = self
            .frames
            .last_mut()
            .expect("widgets are declared inside a pass");
        let key = Key::child(frame.key, id);
        let (node, visit) = self.tree.visit(frame.node, key, || create(mtm));
        frame.logical.push(node);
        if kind == ChildKind::Visual {
            frame.visual.push(node);
        }
        (node, visit)
    }

    /// Closes the innermost container: applies its AppKit children, then
    /// drops the children it no longer declares.
    pub(crate) fn close(&mut self) {
        let mut frame = self.frames.pop().expect("close matches open");
        self.apply_visual(frame.node, &mut frame.visual);
        let events = &self.events;
        self.tree.close(frame.node, &mut frame.logical, |payload| {
            release(events, payload)
        });
        frame.visual.clear();
        self.tree.recycle_list(frame.logical);
        self.tree.recycle_list(frame.visual);
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

    /// Makes `declared` the AppKit children of `node`. Leaves the previous
    /// list in `declared`.
    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>) {
        match &mut self.tree.node_mut(node).payload {
            Payload::Root => {}
            Payload::Window(window) => {
                assert!(
                    declared.len() <= 1,
                    "a window's body declares at most one control"
                );
                let content = declared.first().copied();
                if window.content == content {
                    return;
                }
                window.content = content;
                let window = window.window.clone();
                match content {
                    Some(content) => window.setContentView(Some(&self.control(content).view)),
                    None => window.setContentView(None),
                }
            }
            Payload::Control(control) => {
                let Control::Stack { stack, .. } = &control.control else {
                    assert!(declared.is_empty(), "only stacks have children");
                    return;
                };
                let stack = stack.clone();
                // Afterwards `declared` holds the old list.
                std::mem::swap(&mut control.visual, declared);
                for edit in edits(declared, self.control(node).visual.as_slice()) {
                    match edit {
                        Edit::RemoveAt(index) => {
                            let view = stack.arrangedSubviews().objectAtIndex(index as usize);
                            stack.removeArrangedSubview(&view);
                            view.removeFromSuperview();
                        }
                        Edit::InsertAt(index, child) => stack
                            .insertArrangedSubview_atIndex(&self.control(child).view, index as _),
                    }
                }
            }
        }
    }
}

/// How a declared node joins its container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildKind {
    /// A control: it joins the container's AppKit children as well.
    Visual,
    /// A node without a view of its own (a window): it joins only the tree.
    Logical,
}

pub(crate) enum Payload {
    Root,
    Window(WindowNode),
    Control(ControlNode),
}

pub(crate) struct WindowNode {
    pub(crate) window: Retained<NSWindow>,
    /// The control set as the content view.
    pub(crate) content: Option<NodeId>,
    /// The title last written.
    pub(crate) title: String,
}

pub(crate) struct ControlNode {
    pub(crate) view: Retained<NSView>,
    pub(crate) control: Control,
    /// AppKit children as last applied (stacks only).
    pub(crate) visual: Vec<NodeId>,
}

impl ControlNode {
    /// A control node, wrapped as the payload the tree stores.
    pub(crate) fn payload(view: Retained<NSView>, control: Control) -> Payload {
        Payload::Control(ControlNode {
            view,
            control,
            visual: Vec::new(),
        })
    }
}

/// One open container during a pass.
struct Frame {
    node: NodeId,
    key: Key,
    /// Nodes declared in it so far, in order.
    logical: Vec<NodeId>,
    /// The subset of `logical` that are AppKit children.
    visual: Vec<NodeId>,
}

/// Releases what a dropped node holds outside the tree. A dropped control
/// left its parent's view in that parent's close; dropping the payload
/// releases the view.
fn release(events: &Events, payload: Payload) {
    match payload {
        Payload::Root => {}
        Payload::Window(window) => {
            events.forget(&window.window);
            window.window.setDelegate(None);
            window.window.close();
        }
        Payload::Control(control) => events.forget(&control.view),
    }
}
