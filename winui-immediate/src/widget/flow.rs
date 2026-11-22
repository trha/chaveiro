//! Flows: futures that draw. A flow is polled where the view declares it,
//! with the pass's [`Ui`] and a value the view lends, so a sequence in time
//! can be written as one `async` block.
//!
//! See `plans/winui-flow.md` for the design.

use crate::{
    pass::{ChildKind, Payload, Ui},
    tree::{NodeId, Visit},
    widget::TaskWake,
};
use std::{
    any::Any,
    cell::Cell,
    future::Future,
    hash::Hash,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};
use windows_core::{Error, Result};

/// A family of lent types: `Of<'s>` is the type lent for a borrow of `'s`.
///
/// Rust doesn't have HKTs, so this is a `'static` marker stands in for T<'_> type constructors.
/// A type with one lifetime can derive marker from [`lend!`](crate::lend). By hand, for a
/// type with more parameters, the marker and its two impls are:
///
/// ```
/// use winui_immediate::{Lend, Lent};
///
/// pub struct Rows<'s, T> {
///     pub items: &'s [T],
/// }
///
/// pub struct RowsFamily<T>(std::marker::PhantomData<T>);
///
/// impl<T: 'static> Lend for RowsFamily<T> {
///     type Of<'s> = Rows<'s, T>;
/// }
///
/// impl<'s, T: 'static> Lent<'s> for Rows<'s, T> {
///     type Family = RowsFamily<T>;
/// }
/// ```
pub trait Lend: 'static {
    type Of<'s>;
}

/// A lent type, for a lifetime, names its family, so [`Ui::flow`] infers
/// the family from its argument.
pub trait Lent<'s>: 's {
    type Family: Lend<Of<'s> = Self>;
}

/// A `'static` value lent as itself. Get one with [`owned`].
#[repr(transparent)]
pub struct Owned<T>(T);

/// Views `value` as an [`Owned`], so a plain `&mut` can be lent to a flow.
/// A blanket impl for every `'static` type would overlap with the impls of
/// types that have a lifetime, so plain values take this one step.
pub fn owned<T: 'static>(value: &mut T) -> &mut Owned<T> {
    // SAFETY: `Owned<T>` is `repr(transparent)` over `T`, so the two have
    // one layout, and the borrow keeps its lifetime.
    unsafe { &mut *(value as *mut T).cast::<Owned<T>>() }
}

impl<T> Owned<T> {
    /// The lent value, as [`owned`] took it.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// The family of [`Owned<T>`].
pub struct OwnedFamily<T>(PhantomData<T>);

impl<T: 'static> Lend for OwnedFamily<T> {
    type Of<'s> = Owned<T>;
}

impl<'s, T: 'static> Lent<'s> for Owned<T> {
    type Family = OwnedFamily<T>;
}

/// Makes `Name<'_>` lendable to a flow: declares the marker, whose
/// `Of<'s>` is `Name<'s>`, and the [`Lent`] impl that points back to it.
/// Takes only a type whose one generic argument is its lifetime. A type
/// with more parameters writes the impls by hand, as [`Lend`] shows.
///
/// ```
/// pub struct Screen<'s> {
///     pub names: &'s [String],
/// }
/// winui_immediate::lend!(pub ScreenFamily = Screen<'_>);
///
/// // The family follows from the lent type alone.
/// fn family<'s, S: winui_immediate::Lent<'s>>(_: &mut S) -> &'static str {
///     std::any::type_name::<S::Family>()
/// }
/// let names = vec![String::from("GitHub")];
/// assert!(family(&mut Screen { names: &names }).ends_with("ScreenFamily"));
/// ```
///
/// A type with another parameter is refused, because the macro would
/// expand it wrong:
///
/// ```compile_fail
/// pub struct Rows<'s, T> {
///     pub items: &'s [T],
/// }
/// winui_immediate::lend!(RowsFamily = Rows<'_, u8>);
/// ```
#[macro_export]
macro_rules! lend {
    ($vis:vis $family:ident = $($name:ident)::+ <'_>) => {
        $vis struct $family;

        impl $crate::Lend for $family {
            type Of<'s> = $($name)::+<'s>;
        }

        impl<'s> $crate::Lent<'s> for $($name)::+<'s> {
            type Family = $family;
        }
    };
}

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
        let (node, visit) = self.visit(id, ChildKind::Logical, || {
            let future = start(Flow {
                family: PhantomData,
            });
            Ok(Payload::Flow(Box::new(FlowNode {
                future: Some(Box::pin(
                    async move { Box::new(future.await) as Box<dyn Any> },
                )),
                output: None,
                emitted: Vec::new(),
                wake: None,
            })))
        })?;
        if visit == Visit::Created {
            let wake = TaskWake::new(node, &self.tasks);
            self.flow_mut(node).wake = Some(wake);
        }
        let flow = self.flow_mut(node);
        if let Some(mut future) = flow.future.take() {
            let wake = Arc::clone(flow.wake.as_ref().expect("set when the flow was made"));
            wake.begin_poll();
            let waker = Waker::from(wake);
            let previous = LENDING.replace(Some(Lending {
                ui: self,
                state: (state as *mut S).cast::<()>(),
                node,
                opened: false,
                frame_done: false,
                error: None,
            }));
            // Polled with the future out of the node, so the future's frames
            // can reach the node's container through the `Ui`.
            let polled = future.as_mut().poll(&mut Context::from_waker(&waker));
            let lending = LENDING
                .replace(previous)
                .expect("frames put the lending back after each draw");
            let flow = self.flow_mut(node);
            match polled {
                Poll::Ready(output) => flow.output = Some(output),
                Poll::Pending => flow.future = Some(future),
            }
            // A failed draw may have left containers open inside the flow;
            // the pass fails, and the runner closes every window.
            if let Some(error) = lending.error {
                return Err(error);
            }
            if lending.opened {
                self.close()?;
            } else {
                self.emit_cached(node);
            }
        } else {
            self.emit_cached(node);
        }
        Ok(FlowResponse {
            ui: self,
            node,
            output: PhantomData,
        })
    }

    fn flow_mut(&mut self, node: NodeId) -> &mut FlowNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Flow(flow) => flow,
            _ => unreachable!("flow keys hold flows"),
        }
    }
}

/// The future's handle to what [`Ui::flow`] lends. Holds no data.
pub struct Flow<L: Lend> {
    family: PhantomData<*mut L>,
}

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
    pub fn frame<R, D>(self, draw: D) -> Frame<L, R, D>
    where
        D: FnMut(&mut Ui, &mut L::Of<'_>) -> Result<Option<R>>,
    {
        Frame {
            draw,
            family: PhantomData,
        }
    }
}

/// The future of [`Flow::frame`].
#[must_use = "a frame draws only when awaited"]
pub struct Frame<L: Lend, R, D> {
    draw: D,
    /// The family and output a frame names but does not hold. `*mut L` keeps
    /// the frame `!Send`, as its flow's handle is.
    family: PhantomData<(*mut L, fn() -> R)>,
}

// No field is pinned, so the future moves freely.
impl<L: Lend, R, D> Unpin for Frame<L, R, D> {}

impl<L, R, D> Future for Frame<L, R, D>
where
    L: Lend,
    D: FnMut(&mut Ui, &mut L::Of<'_>) -> Result<Option<R>>,
{
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<R> {
        // Taken out while the closure runs, so a nested frame finds the
        // slot empty and panics instead of making a second `&mut`.
        let mut lending = LENDING.take().expect(
            "Flow::frame is polled only inside Ui::flow, and never inside another frame's closure",
        );
        // SAFETY: `Ui::flow` stored both pointers from borrows it holds for
        // the whole poll that runs this, and the slot stays empty until they
        // are put back below, so these are the only references made from
        // them. `state` came from a `&mut S` with `S = L::Of<'s>`, and the
        // closure is generic over that lifetime.
        let (ui, state) = unsafe { (&mut *lending.ui, &mut *lending.state.cast::<L::Of<'_>>()) };
        if lending.frame_done {
            // One frame per pass: the next step draws in the next pass.
            ui.request_pass();
            LENDING.set(Some(lending));
            return Poll::Pending;
        }
        if !lending.opened {
            ui.open(lending.node);
            lending.opened = true;
        }
        let drawn = (self.draw)(ui, state);
        let polled = match drawn {
            Ok(Some(value)) => {
                lending.frame_done = true;
                Poll::Ready(value)
            }
            Ok(None) => Poll::Pending,
            Err(error) => {
                lending.error.get_or_insert(error);
                Poll::Pending
            }
        };
        LENDING.set(Some(lending));
        polled
    }
}

/// What [`Ui::flow`] returns.
pub struct FlowResponse<'u, T> {
    ui: &'u mut Ui,
    node: NodeId,
    output: PhantomData<fn() -> T>,
}

impl<T: 'static> FlowResponse<'_, T> {
    /// The output, once, in the pass the future finished in or a later
    /// one. Widgets declared before this call show the state from before
    /// the output, as with [`TaskResponse::take`](crate::TaskResponse::take).
    pub fn take(&mut self) -> Option<T> {
        let output = self.ui.flow_mut(self.node).output.take()?;
        Some(
            *output
                .downcast::<T>()
                .unwrap_or_else(|_| panic!("a flow id keeps one output type")),
        )
    }

    /// Whether the future has not finished yet.
    pub fn running(&mut self) -> bool {
        self.ui.flow_mut(self.node).future.is_some()
    }
}

/// Contract: `future` is [`None`] once the future finished, and `emitted`
/// holds the controls of the last poll that drew.
pub(crate) struct FlowNode {
    future: Option<FlowFuture>,
    output: Option<Box<dyn Any>>,
    emitted: Vec<NodeId>,
    /// Set right after the node is made, because it names the node.
    wake: Option<Arc<TaskWake>>,
}

impl FlowNode {
    pub(crate) fn emitted(&self) -> &[NodeId] {
        &self.emitted
    }

    pub(crate) fn emitted_mut(&mut self) -> &mut Vec<NodeId> {
        &mut self.emitted
    }
}

/// The future of a flow, with its output boxed so nodes hold one type.
type FlowFuture = Pin<Box<dyn Future<Output = Box<dyn Any>>>>;

/// What a flow's poll lends its frames.
///
/// Contract: `ui` and `state` are valid for the whole poll of the flow
/// `node`, because `Ui::flow` holds the borrows they came from for that
/// time. `opened` is true once a frame opened `node` as the container, and
/// `frame_done` once a frame completed, in this poll.
struct Lending {
    ui: *mut Ui,
    state: *mut (),
    node: NodeId,
    opened: bool,
    frame_done: bool,
    /// The first error a frame's closure returned in this poll.
    error: Option<Error>,
}

thread_local! {
    /// The lending of the flow being polled. Nested flows save and restore
    /// it, and a frame takes it out while its closure runs.
    static LENDING: Cell<Option<Lending>> = const { Cell::new(None) };
}

#[cfg(test)]
mod tests;
