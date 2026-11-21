//! WinUI events as data: shared delegates queue `(sender, kind)`, and each
//! pass resolves them to nodes.

use crate::{
    bindings::*,
    tree::{NodeId, Tree},
};
use ahash::AHashMap;
use std::{cell::RefCell, rc::Rc};
use windows_core::{IInspectable, IUnknown, Interface, Ref, Result};

/// The event state of one window set.
///
/// Contract: a control is in [`senders`](Self::senders) exactly while it is
/// subscribed to at least one event kind and its node is live; the pass
/// removes it when the node drops. [`current`](Self::current) holds only
/// events whose sender was live when the pass began.
pub(crate) struct Events {
    delegates: Delegates,
    /// COM identity (the `IUnknown` pointer) of each subscribed event source.
    senders: AHashMap<usize, NodeId>,
    inbox: Rc<Inbox>,
    /// Events of the current pass, resolved to nodes.
    current: Vec<(NodeId, EventKind)>,
    /// Swapped with the inbox at the start of each pass, to reuse capacity.
    drained: Vec<(usize, EventKind)>,
}

impl Events {
    /// Creates the shared delegates. `wake` runs after every queued event and
    /// must only schedule a pass, never run one.
    pub(crate) fn new(wake: Rc<dyn Fn()>) -> Self {
        let inbox = Rc::new(Inbox {
            queued: RefCell::new(Vec::new()),
            wake,
        });
        tracing::debug!(count = Delegates::COUNT, "shared delegates");
        Events {
            delegates: Delegates::new(&inbox),
            senders: AHashMap::new(),
            inbox,
            current: Vec::new(),
            drained: Vec::new(),
        }
    }

    /// Resolves the events queued since the last pass and marks each target
    /// and its ancestors in `tree`. Returns whether there were any.
    pub(crate) fn begin_pass<P>(&mut self, tree: &mut Tree<P>) -> bool {
        self.current.clear();
        std::mem::swap(&mut self.drained, &mut *self.inbox.queued.borrow_mut());
        for (sender, kind) in self.drained.drain(..) {
            // An event from a control dropped since it fired has no target.
            if let Some(node) = self.senders.get(&sender).copied() {
                tree.mark_event(node);
                self.current.push((node, kind));
            }
        }
        !self.current.is_empty()
    }

    /// Whether `node` got `kind` since the last pass.
    pub(crate) fn happened(&self, node: NodeId, kind: EventKind) -> bool {
        self.current.contains(&(node, kind))
    }

    /// Of `first` and `second`, the one `node` got last this pass.
    pub(crate) fn last_of(
        &self,
        node: NodeId,
        first: EventKind,
        second: EventKind,
    ) -> Option<EventKind> {
        self.current
            .iter()
            .rev()
            .find(|(target, kind)| *target == node && (*kind == first || *kind == second))
            .map(|(_, kind)| *kind)
    }

    /// Registers the shared delegate for `kind` on `source`, once per node and
    /// kind, and maps `source` to `node`. `subscribed` is the node's record.
    pub(crate) fn subscribe(
        &mut self,
        node: NodeId,
        subscribed: &mut EventSet,
        source: &impl Interface,
        kind: EventKind,
    ) -> Result<()> {
        if subscribed.contains(kind) {
            return Ok(());
        }
        match kind {
            EventKind::Click => source
                .cast::<IButtonBase>()?
                .add_click(&self.delegates.click)?,
            EventKind::CloseRequested => source
                .cast::<IAppWindow>()?
                .add_closing(&self.delegates.close_requested)?,
            EventKind::TextChanged => source
                .cast::<ITextBox>()?
                .add_text_changed(&self.delegates.text_changed)?,
            EventKind::CompositionStarted => source
                .cast::<ITextBox>()?
                .add_text_composition_started(&self.delegates.composition_started)?,
            EventKind::CompositionEnded => source
                .cast::<ITextBox>()?
                .add_text_composition_ended(&self.delegates.composition_ended)?,
            EventKind::Enter => source
                .cast::<IUIElement>()?
                .add_key_down(&self.delegates.enter)?,
            EventKind::DropDownClosed => source
                .cast::<IComboBox>()?
                .add_drop_down_closed(&self.delegates.drop_down_closed)?,
            EventKind::TextSubmitted => source
                .cast::<IComboBox>()?
                .add_text_submitted(&self.delegates.text_submitted)?,
            EventKind::LostFocus => source
                .cast::<IUIElement>()?
                .add_lost_focus(&self.delegates.lost_focus)?,
            EventKind::PasswordChanged => source
                .cast::<IPasswordBox>()?
                .add_password_changed(&self.delegates.password_changed)?,
            EventKind::SizeChanged => source
                .cast::<IFrameworkElement>()?
                .add_size_changed(&self.delegates.size_changed)?,
            EventKind::SelectionChanged => source
                .cast::<ISelector>()?
                .add_selection_changed(&self.delegates.selection_changed)?,
        };
        let identity = identity(source)?;
        tracing::trace!(?kind, source = identity, "subscribe");
        self.senders.insert(identity, node);
        subscribed.insert(kind);
        Ok(())
    }

    /// Forgets `source`, whose node is being dropped.
    pub(crate) fn forget(&mut self, source: &impl Interface) {
        // A source whose identity cannot be read was never subscribed,
        // because `subscribe` reads it the same way.
        if let Ok(identity) = identity(source) {
            self.senders.remove(&identity);
        }
    }
}

/// The kinds of event a widget can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum EventKind {
    Click,
    CloseRequested,
    TextChanged,
    CompositionStarted,
    CompositionEnded,
    /// The Enter key went down; other keys raise nothing.
    Enter,
    SelectionChanged,
    SizeChanged,
    LostFocus,
    PasswordChanged,
    /// An editable combo box took typed text, on Enter or on leaving it.
    TextSubmitted,
    /// A combo box's list closed.
    DropDownClosed,
}

/// Event kinds a node is subscribed to, one bit per [`EventKind`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventSet(u16);

impl EventSet {
    pub(crate) fn contains(self, kind: EventKind) -> bool {
        self.0 & (1 << kind as u8) != 0
    }

    pub(crate) fn insert(&mut self, kind: EventKind) {
        self.0 |= 1 << kind as u8;
    }
}

/// Queued events, shared with the delegates.
struct Inbox {
    queued: RefCell<Vec<(usize, EventKind)>>,
    wake: Rc<dyn Fn()>,
}

impl Inbox {
    fn push(&self, sender: Option<&IUnknown>, kind: EventKind) {
        // A delegate without a sender cannot be routed to a node.
        let Some(sender) = sender else { return };
        self.queued
            .borrow_mut()
            .push((sender.as_raw() as usize, kind));
        (self.wake)();
    }
}

/// One delegate object per event kind, registered on every source.
struct Delegates {
    click: RoutedEventHandler,
    close_requested: TypedEventHandler<AppWindow, AppWindowClosingEventArgs>,
    text_changed: TextChangedEventHandler,
    composition_started: TypedEventHandler<TextBox, TextCompositionStartedEventArgs>,
    composition_ended: TypedEventHandler<TextBox, TextCompositionEndedEventArgs>,
    enter: KeyEventHandler,
    selection_changed: SelectionChangedEventHandler,
    size_changed: SizeChangedEventHandler,
    lost_focus: RoutedEventHandler,
    password_changed: RoutedEventHandler,
    text_submitted: TypedEventHandler<ComboBox, ComboBoxTextSubmittedEventArgs>,
    drop_down_closed: EventHandler<IInspectable>,
}

impl Delegates {
    /// The number of delegate objects, one per event kind. Each field is one
    /// interface pointer, so the size gives the count.
    const COUNT: usize = size_of::<Delegates>() / size_of::<usize>();

    fn new(inbox: &Rc<Inbox>) -> Self {
        let click = {
            let inbox = Rc::clone(inbox);
            RoutedEventHandler::shared(move |sender: Ref<'_, IInspectable>, _| {
                inbox.push(canonical(sender.ok().ok()).as_ref(), EventKind::Click)
            })
        };
        let close_requested = {
            let inbox = Rc::clone(inbox);
            TypedEventHandler::shared(
                move |sender: Ref<'_, AppWindow>, arguments: Ref<'_, AppWindowClosingEventArgs>| {
                    // The app decides: it closes the window by no longer
                    // declaring it. If cancelling fails, the window closes
                    // and the next pass drops its node anyway.
                    if let Ok(arguments) = arguments.ok() {
                        let _ = arguments.SetCancel(true);
                    }
                    inbox.push(
                        canonical(sender.ok().ok()).as_ref(),
                        EventKind::CloseRequested,
                    );
                },
            )
        };
        let text_changed = {
            let inbox = Rc::clone(inbox);
            TextChangedEventHandler::shared(move |sender: Ref<'_, IInspectable>, _| {
                inbox.push(canonical(sender.ok().ok()).as_ref(), EventKind::TextChanged)
            })
        };
        let composition_started = {
            let inbox = Rc::clone(inbox);
            TypedEventHandler::shared(move |sender: Ref<'_, TextBox>, _| {
                inbox.push(
                    canonical(sender.ok().ok()).as_ref(),
                    EventKind::CompositionStarted,
                )
            })
        };
        let composition_ended = {
            let inbox = Rc::clone(inbox);
            TypedEventHandler::shared(move |sender: Ref<'_, TextBox>, _| {
                inbox.push(
                    canonical(sender.ok().ok()).as_ref(),
                    EventKind::CompositionEnded,
                )
            })
        };
        let enter = {
            let inbox = Rc::clone(inbox);
            KeyEventHandler::shared(
                move |sender: Ref<'_, IInspectable>, arguments: Ref<'_, KeyRoutedEventArgs>| {
                    let Ok(arguments) = arguments.ok() else {
                        return;
                    };
                    if arguments.Key().ok() != Some(VirtualKey::Enter) {
                        return;
                    }
                    // Marked handled so a parent does not act on it too. If
                    // the write fails the key only bubbles on.
                    let _ = arguments.SetHandled(true);
                    inbox.push(canonical(sender.ok().ok()).as_ref(), EventKind::Enter);
                },
            )
        };
        let selection_changed = {
            let inbox = Rc::clone(inbox);
            SelectionChangedEventHandler::shared(move |sender: Ref<'_, IInspectable>, _| {
                inbox.push(
                    canonical(sender.ok().ok()).as_ref(),
                    EventKind::SelectionChanged,
                )
            })
        };
        let size_changed = {
            let inbox = Rc::clone(inbox);
            SizeChangedEventHandler::shared(move |sender: Ref<'_, IInspectable>, _| {
                inbox.push(canonical(sender.ok().ok()).as_ref(), EventKind::SizeChanged)
            })
        };
        let lost_focus = {
            let inbox = Rc::clone(inbox);
            RoutedEventHandler::shared(move |sender: Ref<'_, IInspectable>, _| {
                inbox.push(canonical(sender.ok().ok()).as_ref(), EventKind::LostFocus)
            })
        };
        let password_changed = {
            let inbox = Rc::clone(inbox);
            RoutedEventHandler::shared(move |sender: Ref<'_, IInspectable>, _| {
                inbox.push(
                    canonical(sender.ok().ok()).as_ref(),
                    EventKind::PasswordChanged,
                )
            })
        };
        let text_submitted = {
            let inbox = Rc::clone(inbox);
            TypedEventHandler::shared(move |sender: Ref<'_, ComboBox>, _| {
                inbox.push(
                    canonical(sender.ok().ok()).as_ref(),
                    EventKind::TextSubmitted,
                )
            })
        };
        let drop_down_closed = {
            let inbox = Rc::clone(inbox);
            EventHandler::<IInspectable>::shared(move |sender: Ref<'_, IInspectable>, _| {
                inbox.push(
                    canonical(sender.ok().ok()).as_ref(),
                    EventKind::DropDownClosed,
                )
            })
        };
        Delegates {
            click,
            close_requested,
            text_changed,
            composition_started,
            composition_ended,
            enter,
            selection_changed,
            size_changed,
            lost_focus,
            password_changed,
            text_submitted,
            drop_down_closed,
        }
    }
}

/// The COM identity of `source`: its `IUnknown` pointer, equal for every
/// interface of one object.
pub(crate) fn identity(source: &impl Interface) -> Result<usize> {
    Ok(source.cast::<IUnknown>()?.as_raw() as usize)
}

/// The [`IUnknown`] of a delegate's sender, or [`None`] when there is none.
fn canonical(sender: Option<&impl Interface>) -> Option<IUnknown> {
    sender.and_then(|sender| sender.cast::<IUnknown>().ok())
}
