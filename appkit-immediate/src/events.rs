//! Events from AppKit controls, queued for the next pass.

use crate::tree::{NodeId, Tree};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::AnyObject,
};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSWindowDelegate,
};
use objc2_foundation::{NSObject, NSObjectProtocol};
use std::{
    cell::RefCell,
    collections::{HashMap, hash_map::Entry},
    rc::Rc,
};

/// Events seen by the shared target since the last pass, and the events of
/// this pass.
pub(crate) struct Events {
    /// The one action target and delegate of every control and window.
    target: Retained<Target>,
    /// Events this pass reports, taken from the target when it began.
    current: Vec<(NodeId, EventKind)>,
}

impl Events {
    /// `wake` runs when an event arrives, to schedule a pass.
    pub(crate) fn new(mtm: MainThreadMarker, wake: Rc<dyn Fn()>) -> Self {
        let target = Target::alloc(mtm).set_ivars(TargetIvars {
            pending: RefCell::new(Vec::new()),
            senders: RefCell::new(HashMap::new()),
            wake,
        });
        Events {
            target: unsafe { msg_send![super(target), init] },
            current: Vec::new(),
        }
    }

    /// Takes the queued events for this pass and marks their nodes in `tree`.
    /// Drops events of nodes that were dropped since. Returns whether any
    /// event arrived.
    pub(crate) fn begin_pass<P>(&mut self, tree: &mut Tree<P>) -> bool {
        self.current.clear();
        std::mem::swap(
            &mut self.current,
            &mut self.target.ivars().pending.borrow_mut(),
        );
        self.current.retain(|(node, _)| tree.get(*node).is_some());
        for (node, _) in &self.current {
            tree.mark_event(*node);
        }
        !self.current.is_empty()
    }

    /// Whether `kind` happened on `node` since the last pass.
    pub(crate) fn happened(&self, node: NodeId, kind: EventKind) -> bool {
        self.current.contains(&(node, kind))
    }

    /// The shared target, to set as a control's target or a window's
    /// delegate. Register the sender with [`register`](Self::register), or
    /// its events are ignored.
    pub(crate) fn target(&self) -> &Target {
        &self.target
    }

    /// Routes events whose sender is `sender` to `node`.
    ///
    /// Panics when `sender` is already registered.
    pub(crate) fn register(&self, sender: &AnyObject, node: NodeId) {
        match self
            .target
            .ivars()
            .senders
            .borrow_mut()
            .entry(address(sender))
        {
            Entry::Vacant(entry) => entry.insert(node),
            Entry::Occupied(_) => panic!("a sender belongs to one node"),
        };
    }

    /// Stops routing events of `sender`. Call before `sender` is released, so
    /// a later object at the same address is not mistaken for it.
    pub(crate) fn forget(&self, sender: &AnyObject) {
        self.target
            .ivars()
            .senders
            .borrow_mut()
            .remove(&address(sender));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    Click,
    ShouldClose,
    ShouldTerminate,
}

pub(crate) struct TargetIvars {
    pending: RefCell<Vec<(NodeId, EventKind)>>,
    /// The node of each registered sender, by object address.
    senders: RefCell<HashMap<usize, NodeId>>,
    wake: Rc<dyn Fn()>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    pub(crate) struct Target;

    impl Target {
        /// The action selector for controls.
        #[unsafe(method(fire:))]
        fn fire(&self, sender: Option<&AnyObject>) {
            if let Some(sender) = sender {
                self.push(sender, EventKind::Click);
            }
        }
    }

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSWindowDelegate for Target {
        /// Reports the request and keeps the window: the app closes it by
        /// no longer declaring it.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &AnyObject) -> bool {
            self.push(sender, EventKind::ShouldClose);
            false
        }
    }

    unsafe impl NSApplicationDelegate for Target {
        /// Reports the request and keeps running: the app quits by declaring
        /// no window.
        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(
            &self,
            sender: &NSApplication,
        ) -> NSApplicationTerminateReply {
            self.push(sender, EventKind::ShouldTerminate);
            NSApplicationTerminateReply::TerminateCancel
        }
    }
);

impl Target {
    /// Queues `kind` for the node of `sender` and wakes the runner. Ignores a
    /// sender with no node, such as a control dropped while its event was in
    /// flight.
    fn push(&self, sender: &AnyObject, kind: EventKind) {
        let ivars = self.ivars();
        let Some(node) = ivars.senders.borrow().get(&address(sender)).copied() else {
            return;
        };
        ivars.pending.borrow_mut().push((node, kind));
        (ivars.wake)();
    }
}

fn address(object: &AnyObject) -> usize {
    std::ptr::from_ref(object).addr()
}
