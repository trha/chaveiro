use crate::{
    events::EventKind,
    pass::{ChildKind, Payload, Ui, WindowNode},
    tree::{NodeId, Visit},
};
use objc2::{MainThreadOnly, runtime::ProtocolObject};
use objc2_app_kit::{
    NSBackingStoreType, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowButton, NSWindowOrderingMode,
    NSWindowStyleMask, NSWindowTitleVisibility,
};
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
    ) -> WindowResponse<'_> {
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
                effect: None,
                compact: false,
                fitted: None,
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
            // Shown at the end of the pass, after the window's options.
            self.new_windows.push(node);
        }
        WindowResponse { ui: self, node }
    }

    /// Shows the windows this pass created, each at the size its content
    /// asks for, so the user never sees an empty or resized window. A window
    /// that [`size_to_fit`](WindowResponse::size_to_fit) already sized keeps
    /// that size; others open at least 360×200.
    pub(crate) fn show_new_windows(&mut self) {
        for node in self.new_windows.drain(..) {
            // A window created and dropped in the same pass is gone.
            let Some(Payload::Window(window)) = self.tree.get(node).map(|node| &node.payload)
            else {
                continue;
            };
            if window.fitted.is_none() {
                let fitting = window.container.fittingSize();
                window.window.setContentSize(NSSize::new(
                    fitting.width.max(MINIMUM.width),
                    fitting.height.max(MINIMUM.height),
                ));
            }
            window.window.center();
            window.window.makeKeyAndOrderFront(None);
        }
    }

    pub(crate) fn window_mut(&mut self, node: NodeId) -> &mut WindowNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Window(window) => window,
            _ => unreachable!("window keys hold windows"),
        }
    }
}

/// The smallest content size a new window opens at, in points.
const MINIMUM: NSSize = NSSize::new(360.0, 200.0);

/// What [`Ui::window`] returns. Its options run after the window's body, in
/// the same pass, so a new window shows with them already applied.
pub struct WindowResponse<'u> {
    ui: &'u mut Ui,
    node: NodeId,
}

impl WindowResponse<'_> {
    /// Whether the user asked to close the window since the last pass. The
    /// window stays open until a pass no longer declares it.
    pub fn should_close(&self) -> bool {
        self.ui.events.happened(self.node, EventKind::ShouldClose)
    }

    /// Puts a visual effect view behind the content, so the desktop shows
    /// through the window's background as the system material.
    pub fn visual_effect(&mut self) -> &mut Self {
        let mtm = self.ui.mtm;
        let window = self.ui.window_mut(self.node);
        if window.effect.is_none() {
            let effect = NSVisualEffectView::new(mtm);
            effect.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
            effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            effect.setState(NSVisualEffectState::FollowsWindowActiveState);
            effect.setTranslatesAutoresizingMaskIntoConstraints(false);
            let container = &window.container;
            container.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, None);
            for constraint in [
                effect
                    .leadingAnchor()
                    .constraintEqualToAnchor(&container.leadingAnchor()),
                effect
                    .trailingAnchor()
                    .constraintEqualToAnchor(&container.trailingAnchor()),
                effect
                    .topAnchor()
                    .constraintEqualToAnchor(&container.topAnchor()),
                effect
                    .bottomAnchor()
                    .constraintEqualToAnchor(&container.bottomAnchor()),
            ] {
                constraint.setActive(true);
            }
            window.effect = Some(effect);
        }
        self
    }

    /// Draws the content under a transparent title bar with only the close
    /// button, and fixes the window's size.
    pub fn compact(&mut self) -> &mut Self {
        let window = self.ui.window_mut(self.node);
        if !window.compact {
            let window_view = &window.window;
            window_view.setStyleMask(
                (window_view.styleMask() | NSWindowStyleMask::FullSizeContentView)
                    & !(NSWindowStyleMask::Resizable | NSWindowStyleMask::Miniaturizable),
            );
            window_view.setTitlebarAppearsTransparent(true);
            window_view.setTitleVisibility(NSWindowTitleVisibility::Hidden);
            for button in [
                NSWindowButton::MiniaturizeButton,
                NSWindowButton::ZoomButton,
            ] {
                if let Some(button) = window_view.standardWindowButton(button) {
                    button.setHidden(true);
                }
            }
            window.compact = true;
        }
        self
    }

    /// Sizes the window to its content's fitting size on every pass where
    /// that size changed, in place of the 360×200 minimum.
    pub fn size_to_fit(&mut self) -> &mut Self {
        let window = self.ui.window_mut(self.node);
        window.container.layoutSubtreeIfNeeded();
        let fitting = window.container.fittingSize();
        if window.fitted != Some((fitting.width, fitting.height)) {
            window.window.setContentSize(fitting);
            window.fitted = Some((fitting.width, fitting.height));
        }
        self
    }
}
