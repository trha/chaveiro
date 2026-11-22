use super::{Control, Response, must_write};
use crate::pass::{ChildKind, ControlNode, Ui};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBox, NSBoxType, NSColor,
    NSLayoutAttribute, NSStackView, NSTitlePosition, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSSize};
use std::{hash::Hash, ptr::NonNull};

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

impl Ui {
    /// A rounded box as wide as its container, holding a vertical stack of
    /// the controls `body` declares, edge to edge and with no space
    /// between, for a group of rows that [`separator`](Self::separator)s
    /// set apart. Its fill, corners and dividers match the grouped form of
    /// System Settings in both appearances.
    pub fn group_box(&mut self, id: impl Hash, body: impl FnOnce(&mut Ui)) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let frame = NSBox::new(mtm);
            frame.setBoxType(NSBoxType::Custom);
            frame.setTitlePosition(NSTitlePosition::NoTitle);
            frame.setCornerRadius(12.0);
            frame.setBorderWidth(0.0);
            frame.setFillColor(&group_fill());
            frame.setContentViewMargins(NSSize::new(0.0, 0.0));
            let stack = NSStackView::new(mtm);
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setAlignment(NSLayoutAttribute::NotAnAttribute);
            stack.setSpacing(0.0);
            stack.setEdgeInsets(NSEdgeInsets {
                top: 0.0,
                left: 0.0,
                bottom: 0.0,
                right: 0.0,
            });
            frame.setContentView(Some(&stack));
            ControlNode::payload(
                Retained::clone(&frame).into_super(),
                Control::BoxView { stack },
            )
        });
        self.open(node);
        body(self);
        self.close();
        Response { ui: self, node }
    }

    /// A one-point line across its stack view, 10 points in from each
    /// end, such as between the rows of a [`group_box`](Self::group_box).
    pub fn separator(&mut self, id: impl Hash) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let row = NSView::new(mtm);
            row.heightAnchor()
                .constraintEqualToConstant(1.0)
                .setActive(true);
            let line = NSBox::new(mtm);
            line.setBoxType(NSBoxType::Custom);
            line.setTitlePosition(NSTitlePosition::NoTitle);
            line.setBorderWidth(0.0);
            // System Settings draws its row dividers in this fill.
            line.setFillColor(&NSColor::tertiarySystemFillColor());
            line.setTranslatesAutoresizingMaskIntoConstraints(false);
            row.addSubview(&line);
            for constraint in [
                line.leadingAnchor()
                    .constraintEqualToAnchor_constant(&row.leadingAnchor(), 10.0),
                line.trailingAnchor()
                    .constraintEqualToAnchor_constant(&row.trailingAnchor(), -10.0),
                line.topAnchor().constraintEqualToAnchor(&row.topAnchor()),
                line.bottomAnchor()
                    .constraintEqualToAnchor(&row.bottomAnchor()),
            ] {
                constraint.setActive(true);
            }
            ControlNode::payload(row, Control::Native)
        });
        Response { ui: self, node }
    }
}

/// The fill of a [`Ui::group_box`]: 3% black in the light appearance and
/// 3% white in the dark one, as measured in a SwiftUI grouped form, which
/// System Settings uses. No system color has this value:
/// `quaternarySystemFill` is 2.7%.
fn group_fill() -> Retained<NSColor> {
    let light = NSColor::colorWithWhite_alpha(0.0, 0.03);
    let dark = NSColor::colorWithWhite_alpha(1.0, 0.03);
    let provider = RcBlock::new(move |appearance: NonNull<NSAppearance>| {
        // SAFETY: AppKit passes the appearance the color is drawn in.
        let appearance = unsafe { appearance.as_ref() };
        let names = NSArray::from_slice(&[unsafe { NSAppearanceNameAqua }, unsafe {
            NSAppearanceNameDarkAqua
        }]);
        let dark_match = appearance
            .bestMatchFromAppearancesWithNames(&names)
            .is_some_and(|name| &*name == unsafe { NSAppearanceNameDarkAqua });
        // The block keeps both colors, so the pointer stays valid while
        // the dynamic color lives.
        NonNull::from(match dark_match {
            true => &*dark,
            false => &*light,
        })
    });
    // SAFETY: the provider returns a color the block keeps alive.
    unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) }
}

/// The direction a [`Ui::stack_view`] lays out its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}
