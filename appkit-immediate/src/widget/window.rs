use crate::{
    events::EventKind,
    pass::{ChildKind, Payload, Ui, WindowNode},
    tree::{NodeId, Visit},
};
use objc2::{MainThreadOnly, runtime::ProtocolObject};
use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::hash::Hash;

impl Ui {
    /// A top-level window titled `title`, holding the one control `body`
    /// declares, which fills it. A window opens on the first pass that
    /// declares it, sized to its content but at least 360×200 points, and
    /// closes on the first pass that does not. The user cannot shrink it
    /// below its content.
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
            let container = NSView::new(mtm);
            window.setContentView(Some(&container));
            Payload::Window(WindowNode {
                window,
                container,
                content: None,
                pinned: None,
                pins: Vec::new(),
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
            // Shown only now, at the size its content asks for, so the user
            // never sees an empty or resized window.
            let window = self.window_mut(node);
            let fitting = window.container.fittingSize();
            window.window.setContentSize(NSSize::new(
                fitting.width.max(MINIMUM.width),
                fitting.height.max(MINIMUM.height),
            ));
            window.window.center();
            window.window.makeKeyAndOrderFront(None);
        }
        WindowResponse {
            should_close: self.events.happened(node, EventKind::ShouldClose),
        }
    }

    fn window_mut(&mut self, node: NodeId) -> &mut WindowNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Window(window) => window,
            _ => unreachable!("window keys hold windows"),
        }
    }
}

/// The smallest content size a new window opens at, in points.
const MINIMUM: NSSize = NSSize::new(360.0, 200.0);

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
