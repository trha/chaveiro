use crate::{
    pass::{CacheNode, ChildKind, Payload, Ui},
    tree::{NodeId, Visit},
};
use std::{any::Any, hash::Hash};

impl Ui {
    /// Runs `body` only when `deps` differs from the value of the last run,
    /// on the first pass that declares it, or when an event reached a control
    /// inside it this pass. Otherwise the controls the last run declared stay
    /// as they are, and the pass costs one comparison.
    ///
    /// `body` must declare the same controls whenever `deps` is equal, because
    /// a skipped run keeps the old ones. State `body` reads that is not in
    /// `deps` will look stale until `deps` changes or an event arrives.
    pub fn cached<D: 'static + PartialEq>(
        &mut self,
        id: impl Hash,
        deps: D,
        body: impl FnOnce(&mut Ui),
    ) {
        let (node, visit) = self.visit(id, ChildKind::Logical, |_| {
            Payload::Cache(CacheNode {
                deps: None,
                emitted: Vec::new(),
            })
        });
        let event = self.tree.has_event(node);
        let cache = self.cache_mut(node);
        let stored = cache
            .deps
            .as_mut()
            .and_then(|stored| stored.downcast_mut::<D>());
        if visit == Visit::Existing && !event && stored.as_deref() == Some(&deps) {
            self.emit_cached(node);
            return;
        }
        match stored {
            Some(stored) => *stored = deps,
            None => cache.deps = Some(Box::new(deps)),
        }
        self.open(node);
        body(self);
        self.close();
    }

    /// UI-only state that lives while each pass declares `id` here: an
    /// expanded flag, a scroll anchor. `init` runs when the state is created.
    ///
    /// To keep a value derived from inputs, recomputed only when they change,
    /// put the inputs in `id`. A pass with new inputs declares a new id, so
    /// `init` computes the value again, and the old state is dropped when the
    /// container closes:
    ///
    /// ```no_run
    /// # fn view(ui: &mut appkit_immediate::Ui, names: &[String], filter: &str) {
    /// let shown: &mut Vec<usize> = ui.state(("matches", filter, names.len()), || {
    ///     (0..names.len()).filter(|&index| names[index].contains(filter)).collect()
    /// });
    /// # }
    /// ```
    ///
    /// The inputs must be [`Hash`], and they are compared through their 64-bit
    /// key, so two different inputs that collide would keep the old value.
    /// Inputs that are not `Hash`, such as floats, need a stand-in such as a
    /// revision number. Alternating inputs recompute every time.
    ///
    /// Panics when the same `id` held state of another type.
    pub fn state<T: 'static>(&mut self, id: impl Hash, init: impl FnOnce() -> T) -> &mut T {
        let (node, _) = self.visit(id, ChildKind::Logical, |_| Payload::State(Box::new(init())));
        self.state_mut(node)
            .downcast_mut::<T>()
            .expect("an id keeps one state type")
    }

    fn cache_mut(&mut self, node: NodeId) -> &mut CacheNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Cache(cache) => cache,
            _ => unreachable!("cache keys hold caches"),
        }
    }

    fn state_mut(&mut self, node: NodeId) -> &mut Box<dyn Any> {
        match &mut self.tree.node_mut(node).payload {
            Payload::State(state) => state,
            _ => unreachable!("state keys hold state"),
        }
    }
}

#[cfg(test)]
mod tests;
