//! The one COM type behind every event handler this crate registers.
//!
//! A WinRT event delegate (`RoutedEventHandler`, `TypedEventHandler<S, A>`,
//! ...) is `IUnknown` plus `Invoke(this, sender, args) -> HRESULT`, with
//! object pointers for both arguments, so every delegate shares one ABI and
//! differs only in its IID. [`Delegate`] has one vtable, answers the IIDs its
//! [`Action`] is registered under, and does the action itself in `Invoke`,
//! so there is no closure per handler. Like windows-core's own delegates, it
//! is agile and marshals free-threaded.

#[cfg(test)]
mod tests;

use super::{EventKind, Inbox, Key};
use crate::bindings::*;
use core::{ffi::c_void, ptr::NonNull};
use std::{rc::Rc, sync::OnceLock};
use windows_core::{GUID, HRESULT, IInspectable, IUnknown, Interface, Result, Weak, imp::RefCount};

/// An owned reference to a delegate object, released on drop.
pub(crate) struct Delegate(NonNull<c_void>);

impl Delegate {
    /// The handler for `kind`, which queues `(sender, kind)` in `inbox`.
    /// Register it only on events whose delegate IID [`iids`] lists for
    /// `kind`.
    pub(super) fn event(kind: EventKind, inbox: &Rc<Inbox>) -> Delegate {
        Delegate::new(Action::Event(kind, Rc::clone(inbox)))
    }

    /// A `GotFocus` handler that selects all the text of its sender, a text
    /// box.
    pub(super) fn select_all() -> Delegate {
        Delegate::new(Action::SelectAll)
    }

    /// A keyboard accelerator's `Invoked` handler that focuses `target`,
    /// selects its text, and marks the key handled. It holds `target`
    /// weakly, so it does nothing once the box is gone.
    pub(crate) fn focus(target: Weak<TextBox>) -> Delegate {
        Delegate::new(Action::Focus(target))
    }

    /// A `TextChanging` handler that runs `hook` on its sender, a text box,
    /// at once, before the box draws the new text.
    pub(crate) fn text_changing(hook: TextHook) -> Delegate {
        Delegate::new(Action::TextChanging(hook))
    }

    /// A `PreviewKeyDown` handler that runs `hook` at once, before the
    /// sender acts on the key.
    pub(crate) fn preview_key_down(hook: KeyHook) -> Delegate {
        Delegate::new(Action::PreviewKeyDown(hook))
    }

    /// An `ActualThemeChanged` handler that sets its sender's theme brushes
    /// again; see [`crate::theme`].
    pub(crate) fn theme_changed() -> Delegate {
        Delegate::new(Action::ThemeChanged)
    }

    /// A `RoutedEventHandler` that runs `run`, as an app with full
    /// bindings makes one from a closure.
    #[cfg(test)]
    pub(crate) fn routed(run: Box<dyn Fn()>) -> Delegate {
        Delegate::new(Action::Routed(run))
    }

    fn new(action: Action) -> Delegate {
        let object = Box::new(Object {
            vtable: &VTABLE,
            iids: iids(&action),
            count: RefCount::new(1),
            action,
            marshaler: OnceLock::new(),
        });
        Delegate(NonNull::from(Box::leak(object)).cast())
    }

    /// Registers this object on `source` through the raw `add_*` vtable slot
    /// that `slot` picks, such as `|table| table.Click`, which the source
    /// keeps its own reference through. Returns the token; the registration
    /// ends when the source is released. The slot's delegate IID must be one
    /// this object answers.
    pub(crate) fn register<I: Interface>(
        &self,
        source: &I,
        slot: impl FnOnce(&I::Vtable) -> AddSlot,
    ) -> Result<i64> {
        let mut token = 0;
        // SAFETY: both pointers are live COM objects, and an `add_*` slot
        // takes the source, the handler, and the token's address.
        unsafe { slot(source.vtable())(source.as_raw(), self.as_raw(), &mut token).map(|| token) }
    }

    fn as_raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }
}

impl Drop for Delegate {
    fn drop(&mut self) {
        // SAFETY: this handle owns one reference.
        unsafe { Object::release(self.0.as_ptr()) };
    }
}

/// The ABI of every generated `add_<Event>` vtable slot.
pub(crate) type AddSlot = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> HRESULT;

/// What `Invoke` does.
enum Action {
    /// Queues the event, with any side effect its kind needs on the
    /// arguments.
    Event(EventKind, Rc<Inbox>),
    SelectAll,
    Focus(Weak<TextBox>),
    /// Runs on a text box's `TextChanging`, before the box draws the text.
    TextChanging(TextHook),
    /// Runs on an element's `PreviewKeyDown`, before the element acts on
    /// the key.
    PreviewKeyDown(KeyHook),
    /// Runs on an element's `ActualThemeChanged`.
    ThemeChanged,
    #[cfg(test)]
    Routed(Box<dyn Fn()>),
}

/// What a [`Delegate::text_changing`] does to its sender.
pub(crate) type TextHook = Box<dyn Fn(&TextBox) -> Result<()>>;
/// What a [`Delegate::preview_key_down`] does with its sender and key.
pub(crate) type KeyHook = Box<dyn Fn(&IInspectable, &KeyRoutedEventArgs) -> Result<()>>;

/// The delegate IIDs an action answers: those of the events it is
/// registered on.
fn iids(action: &Action) -> &'static [GUID] {
    type Typed<S, A> = TypedEventHandler<S, A>;
    match action {
        Action::Event(kind, _) => match kind {
            // A split button's click has its own delegate type.
            EventKind::Click => {
                const {
                    &[
                        RoutedEventHandler::IID,
                        Typed::<SplitButton, SplitButtonClickEventArgs>::IID,
                    ]
                }
            }
            EventKind::LostFocus | EventKind::PasswordChanged => {
                const { &[RoutedEventHandler::IID] }
            }
            EventKind::CloseRequested => {
                const { &[Typed::<AppWindow, AppWindowClosingEventArgs>::IID] }
            }
            EventKind::TextChanged => const { &[TextChangedEventHandler::IID] },
            EventKind::CompositionStarted => {
                const { &[Typed::<TextBox, TextCompositionStartedEventArgs>::IID] }
            }
            EventKind::CompositionEnded => {
                const { &[Typed::<TextBox, TextCompositionEndedEventArgs>::IID] }
            }
            EventKind::Enter | EventKind::Key => const { &[KeyEventHandler::IID] },
            EventKind::SelectionChanged => const { &[SelectionChangedEventHandler::IID] },
            EventKind::SizeChanged => const { &[SizeChangedEventHandler::IID] },
            EventKind::ThemeChanged => const { &[Typed::<FrameworkElement, IInspectable>::IID] },
            EventKind::TextSubmitted => {
                const { &[Typed::<ComboBox, ComboBoxTextSubmittedEventArgs>::IID] }
            }
            EventKind::DropDownClosed | EventKind::FlyoutClosed => {
                const { &[EventHandler::<IInspectable>::IID] }
            }
            EventKind::BackRequested => const { &[Typed::<TitleBar, IInspectable>::IID] },
            EventKind::NavigationSelectionChanged => {
                const { &[Typed::<NavigationView, NavigationViewSelectionChangedEventArgs>::IID] }
            }
            EventKind::ContextRequested => {
                const { &[Typed::<UIElement, ContextRequestedEventArgs>::IID] }
            }
            EventKind::Invoked => {
                const { &[Typed::<KeyboardAccelerator, KeyboardAcceleratorInvokedEventArgs>::IID] }
            }
            EventKind::DialogPrimary | EventKind::DialogSecondary => {
                const { &[Typed::<ContentDialog, ContentDialogButtonClickEventArgs>::IID] }
            }
            EventKind::DialogClosed => {
                const { &[Typed::<ContentDialog, ContentDialogClosedEventArgs>::IID] }
            }
        },
        Action::SelectAll => const { &[RoutedEventHandler::IID] },
        #[cfg(test)]
        Action::Routed(_) => const { &[RoutedEventHandler::IID] },
        Action::TextChanging(_) => const { &[Typed::<TextBox, TextBoxTextChangingEventArgs>::IID] },
        Action::PreviewKeyDown(_) => const { &[KeyEventHandler::IID] },
        Action::ThemeChanged => const { &[Typed::<FrameworkElement, IInspectable>::IID] },
        Action::Focus(_) => {
            const { &[Typed::<KeyboardAccelerator, KeyboardAcceleratorInvokedEventArgs>::IID] }
        }
    }
}

#[repr(C)]
struct Object {
    vtable: &'static Vtable,
    iids: &'static [GUID],
    count: RefCount,
    action: Action,
    /// The free-threaded marshaler aggregated into this object, made on the
    /// first `IMarshal` query: its non-delegating `IUnknown`.
    marshaler: OnceLock<InnerUnknown>,
}

/// An inner `IUnknown` an aggregate owns.
struct InnerUnknown(*mut c_void);

// SAFETY: the free-threaded marshaler may be used from any thread.
unsafe impl Send for InnerUnknown {}
// SAFETY: as for `Send`.
unsafe impl Sync for InnerUnknown {}

impl Drop for InnerUnknown {
    fn drop(&mut self) {
        // SAFETY: the pointer is the reference `CoCreateFreeThreadedMarshaler`
        // returned, released once here.
        unsafe { drop(IUnknown::from_raw(self.0)) };
    }
}

windows_core::link!("ole32.dll" "system" fn CoCreateFreeThreadedMarshaler(outer: *mut c_void, marshaler: *mut *mut c_void) -> HRESULT);

const IID_IMARSHAL: GUID = GUID::from_u128(0x00000003_0000_0000_c000_000000000046);

#[repr(C)]
struct Vtable {
    base: windows_core::IUnknown_Vtbl,
    invoke: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
}

static VTABLE: Vtable = Vtable {
    base: windows_core::IUnknown_Vtbl {
        QueryInterface: Object::query_interface,
        AddRef: Object::add_ref,
        Release: Object::release,
    },
    invoke: Object::invoke,
};

impl Object {
    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> HRESULT {
        const E_POINTER: HRESULT = HRESULT(0x8000_4003_u32 as i32);
        const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002_u32 as i32);
        unsafe {
            if iid.is_null() || interface.is_null() {
                return E_POINTER;
            }
            *interface = core::ptr::null_mut();
            let object = &*(this as *const Object);
            if object.iids.contains(&*iid)
                || *iid == IUnknown::IID
                || *iid == windows_core::imp::IAgileObject::IID
            {
                object.count.add_ref();
                *interface = this;
                return HRESULT(0);
            }
            if *iid != IID_IMARSHAL {
                return E_NOINTERFACE;
            }
            // Aggregated with this object as its outer, so the marshaler's
            // `IMarshal` hands `IUnknown` calls back here and keeps this
            // object alive while it lives.
            let inner = object.marshaler.get_or_init(|| {
                let mut inner = core::ptr::null_mut();
                // A failure leaves `inner` null, which the query reports.
                let _ = CoCreateFreeThreadedMarshaler(this, &mut inner);
                InnerUnknown(inner)
            });
            if inner.0.is_null() {
                return E_NOINTERFACE;
            }
            let inner_vtable = *(inner.0 as *const *const windows_core::IUnknown_Vtbl);
            ((*inner_vtable).QueryInterface)(inner.0, iid, interface)
        }
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        unsafe { (*(this as *const Object)).count.add_ref() }
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        unsafe {
            let remaining = (*(this as *const Object)).count.release();
            if remaining == 0 {
                drop(Box::from_raw(this as *mut Object));
            }
            remaining
        }
    }

    unsafe extern "system" fn invoke(
        this: *mut c_void,
        sender: *mut c_void,
        arguments: *mut c_void,
    ) -> HRESULT {
        // SAFETY: `this` is live for the call, and the source passes the
        // sender and arguments types of the delegate it registered under,
        // which `iids` ties to the action.
        unsafe {
            match &(*(this as *const Object)).action {
                Action::Event(kind, inbox) => queue(*kind, inbox, &sender, &arguments),
                // Only text boxes register it.
                Action::SelectAll => {
                    // A failed selection leaves the caret where WinUI put it,
                    // which is still usable.
                    if let Some(sender) = IInspectable::from_raw_borrowed(&sender)
                        && let Ok(text_box) = sender.cast::<TextBox>()
                    {
                        let _ = text_box.SelectAll();
                    }
                }
                // A hook that fails leaves the control as the event left
                // it; the event itself goes on.
                Action::TextChanging(hook) => {
                    if let Some(text_box) = TextBox::from_raw_borrowed(&sender) {
                        let _ = hook(text_box);
                    }
                }
                Action::PreviewKeyDown(hook) => {
                    if let (Some(sender), Some(arguments)) = (
                        IInspectable::from_raw_borrowed(&sender),
                        KeyRoutedEventArgs::from_raw_borrowed(&arguments),
                    ) {
                        let _ = hook(sender, arguments);
                    }
                }
                #[cfg(test)]
                Action::Routed(run) => run(),
                Action::ThemeChanged => {
                    // A brush that fails to update keeps the old theme's
                    // color, which is still drawn.
                    if let Some(element) = FrameworkElement::from_raw_borrowed(&sender) {
                        if let Err(error) = crate::theme::refresh(element) {
                            tracing::debug!(%error, "theme brushes not refreshed");
                        }
                    }
                }
                Action::Focus(target) => {
                    // A box that is gone, or that refuses focus, leaves focus
                    // where it was, and the key does nothing else.
                    if let Some(text_box) = target.upgrade()
                        && let Ok(element) = text_box.cast::<UIElement>()
                        && element.Focus(FocusState::Keyboard).unwrap_or(false)
                    {
                        let _ = text_box.SelectAll();
                    }
                    // Handled, so no other accelerator acts on the key; if the
                    // write fails, the key only goes on to others.
                    if let Some(arguments) =
                        KeyboardAcceleratorInvokedEventArgs::from_raw_borrowed(&arguments)
                    {
                        let _ = arguments.SetHandled(true);
                    }
                }
            }
        }
        HRESULT(0)
    }
}

/// Queues `kind` from `sender` in `inbox`. A null sender cannot be routed
/// to a node, so it queues nothing.
///
/// # Safety
///
/// `sender` and `arguments` are null or live objects of the sender and
/// arguments types of `kind`'s delegate.
unsafe fn queue(kind: EventKind, inbox: &Inbox, source: &*mut c_void, arguments: &*mut c_void) {
    // The canonical `IUnknown`, which `subscribe` mapped to the node.
    // SAFETY: the caller's contract.
    let Some(Ok(sender)) =
        (unsafe { IUnknown::from_raw_borrowed(source) }).map(|source| source.cast::<IUnknown>())
    else {
        return;
    };
    match kind {
        EventKind::CloseRequested => {
            // The app decides: it closes the window by no longer declaring
            // it. If cancelling fails, the window closes and the next pass
            // drops its node anyway.
            // SAFETY: the caller's contract.
            if let Some(arguments) =
                unsafe { AppWindowClosingEventArgs::from_raw_borrowed(arguments) }
            {
                let _ = arguments.SetCancel(true);
            }
            inbox.push(&sender, kind);
        }
        EventKind::Enter => {
            // SAFETY: the caller's contract.
            let Some(arguments) = (unsafe { KeyRoutedEventArgs::from_raw_borrowed(arguments) })
            else {
                return;
            };
            if arguments.Key().ok() != Some(VirtualKey::Enter) {
                return;
            }
            // Marked handled so a parent does not act on it too. If the
            // write fails the key only bubbles on.
            let _ = arguments.SetHandled(true);
            inbox.push(&sender, kind);
        }
        EventKind::Key => {
            // Left unhandled, so the control still acts on the key.
            // SAFETY: the caller's contract.
            let pressed = unsafe { KeyRoutedEventArgs::from_raw_borrowed(arguments) }
                .and_then(|arguments| arguments.Key().ok())
                .and_then(Key::from_virtual);
            if let Some(key) = pressed {
                inbox.push_key(&sender, key);
            }
        }
        EventKind::ContextRequested => {
            // SAFETY: the caller's contract: the sender is a `UIElement`.
            let (element, arguments) = unsafe {
                (
                    UIElement::from_raw_borrowed(source),
                    ContextRequestedEventArgs::from_raw_borrowed(arguments),
                )
            };
            let (Some(element), Some(arguments)) = (element, arguments) else {
                return;
            };
            // Handled, so a parent does not open its own menu too; if the
            // write fails, a parent may also act on it.
            let _ = arguments.SetHandled(true);
            let mut point = Point::default();
            // From the keyboard there is no position, and the menu opens at
            // its control.
            let position = arguments
                .TryGetPosition(element, &mut point)
                .unwrap_or(false)
                .then_some(point);
            inbox.push_context(&sender, position);
        }
        EventKind::Invoked => {
            // Handled, so no other accelerator acts on the key; if the write
            // fails, the key only goes on to others.
            // SAFETY: the caller's contract.
            if let Some(arguments) =
                unsafe { KeyboardAcceleratorInvokedEventArgs::from_raw_borrowed(arguments) }
            {
                let _ = arguments.SetHandled(true);
            }
            inbox.push(&sender, kind);
        }
        _ => inbox.push(&sender, kind),
    }
}
