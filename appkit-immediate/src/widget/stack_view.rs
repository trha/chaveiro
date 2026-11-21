use super::{Control, Response, must_write};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSLayoutAttribute, NSStackView, NSTitlePosition,
    NSUserInterfaceLayoutOrientation,
};
use objc2_foundation::{NSEdgeInsets, NSSize};
use std::hash::Hash;

impl Ui {
    /// A stack of the controls `body` declares, in declaration order.
    pub fn stack_view(
        &mut self,
        id: impl Hash,
        orientation: Orientation,
        spacing: f64,
        body: impl FnOnce(&mut Ui),
    ) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let stack = NSStackView::new(mtm);
            // Children carry their own cross-axis constraints, by their
            // `align`, so the stack adds none.
            stack.setAlignment(NSLayoutAttribute::NotAnAttribute);
            stack.setEdgeInsets(NSEdgeInsets {
                top: 12.0,
                left: 12.0,
                bottom: 12.0,
                right: 12.0,
            });
            ControlNode::payload(
                Retained::clone(&stack).into_super(),
                Control::Stack {
                    stack,
                    orientation: None,
                    spacing: f64::NAN,
                },
            )
        });
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Stack {
            stack,
            orientation: written_orientation,
            spacing: written_spacing,
        } = &mut control.control
        else {
            unreachable!("stack keys hold stacks")
        };
        if must_write(node, stale, written_orientation, &Some(orientation)) {
            stack.setOrientation(match orientation {
                Orientation::Vertical => NSUserInterfaceLayoutOrientation::Vertical,
                Orientation::Horizontal => NSUserInterfaceLayoutOrientation::Horizontal,
            });
            *written_orientation = Some(orientation);
        }
        if must_write(node, stale, written_spacing, &spacing) {
            stack.setSpacing(spacing);
            *written_spacing = spacing;
        }
        self.open(node);
        body(self);
        self.close();
        Response { ui: self, node }
    }
}

impl Ui {
    /// A vertical stack of form rows, 8 points apart and with no padding.
    /// Each field `body` declares shows its header beside it, in a column
    /// `header_width` points wide, as "Name:" right-aligned on the field's
    /// baseline.
    pub fn form(
        &mut self,
        id: impl Hash,
        header_width: f64,
        body: impl FnOnce(&mut Ui),
    ) -> Response<'_> {
        let outer = self.form_header_width.replace(header_width);
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let stack = NSStackView::new(mtm);
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setAlignment(NSLayoutAttribute::NotAnAttribute);
            stack.setSpacing(8.0);
            stack.setEdgeInsets(NSEdgeInsets {
                top: 0.0,
                left: 0.0,
                bottom: 0.0,
                right: 0.0,
            });
            ControlNode::payload(
                Retained::clone(&stack).into_super(),
                Control::Stack {
                    stack,
                    orientation: Some(Orientation::Vertical),
                    spacing: 8.0,
                },
            )
        });
        // A form writes nothing after it is made, so an invalidate has
        // nothing to redo.
        self.control_mut(node).stale = false;
        self.open(node);
        body(self);
        self.close();
        self.form_header_width = outer;
        Response { ui: self, node }
    }

    /// A box `width` points wide, holding a vertical stack of the controls
    /// `body` declares, 8 points apart: the system's control background,
    /// a separator-colored border, 8-point corners and 24 points of
    /// padding. Its colors follow the system appearance.
    pub fn box_view(
        &mut self,
        id: impl Hash,
        width: f64,
        body: impl FnOnce(&mut Ui),
    ) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let frame = NSBox::new(mtm);
            frame.setBoxType(NSBoxType::Custom);
            frame.setTitlePosition(NSTitlePosition::NoTitle);
            frame.setCornerRadius(8.0);
            frame.setBorderWidth(1.0);
            frame.setFillColor(&NSColor::controlBackgroundColor());
            frame.setBorderColor(&NSColor::separatorColor());
            frame.setContentViewMargins(NSSize::new(0.0, 0.0));
            let stack = NSStackView::new(mtm);
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setAlignment(NSLayoutAttribute::NotAnAttribute);
            stack.setSpacing(8.0);
            stack.setEdgeInsets(NSEdgeInsets {
                top: 24.0,
                left: 24.0,
                bottom: 24.0,
                right: 24.0,
            });
            frame.setContentView(Some(&stack));
            ControlNode::payload(
                Retained::clone(&frame).into_super(),
                Control::BoxView { stack },
            )
        });
        let mut response = Response { ui: self, node };
        response.width(width);
        let Response { ui, node } = response;
        ui.open(node);
        body(ui);
        ui.close();
        Response { ui, node }
    }
}

/// The direction a [`Ui::stack_view`] lays out its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}
