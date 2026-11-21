use super::{Control, Response, must_write, resource};
use crate::{
    bindings::{
        AutomationProperties, Button, ContentControl, DependencyObject, FrameworkElement, IPanel,
        Orientation, ProgressRing, StackPanel, Style, TextBlock, UIElement, Visibility,
    },
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A push button showing `label`.
    pub fn button(&mut self, id: impl Hash, label: &str) -> Result<ButtonResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let button = Button::new()?;
            let content = TextBlock::new()?;
            button.cast::<ContentControl>()?.SetContent(&content)?;
            Ok(ControlNode::new(
                button.cast()?,
                Control::Button {
                    button,
                    content,
                    label: String::new(),
                    accent: false,
                    ring: None,
                    busy: false,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Button {
            button,
            content,
            label: written,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, written.as_str(), label) {
            content.SetText(label)?;
            // A busy button's content is a row, which gives it no name of its
            // own, so screen readers get the label here.
            AutomationProperties::SetName(&button.cast::<DependencyObject>()?, label)?;
            written.clear();
            written.push_str(label);
        }
        Ok(ButtonResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::button`] returns.
pub struct ButtonResponse<'u>(Response<'u>);

impl ButtonResponse<'_> {
    /// Whether the button was clicked since the last pass. The first call
    /// subscribes the button, so a button whose clicks are never asked for
    /// registers no handler.
    pub fn clicked(&mut self) -> Result<bool> {
        let Response { ui, node } = &mut self.0;
        let control = ui.control_mut(*node);
        let Control::Button { button, .. } = &control.control else {
            unreachable!("button keys hold buttons")
        };
        let button = button.clone();
        let mut subscribed = control.subscribed;
        ui.events
            .subscribe(*node, &mut subscribed, &button, EventKind::Click)?;
        ui.control_mut(*node).subscribed = subscribed;
        Ok(ui.events.happened(*node, EventKind::Click))
    }

    /// Gives the button the accent style, for the one primary action of a
    /// screen, or the standard style.
    pub fn accent(&mut self, on: bool) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::Button { button, accent, .. } = &mut control.control else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, accent, &on) {
            let frame = button.cast::<FrameworkElement>()?;
            match on {
                true => frame.SetStyle(&resource::<Style>("AccentButtonStyle")?)?,
                false => frame.SetStyle(None::<&Style>)?,
            }
            *accent = on;
        }
        Ok(self)
    }

    /// While `on`, the button is disabled and shows a 16px progress ring
    /// before its label, for a request it started. It keeps its width only
    /// when the layout gives it one, such as a full-width button.
    pub fn busy(&mut self, on: bool) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::Button {
            button,
            content,
            ring,
            busy,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, busy, &on) {
            if ring.is_none() && on {
                // The content becomes a row of the ring and the label.
                let row = StackPanel::new()?;
                row.SetOrientation(Orientation::Horizontal)?;
                row.SetSpacing(8.0)?;
                let made = ProgressRing::new()?;
                let frame = made.cast::<FrameworkElement>()?;
                frame.SetWidth(16.0)?;
                frame.SetHeight(16.0)?;
                button.cast::<ContentControl>()?.SetContent(&row)?;
                let children = row.cast::<IPanel>()?.Children()?;
                children.Append(&made.cast::<UIElement>()?)?;
                children.Append(&content.cast::<UIElement>()?)?;
                *ring = Some(made);
            }
            if let Some(ring) = ring {
                ring.SetIsActive(on)?;
                ring.cast::<UIElement>()?.SetVisibility(match on {
                    true => Visibility::Visible,
                    false => Visibility::Collapsed,
                })?;
            }
            *busy = on;
        }
        self.0.enabled(!on)?;
        Ok(self)
    }
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
