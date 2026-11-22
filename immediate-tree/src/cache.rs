//! Caches, which skip a body whose inputs did not change, and state, which
//! keeps UI-only data across passes.

use crate::{
    shell::{ChildKind, Core, Node, Shell, close},
    tree::{NodeId, Visit},
};
use std::{any::Any, hash::Hash};

/// Runs `body` only when `deps` differs from the value of the last run, the
/// node is new, or an event reached a node inside it this pass. A skipped
/// run declares the controls of the last run again, so the parent keeps
/// them.
///
/// Returns the first error of `body` or of closing the cache. After an
/// error, containers `body` opened may still be open; the caller unwinds.
pub fn cache<S: Shell, D: 'static + PartialEq>(
    shell: &mut S,
    id: impl Hash,
    deps: D,
    body: impl FnOnce(&mut S) -> Result<(), S::Error>,
) -> Result<(), S::Error> {
    let core = shell.core();
    let (node, visit) = core.visit(id, ChildKind::Logical, || {
        Node::Cache(CacheNode {
            deps: None,
            emitted: Vec::new(),
        })
    });
    let event = core.tree.has_event(node);
    let cache = cache_mut(core, node);
    let stored = cache
        .deps
        .as_mut()
        .and_then(|stored| stored.downcast_mut::<D>());
    if visit == Visit::Existing && !event && stored.as_deref() == Some(&deps) {
        core.emit_cached(node);
        return Ok(());
    }
    match stored {
        Some(stored) => *stored = deps,
        None => cache.deps = Some(Box::new(deps)),
    }
    core.open(node);
    body(shell)?;
    close(shell)
}

impl<N> Core<N> {
    /// UI-only state that lives while each pass declares `id` here. The
    /// first pass calls `init`; later passes return the same value.
    ///
    /// Panics when the same `id` held state of another type.
    pub fn state<T: 'static>(&mut self, id: impl Hash, init: impl FnOnce() -> T) -> &mut T {
        let (node, _) = self.visit(id, ChildKind::Logical, || Node::State(Box::new(init())));
        match &mut self.tree.node_mut(node).payload {
            Node::State(state) => state
                .downcast_mut::<T>()
                .expect("an id keeps one state type"),
            _ => unreachable!("state keys hold state"),
        }
    }
}

fn cache_mut<N>(core: &mut Core<N>, node: NodeId) -> &mut CacheNode {
    match &mut core.tree.node_mut(node).payload {
        Node::Cache(cache) => cache,
        _ => unreachable!("cache keys hold caches"),
    }
}

pub struct CacheNode {
    /// The dependencies of the last run, reassigned in place while their type
    /// stays the same.
    pub(crate) deps: Option<Box<dyn Any>>,
    /// The controls the last run declared, in order, which a skipped run
    /// declares again for the parent.
    pub(crate) emitted: Vec<NodeId>,
}

#[cfg(test)]
mod tests;
