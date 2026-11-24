use super::{Control, Length, Response, must_write};
use crate::{
    bindings::{
        CornerRadius, FrameworkElement, HorizontalAlignment, Panel, StackPanel, Thickness,
        VerticalAlignment,
    },
    pass::{ChildKind, ControlNode, Ui},
    theme::{clear_brush, set_brush},
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
                    fill: None,
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
            card_look(panel)?;
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

    /// A Fluent card that groups rows, such as the fields of an item: a
    /// vertical stack of the controls `body` declares, with no gap or
    /// padding, stretched across its container, with the fill, stroke and
    /// corners of [`Ui::card`]. Fill it with [`Ui::card_row`].
    ///
    /// The fill and stroke are the theme's at creation, as for
    /// [`Ui::card`].
    pub fn card_group(
        &mut self,
        id: impl Hash,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let node = self.stack(id, Orientation::Vertical, 0.0, body)?.node;
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
            card_look(panel)?;
            *written = f64::INFINITY;
        }
        Ok(Response { ui: self, node })
    }

    /// One row of a [`Ui::card_group`]: a grid of one row with one column
    /// per entry of `columns`, 8px apart, filled in order like
    /// [`Ui::columns`]. It is at least 60px tall, the height of a caption
    /// over a value, with 16px of padding on the left and 8px elsewhere.
    ///
    /// `divider` draws a 1px line at its top; pass false for the first row
    /// shown in its group, and true for the rest.
    pub fn card_row(
        &mut self,
        id: impl Hash,
        divider: bool,
        columns: &[Length],
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let node = self.grid_node(
            id,
            true,
            columns,
            |grid| {
                grid.SetColumnSpacing(8.0)?;
                grid.SetPadding(Thickness {
                    left: 16.0,
                    top: 8.0,
                    right: 8.0,
                    bottom: 8.0,
                })?;
                let frame = grid.cast::<FrameworkElement>()?;
                frame.SetMinHeight(60.0)
            },
            body,
        )?;
        let mut response = Response { ui: self, node };
        response.dividers([false, divider, false, false])?;
        Ok(response)
    }
}

impl Response<'_> {
    /// Fills a stack with the theme brush named `brush`, such as
    /// `CardBackgroundFillColorSecondaryBrush`, with `corners` rounded in
    /// pixels (top left, top right, bottom right, bottom left), or takes the
    /// fill and corners away for [`None`]. The brush is the theme's when
    /// written. Panics on anything but a stack.
    pub fn fill(&mut self, brush: Option<&'static str>, corners: [f64; 4]) -> Result<&mut Self> {
        let Control::Stack { panel, fill, .. } = &mut self.ui.control_mut(self.node).control else {
            panic!("only stacks take a fill");
        };
        let wanted = brush.map(|brush| (brush, corners));
        if *fill != wanted {
            let [top_left, top_right, bottom_right, bottom_left] = match brush {
                Some(_) => corners,
                None => [0.0; 4],
            };
            let property = Panel::BackgroundProperty()?;
            match brush {
                Some(brush) => set_brush(panel, &property, brush)?,
                None => clear_brush(panel, &property)?,
            }
            panel.SetCornerRadius(CornerRadius {
                top_left,
                top_right,
                bottom_right,
                bottom_left,
            })?;
            *fill = wanted;
        }
        Ok(self)
    }
}

/// Gives `panel` the fill, stroke and 8px corners of a Fluent card.
fn card_look(panel: &StackPanel) -> Result<()> {
    set_brush(
        panel,
        &Panel::BackgroundProperty()?,
        "CardBackgroundFillColorDefaultBrush",
    )?;
    set_brush(
        panel,
        &StackPanel::BorderBrushProperty()?,
        "CardStrokeColorDefaultBrush",
    )?;
    panel.SetBorderThickness(uniform(1.0))?;
    panel.SetCornerRadius(CornerRadius {
        top_left: 8.0,
        top_right: 8.0,
        bottom_right: 8.0,
        bottom_left: 8.0,
    })
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
