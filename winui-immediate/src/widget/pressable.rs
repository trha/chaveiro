use super::{Control, Response, make_subtle};
use crate::{
    bindings::{
        AutomationProperties, Button, ColumnDefinition, ContentControl, Control as XamlControl,
        CornerRadius, DependencyObject, FrameworkElement, Grid, GridLength, GridUnitType,
        HorizontalAlignment, IPanel, Thickness, UIElement, UIElementCollection, VerticalAlignment,
        Visibility,
    },
    pass::{ChildKind, ControlNode, Ui},
    theme::{clear_brush, set_brush},
    tree::NodeId,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A clickable area around the one control `body` declares, such as a
    /// list row: a subtle button stretched across its container, which
    /// takes clicks, Enter, Space and screen-reader Invoke. Give it a name
    /// with [`name`](PressableResponse::name), because its content does not
    /// name it.
    ///
    /// In a list, declare it under the same id in every row, so a recycled
    /// row keeps its control and its one click handler.
    pub fn pressable(
        &mut self,
        id: impl Hash,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<PressableResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let parts = Parts::new()?;
            Ok(ControlNode::new(
                parts.button.cast()?,
                Control::Pressable(Box::new(parts)),
            ))
        })?;
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(PressableResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::pressable`] returns.
pub struct PressableResponse<'u>(Response<'u>);

impl PressableResponse<'_> {
    /// Whether it was clicked or invoked since the last pass. The first
    /// call subscribes it.
    pub fn clicked(&mut self) -> Result<bool> {
        self.0.button_clicked()
    }

    /// Shows it as selected, as a list draws its selected row: a subtle
    /// fill and a 3px accent pill at its left edge.
    pub fn selected(&mut self, on: bool) -> Result<&mut Self> {
        let parts = self.parts();
        if parts.selected != on {
            parts.pill.SetVisibility(match on {
                true => Visibility::Visible,
                false => Visibility::Collapsed,
            })?;
            let property = XamlControl::BackgroundProperty()?;
            match on {
                true => set_brush(&parts.button, &property, "SubtleFillColorSecondaryBrush")?,
                false => clear_brush(&parts.button, &property)?,
            }
            parts.selected = on;
        }
        Ok(self)
    }

    /// The name screen readers say for it.
    pub fn name(&mut self, name: &str) -> Result<&mut Self> {
        let parts = self.parts();
        if parts.name != name {
            AutomationProperties::SetName(&parts.button.cast::<DependencyObject>()?, name)?;
            parts.name.clear();
            parts.name.push_str(name);
        }
        Ok(self)
    }

    fn parts(&mut self) -> &mut Parts {
        match &mut self.0.ui.control_mut(self.0.node).control {
            Control::Pressable(parts) => parts,
            _ => unreachable!("pressable keys hold pressables"),
        }
    }
}

impl<'u> Deref for PressableResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for PressableResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}

/// The button and what this layer last wrote to it.
///
/// Contract: `children` holds the pill first, then the app's control
/// when `content` is set.
pub(crate) struct Parts {
    button: Button,
    /// The children of the button's content grid: the pill in column 0
    /// and the app's control in column 1.
    pub(crate) children: UIElementCollection,
    pill: UIElement,
    pub(crate) content: Option<NodeId>,
    selected: bool,
    /// Empty until first written.
    name: String,
}

impl Parts {
    fn new() -> Result<Self> {
        let button = Button::new()?;
        make_subtle(&button)?;
        let control = button.cast::<XamlControl>()?;
        control.SetPadding(Thickness {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        })?;
        control.SetHorizontalContentAlignment(HorizontalAlignment::Stretch)?;
        control.SetVerticalContentAlignment(VerticalAlignment::Stretch)?;
        let frame = button.cast::<FrameworkElement>()?;
        frame.SetHorizontalAlignment(HorizontalAlignment::Stretch)?;

        // The pill column is as wide as a list item's left inset, so the
        // content does not move when the pill shows.
        let layout = Grid::new()?;
        let columns = layout.ColumnDefinitions()?;
        for (value, grid_unit_type) in [
            (PILL_COLUMN, GridUnitType::Pixel),
            (1.0, GridUnitType::Star),
        ] {
            let column = ColumnDefinition::new()?;
            column.SetWidth(GridLength {
                value,
                grid_unit_type,
            })?;
            columns.Append(&column)?;
        }
        let pill = Grid::new()?;
        set_brush(
            &pill,
            &crate::bindings::Panel::BackgroundProperty()?,
            "AccentFillColorDefaultBrush",
        )?;
        pill.SetCornerRadius(CornerRadius {
            top_left: 1.5,
            top_right: 1.5,
            bottom_right: 1.5,
            bottom_left: 1.5,
        })?;
        let pill_frame = pill.cast::<FrameworkElement>()?;
        pill_frame.SetWidth(3.0)?;
        pill_frame.SetHeight(16.0)?;
        pill_frame.SetHorizontalAlignment(HorizontalAlignment::Left)?;
        pill_frame.SetVerticalAlignment(VerticalAlignment::Center)?;
        let pill: UIElement = pill.cast()?;
        pill.SetVisibility(Visibility::Collapsed)?;
        let children = layout.cast::<IPanel>()?.Children()?;
        children.Append(&pill)?;
        button.cast::<ContentControl>()?.SetContent(&layout)?;
        Ok(Parts {
            button,
            children,
            pill,
            content: None,
            selected: false,
            name: String::new(),
        })
    }
}

/// The width of the column the pill sits in, in view pixels.
const PILL_COLUMN: f64 = 12.0;
