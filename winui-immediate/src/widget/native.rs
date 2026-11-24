use super::{Control, Response};
use crate::pass::{ChildKind, ControlNode, NativeNode, NativeTree, Payload, Ui};
use immediate_tree::Node;
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
    task::Waker,
};
use windows_core::{Interface, Result};

#[cfg(test)]
mod button;
#[cfg(test)]
mod check_box;

impl Ui {
    /// A control this crate has no widget for, with state `S` that lives as
    /// long as the control and events of type `M`. `create` runs on the
    /// first pass that declares it and returns the control, which must be a
    /// XAML `UIElement`, and the starting state. The crate owns the
    /// control's place among its siblings and its lifetime; the app owns its
    /// properties, through the [`NativeResponse`]. A `native` control
    /// cannot hold declared children.
    ///
    /// `create` gets the control's [`NativeSender`], for the event handlers it
    /// registers; [`NativeResponse::sender`] gives it to handlers registered
    /// later. A message sent from any thread runs a pass, even through a
    /// [`cached`](Ui::cached) body, whose [`events`](NativeResponse::events)
    /// holds it.
    ///
    /// Panics when one id is declared with a different `E`, `S` or `M` than
    /// on the pass that made it.
    pub fn native<E, S, M>(
        &mut self,
        id: impl Hash,
        create: impl FnOnce(&NativeSender<M>) -> Result<(E, S)>,
    ) -> Result<NativeResponse<'_, E, S, M>>
    where
        E: 'static + Interface,
        S: 'static,
        M: 'static + Send,
    {
        let (node, _) = self.core.try_visit_waking(id, ChildKind::Visual, |waker| {
            let sender = NativeSender(Arc::new(Inbox {
                queue: Mutex::new(Vec::new()),
                waker: waker.clone(),
            }));
            let (element, state) = create(&sender)?;
            Ok::<_, windows_core::Error>(Node::Native(NativeNode(ControlNode::new(
                element.cast()?,
                Control::Native(Box::new(Parts {
                    element,
                    state,
                    sender,
                    events: Vec::new(),
                })),
            ))))
        })?;
        let Payload::Control(control) = self.core.tree.native_mut(node) else {
            unreachable!("widget keys hold controls")
        };
        let stale = std::mem::take(&mut control.stale);
        // The events of this pass replace those of the last, as for the
        // crate's own widgets. The swap keeps both buffers' capacity.
        let parts = parts_mut::<E, S, M>(&mut control.control);
        parts.events.clear();
        std::mem::swap(
            &mut parts.events,
            &mut *parts
                .sender
                .0
                .queue
                .lock()
                .expect("no code panics while holding the queue lock"),
        );
        Ok(NativeResponse {
            response: Response { ui: self, node },
            stale,
            _parts: std::marker::PhantomData,
        })
    }
}

/// What [`Control::Native`] holds.
struct Parts<E, S, M> {
    element: E,
    state: S,
    sender: NativeSender<M>,
    /// The messages of this pass, in the order sent.
    events: Vec<M>,
}

fn parts_mut<E: 'static, S: 'static, M: 'static>(control: &mut Control) -> &mut Parts<E, S, M> {
    let Control::Native(parts) = control else {
        unreachable!("native keys hold native controls")
    };
    parts
        .downcast_mut()
        .expect("a native id keeps the types it was made with")
}

/// Sends messages to one [`Ui::native`] control from its event handlers,
/// on any thread. Each message wakes the UI for a pass.
pub struct NativeSender<M>(Arc<Inbox<M>>);

struct Inbox<M> {
    queue: Mutex<Vec<M>>,
    waker: Waker,
}

impl<M> Clone for NativeSender<M> {
    fn clone(&self) -> Self {
        NativeSender(Arc::clone(&self.0))
    }
}

impl<M> NativeSender<M> {
    /// Queues `message` for the control's next pass. A message sent after
    /// the control dropped is dropped.
    pub fn send(&self, message: M) {
        self.0
            .queue
            .lock()
            .expect("no code panics while holding the queue lock")
            .push(message);
        self.0.waker.wake_by_ref();
    }
}

/// What [`Ui::native`] returns. Wrap it in the app's own response type to
/// give a native widget methods, as the crate's widgets have.
pub struct NativeResponse<'u, E, S, M> {
    response: Response<'u>,
    stale: bool,
    _parts: std::marker::PhantomData<Parts<E, S, M>>,
}

impl<E: 'static, S: 'static, M: 'static> NativeResponse<'_, E, S, M> {
    /// The control, as `create` returned it.
    pub fn element(&mut self) -> &E {
        &self.parts().element
    }

    /// The state, as the last pass left it.
    pub fn state(&mut self) -> &mut S {
        &mut self.parts().state
    }

    /// The control and the state at once, to write one from the other.
    pub fn split(&mut self) -> (&E, &mut S) {
        let parts = self.parts();
        (&parts.element, &mut parts.state)
    }

    /// The messages sent since the last pass, in the order sent. They last
    /// until the next pass declares the control.
    pub fn events(&mut self) -> &[M] {
        &self.parts().events
    }

    /// The control's sender, for a handler registered after `create`, such
    /// as on the first ask for an event.
    pub fn sender(&mut self) -> &NativeSender<M> {
        &self.parts().sender
    }

    /// Whether the app called [`Response::invalidate`] since the last
    /// pass. Then write every property, whatever the state says was
    /// written.
    pub fn stale(&self) -> bool {
        self.stale
    }

    fn parts(&mut self) -> &mut Parts<E, S, M> {
        let node = self.response.node;
        parts_mut(&mut self.response.ui.control_mut(node).control)
    }
}

impl<'u, E, S, M> Deref for NativeResponse<'u, E, S, M> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u, E, S, M> DerefMut for NativeResponse<'u, E, S, M> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
