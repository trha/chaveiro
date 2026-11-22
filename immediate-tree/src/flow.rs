//! Flows: futures that draw. A flow is polled where the view declares it,
//! with the pass's [`Shell`] and a value the view lends, so a sequence in
//! time can be written as one `async` block.
//!
//! See `plans/winui-flow.md` for the design.

use crate::{
    shell::{ChildKind, Node, Shell, close},
    task::TaskWake,
    tree::{NodeId, Visit},
};
use std::{
    any::{Any, TypeId},
    cell::Cell,
    future::Future,
    hash::Hash,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

/// A family of lent types: `Of<'s>` is the type lent for a borrow of `'s`.
///
/// Rust doesn't have HKTs, so this is a `'static` marker stands in for T<'_> type constructors.
/// A type with one lifetime can derive marker from [`lend!`](crate::lend). By hand, for a
/// type with more parameters, the marker and its two impls are:
///
/// ```
/// use immediate_tree::{Lend, Lent};
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

/// A lent type, for a lifetime, names its family, so [`flow`] infers
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
/// immediate_tree::lend!(pub ScreenFamily = Screen<'_>);
///
/// // The family follows from the lent type alone.
/// fn family<'s, S: immediate_tree::Lent<'s>>(_: &mut S) -> &'static str {
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
/// immediate_tree::lend!(RowsFamily = Rows<'_, u8>);
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

/// Declares the flow `id`. The first pass that declares it calls `start`.
/// Every pass that declares it polls the future exactly once, right here,
/// lending `state` and `shell` to the future's frames for that poll only.
/// The controls the frames declare join the parent, as a cache's do. A
/// poll that draws nothing shows the controls from the last poll that
/// drew. A pass that leaves the flow out drops the future and its
/// controls.
///
/// The future's waker only asks for a turn; the flow runs when the view
/// reaches it.
///
/// Returns the first error a frame's closure returned in this pass, with
/// containers the frame opened maybe still open; the caller unwinds.
/// Panics when `id` held a flow with another output type.
pub fn flow<'u, 's, S, L, F>(
    shell: &'u mut S,
    id: impl Hash,
    state: &mut L,
    start: impl FnOnce(Flow<L::Family, S>) -> F,
) -> Result<FlowResponse<'u, F::Output>, S::Error>
where
    S: Shell,
    L: Lent<'s>,
    F: 'static + Future,
    F::Output: 'static,
{
    let core = shell.core();
    let (node, visit) = core.visit(id, ChildKind::Logical, || {
        let future = start(Flow {
            family: PhantomData,
        });
        Node::Flow(Box::new(FlowNode {
            future: Some(Box::pin(
                async move { Box::new(future.await) as Box<dyn Any> },
            )),
            output: None,
            emitted: Vec::new(),
            wake: None,
        }))
    });
    if visit == Visit::Created {
        let wake = TaskWake::new(node, &core.tasks);
        flow_mut(&mut core.tree, node).wake = Some(wake);
    }
    let flow = flow_mut(&mut core.tree, node);
    let Some(mut future) = flow.future.take() else {
        core.emit_cached(node);
        return Ok(FlowResponse {
            node: flow_mut(&mut shell.core().tree, node),
            output: PhantomData,
        });
    };
    let wake = Arc::clone(flow.wake.as_ref().expect("set when the flow was made"));
    wake.begin_poll();
    let waker = Waker::from(wake);
    let mut error: Option<S::Error> = None;
    let previous = LENDING.replace(Some(Lending {
        shell: (shell as *mut S).cast::<()>(),
        state: (state as *mut L).cast::<()>(),
        error: (&mut error as *mut Option<S::Error>).cast::<()>(),
        types: (TypeId::of::<L::Family>(), TypeId::of::<S>()),
        node,
        opened: false,
        frame_done: false,
    }));
    // Polled with the future out of the node, so the future's frames can
    // reach the node's container through the shell.
    let polled = future.as_mut().poll(&mut Context::from_waker(&waker));
    let lending = LENDING
        .replace(previous)
        .expect("frames put the lending back after each draw");
    let flow = flow_mut(&mut shell.core().tree, node);
    match polled {
        Poll::Ready(output) => flow.output = Some(output),
        Poll::Pending => flow.future = Some(future),
    }
    if let Some(error) = error {
        return Err(error);
    }
    match lending.opened {
        true => close(shell)?,
        false => shell.core().emit_cached(node),
    }
    Ok(FlowResponse {
        node: flow_mut(&mut shell.core().tree, node),
        output: PhantomData,
    })
}

fn flow_mut<N>(tree: &mut crate::tree::Tree<Node<N>>, node: NodeId) -> &mut FlowNode {
    match &mut tree.node_mut(node).payload {
        Node::Flow(flow) => flow,
        _ => unreachable!("flow keys hold flows"),
    }
}

/// The future's handle to what [`flow`] lends: the value of the family `L`
/// and the shell `S`. Holds no data.
pub struct Flow<L: Lend, S: Shell> {
    family: PhantomData<(*mut L, *mut S)>,
}

impl<L: Lend, S: Shell> Clone for Flow<L, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Lend, S: Shell> Copy for Flow<L, S> {}

impl<L: Lend, S: Shell> Flow<L, S> {
    /// Draws `draw` once per pass until it returns `Some`, then resolves
    /// with that value. A frame that completes ends the flow's drawing for
    /// this pass, so the next frame first draws in the next pass, which
    /// this asks for.
    ///
    /// `draw` gets the lent value for any lifetime, so it can neither keep
    /// a borrow it reads past its call, nor store shorter-lived data into
    /// the value. The platform crates test both with `compile_fail` docs.
    ///
    /// Panics when polled outside [`flow`], inside another frame's closure,
    /// or inside a flow that lends another family or shell.
    pub fn frame<R, D>(self, draw: D) -> Frame<L, S, R, D>
    where
        D: FnMut(&mut S, &mut L::Of<'_>) -> Result<Option<R>, S::Error>,
    {
        Frame {
            draw,
            family: PhantomData,
        }
    }
}

/// The future of [`Flow::frame`].
#[must_use = "a frame draws only when awaited"]
pub struct Frame<L: Lend, S: Shell, R, D> {
    draw: D,
    family: PhantomData<(*mut L, *mut S, fn() -> R)>,
}

// No field is pinned, so the future moves freely.
impl<L: Lend, S: Shell, R, D> Unpin for Frame<L, S, R, D> {}

impl<L, S, R, D> Future for Frame<L, S, R, D>
where
    L: Lend,
    S: Shell,
    D: FnMut(&mut S, &mut L::Of<'_>) -> Result<Option<R>, S::Error>,
{
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<R> {
        // Taken out while the closure runs, so a nested frame finds the
        // slot empty and panics instead of making a second `&mut`.
        let mut lending = LENDING.take().expect(
            "Flow::frame is polled only inside Ui::flow, and never inside another frame's closure",
        );
        // A `Flow` is `Copy`, so a future can carry it into another flow.
        // The casts below are sound only for the flow's own types.
        assert!(
            lending.types == (TypeId::of::<L>(), TypeId::of::<S>()),
            "a frame is polled only inside a flow of its own family and shell"
        );
        // SAFETY: `flow` stored the three pointers from borrows it holds for
        // the whole poll that runs this, and the slot stays empty until they
        // are put back below, so these are the only references made from
        // them. The assert above checked that `S` and `L` are the types
        // `flow` stored them as: `state` came from a `&mut L::Of<'s>`, and
        // the closure is generic over that lifetime.
        let (shell, state, error) = unsafe {
            (
                &mut *lending.shell.cast::<S>(),
                &mut *lending.state.cast::<L::Of<'_>>(),
                &mut *lending.error.cast::<Option<S::Error>>(),
            )
        };
        if lending.frame_done {
            // One frame per pass: the next step draws in the next pass.
            shell.core().request_pass();
            LENDING.set(Some(lending));
            return Poll::Pending;
        }
        if !lending.opened {
            shell.core().open(lending.node);
            lending.opened = true;
        }
        let polled = match (self.draw)(shell, state) {
            Ok(Some(value)) => {
                lending.frame_done = true;
                Poll::Ready(value)
            }
            Ok(None) => Poll::Pending,
            Err(drawn) => {
                error.get_or_insert(drawn);
                Poll::Pending
            }
        };
        LENDING.set(Some(lending));
        polled
    }
}

/// What [`flow`] returns.
pub struct FlowResponse<'u, T> {
    node: &'u mut FlowNode,
    output: PhantomData<fn() -> T>,
}

impl<T: 'static> FlowResponse<'_, T> {
    /// The output, once, in the pass the future finished in or a later
    /// one.
    pub fn take(&mut self) -> Option<T> {
        let output = self.node.output.take()?;
        Some(
            *output
                .downcast::<T>()
                .unwrap_or_else(|_| panic!("a flow id keeps one output type")),
        )
    }

    /// Whether the future has not finished yet.
    pub fn running(&mut self) -> bool {
        self.node.future.is_some()
    }
}

/// Contract: `future` is [`None`] once the future finished, and `emitted`
/// holds the controls of the last poll that drew.
pub struct FlowNode {
    future: Option<Pin<Box<dyn Future<Output = Box<dyn Any>>>>>,
    output: Option<Box<dyn Any>>,
    pub(crate) emitted: Vec<NodeId>,
    /// Set right after the node is made, because it names the node.
    wake: Option<Arc<TaskWake>>,
}

/// What a flow's poll lends its frames.
///
/// Contract: `shell`, `state` and `error` are valid for the whole poll of
/// the flow `node`, because `flow` holds the borrows they came from for
/// that time, and `types` names the family and shell they point to.
/// `opened` is true once a frame opened `node` as the container, and
/// `frame_done` once a frame completed, in this poll.
struct Lending {
    shell: *mut (),
    state: *mut (),
    /// An `Option<S::Error>`: the first error a frame's closure returned.
    error: *mut (),
    types: (TypeId, TypeId),
    node: NodeId,
    opened: bool,
    frame_done: bool,
}

thread_local! {
    /// The lending of the flow being polled. Nested flows save and restore
    /// it, and a frame takes it out while its closure runs.
    static LENDING: Cell<Option<Lending>> = const { Cell::new(None) };
}

#[cfg(test)]
mod tests;
