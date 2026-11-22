use super::{Control, Response, boxed, must_write, resource};
use crate::{
    bindings::{
        AutomationProperties, Button, ContentControl, DependencyObject, FontIcon, FrameworkElement,
        IPanel, Orientation, ProgressRing, StackPanel, Style, TextBlock, ToolTipService, UIElement,
        Visibility,
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
                    row: None,
                    glyph: None,
                    tooltip: String::new(),
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
        ui.subscribe(*node, &[EventKind::Click], |control| {
            match &control.control {
                Control::Button { button, .. } => button,
                _ => unreachable!("button keys hold buttons"),
            }
        })?;
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

    /// Shows `text` on hover, such as the key that does the same:
    /// "Save (Ctrl+S)".
    pub fn tooltip(&mut self, text: &str) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::Button {
            button,
            tooltip: written,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, written.as_str(), text) {
            ToolTipService::SetToolTip(&button.cast::<DependencyObject>()?, &boxed(text))?;
            written.clear();
            written.push_str(text);
        }
        Ok(self)
    }

    /// Shows `glyph`, a Segoe Fluent Icons glyph, before the label, as for
    /// Save.
    pub fn icon(&mut self, glyph: char) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::Button {
            button,
            content,
            row,
            glyph: written,
            busy,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, written, &Some(glyph)) {
            let row = row_of(button, content, row)?;
            row.icon.SetGlyph(glyph.encode_utf8(&mut [0; 4]) as &str)?;
            row.icon.cast::<UIElement>()?.SetVisibility(shown(!*busy))?;
            *written = Some(glyph);
        }
        Ok(self)
    }

    /// While `on`, the button is disabled and shows a 16px progress ring
    /// before its label, in place of its icon, for a request it started. It
    /// keeps its width only when the layout gives it one, such as a
    /// full-width button.
    pub fn busy(&mut self, on: bool) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::Button {
            button,
            content,
            row,
            glyph,
            busy,
            ..
        } = &mut control.control
        else {
            unreachable!("button keys hold buttons")
        };
        if must_write(stale, busy, &on) {
            // A button that was never busy and has no icon keeps its plain
            // label, with no row.
            if row.is_some() || on {
                let row = row_of(button, content, row)?;
                row.ring.SetIsActive(on)?;
                row.ring.cast::<UIElement>()?.SetVisibility(shown(on))?;
                row.icon
                    .cast::<UIElement>()?
                    .SetVisibility(shown(!on && glyph.is_some()))?;
            }
            *busy = on;
        }
        self.0.enabled(!on)?;
        Ok(self)
    }
}

/// The parts of a button whose content became a row: the icon and the ring,
/// both collapsed until used, then the label.
pub(crate) struct ButtonRow {
    icon: FontIcon,
    ring: ProgressRing,
}

/// The row of `button`, made on first use: the content becomes a row of
/// the icon, the ring and `content`, the label.
fn row_of<'r>(
    button: &Button,
    content: &TextBlock,
    row: &'r mut Option<ButtonRow>,
) -> Result<&'r ButtonRow> {
    if row.is_none() {
        let panel = StackPanel::new()?;
        panel.SetOrientation(Orientation::Horizontal)?;
        panel.SetSpacing(8.0)?;
        let icon = FontIcon::new()?;
        icon.SetFontSize(16.0)?;
        let ring = ProgressRing::new()?;
        let frame = ring.cast::<FrameworkElement>()?;
        frame.SetWidth(16.0)?;
        frame.SetHeight(16.0)?;
        for part in [icon.cast::<UIElement>()?, ring.cast::<UIElement>()?] {
            part.SetVisibility(Visibility::Collapsed)?;
        }
        // Set first, so the label leaves the button before the row takes it.
        button.cast::<ContentControl>()?.SetContent(&panel)?;
        let children = panel.cast::<IPanel>()?.Children()?;
        children.Append(&icon.cast::<UIElement>()?)?;
        children.Append(&ring.cast::<UIElement>()?)?;
        children.Append(&content.cast::<UIElement>()?)?;
        *row = Some(ButtonRow { icon, ring });
    }
    Ok(row.as_ref().expect("made above"))
}

fn shown(visible: bool) -> Visibility {
    match visible {
        true => Visibility::Visible,
        false => Visibility::Collapsed,
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
