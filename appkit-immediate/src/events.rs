//! Events from AppKit controls, queued for the next pass.

use crate::tree::{NodeId, Tree};
use ahash::AHashMap;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Sel},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSComboBoxDelegate,
    NSControl, NSControlTextEditingDelegate, NSTableColumn, NSTableRowView, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate, NSTextView, NSView,
    NSWindowDelegate,
};
use objc2_foundation::{NSInteger, NSNotification, NSObject, NSObjectProtocol};
use std::{cell::RefCell, collections::hash_map::Entry, rc::Rc};

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
            senders: RefCell::new(AHashMap::new()),
            row_counts: RefCell::new(AHashMap::new()),
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

    /// The editing commands `node` took since the last pass, in order.
    pub(crate) fn commands(&self, node: NodeId) -> impl Iterator<Item = Command> {
        self.current
            .iter()
            .filter_map(move |(from, kind)| match kind {
                EventKind::Command(command) if *from == node => Some(*command),
                _ => None,
            })
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
        let ivars = self.target.ivars();
        ivars.senders.borrow_mut().remove(&address(sender));
        ivars.row_counts.borrow_mut().remove(&address(sender));
    }

    /// Makes the shared target report `count` rows for `table`, which AppKit
    /// may ask while a pass runs.
    pub(crate) fn set_row_count(&self, table: &NSTableView, count: usize) {
        self.target
            .ivars()
            .row_counts
            .borrow_mut()
            .insert(address(table), count);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    /// A control sent its action: a button click, a pop-up pick, Return in
    /// a text field.
    Action,
    /// The user changed a text field's text.
    TextDidChange,
    /// A text field lost the keyboard focus after editing.
    DidEndEditing,
    /// A text field's editor took a command, such as an arrow key. Only
    /// the commands of [`Command`] are reported.
    Command(Command),
    ShouldClose,
    ShouldTerminate,
}

/// An editing command a text field's editor ran, by the key that sends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Left,
    Right,
    /// Home, or Command-Left.
    Home,
    /// End, or Command-Right.
    End,
    Backspace,
}

impl Command {
    fn from_selector(selector: Sel) -> Option<Self> {
        Some(match selector {
            selector if selector == sel!(moveLeft:) => Command::Left,
            selector if selector == sel!(moveRight:) => Command::Right,
            selector
                if selector == sel!(moveToBeginningOfLine:)
                    || selector == sel!(moveToLeftEndOfLine:)
                    || selector == sel!(moveToBeginningOfDocument:)
                    || selector == sel!(scrollToBeginningOfDocument:) =>
            {
                Command::Home
            }
            selector
                if selector == sel!(moveToEndOfLine:)
                    || selector == sel!(moveToRightEndOfLine:)
                    || selector == sel!(moveToEndOfDocument:)
                    || selector == sel!(scrollToEndOfDocument:) =>
            {
                Command::End
            }
            selector if selector == sel!(deleteBackward:) => Command::Backspace,
            _ => return None,
        })
    }
}

pub(crate) struct TargetIvars {
    pending: RefCell<Vec<(NodeId, EventKind)>>,
    /// The node of each registered sender, by object address.
    senders: RefCell<AHashMap<usize, NodeId>>,
    /// The row count of each table view, by address, kept here so AppKit can
    /// read it without the [`Ui`](crate::Ui).
    row_counts: RefCell<AHashMap<usize, usize>>,
    wake: Rc<dyn Fn()>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    pub(crate) struct Target;

    impl Target {
        /// The action selector for controls. A text field sends it on
        /// Return.
        #[unsafe(method(fire:))]
        fn fire(&self, sender: Option<&AnyObject>) {
            if let Some(sender) = sender {
                self.push(sender, EventKind::Action);
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

    unsafe impl NSControlTextEditingDelegate for Target {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, notification: &NSNotification) {
            if let Some(field) = notification.object() {
                self.push(&field, EventKind::TextDidChange);
            }
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, notification: &NSNotification) {
            if let Some(field) = notification.object() {
                self.push(&field, EventKind::DidEndEditing);
            }
        }

        /// Reports the commands of [`Command`] and lets the editor run
        /// every command as usual.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, control: &NSControl, _editor: &NSTextView, selector: Sel) -> bool {
            if let Some(command) = Command::from_selector(selector) {
                self.push(control, EventKind::Command(command));
            }
            false
        }
    }

    unsafe impl NSTextFieldDelegate for Target {}

    unsafe impl NSComboBoxDelegate for Target {}

    unsafe impl NSTableViewDataSource for Target {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, table: &NSTableView) -> NSInteger {
            let count = self
                .ivars()
                .row_counts
                .borrow()
                .get(&address(table))
                .copied()
                .unwrap_or(0);
            NSInteger::try_from(count).expect("a table view has fewer than 2^63 rows")
        }
    }

    unsafe impl NSTableViewDelegate for Target {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            // The macro wraps the body, so `?` is not available here.
            let node = self.node_of(table);
            let row = usize::try_from(row).ok();
            match (node, row) {
                (Some(node), Some(row)) => Some(crate::runner::realize_row(self.mtm(), node, row)),
                _ => None,
            }
        }

        #[unsafe(method(tableView:didRemoveRowView:forRow:))]
        fn did_remove_row_view(&self, table: &NSTableView, row_view: &NSTableRowView, _row: NSInteger) {
            let Some(node) = self.node_of(table) else {
                return;
            };
            // A row view without a cell held a placeholder or nothing.
            if let Some(cell) = row_view.viewAtColumn(0) {
                crate::runner::recycle_row(node, address(&cell));
            }
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
    /// The node registered for `sender`.
    fn node_of(&self, sender: &AnyObject) -> Option<NodeId> {
        self.ivars().senders.borrow().get(&address(sender)).copied()
    }

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
