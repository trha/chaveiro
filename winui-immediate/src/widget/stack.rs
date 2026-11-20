use super::{Control, Response, must_write};
use crate::{
    bindings::{Panel, StackPanel},
    pass::{ChildKind, ControlNode, Ui},
};
use std::hash::Hash;
use windows_core::{Interface, Result};

impl Ui {
    /// A stack of the controls `body` declares, in declaration order. A stack
    /// measures its children with unbounded length, so a scrolling list
    /// inside one never scrolls; use [`Ui::grid`] to give it space.
    pub fn stack(
        &mut self,
        id: impl Hash,
        orientation: Orientation,
        spacing: f64,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let panel = StackPanel::new()?;
            let children = panel.cast::<Panel>()?.Children()?;
            Ok(ControlNode::new(
                panel.cast()?,
                Control::Stack {
                    panel,
                    children,
                    orientation: None,
                    spacing: f64::NAN,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Stack {
            panel,
            orientation: written_orientation,
            spacing: written_spacing,
            ..
        } = &mut control.control
        else {
            unreachable!("stack keys hold stacks")
        };
        if must_write(stale, written_orientation, &Some(orientation)) {
            panel.SetOrientation(match orientation {
                Orientation::Vertical => crate::bindings::Orientation::Vertical,
                Orientation::Horizontal => crate::bindings::Orientation::Horizontal,
            })?;
            *written_orientation = Some(orientation);
        }
        if must_write(stale, written_spacing, &spacing) {
            panel.SetSpacing(spacing)?;
            *written_spacing = spacing;
        }
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(Response { ui: self, node })
    }
}

/// The direction a [`Ui::stack`] lays out its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}
