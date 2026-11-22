//! The part of an immediate-mode UI over a retained toolkit that does not
//! touch the toolkit: the keyed node tree, the stack of open containers,
//! caches, state, tasks, streams and flows.
//!
//! A platform crate implements [`Shell`] for its `Ui` and wraps the
//! functions here in its own methods and types, so apps depend on the
//! platform crate alone. See `plans/immediate-tree.md` for the design.

mod cache;
mod children;
mod flow;
mod shell;
mod task;
mod tree;

#[cfg(test)]
mod testing;

pub use cache::{CacheNode, cache};
pub use children::{Edit, edits};
pub use flow::{Flow, FlowNode, FlowResponse, Frame, Lend, Lent, Owned, OwnedFamily, flow, owned};
pub use shell::{ChildKind, Core, Node, Shell, close, close_all, unwind};
pub use task::{StreamNode, StreamResponse, TaskNode, TaskResponse};
pub use tree::{Key, NodeId, Tree, Visit};
