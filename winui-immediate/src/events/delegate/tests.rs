//! The COM contract of [`Delegate`], driven through raw vtable calls as an
//! event source would make them.

use super::*;
use std::cell::{Cell, RefCell};

/// The `IUnknown` slots of any COM object.
fn unknown_vtable(object: *mut c_void) -> &'static windows_core::IUnknown_Vtbl {
    // SAFETY: every COM object starts with a pointer to a vtable that starts
    // with the `IUnknown` slots.
    unsafe { &**(object as *const *const windows_core::IUnknown_Vtbl) }
}

/// Queries `object` for `iid`. Returns the HRESULT and the pointer.
fn query(object: *mut c_void, iid: GUID) -> (HRESULT, *mut c_void) {
    let mut result = core::ptr::null_mut();
    // SAFETY: `object` is live and the out pointer is valid.
    let status = unsafe { (unknown_vtable(object).QueryInterface)(object, &iid, &mut result) };
    (status, result)
}

fn release(object: *mut c_void) -> u32 {
    // SAFETY: the caller owns one reference to `object`.
    unsafe { (unknown_vtable(object).Release)(object) }
}

fn invoke(object: *mut c_void, sender: *mut c_void) -> HRESULT {
    // SAFETY: `object` is a delegate; a plain kind reads no arguments.
    unsafe {
        let vtable = &**(object as *const *const Vtable);
        (vtable.invoke)(object, sender, core::ptr::null_mut())
    }
}

/// An inbox that counts its wakes.
fn inbox() -> (Rc<Inbox>, Rc<Cell<u32>>) {
    let wakes = Rc::new(Cell::new(0));
    let inbox = Rc::new(Inbox {
        queued: RefCell::new(Vec::new()),
        keys: RefCell::new(Vec::new()),
        contexts: RefCell::new(Vec::new()),
        wake: Rc::new({
            let wakes = Rc::clone(&wakes);
            move || wakes.set(wakes.get() + 1)
        }),
    });
    (inbox, wakes)
}

#[test]
fn a_click_answers_both_click_iids_iunknown_and_iagileobject_with_itself() {
    let (inbox, _) = inbox();
    let delegate = Delegate::event(EventKind::Click, &inbox);
    let object = delegate.0.as_ptr();
    for iid in [
        RoutedEventHandler::IID,
        TypedEventHandler::<SplitButton, SplitButtonClickEventArgs>::IID,
        IUnknown::IID,
        windows_core::imp::IAgileObject::IID,
    ] {
        let (status, result) = query(object, iid);
        assert_eq!(status, HRESULT(0));
        assert_eq!(result, object);
        release(result);
    }
}

#[test]
fn refuses_another_delegates_iid() {
    let (inbox, _) = inbox();
    let delegate = Delegate::event(EventKind::Click, &inbox);
    let (status, result) = query(delegate.0.as_ptr(), KeyEventHandler::IID);
    assert_eq!(status, HRESULT(0x8000_4002_u32 as i32));
    assert!(result.is_null());
}

#[test]
fn invoke_queues_the_senders_identity_and_wakes_once_per_call() {
    let (inbox, wakes) = inbox();
    let delegate = Delegate::event(EventKind::LostFocus, &inbox);
    // Any COM object serves as a sender; this one is another delegate.
    let sender = Delegate::select_all();
    for _ in 0..3 {
        assert_eq!(invoke(delegate.0.as_ptr(), sender.0.as_ptr()), HRESULT(0));
    }
    let identity = sender.0.as_ptr() as usize;
    assert_eq!(
        *inbox.queued.borrow(),
        vec![(identity, EventKind::LostFocus); 3]
    );
    assert_eq!(wakes.get(), 3);
}

#[test]
fn invoke_without_a_sender_queues_nothing_and_does_not_wake() {
    let (inbox, wakes) = inbox();
    let delegate = Delegate::event(EventKind::Click, &inbox);
    assert_eq!(
        invoke(delegate.0.as_ptr(), core::ptr::null_mut()),
        HRESULT(0)
    );
    assert!(inbox.queued.borrow().is_empty());
    assert_eq!(wakes.get(), 0);
}

#[test]
fn the_last_release_frees_the_object_and_not_before() {
    let (inbox, _) = inbox();
    let delegate = Delegate::event(EventKind::Click, &inbox);
    let (_, second) = query(delegate.0.as_ptr(), IUnknown::IID);
    drop(delegate);
    assert_eq!(Rc::strong_count(&inbox), 2, "the second reference keeps it");
    assert_eq!(release(second), 0);
    assert_eq!(Rc::strong_count(&inbox), 1);
}

#[test]
fn imarshal_comes_from_the_free_threaded_marshaler_and_keeps_the_delegate_alive() {
    let (inbox, _) = inbox();
    let delegate = Delegate::event(EventKind::Click, &inbox);
    let object = delegate.0.as_ptr();
    let (status, marshal) = query(object, IID_IMARSHAL);
    assert_eq!(status, HRESULT(0));
    assert!(!marshal.is_null());
    assert_ne!(marshal, object, "IMarshal is the aggregated marshaler's");
    // Aggregation: the marshaler's IUnknown is the delegate's.
    let (status, identity) = query(marshal, IUnknown::IID);
    assert_eq!(status, HRESULT(0));
    assert_eq!(identity, object);
    release(identity);
    drop(delegate);
    assert_eq!(Rc::strong_count(&inbox), 2, "IMarshal holds the delegate");
    release(marshal);
    assert_eq!(Rc::strong_count(&inbox), 1);
}
