use super::{Control, Response};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::rc::Retained;
use objc2_app_kit::{NSStackView, NSUserInterfaceLayoutOrientation};
use objc2_foundation::NSEdgeInsets;
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
            stack.setEdgeInsets(NSEdgeInsets {
                top: 12.0,
                left: 12.0,
                bottom: 12.0,
                right: 12.0,
            });
            ControlNode::new(
                Retained::clone(&stack).into_super(),
                Control::Stack {
                    stack,
                    orientation: None,
                    spacing: f64::NAN,
                },
            )
        });
        let Control::Stack {
            stack,
            orientation: written_orientation,
            spacing: written_spacing,
        } = &mut self.control_mut(node).control
        else {
            unreachable!("stack keys hold stacks")
        };
        if *written_orientation != Some(orientation) {
            stack.setOrientation(match orientation {
                Orientation::Vertical => NSUserInterfaceLayoutOrientation::Vertical,
                Orientation::Horizontal => NSUserInterfaceLayoutOrientation::Horizontal,
            });
            *written_orientation = Some(orientation);
        }
        if *written_spacing != spacing {
            stack.setSpacing(spacing);
            *written_spacing = spacing;
        }
        self.open(node);
        body(self);
        self.close();
        Response { ui: self, node }
    }
}

/// The direction a [`Ui::stack_view`] lays out its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}
