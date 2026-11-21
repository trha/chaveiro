//! The debug check that a pass with nothing new to show writes nothing to
//! AppKit.
//!
//! Writes reach the recorder in two ways. Code in this crate that decides
//! to write, such as `must_write`, calls [`record`]. A hook on a fixed list
//! of AppKit setters also records every send, so it catches a write that
//! skipped `record`. The recorder listens only between [`begin`] and
//! [`end`], so outside a check pass a hooked setter costs one flag test. In
//! release builds the functions here do nothing and no hook goes in.

use crate::{
    pass::{Payload, Ui},
    tree::NodeId,
};
use ahash::AHashMap;
use objc2::{
    ClassType, Message,
    ffi::{class_addMethod, method_getTypeEncoding},
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::{
    NSBox, NSButton, NSComboBox, NSControl, NSGlassEffectView, NSImageView, NSLayoutConstraint,
    NSPopUpButton, NSProgressIndicator, NSScrollView, NSSecureTextField, NSStackView, NSTableView,
    NSTextField, NSView, NSVisualEffectView, NSWindow,
};
use objc2_foundation::{NSEdgeInsets, NSInteger, NSSize, NSUInteger};
use std::{
    cell::{Cell, RefCell},
    panic::Location,
    sync::{Once, OnceLock},
};

/// A write seen during a check pass.
#[derive(Debug)]
pub(crate) enum Write {
    /// This crate decided to write a property of `node` at `site`.
    Decided {
        node: NodeId,
        site: &'static Location<'static>,
    },
    /// The hook saw `selector` sent to `receiver`, kept alive so the report
    /// can find the view it belongs to.
    Sent {
        selector: Sel,
        receiver: Retained<AnyObject>,
    },
}

/// Starts recording, dropping anything left from before.
pub(crate) fn begin() {
    if cfg!(debug_assertions) {
        RECORDER.with(|recorder| {
            recorder.writes.borrow_mut().clear();
            recorder.on.set(true);
        });
    }
}

/// Stops recording and returns the writes seen since [`begin`].
pub(crate) fn end() -> Vec<Write> {
    if !cfg!(debug_assertions) {
        return Vec::new();
    }
    RECORDER.with(|recorder| {
        recorder.on.set(false);
        recorder.writes.take()
    })
}

/// Runs `body` with recording paused, for code that writes on purpose, such
/// as a [`native`](crate::Ui::native) view's update, or that lets AppKit lay
/// out and so set properties of its own.
pub(crate) fn paused<R>(body: impl FnOnce() -> R) -> R {
    if !cfg!(debug_assertions) {
        return body();
    }
    // Restores the depth when `body` panics too, so a caught panic leaves
    // the recorder usable.
    struct Resume;
    impl Drop for Resume {
        fn drop(&mut self) {
            RECORDER.with(|recorder| recorder.paused.set(recorder.paused.get() - 1));
        }
    }
    RECORDER.with(|recorder| recorder.paused.set(recorder.paused.get() + 1));
    let _resume = Resume;
    body()
}

/// Records that this crate decided to write a property of `node`, at the
/// caller's location.
#[track_caller]
pub(crate) fn record(node: NodeId) {
    if cfg!(debug_assertions) {
        push(Write::Decided {
            node,
            site: Location::caller(),
        });
    }
}

fn push(write: Write) {
    // Dropped while the thread exits, when nothing is recording anyway.
    let _ = RECORDER.try_with(|recorder| {
        if recorder.on.get() && recorder.paused.get() == 0 {
            recorder.writes.borrow_mut().push(write);
        }
    });
}

struct Recorder {
    on: Cell<bool>,
    /// How many [`paused`] calls are running.
    paused: Cell<u32>,
    writes: RefCell<Vec<Write>>,
}

thread_local! {
    static RECORDER: Recorder = const {
        Recorder {
            on: Cell::new(false),
            paused: Cell::new(0),
            writes: RefCell::new(Vec::new()),
        }
    };
}

/// Installs the setter hooks. Runs once per process; later calls and
/// release builds do nothing.
pub(crate) fn install() {
    if !cfg!(debug_assertions) {
        return;
    }
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let hooks = hooks();
        // A subclass hooked for a selector its superclass is also hooked for
        // would loop: the superclass's trampoline finds the receiver's
        // nearest hook, which is the subclass's again.
        for (class, selector, _) in &hooks {
            let mut ancestor = class.superclass();
            while let Some(current) = ancestor {
                assert!(
                    !hooks
                        .iter()
                        .any(|(other, other_selector, _)| *other == current
                            && other_selector == selector),
                    "{class} and its superclass {current} are both hooked for {selector}"
                );
                ancestor = current.superclass();
            }
        }
        // Every original is read before any hook goes in, so a hook on a
        // subclass forwards to the superclass's real method and not to its
        // hook.
        let originals = hooks
            .iter()
            .map(|(class, selector, _)| {
                let method = class
                    .instance_method(*selector)
                    .unwrap_or_else(|| panic!("{class} answers {selector}"));
                (*class, *selector, method.implementation())
            })
            .collect();
        ORIGINALS.set(originals).expect("set only here, once");
        for (class, selector, argument) in hooks {
            let method = class.instance_method(selector).expect("checked above");
            let trampoline = argument.trampoline();
            // Adding the method to the class leaves its superclass and
            // siblings alone; a class that defines the method itself gets
            // the trampoline in place.
            let added = unsafe {
                class_addMethod(
                    std::ptr::from_ref(class).cast_mut(),
                    selector,
                    trampoline,
                    method_getTypeEncoding(method),
                )
            };
            if !added.as_bool() {
                unsafe { method.set_implementation(trampoline) };
            }
        }
    });
}

/// The original method of each hooked class and selector.
static ORIGINALS: OnceLock<Vec<(&'static AnyClass, Sel, Imp)>> = OnceLock::new();

/// The setters this crate sends on the update path, each on the highest
/// class that defines it. A subclass that overrides one reaches the hook
/// when it calls `super`, as AppKit's overrides do.
fn hooks() -> Vec<(&'static AnyClass, Sel, Argument)> {
    use Argument::*;
    let mut hooks = Vec::new();
    hooks.extend([
        (NSControl::class(), sel!(setStringValue:), Object),
        (NSControl::class(), sel!(setEnabled:), Bool),
        (NSControl::class(), sel!(setFont:), Object),
        (NSTextField::class(), sel!(setPlaceholderString:), Object),
        (NSTextField::class(), sel!(setTextColor:), Object),
        (NSButton::class(), sel!(setTitle:), Object),
        (NSButton::class(), sel!(setKeyEquivalent:), Object),
        (NSButton::class(), sel!(setImage:), Object),
        (NSButton::class(), sel!(setImagePosition:), Unsigned),
        (NSButton::class(), sel!(setState:), Integer),
        (NSPopUpButton::class(), sel!(removeAllItems), Nothing),
        (NSPopUpButton::class(), sel!(addItemsWithTitles:), Object),
        (NSPopUpButton::class(), sel!(selectItemAtIndex:), Integer),
        (NSComboBox::class(), sel!(removeAllItems), Nothing),
        (NSComboBox::class(), sel!(addItemsWithObjectValues:), Object),
        (NSWindow::class(), sel!(setTitle:), Object),
        (NSWindow::class(), sel!(setContentSize:), Size),
        (NSWindow::class(), sel!(setStyleMask:), Unsigned),
        (NSView::class(), sel!(setHidden:), Bool),
        (NSView::class(), sel!(addSubview:), Object),
        (NSView::class(), sel!(removeFromSuperview), Nothing),
        (
            NSView::class(),
            sel!(setContentHuggingPriority:forOrientation:),
            PriorityOrientation,
        ),
        (NSStackView::class(), sel!(setOrientation:), Integer),
        (NSStackView::class(), sel!(setSpacing:), Float),
        (NSStackView::class(), sel!(setEdgeInsets:), EdgeInsets),
        (NSStackView::class(), sel!(addArrangedSubview:), Object),
        (
            NSStackView::class(),
            sel!(insertArrangedSubview:atIndex:),
            ObjectInteger,
        ),
        (NSStackView::class(), sel!(removeArrangedSubview:), Object),
        (NSLayoutConstraint::class(), sel!(setConstant:), Float),
        (NSLayoutConstraint::class(), sel!(setActive:), Bool),
        (NSProgressIndicator::class(), sel!(startAnimation:), Object),
        (NSProgressIndicator::class(), sel!(stopAnimation:), Object),
    ]);
    hooks
}

/// The arguments a hooked method takes after `self` and `_cmd`. Each has its
/// own trampoline, because the trampoline must pass them on with their ABI.
#[derive(Clone, Copy)]
enum Argument {
    Nothing,
    Object,
    Bool,
    Float,
    Integer,
    Unsigned,
    Size,
    EdgeInsets,
    PriorityOrientation,
    ObjectInteger,
}

macro_rules! trampoline {
    ($name:ident ($($argument:ident: $type:ty),*)) => {
        unsafe extern "C-unwind" fn $name(receiver: &AnyObject, selector: Sel $(, $argument: $type)*) {
            let imp = sent(receiver, selector);
            // SAFETY: `imp` is the original method for this selector, whose
            // arguments the hook table names.
            let original: unsafe extern "C-unwind" fn(&AnyObject, Sel $(, $type)*) =
                unsafe { std::mem::transmute(imp) };
            unsafe { original(receiver, selector $(, $argument)*) }
        }
    };
}

trampoline!(nothing());
trampoline!(object(value: *mut AnyObject));
trampoline!(boolean(value: objc2::runtime::Bool));
trampoline!(float(value: f64));
trampoline!(integer(value: NSInteger));
trampoline!(unsigned(value: NSUInteger));
trampoline!(size(value: NSSize));
trampoline!(edge_insets(value: NSEdgeInsets));
trampoline!(priority_orientation(priority: f32, orientation: NSInteger));
trampoline!(object_integer(value: *mut AnyObject, index: NSInteger));

impl Argument {
    fn trampoline(self) -> Imp {
        // SAFETY: an `Imp` is called only through a cast back to the type
        // the method was registered with.
        unsafe {
            match self {
                Argument::Nothing => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel),
                    Imp,
                >(nothing),
                Argument::Object => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject),
                    Imp,
                >(object),
                Argument::Bool => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, objc2::runtime::Bool),
                    Imp,
                >(boolean),
                Argument::Float => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, f64),
                    Imp,
                >(float),
                Argument::Integer => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, NSInteger),
                    Imp,
                >(integer),
                Argument::Unsigned => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, NSUInteger),
                    Imp,
                >(unsigned),
                Argument::Size => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, NSSize),
                    Imp,
                >(size),
                Argument::EdgeInsets => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, NSEdgeInsets),
                    Imp,
                >(edge_insets),
                Argument::PriorityOrientation => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, f32, NSInteger),
                    Imp,
                >(priority_orientation),
                Argument::ObjectInteger => std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject, NSInteger),
                    Imp,
                >(object_integer),
            }
        }
    }
}

/// Records the send and returns the original method of the most derived
/// hooked class that `receiver` belongs to.
fn sent(receiver: &AnyObject, selector: Sel) -> Imp {
    // AppKit still sends hooked setters while the thread exits, after the
    // recorder is gone; nothing is recording then.
    let _ = RECORDER.try_with(|recorder| {
        if recorder.on.get() && recorder.paused.get() == 0 && ours(receiver.class()) {
            // SAFETY: `receiver` is a live object, the receiver of this send.
            let receiver = unsafe { Retained::retain(std::ptr::from_ref(receiver).cast_mut()) }
                .expect("a receiver is not nil");
            recorder
                .writes
                .borrow_mut()
                .push(Write::Sent { selector, receiver });
        }
    });
    let originals = ORIGINALS.get().expect("set before any hook goes in");
    let mut class = Some(receiver.class());
    while let Some(current) = class {
        if let Some((_, _, imp)) = originals
            .iter()
            .find(|(hooked, hooked_selector, _)| *hooked == current && *hooked_selector == selector)
        {
            return *imp;
        }
        class = current.superclass();
    }
    unreachable!("a hook runs only for a receiver of a hooked class")
}

impl Ui {
    /// One line per write, naming the node by its path from the root, such
    /// as `window[0]/stack_view[0]/label[2]`, and the write by its source
    /// location or selector.
    pub(crate) fn describe_writes(&self, writes: &[Write]) -> String {
        // Each node's path, and the node behind each view and window.
        let mut paths = AHashMap::new();
        let mut owners = AHashMap::new();
        let mut stack = vec![(self.tree.root(), String::new())];
        while let Some((id, path)) = stack.pop() {
            for (index, child) in self.tree.children(id).iter().enumerate() {
                let node = self.tree.node(*child);
                let kind = match &node.payload {
                    Payload::Root => "root",
                    Payload::Window(window) => {
                        owners.insert(Retained::as_ptr(&window.window).addr(), *child);
                        owners.insert(Retained::as_ptr(&window.container).addr(), *child);
                        "window"
                    }
                    Payload::Control(control) => {
                        owners.insert(Retained::as_ptr(&control.view).addr(), *child);
                        if let Some(inner) = control.control.as_control() {
                            owners.insert(Retained::as_ptr(&inner).addr(), *child);
                        }
                        control.control.kind()
                    }
                    Payload::Cache(_) => "cached",
                    Payload::State(_) => "state",
                };
                let child_path = if path.is_empty() {
                    format!("{kind}[{index}]")
                } else {
                    format!("{path}/{kind}[{index}]")
                };
                paths.insert(*child, child_path.clone());
                stack.push((*child, child_path));
            }
        }
        let path = |node: &NodeId| {
            paths
                .get(node)
                .map_or("a node no longer in the tree", String::as_str)
                .to_owned()
        };
        let mut lines = format!("an idle pass wrote {} times:", writes.len());
        for write in writes {
            let line = match write {
                Write::Decided { node, site } => format!("{} decided at {site}", path(node)),
                Write::Sent { selector, receiver } => {
                    let class = receiver.class();
                    match owner(&owners, receiver) {
                        Some(node) => format!("{} was sent {selector} ({class})", path(&node)),
                        None => format!("a {class} outside the tree was sent {selector}"),
                    }
                }
            };
            lines.push_str("\n  ");
            lines.push_str(&line);
        }
        lines
    }
}

/// Whether `class` is one this crate makes objects of. AppKit also sends
/// hooked setters to its private views, such as the caret overlay of a
/// focused text field, and those are no write of ours.
fn ours(class: &AnyClass) -> bool {
    // Key-value observing swaps an object's class for a subclass it names
    // after the original.
    let class = match class.superclass() {
        Some(superclass) if class.name().to_bytes().starts_with(b"NSKVONotifying_") => superclass,
        _ => class,
    };
    let created = [
        NSBox::class(),
        NSButton::class(),
        NSComboBox::class(),
        NSImageView::class(),
        NSPopUpButton::class(),
        NSProgressIndicator::class(),
        NSScrollView::class(),
        NSSecureTextField::class(),
        NSStackView::class(),
        NSTableView::class(),
        NSTextField::class(),
        NSView::class(),
        NSGlassEffectView::class(),
        NSVisualEffectView::class(),
        NSWindow::class(),
    ];
    // Constraints come from anchors, which may make a private subclass.
    let mut ancestor = Some(class);
    while let Some(current) = ancestor {
        if current == NSLayoutConstraint::class() {
            return true;
        }
        ancestor = current.superclass();
    }
    created.contains(&class)
}

/// The node whose view is `receiver` or holds it: for a view, the nearest
/// owned view among it and its superviews; for a constraint, that of its
/// first item.
fn owner(owners: &AHashMap<usize, NodeId>, receiver: &AnyObject) -> Option<NodeId> {
    let mut view = if let Some(constraint) = receiver.downcast_ref::<NSLayoutConstraint>() {
        // SAFETY: the first item of a constraint between views is a view or
        // a layout guide, and the downcast checks which.
        unsafe { constraint.firstItem() }.and_then(|item| item.downcast::<NSView>().ok())
    } else {
        receiver.downcast_ref::<NSView>().map(Message::retain)
    };
    while let Some(current) = view {
        if let Some(node) = owners.get(&Retained::as_ptr(&current).addr()) {
            return Some(*node);
        }
        view = unsafe { current.superview() };
    }
    owners.get(&std::ptr::from_ref(receiver).addr()).copied()
}

#[cfg(test)]
mod tests;
