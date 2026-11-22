//! Flows: futures that draw. A flow is polled where the view declares it,
//! with the pass's [`Ui`] and a value the view lends, so a sequence in time
//! can be written as one `async` block.
//!
//! See `plans/winui-flow.md` for the design.

use crate::pass::Ui;
use std::{future::Future, hash::Hash};
use windows_core::Result;

#[doc(inline)]
pub use immediate_tree::{Lend, Lent, Owned, OwnedFamily, owned};

impl Ui {
    /// Declares the flow `id`. The first pass that declares it calls
    /// `start`. Every pass that declares it polls the future exactly once,
    /// right here, lending `state` and this `Ui` to the future's frames for
    /// that poll only. The controls the frames declare join the parent, as
    /// a cache's do. A poll that draws nothing shows the controls from the
    /// last poll that drew. A pass that leaves the flow out drops the
    /// future and its controls, which cancels the future's work.
    ///
    /// The future's waker only asks for a turn; the flow runs when the view
    /// reaches it. A future that does work on every poll pays for each
    /// pass, because every pass polls it.
    ///
    /// Returns the first error a frame's closure returned in this pass.
    /// Panics when `id` held a flow with another output type.
    pub fn flow<'s, S, F>(
        &mut self,
        id: impl Hash,
        state: &mut S,
        start: impl FnOnce(Flow<S::Family>) -> F,
    ) -> Result<FlowResponse<'_, F::Output>>
    where
        S: Lent<'s>,
        F: 'static + Future,
        F::Output: 'static,
    {
        immediate_tree::flow(self, id, state, |flow| start(Flow(flow))).map(FlowResponse)
    }
}

/// The future's handle to what [`Ui::flow`] lends. Holds no data.
pub struct Flow<L: Lend>(immediate_tree::Flow<L, Ui>);

impl<L: Lend> Clone for Flow<L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Lend> Copy for Flow<L> {}

impl<L: Lend> Flow<L> {
    /// Draws `draw` once per pass until it returns `Some`, then resolves
    /// with that value. A frame that completes ends the flow's drawing for
    /// this pass, so the next frame first draws in the next pass, which
    /// this asks for.
    ///
    /// `draw` gets the lent value for any lifetime, so it can neither keep
    /// a borrow it reads past its call, nor store shorter-lived data into
    /// the value. Both of these fail to compile:
    ///
    /// ```compile_fail,E0521
    /// # use winui_immediate::{Flow, Lend};
    /// # struct Screen<'s> { picked: Option<&'s str> }
    /// # winui_immediate::lend!(ScreenFamily = Screen<'_>);
    /// fn keep(flow: Flow<ScreenFamily>) {
    ///     let mut kept = None;
    ///     let _ = flow.frame(|_, screen| {
    ///         kept = screen.picked;
    ///         Ok(Some(()))
    ///     });
    ///     drop(kept);
    /// }
    /// ```
    ///
    /// ```compile_fail,E0597
    /// # use winui_immediate::{Flow, Lend};
    /// # struct Screen<'s> { picked: Option<&'s str> }
    /// # winui_immediate::lend!(ScreenFamily = Screen<'_>);
    /// fn store(flow: Flow<ScreenFamily>) {
    ///     let short = String::from("short");
    ///     let _ = flow.frame(|_, screen| {
    ///         screen.picked = Some(&short);
    ///         Ok(Some(()))
    ///     });
    /// }
    /// ```
    ///
    /// Panics when polled outside [`Ui::flow`], or inside another frame's
    /// closure.
    pub fn frame<R>(
        self,
        draw: impl FnMut(&mut Ui, &mut L::Of<'_>) -> Result<Option<R>>,
    ) -> impl Future<Output = R> {
        self.0.frame(draw)
    }
}

/// What [`Ui::flow`] returns.
pub struct FlowResponse<'u, T>(immediate_tree::FlowResponse<'u, T>);

impl<T: 'static> FlowResponse<'_, T> {
    /// The output, once, in the pass the future finished in or a later
    /// one. Widgets declared before this call show the state from before
    /// the output, as with [`TaskResponse::take`](crate::TaskResponse::take).
    pub fn take(&mut self) -> Option<T> {
        self.0.take()
    }

    /// Whether the future has not finished yet.
    pub fn running(&mut self) -> bool {
        self.0.running()
    }
}
