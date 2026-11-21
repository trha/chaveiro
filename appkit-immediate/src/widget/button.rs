use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{AnyThread, rc::Retained, sel};
use objc2_app_kit::{
    NSButton, NSCellImagePosition, NSControlSize, NSImage, NSProgressIndicator,
    NSProgressIndicatorStyle,
};
use objc2_foundation::{NSSize, NSString};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};

impl Ui {
    /// A push button showing `label`.
    pub fn button(&mut self, id: impl Hash, label: &str) -> ButtonResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(&NSString::new(), None, None, mtm)
            };
            ControlNode::payload(
                Retained::clone(&button).into_super().into_super(),
                Control::Button {
                    button,
                    label: String::new(),
                    // A new button has no key equivalent and no spinner.
                    key: Some(""),
                    spinner: None,
                    busy: Some(false),
                },
            )
        });
        if visit == Visit::Created {
            // Registering needs the node id, which exists only now.
            let control = self.control(node);
            let Control::Button { button, .. } = &control.control else {
                unreachable!("button keys hold buttons")
            };
            self.events.register(button, node);
            unsafe {
                button.setTarget(Some(self.events.target()));
                button.setAction(Some(sel!(fire:)));
            }
        }
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Button {
            button,
            label: written,
            key,
            busy,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        // The options read no `stale`, so an invalidate forgets what they
        // wrote, and their next call writes again.
        if stale {
            *key = None;
            *busy = None;
        }
        if must_write(node, stale, written.as_str(), label) {
            button.setTitle(&NSString::from_str(label));
            written.clear();
            written.push_str(label);
        }
        ButtonResponse(Response { ui: self, node })
    }
}

/// What [`Ui::button`] returns.
pub struct ButtonResponse<'u>(Response<'u>);

impl ButtonResponse<'_> {
    /// Whether the button was clicked since the last pass.
    pub fn clicked(&self) -> bool {
        self.0.ui.events.happened(self.0.node, EventKind::Action)
    }

    /// Makes the button the window's default button: AppKit draws it in the
    /// accent color, and Return presses it.
    pub fn default_button(&mut self, on: bool) -> &mut Self {
        self.key_equivalent(if on { "\r" } else { "" })
    }

    /// Makes Escape press the button, for a Cancel
    /// button.
    pub fn cancel_button(&mut self, on: bool) -> &mut Self {
        self.key_equivalent(if on { "\u{1b}" } else { "" })
    }

    #[track_caller]
    fn key_equivalent(&mut self, declared: &'static str) -> &mut Self {
        let control = self.0.ui.control_mut(self.0.node);
        let Control::Button { button, key, .. } = &mut control.control else {
            unreachable!("button keys hold buttons")
        };
        if must_write(self.0.node, false, key, &Some(declared)) {
            button.setKeyEquivalent(&NSString::from_str(declared));
            *key = Some(declared);
        }
        self
    }

    /// Shows a small spinner before the title while `on`, for work the
    /// click started. The button stays clickable; disable it with
    /// [`Response::enabled`] if a second click must wait.
    pub fn busy(&mut self, on: bool) -> &mut Self {
        let mtm = self.0.ui.mtm;
        let control = self.0.ui.control_mut(self.0.node);
        let Control::Button {
            button,
            spinner,
            busy,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if !must_write(self.0.node, false, busy, &Some(on)) {
            return self;
        }
        *busy = Some(on);
        let spinner = spinner.get_or_insert_with(|| {
            let spinner = NSProgressIndicator::new(mtm);
            spinner.setStyle(NSProgressIndicatorStyle::Spinning);
            spinner.setControlSize(NSControlSize::Small);
            spinner.setDisplayedWhenStopped(false);
            spinner.setTranslatesAutoresizingMaskIntoConstraints(false);
            button.addSubview(&spinner);
            spinner
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&button.leadingAnchor(), 8.0)
                .setActive(true);
            spinner
                .centerYAnchor()
                .constraintEqualToAnchor(&button.centerYAnchor())
                .setActive(true);
            spinner
        });
        // A blank leading image reserves the spinner's place before the title.
        button.setImage(on.then(|| blank_image(NSSize::new(16.0, 16.0))).as_deref());
        button.setImagePosition(NSCellImagePosition::ImageLeading);
        // Animating needs no sender.
        unsafe {
            if on {
                spinner.startAnimation(None);
            } else {
                spinner.stopAnimation(None);
            }
        }
        self
    }
}

/// A transparent image of `size`.
fn blank_image(size: NSSize) -> Retained<NSImage> {
    NSImage::initWithSize(NSImage::alloc(), size)
}

impl<'u> Deref for ButtonResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for ButtonResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}
