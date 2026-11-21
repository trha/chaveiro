use super::{Control, Response, must_write, resource};
use crate::{
    bindings::{
        Brush, CornerRadius, FrameworkElement, HorizontalAlignment, IPanel, Panel, StackPanel,
        Thickness, VerticalAlignment,
    },
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
                    width: f64::NAN,
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

impl Ui {
    /// A Fluent card centered in its container, `width` wide: a vertical
    /// stack of the controls `body` declares, 8px apart, with the card fill
    /// and stroke, 8px corners and 24px padding.
    ///
    /// The fill and stroke are the theme's at creation, so a card made
    /// before a theme change keeps the old colors.
    pub fn card(
        &mut self,
        id: impl Hash,
        width: f64,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let node = self.stack(id, Orientation::Vertical, 8.0, body)?.node;
        let control = self.control_mut(node);
        let Control::Stack {
            panel,
            width: written,
            ..
        } = &mut control.control
        else {
            unreachable!("card keys hold stacks")
        };
        if written.is_nan() {
            // The first pass: a stack becomes a card once.
            panel
                .cast::<IPanel>()?
                .SetBackground(&resource::<Brush>("CardBackgroundFillColorDefaultBrush")?)?;
            panel.SetBorderBrush(&resource::<Brush>("CardStrokeColorDefaultBrush")?)?;
            panel.SetBorderThickness(uniform(1.0))?;
            panel.SetCornerRadius(CornerRadius {
                top_left: 8.0,
                top_right: 8.0,
                bottom_right: 8.0,
                bottom_left: 8.0,
            })?;
            panel.SetPadding(uniform(24.0))?;
            let frame = panel.cast::<FrameworkElement>()?;
            frame.SetHorizontalAlignment(HorizontalAlignment::Center)?;
            frame.SetVerticalAlignment(VerticalAlignment::Center)?;
        }
        if *written != width {
            panel.cast::<FrameworkElement>()?.SetWidth(width)?;
            *written = width;
        }
        Ok(Response { ui: self, node })
    }
}

fn uniform(length: f64) -> Thickness {
    Thickness {
        left: length,
        top: length,
        right: length,
        bottom: length,
    }
}

/// The direction a [`Ui::stack`] lays out its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}
