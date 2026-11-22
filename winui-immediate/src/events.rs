//! WinUI events as data: shared delegates queue `(sender, kind)`, and each
//! pass resolves them to nodes.

mod delegate;

pub(crate) use delegate::Delegate;

use crate::{
    bindings::*,
    tree::{NodeId, Tree},
};
use ahash::AHashMap;
use std::{cell::RefCell, rc::Rc};
use windows_core::{IUnknown, Interface, Result};

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
    /// Keys pressed since the last pass, resolved to nodes, in order.
    keys: Vec<(NodeId, Key)>,
    /// Context menu requests since the last pass, resolved to nodes, each
    /// with the pointer position in the node's element, if any.
    contexts: Vec<(NodeId, Option<Point>)>,
    /// Swapped with the inbox at the start of each pass, to reuse capacity.
    drained: Vec<(usize, EventKind)>,
    drained_keys: Vec<(usize, Key)>,
    drained_contexts: Vec<(usize, Option<Point>)>,
}

impl Events {
    /// Creates the shared delegates. `wake` runs after every queued event and
    /// must only schedule a pass, never run one.
    pub(crate) fn new(wake: Rc<dyn Fn()>) -> Self {
        let inbox = Rc::new(Inbox {
            queued: RefCell::new(Vec::new()),
            keys: RefCell::new(Vec::new()),
            contexts: RefCell::new(Vec::new()),
            wake,
        });
        tracing::debug!(count = EventKind::ALL.len() + 1, "shared delegates");
        Events {
            delegates: Delegates::new(&inbox),
            senders: AHashMap::new(),
            inbox,
            current: Vec::new(),
            keys: Vec::new(),
            contexts: Vec::new(),
            drained: Vec::new(),
            drained_keys: Vec::new(),
            drained_contexts: Vec::new(),
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
        self.keys.clear();
        std::mem::swap(&mut self.drained_keys, &mut *self.inbox.keys.borrow_mut());
        for (sender, key) in self.drained_keys.drain(..) {
            if let Some(node) = self.senders.get(&sender).copied() {
                tree.mark_event(node);
                self.keys.push((node, key));
            }
        }
        self.contexts.clear();
        std::mem::swap(
            &mut self.drained_contexts,
            &mut *self.inbox.contexts.borrow_mut(),
        );
        for (sender, position) in self.drained_contexts.drain(..) {
            if let Some(node) = self.senders.get(&sender).copied() {
                tree.mark_event(node);
                self.contexts.push((node, position));
            }
        }
        !self.current.is_empty() || !self.keys.is_empty() || !self.contexts.is_empty()
    }

    /// The last context menu request `node` got since the last pass: the
    /// pointer position in its element, or [`None`] inside for a request
    /// from the keyboard.
    pub(crate) fn context(&self, node: NodeId) -> Option<Option<Point>> {
        self.contexts
            .iter()
            .rev()
            .find(|(target, _)| *target == node)
            .map(|(_, position)| *position)
    }

    /// The keys `node` got since the last pass, in the order pressed.
    pub(crate) fn keys(&self, node: NodeId) -> impl Iterator<Item = Key> + '_ {
        self.keys
            .iter()
            .filter(move |(target, _)| *target == node)
            .map(|(_, key)| *key)
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
        let delegate = self.delegates.get(kind);
        match kind {
            // A split button is no ButtonBase, and its click has its own
            // delegate type.
            EventKind::Click => match (source.cast::<IButtonBase>(), source.cast::<ISplitButton>())
            {
                (Ok(button), _) => delegate.register(&button, |table| table.Click)?,
                (_, Ok(button)) => delegate.register(&button, |table| table.Click)?,
                _ => delegate.register(&source.cast::<IMenuFlyoutItem>()?, |table| table.Click)?,
            },
            EventKind::ContextRequested => delegate
                .register(&source.cast::<IUIElement>()?, |table| {
                    table.ContextRequested
                })?,
            EventKind::Invoked => delegate
                .register(&source.cast::<IKeyboardAccelerator>()?, |table| {
                    table.Invoked
                })?,
            EventKind::DialogPrimary => delegate
                .register(&source.cast::<IContentDialog>()?, |table| {
                    table.PrimaryButtonClick
                })?,
            EventKind::DialogClosed => {
                delegate.register(&source.cast::<IContentDialog>()?, |table| table.Closed)?
            }
            EventKind::FlyoutClosed => {
                delegate.register(&source.cast::<IFlyoutBase>()?, |table| table.Closed)?
            }
            EventKind::CloseRequested => {
                delegate.register(&source.cast::<IAppWindow>()?, |table| table.Closing)?
            }
            EventKind::TextChanged => {
                delegate.register(&source.cast::<ITextBox>()?, |table| table.TextChanged)?
            }
            EventKind::CompositionStarted => delegate
                .register(&source.cast::<ITextBox>()?, |table| {
                    table.TextCompositionStarted
                })?,
            EventKind::CompositionEnded => delegate
                .register(&source.cast::<ITextBox>()?, |table| {
                    table.TextCompositionEnded
                })?,
            EventKind::Enter => {
                delegate.register(&source.cast::<IUIElement>()?, |table| table.KeyDown)?
            }
            EventKind::DropDownClosed => {
                delegate.register(&source.cast::<IComboBox>()?, |table| table.DropDownClosed)?
            }
            EventKind::TextSubmitted => {
                delegate.register(&source.cast::<IComboBox>()?, |table| table.TextSubmitted)?
            }
            EventKind::LostFocus => {
                delegate.register(&source.cast::<IUIElement>()?, |table| table.LostFocus)?
            }
            EventKind::PasswordChanged => delegate
                .register(&source.cast::<IPasswordBox>()?, |table| {
                    table.PasswordChanged
                })?,
            EventKind::SizeChanged => delegate
                .register(&source.cast::<IFrameworkElement>()?, |table| {
                    table.SizeChanged
                })?,
            EventKind::SelectionChanged => {
                delegate.register(&source.cast::<ISelector>()?, |table| table.SelectionChanged)?
            }
            EventKind::Key => {
                delegate.register(&source.cast::<IUIElement>()?, |table| table.PreviewKeyDown)?
            }
            EventKind::BackRequested => {
                delegate.register(&source.cast::<ITitleBar>()?, |table| table.BackRequested)?
            }
            EventKind::NavigationSelectionChanged => delegate
                .register(&source.cast::<INavigationView>()?, |table| {
                    table.SelectionChanged
                })?,
        };
        let identity = identity(source)?;
        tracing::trace!(?kind, source = identity, "subscribe");
        self.senders.insert(identity, node);
        subscribed.insert(kind);
        Ok(())
    }

    /// Makes `text_box` select all its text whenever it takes focus, so the
    /// next key or paste replaces it. The handler runs at once, before any
    /// key that follows, and ends with the control.
    pub(crate) fn select_all_on_focus(&self, text_box: &TextBox) -> Result<()> {
        self.delegates
            .select_all
            .register(&text_box.cast::<IUIElement>()?, |table| table.GotFocus)?;
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
    /// A title bar's back button was pressed.
    BackRequested,
    /// A [`Key`] went down, seen before the control acts on it. The key
    /// itself is kept apart, in [`Events::keys`].
    Key,
    /// A navigation view's selected item changed, by the user or by a write.
    NavigationSelectionChanged,
    /// The user asked for a context menu, by right-click, the Menu key or
    /// Shift+F10. The position is kept apart, in [`Events::contexts`].
    ContextRequested,
    /// A flyout closed, by a pick, a click outside it, or Escape.
    FlyoutClosed,
    /// A keyboard accelerator's key was pressed.
    Invoked,
    /// A dialog's primary button was clicked; the dialog then closes.
    DialogPrimary,
    /// A dialog closed, by any button or Escape.
    DialogClosed,
}

impl EventKind {
    /// Every kind, in discriminant order, so `ALL[kind as usize] == kind`.
    pub(crate) const ALL: [EventKind; 20] = {
        use EventKind::*;
        let all = [
            Click,
            CloseRequested,
            TextChanged,
            CompositionStarted,
            CompositionEnded,
            Enter,
            SelectionChanged,
            SizeChanged,
            LostFocus,
            PasswordChanged,
            TextSubmitted,
            DropDownClosed,
            BackRequested,
            Key,
            NavigationSelectionChanged,
            ContextRequested,
            FlyoutClosed,
            Invoked,
            DialogPrimary,
            DialogClosed,
        ];
        let mut index = 0;
        while index < all.len() {
            assert!(
                all[index] as usize == index,
                "kinds out of discriminant order"
            );
            index += 1;
        }
        all
    };
}

/// A key a control reports through [`TextBoxResponse::keys`]. Enter is not
/// one: it has [`TextBoxResponse::enter_pressed`], which also stops it.
///
/// [`TextBoxResponse::keys`]: crate::TextBoxResponse::keys
/// [`TextBoxResponse::enter_pressed`]: crate::TextBoxResponse::enter_pressed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Escape,
}

impl Key {
    fn from_virtual(key: VirtualKey) -> Option<Key> {
        Some(match key {
            VirtualKey::Back => Key::Backspace,
            VirtualKey::Delete => Key::Delete,
            VirtualKey::Left => Key::Left,
            VirtualKey::Right => Key::Right,
            VirtualKey::Up => Key::Up,
            VirtualKey::Down => Key::Down,
            VirtualKey::Home => Key::Home,
            VirtualKey::End => Key::End,
            VirtualKey::Escape => Key::Escape,
            _ => return None,
        })
    }
}

/// Event kinds a node is subscribed to, one bit per [`EventKind`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventSet(u32);

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
    keys: RefCell<Vec<(usize, Key)>>,
    contexts: RefCell<Vec<(usize, Option<Point>)>>,
    wake: Rc<dyn Fn()>,
}

impl Inbox {
    fn push_key(&self, sender: &IUnknown, key: Key) {
        self.keys.borrow_mut().push((sender.as_raw() as usize, key));
        (self.wake)();
    }

    fn push_context(&self, sender: &IUnknown, position: Option<Point>) {
        self.contexts
            .borrow_mut()
            .push((sender.as_raw() as usize, position));
        (self.wake)();
    }

    fn push(&self, sender: &IUnknown, kind: EventKind) {
        self.queued
            .borrow_mut()
            .push((sender.as_raw() as usize, kind));
        (self.wake)();
    }
}

/// One delegate object per event kind, registered on every source.
struct Delegates {
    /// Indexed by [`EventKind`].
    events: [Delegate; EventKind::ALL.len()],
    /// Not an event the app sees: it selects the sender's text.
    select_all: Delegate,
}

impl Delegates {
    fn new(inbox: &Rc<Inbox>) -> Self {
        Delegates {
            events: std::array::from_fn(|index| Delegate::event(EventKind::ALL[index], inbox)),
            select_all: Delegate::select_all(),
        }
    }

    fn get(&self, kind: EventKind) -> &Delegate {
        &self.events[kind as usize]
    }
}

/// The COM identity of `source`: its `IUnknown` pointer, equal for every
/// interface of one object.
pub(crate) fn identity(source: &impl Interface) -> Result<usize> {
    Ok(source.cast::<IUnknown>()?.as_raw() as usize)
}
