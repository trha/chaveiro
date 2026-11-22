//! A shell with no toolkit, for the tests of this crate. Its native nodes
//! hold nothing, and applying children only records them.

use crate::{
    cache::cache,
    flow::{Flow, FlowResponse, Lent, flow},
    shell::{Core, Node, Shell, close, close_all},
    task::{StreamResponse, TaskResponse},
    tree::NodeId,
};
use futures_core::Stream;
use std::{
    future::Future,
    hash::Hash,
    ops::{Deref, DerefMut},
};

/// What a test toolkit call fails with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TestError(pub(crate) &'static str);

pub(crate) struct Ui {
    core: Core<()>,
}

impl Shell for Ui {
    type Native = ();
    type Error = TestError;

    fn core(&mut self) -> &mut Core<()> {
        &mut self.core
    }

    fn apply_visual(&mut self, _: NodeId, _: &mut Vec<NodeId>) -> Result<(), TestError> {
        Ok(())
    }

    fn release(&mut self, (): ()) {}
}

/// Read access to the tree, as the tests wrote `ui.tree`.
impl Deref for Ui {
    type Target = Core<()>;

    fn deref(&self) -> &Core<()> {
        &self.core
    }
}

impl Ui {
    pub(crate) fn new(post: impl 'static + Send + Sync + Fn() -> bool) -> Self {
        Ui {
            core: Core::new(post),
        }
    }

    pub(crate) fn begin(&mut self) {
        self.core.tree.begin_pass();
        let root = self.core.tree.root();
        self.core.open(root);
    }

    /// Ends a pass. Returns the number of native nodes at the root.
    pub(crate) fn end(&mut self) -> Result<usize, TestError> {
        close(self)?;
        let root = self.core.tree.root();
        Ok(self
            .core
            .tree
            .children(root)
            .iter()
            .filter(|node| matches!(self.core.tree.node(**node).payload, Node::Native(())))
            .count())
    }

    pub(crate) fn close_all(&mut self) {
        close_all(self);
    }

    pub(crate) fn take_pass_request(&mut self) -> bool {
        self.core.take_pass_request()
    }

    pub(crate) fn poll_woken_tasks(&mut self) {
        self.core.poll_woken_tasks();
    }

    pub(crate) fn state<T: 'static>(
        &mut self,
        id: impl Hash,
        init: impl FnOnce() -> T,
    ) -> Result<&mut T, TestError> {
        Ok(self.core.state(id, init))
    }

    pub(crate) fn cached<D: 'static + PartialEq>(
        &mut self,
        id: impl Hash,
        deps: D,
        body: impl FnOnce(&mut Ui) -> Result<(), TestError>,
    ) -> Result<(), TestError> {
        cache(self, id, deps, body)
    }

    pub(crate) fn task<F>(
        &mut self,
        id: impl Hash,
        start: impl FnOnce() -> F,
    ) -> TaskResponse<'_, F::Output>
    where
        F: 'static + Future,
        F::Output: 'static,
    {
        self.core.task(id, start)
    }

    pub(crate) fn stream<S>(
        &mut self,
        id: impl Hash,
        start: impl FnOnce() -> S,
    ) -> StreamResponse<'_, S::Item>
    where
        S: 'static + Stream,
        S::Item: 'static,
    {
        self.core.stream(id, start)
    }

    pub(crate) fn flow<'s, L, F>(
        &mut self,
        id: impl Hash,
        state: &mut L,
        start: impl FnOnce(Flow<L::Family, Ui>) -> F,
    ) -> Result<FlowResponse<'_, F::Output>, TestError>
    where
        L: Lent<'s>,
        F: 'static + Future,
        F::Output: 'static,
    {
        flow(self, id, state, start)
    }
}

impl DerefMut for Ui {
    fn deref_mut(&mut self) -> &mut Core<()> {
        &mut self.core
    }
}

/// A UI whose tasks post nothing, for tests that drive turns by hand.
pub(crate) fn ui() -> Ui {
    Ui::new(|| true)
}
