//! The tree with its stack of open containers, and the trait a platform
//! implements to edit native children.

use crate::{
    cache::CacheNode,
    flow::FlowNode,
    task::{StreamNode, TaskNode, TaskWakes},
    tree::{Key, NodeId, Tree, Visit},
};
use std::{any::Any, hash::Hash, sync::Arc};

/// The platform's `Ui`. The functions of this crate that run user code take
/// `&mut S` and hand the same `&mut S` to that code, so it sees the
/// platform's widgets.
pub trait Shell: 'static + Sized {
    /// The platform's own payloads, such as windows and controls.
    type Native;
    /// What a toolkit call fails with. `!` on a toolkit whose calls cannot
    /// fail.
    type Error;

    fn core(&mut self) -> &mut Core<Self::Native>;

    /// Makes `declared` the native children of the native node `node`, and
    /// leaves the previous list in `declared`. Called only for
    /// [`Node::Native`] nodes, from [`close`], and only when the node's
    /// children differ from the last pass's: a node added, dropped or
    /// moved, or a cache or flow inside that ran its body. A call that fails
    /// is not repeated until the children change again.
    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>)
    -> Result<(), Self::Error>;

    /// Releases what a dropped native node holds outside the tree. Called
    /// after the tree let go of the node, in drop order.
    fn release(&mut self, native: Self::Native);
}

/// The tree's payload: the nodes this crate owns, and the platform's.
pub enum Node<N> {
    Root,
    Cache(CacheNode),
    /// UI-only data from [`state`](crate::state).
    State(Box<dyn Any>),
    /// Dropping it drops the future.
    Task(TaskNode),
    /// Dropping it drops the stream.
    Stream(StreamNode),
    /// Dropping it drops the future and, with the subtree, its controls.
    Flow(Box<FlowNode>),
    Native(N),
}

/// How a declared node joins its container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildKind {
    /// A control: it joins the container's native children as well.
    Visual,
    /// A node without a native element of its own, such as a window, a
    /// cache or a task: it joins only the tree.
    Logical,
}

/// The tree, the containers open in the current pass, and the wakes of
/// its tasks.
pub struct Core<N> {
    pub tree: Tree<Node<N>>,
    /// Mirrors the call stack of open containers: the root frame at the
    /// bottom during a pass, and no frame between passes.
    frames: Vec<Frame>,
    /// Shared with the waker of every task, stream and flow node.
    pub(crate) tasks: Arc<TaskWakes>,
    /// Set by [`request_pass`](Self::request_pass).
    pass_requested: bool,
    /// Native payloads dropped by the close in progress, released after it
    /// lets go of the tree. Kept to reuse its capacity.
    dropped: Vec<N>,
}

impl<N> Core<N> {
    /// A tree holding only the root. `post` asks the UI thread for a turn,
    /// from any thread, and returns false once that thread can no longer
    /// run one.
    pub fn new(post: impl 'static + Send + Sync + Fn() -> bool) -> Self {
        Core {
            tree: Tree::new(Node::Root),
            frames: Vec::new(),
            tasks: TaskWakes::new(post),
            pass_requested: false,
            dropped: Vec::new(),
        }
    }

    /// Asks for another pass soon after this one.
    pub fn request_pass(&mut self) {
        self.pass_requested = true;
    }

    /// Whether a pass asked for another one, and forgets it.
    pub fn take_pass_request(&mut self) -> bool {
        std::mem::take(&mut self.pass_requested)
    }

    /// The open containers, outermost first.
    pub fn open_nodes(&self) -> impl DoubleEndedIterator<Item = NodeId> + '_ {
        self.frames.iter().map(|frame| frame.node)
    }

    /// The number of open containers, for [`unwind`].
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// The native children declared so far in the open container at
    /// `depth`, counted from the outermost, which is 0.
    ///
    /// Panics when fewer than `depth + 1` containers are open.
    pub fn declared_visual(&self, depth: usize) -> &[NodeId] {
        &self.frames[depth].visual
    }

    /// Opens `node` as the container for the nodes declared next.
    pub fn open(&mut self, node: NodeId) {
        let key = self.tree.node(node).key();
        self.open_with_key(node, key);
    }

    /// Opens `node` like [`open`](Self::open), but the children's keys
    /// derive from `scope` as well, so a new `scope` makes every child new.
    pub fn open_scoped(&mut self, node: NodeId, scope: impl Hash) {
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
            changed: false,
        });
    }

    /// Finds or creates the node `id` in the open container and records it
    /// as a child of the kind `kind` says. Calls `create` only for a new
    /// node.
    ///
    /// Panics outside a pass.
    pub fn visit(
        &mut self,
        id: impl Hash,
        kind: ChildKind,
        create: impl FnOnce() -> Node<N>,
    ) -> (NodeId, Visit) {
        let Ok(visited) = self.try_visit(id, kind, || Ok::<_, !>(create()));
        visited
    }

    /// [`visit`](Self::visit) for a `create` that can fail. A failed
    /// `create` leaves the tree as it was.
    pub fn try_visit<E>(
        &mut self,
        id: impl Hash,
        kind: ChildKind,
        create: impl FnOnce() -> Result<Node<N>, E>,
    ) -> Result<(NodeId, Visit), E> {
        let frame = self
            .frames
            .last()
            .expect("widgets are declared inside a pass");
        let key = Key::child(frame.key, id);
        let parent = frame.node;
        let found = self.tree.find_child(parent, frame.logical.len(), key);
        // Created before the tree is touched, so a failure changes nothing.
        let created = match found {
            Some(_) => None,
            None => Some(create()?),
        };
        let (node, visit) = self.tree.visit_found(parent, key, found, || {
            created.expect("a payload was created because the key was missing")
        });
        let frame = self
            .frames
            .last_mut()
            .expect("widgets are declared inside a pass");
        // A child that is not the one at its place in the last pass's list
        // changes the container, so `close` applies its children.
        if self.tree.children(parent).get(frame.logical.len()) != Some(&node) {
            frame.changed = true;
        }
        frame.logical.push(node);
        if kind == ChildKind::Visual {
            frame.visual.push(node);
        }
        Ok((node, visit))
    }

    /// Makes the open container apply its children when it closes.
    fn mark_changed(&mut self) {
        if let Some(frame) = self.frames.last_mut() {
            frame.changed = true;
        }
    }

    /// Adds the controls the cache or flow `node` declared on its last run
    /// to the open container, for a pass that skips its body.
    pub(crate) fn emit_cached(&mut self, node: NodeId) {
        let emitted = match &self.tree.node(node).payload {
            Node::Cache(cache) => &cache.emitted,
            Node::Flow(flow) => &flow.emitted,
            _ => unreachable!("only caches and flows emit controls"),
        };
        self.frames
            .last_mut()
            .expect("a cache or a flow is declared inside a container")
            .visual
            .extend_from_slice(emitted);
    }
}

/// Closes the innermost container: applies its native children, then drops
/// the children it no longer declares. Returns the error of applying the
/// children; the frame is closed either way.
pub fn close<S: Shell>(shell: &mut S) -> Result<(), S::Error> {
    let core = shell.core();
    let mut frame = core.frames.pop().expect("close matches open");
    // Every child matched its place, so only a shorter list is left to
    // tell a change.
    if core.tree.children(frame.node).len() != frame.logical.len() {
        frame.changed = true;
    }
    let applied = match &mut core.tree.node_mut(frame.node).payload {
        Node::Root => Ok(()),
        // The same children in the same order give the same native
        // children, so the shell has nothing to apply.
        Node::Native(_) if !frame.changed => Ok(()),
        Node::Native(_) => shell.apply_visual(frame.node, &mut frame.visual),
        // A cache or a flow has no element: its controls belong to the
        // parent, which changes with them.
        Node::Cache(CacheNode { emitted, .. }) => {
            std::mem::swap(emitted, &mut frame.visual);
            core.emit_cached(frame.node);
            if frame.changed {
                core.mark_changed();
            }
            Ok(())
        }
        Node::Flow(flow) => {
            std::mem::swap(&mut flow.emitted, &mut frame.visual);
            core.emit_cached(frame.node);
            if frame.changed {
                core.mark_changed();
            }
            Ok(())
        }
        Node::State(_) | Node::Task(_) | Node::Stream(_) => {
            unreachable!("state, task and stream nodes are never opened")
        }
    };
    let core = shell.core();
    let dropped = &mut core.dropped;
    core.tree.close(frame.node, &mut frame.logical, |node| {
        if let Node::Native(native) = node {
            dropped.push(native);
        }
    });
    frame.visual.clear();
    core.tree.recycle_list(frame.logical);
    core.tree.recycle_list(frame.visual);
    release_dropped(shell);
    applied
}

/// Closes the containers opened after `depth`, innermost first, after a
/// build failed inside them. Closing attaches every node they visited to
/// its parent, so no node is left live but unreachable.
pub fn unwind<S: Shell>(shell: &mut S, depth: usize) {
    while shell.core().frames.len() > depth {
        // The build already failed with the error the caller returns; a
        // second error while closing changes nothing for it, and the frame
        // is popped either way.
        let _ = close(shell);
    }
}

/// Drops every node but the root, as a pass that declares nothing would.
/// Used after a failed pass, which may have left frames open.
pub fn close_all<S: Shell>(shell: &mut S) {
    unwind(shell, 0);
    let core = shell.core();
    core.tree.begin_pass();
    let root = core.tree.root();
    let mut nothing = core.tree.take_list();
    let dropped = &mut core.dropped;
    core.tree.close(root, &mut nothing, |node| {
        if let Node::Native(native) = node {
            dropped.push(native);
        }
    });
    core.tree.recycle_list(nothing);
    release_dropped(shell);
}

fn release_dropped<S: Shell>(shell: &mut S) {
    let mut dropped = std::mem::take(&mut shell.core().dropped);
    for native in dropped.drain(..) {
        shell.release(native);
    }
    shell.core().dropped = dropped;
}

/// One open container during a pass.
struct Frame {
    node: NodeId,
    key: Key,
    /// Nodes declared in it so far, in order.
    logical: Vec<NodeId>,
    /// The subset of `logical` that are native children.
    visual: Vec<NodeId>,
    /// `logical` differs from the last pass's children so far, or a cache or
    /// flow inside changed what it emits.
    changed: bool,
}
