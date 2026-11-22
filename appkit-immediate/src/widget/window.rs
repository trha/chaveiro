use crate::{
    events::EventKind,
    pass::{ChildKind, Payload, Ui, WindowNode},
    tree::{NodeId, Visit},
};
use objc2::{MainThreadOnly, runtime::ProtocolObject};
use objc2_app_kit::{
    NSBackingStoreType, NSGlassEffectView, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowButton,
    NSWindowOrderingMode, NSWindowStyleMask, NSWindowTitleVisibility,
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
                glass: None,
                compact: false,
                full_size: false,
                fitted: None,
                sized: false,
                title: String::new(),
                subtitle: None,
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
            crate::write_check::record(node);
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
            if window.fitted.is_none() && !window.sized {
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

    /// Hides the title bar and all three window buttons, fixes the
    /// window's size, and lets the user drag the window by any point of
    /// its background. The user closes it only through the menu or a key.
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
            window_view.setMovableByWindowBackground(true);
            for button in [
                NSWindowButton::CloseButton,
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

    /// Fills the whole window with Liquid Glass, so the desktop shows
    /// through it. Glass draws only when the binary is linked against
    /// SDK 26 or later.
    pub fn glass(&mut self) -> &mut Self {
        let mtm = self.ui.mtm;
        let window = self.ui.window_mut(self.node);
        if window.glass.is_none() {
            // The glass becomes the window's content view and holds the
            // container, as in the system's About This Mac window. A glass
            // view behind the container, with the window made clear, draws
            // only a pale blur.
            let glass = NSGlassEffectView::new(mtm);
            window.window.setContentView(Some(&glass));
            glass.setContentView(Some(&window.container));
            window.glass = Some(glass);
        }
        self
    }

    /// Opens the window with content `width`×`height` points, or its
    /// content's fitting size where that is larger, in place of the
    /// 360×200 minimum. Later passes leave the size to the user.
    pub fn initial_size(&mut self, width: f64, height: f64) -> &mut Self {
        let window = self.ui.window_mut(self.node);
        if !window.sized {
            let fitting = crate::write_check::paused(|| {
                window.container.layoutSubtreeIfNeeded();
                window.container.fittingSize()
            });
            crate::write_check::record(self.node);
            window.window.setContentSize(NSSize::new(
                width.max(fitting.width),
                height.max(fitting.height),
            ));
            window.sized = true;
        }
        self
    }

    /// Draws the content up under the title bar, which shows no title
    /// (until [`subtitle`](Self::subtitle) shows it) and keeps all three
    /// window buttons, so a [`split_view`](Ui::split_view)'s panes reach the
    /// window's top edge. The title bar keeps its own background, which on
    /// macOS 26 is the Liquid Glass that a toolbar shows the content
    /// through; making it transparent would leave the toolbar clear.
    pub fn full_size_content(&mut self) -> &mut Self {
        let window = self.ui.window_mut(self.node);
        if !window.full_size {
            crate::write_check::record(self.node);
            let window_view = &window.window;
            window_view
                .setStyleMask(window_view.styleMask() | NSWindowStyleMask::FullSizeContentView);
            window_view.setTitleVisibility(NSWindowTitleVisibility::Hidden);
            window.full_size = true;
        }
        self
    }

    /// Shows the window's title in its toolbar, with `subtitle` under it,
    /// such as the name of the list a window shows over its item count. A
    /// unified toolbar puts both after the sidebar.
    pub fn subtitle(&mut self, subtitle: &str) -> &mut Self {
        let node = self.node;
        let window = self.ui.window_mut(node);
        if window.subtitle.as_deref() != Some(subtitle) {
            crate::write_check::record(node);
            if window.subtitle.is_none() {
                window
                    .window
                    .setTitleVisibility(NSWindowTitleVisibility::Visible);
            }
            window.window.setSubtitle(&NSString::from_str(subtitle));
            window.subtitle = Some(subtitle.to_owned());
        }
        self
    }

    /// Sizes the window to its content's fitting size on every pass where
    /// that size changed, in place of the 360×200 minimum.
    pub fn size_to_fit(&mut self) -> &mut Self {
        let node = self.node;
        let window = self.ui.window_mut(node);
        // Layout sets properties of AppKit's own, which are no write of ours.
        let fitting = crate::write_check::paused(|| {
            window.container.layoutSubtreeIfNeeded();
            window.container.fittingSize()
        });
        if window.fitted != Some((fitting.width, fitting.height)) {
            crate::write_check::record(node);
            window.window.setContentSize(fitting);
            window.fitted = Some((fitting.width, fitting.height));
        }
        self
    }
}
