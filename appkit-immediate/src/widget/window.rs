use crate::{
    events::EventKind,
    pass::{ChildKind, Payload, Ui, WindowNode},
    tree::{NodeId, Visit},
};
use objc2::{MainThreadOnly, runtime::ProtocolObject};
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::hash::Hash;

impl Ui {
    /// A top-level window titled `title`, holding the one control `body`
    /// declares. A window opens on the first pass that declares it and
    /// closes on the first pass that does not.
    pub fn window(
        &mut self,
        id: impl Hash,
        title: &str,
        body: impl FnOnce(&mut Ui),
    ) -> WindowResponse {
        let (node, visit) = self.visit(id, ChildKind::Logical, |mtm| {
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(360.0, 200.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Resizable
                        | NSWindowStyleMask::Miniaturizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // `Retained` owns the window; AppKit must not release it again.
            unsafe { window.setReleasedWhenClosed(false) };
            Payload::Window(WindowNode {
                window,
                content: None,
                title: String::new(),
            })
        });
        if visit == Visit::Created {
            let Payload::Window(window) = &self.tree.node(node).payload else {
                unreachable!("window keys hold windows")
            };
            self.events.register(&window.window, node);
            window
                .window
                .setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
        }
        let window = self.window_mut(node);
        if window.title != title {
            window.window.setTitle(&NSString::from_str(title));
            window.title.clear();
            window.title.push_str(title);
        }
        self.open(node);
        body(self);
        self.close();
        if visit == Visit::Created {
            let window = &self.window_mut(node).window;
            window.center();
            window.makeKeyAndOrderFront(None);
        }
        WindowResponse {
            should_close: self.events.happened(node, EventKind::CloseRequested),
        }
    }

    fn window_mut(&mut self, node: NodeId) -> &mut WindowNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Window(window) => window,
            _ => unreachable!("window keys hold windows"),
        }
    }
}

/// What [`Ui::window`] returns.
pub struct WindowResponse {
    should_close: bool,
}

impl WindowResponse {
    /// Whether the user asked to close the window since the last pass. The
    /// window stays open until a pass no longer declares it.
    pub fn should_close(&self) -> bool {
        self.should_close
    }
}
