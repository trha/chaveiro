//! Spike: can a debug build replace an AppKit setter with a version that
//! counts the call and then runs the original, without breaking AppKit?
//!
//! Replaces `-[NSTextField setStringValue:]` and `-[NSWindow setTitle:]`,
//! sends both, and checks that the values took effect and each count rose
//! by one. Also checks that `-[NSButton setTitle:]`, a different method with
//! the same selector, still works and is not counted.
//!
//! Run: `cargo run -p appkit-immediate --example write_hook`.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use objc2::{
    ClassType, MainThreadMarker, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSButton, NSTextField, NSWindow};
use objc2_foundation::NSString;
use std::{cell::Cell, sync::OnceLock};

thread_local! {
    static COUNT: Cell<u32> = const { Cell::new(0) };
}

/// The original implementation of each hooked (class, selector).
static ORIGINALS: OnceLock<Vec<(&'static AnyClass, Sel, Imp)>> = OnceLock::new();

/// Counts the call, then forwards to the original of the most derived
/// hooked class that `receiver` is a kind of.
unsafe extern "C-unwind" fn object_setter(
    receiver: &AnyObject,
    selector: Sel,
    value: *mut AnyObject,
) {
    COUNT.with(|count| count.set(count.get() + 1));
    let originals = ORIGINALS.get().expect("set before any hook is installed");
    let mut class = Some(receiver.class());
    let imp = loop {
        let current = class.expect("a hooked receiver is a kind of a hooked class");
        if let Some((_, _, imp)) = originals
            .iter()
            .find(|(hooked, hooked_selector, _)| *hooked == current && *hooked_selector == selector)
        {
            break *imp;
        }
        class = current.superclass();
    };
    let original: unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) =
        unsafe { std::mem::transmute(imp) };
    unsafe { original(receiver, selector, value) }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("main runs on the main thread");
    let hooks = [
        (NSTextField::class(), sel!(setStringValue:)),
        (NSWindow::class(), sel!(setTitle:)),
    ];
    let trampoline: Imp = unsafe {
        std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject), Imp>(
            object_setter,
        )
    };
    // Each hook takes the class's own method, adding one when the method is
    // inherited, so a sibling class with the same selector is untouched.
    let mut originals = Vec::new();
    for (class, selector) in hooks {
        let method = class
            .instance_method(selector)
            .expect("the class answers the selector");
        originals.push((class, selector, method.implementation()));
    }
    ORIGINALS.set(originals).expect("set once");
    for (class, selector) in hooks {
        let method = class.instance_method(selector).expect("checked above");
        let encoding = unsafe { objc2::ffi::method_getTypeEncoding(method) };
        let added = unsafe {
            objc2::ffi::class_addMethod(
                std::ptr::from_ref(class).cast_mut(),
                selector,
                trampoline,
                encoding,
            )
        };
        if !added.as_bool() {
            // The class defines the method itself.
            unsafe { method.set_implementation(trampoline) };
        }
    }

    let field = NSTextField::labelWithString(&NSString::new(), mtm);
    COUNT.set(0);
    field.setStringValue(&NSString::from_str("hello"));
    assert_eq!(field.stringValue().to_string(), "hello", "the original ran");
    assert_eq!(COUNT.get(), 1, "the field write was counted once");

    let window = unsafe { NSWindow::new(mtm) };
    unsafe { window.setReleasedWhenClosed(false) };
    COUNT.set(1);
    window.setTitle(&NSString::from_str("title"));
    assert_eq!(window.title().to_string(), "title", "the original ran");
    assert_eq!(COUNT.get(), 2, "the window write was counted");

    let button: Retained<NSButton> = unsafe { msg_send![NSButton::class(), new] };
    COUNT.set(2);
    button.setTitle(&NSString::from_str("press"));
    assert_eq!(
        button.title().to_string(),
        "press",
        "the sibling still works"
    );
    assert_eq!(COUNT.get(), 2, "the sibling was not counted");

    // A subclass of a hooked class reaches the hook through inheritance.
    let secure = objc2_app_kit::NSSecureTextField::new(mtm);
    COUNT.set(2);
    secure.setStringValue(&NSString::from_str("secret"));
    assert_eq!(secure.stringValue().to_string(), "secret");
    println!("subclass counted: {}", COUNT.get() == 3);
    println!("ok, count {}", COUNT.get());
}
