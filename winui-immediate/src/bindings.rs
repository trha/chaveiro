//! WinUI bindings: the generated file plus hand-written helpers that need its
//! private items.
//!
//! `build.rs` generates the bindings into `OUT_DIR` from the members
//! `bindings.txt` names.
//!
//! The helpers here exist because the generated event methods build a new
//! delegate per call and capture a closure. This crate instead builds one
//! delegate per event kind (such as [`RoutedEventHandler::shared`]) and
//! registers that same object on every control through the raw vtable slot
//! (such as [`IButtonBase::add_click`]). Both need items the generator keeps
//! private to this module, such as [`RoutedEventHandlerBox`].

#![allow(nonstandard_style, dead_code, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use windows_core::{
    IInspectable, Interface, Ref,
    imp::{DelegateBox, box_new},
};

macro_rules! shared_delegate {
    ($delegate:ty, $box:ident, $($argument:ty),+) => {
        impl $delegate {
            /// Builds one delegate object that many event sources can share.
            /// The handler tells sources apart by its sender argument.
            pub(crate) fn shared<F: 'static + Fn($(Ref<'_, $argument>),+)>(invoke: F) -> Self {
                let delegate = DelegateBox::<$delegate, F>::new(&$box::<F>::VTABLE, invoke);
                // SAFETY: the same conversion the generated event methods do.
                unsafe { core::mem::transmute(box_new(delegate)) }
            }
        }
    };
}

shared_delegate!(
    RoutedEventHandler,
    RoutedEventHandlerBox,
    IInspectable,
    RoutedEventArgs
);
shared_delegate!(
    TextChangedEventHandler,
    TextChangedEventHandlerBox,
    IInspectable,
    TextChangedEventArgs
);
shared_delegate!(
    KeyEventHandler,
    KeyEventHandlerBox,
    IInspectable,
    KeyRoutedEventArgs
);
shared_delegate!(
    SizeChangedEventHandler,
    SizeChangedEventHandlerBox,
    IInspectable,
    SizeChangedEventArgs
);
shared_delegate!(
    SelectionChangedEventHandler,
    SelectionChangedEventHandlerBox,
    IInspectable,
    SelectionChangedEventArgs
);

impl EventHandler<IInspectable> {
    /// Builds one delegate object that many event sources can share.
    pub(crate) fn shared<F>(invoke: F) -> Self
    where
        F: 'static + Fn(Ref<'_, IInspectable>, Ref<'_, IInspectable>),
    {
        let delegate =
            DelegateBox::<Self, F>::new(&EventHandlerBox::<IInspectable, F>::VTABLE, invoke);
        // SAFETY: the same conversion the generated event methods do.
        unsafe { core::mem::transmute(box_new(delegate)) }
    }
}

impl EventHandler<ScrollViewerViewChangedEventArgs> {
    /// Builds one delegate object that many scroll viewers can share.
    pub(crate) fn shared<F>(invoke: F) -> Self
    where
        F: 'static + Fn(Ref<'_, IInspectable>, Ref<'_, ScrollViewerViewChangedEventArgs>),
    {
        let delegate = DelegateBox::<Self, F>::new(
            &EventHandlerBox::<ScrollViewerViewChangedEventArgs, F>::VTABLE,
            invoke,
        );
        // SAFETY: the same conversion the generated event methods do.
        unsafe { core::mem::transmute(box_new(delegate)) }
    }
}

impl<Sender, Args> TypedEventHandler<Sender, Args>
where
    Sender: 'static + windows_core::RuntimeType,
    Args: 'static + windows_core::RuntimeType,
{
    /// Builds one delegate object that many event sources can share.
    pub(crate) fn shared<F: 'static + Fn(Ref<'_, Sender>, Ref<'_, Args>)>(invoke: F) -> Self {
        let delegate =
            DelegateBox::<Self, F>::new(&TypedEventHandlerBox::<Sender, Args, F>::VTABLE, invoke);
        // SAFETY: the same conversion the generated event methods do.
        unsafe { core::mem::transmute(box_new(delegate)) }
    }
}

macro_rules! add_handler {
    ($interface:ty, $method:ident, $slot:ident, $delegate:ty) => {
        impl $interface {
            /// Registers `handler` without wrapping it. Returns the token; the
            /// registration ends when the source is released.
            pub(crate) fn $method(&self, handler: &$delegate) -> windows_core::Result<i64> {
                let mut token = 0;
                // SAFETY: both pointers are live COM objects of the slot's types.
                unsafe {
                    (Interface::vtable(self).$slot)(
                        Interface::as_raw(self),
                        Interface::as_raw(handler),
                        &mut token,
                    )
                    .map(|| token)
                }
            }
        }
    };
}

add_handler!(IButtonBase, add_click, Click, RoutedEventHandler);
add_handler!(
    ITextBox,
    add_text_changed,
    TextChanged,
    TextChangedEventHandler
);
add_handler!(ITextBox, add_text_composition_started, TextCompositionStarted, TypedEventHandler<TextBox, TextCompositionStartedEventArgs>);
add_handler!(ITextBox, add_text_composition_ended, TextCompositionEnded, TypedEventHandler<TextBox, TextCompositionEndedEventArgs>);
add_handler!(
    IPasswordBox,
    add_password_changed,
    PasswordChanged,
    RoutedEventHandler
);
add_handler!(
    IScrollViewer,
    add_view_changed,
    ViewChanged,
    EventHandler<ScrollViewerViewChangedEventArgs>
);
add_handler!(IUIElement, add_key_down, KeyDown, KeyEventHandler);
add_handler!(
    IUIElement,
    add_preview_key_down,
    PreviewKeyDown,
    KeyEventHandler
);
add_handler!(IUIElement, add_lost_focus, LostFocus, RoutedEventHandler);
add_handler!(IUIElement, add_got_focus, GotFocus, RoutedEventHandler);
add_handler!(
    IFrameworkElement,
    add_size_changed,
    SizeChanged,
    SizeChangedEventHandler
);
add_handler!(
    ISelector,
    add_selection_changed,
    SelectionChanged,
    SelectionChangedEventHandler
);
add_handler!(
    IComboBox,
    add_drop_down_closed,
    DropDownClosed,
    EventHandler<IInspectable>
);
add_handler!(IComboBox, add_text_submitted, TextSubmitted, TypedEventHandler<ComboBox, ComboBoxTextSubmittedEventArgs>);
add_handler!(IAppWindow, add_closing, Closing, TypedEventHandler<AppWindow, AppWindowClosingEventArgs>);
add_handler!(ITitleBar, add_back_requested, BackRequested, TypedEventHandler<TitleBar, windows_core::IInspectable>);
add_handler!(IKeyboardAccelerator, add_invoked, Invoked, TypedEventHandler<KeyboardAccelerator, KeyboardAcceleratorInvokedEventArgs>);
add_handler!(INavigationView, add_selection_changed, SelectionChanged, TypedEventHandler<NavigationView, NavigationViewSelectionChangedEventArgs>);

impl IPasswordBox {
    /// The password as the `HSTRING` WinUI returns, unconverted, so the
    /// caller can wipe its buffer.
    pub(crate) fn password_string(&self) -> windows_core::Result<windows_core::HSTRING> {
        // SAFETY: the slot's signature is `HRESULT(this, *mut HSTRING)`; on
        // success the out value is an owned HSTRING.
        unsafe {
            let mut result = core::mem::zeroed();
            (Interface::vtable(self).Password)(Interface::as_raw(self), &mut result)
                .map(|| core::mem::transmute::<_, windows_core::HSTRING>(result))
        }
    }
}
