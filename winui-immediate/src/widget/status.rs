//! Small controls for status and identity: a button around any content,
//! a person's initials, progress, and a tag.

use super::{Control, Response, boxed, make_subtle, must_write};
use crate::{
    bindings::{
        AutomationProperties, Button, Control as XamlControl, CornerRadius, DependencyObject,
        FontIcon, FrameworkElement, Orientation as XamlOrientation, Panel, PersonPicture,
        ProgressBar, ProgressRing, StackPanel, TextBlock, Thickness, ToolTipService,
        VerticalAlignment,
    },
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A subtle button around the one control `body` declares, such as a
    /// person's picture. `label` is its tooltip and the name screen readers
    /// say.
    pub fn content_button(
        &mut self,
        id: impl Hash,
        label: &str,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<ContentButtonResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let button = Button::new()?;
            make_subtle(&button)?;
            button.cast::<XamlControl>()?.SetPadding(Thickness {
                left: 6.0,
                top: 6.0,
                right: 6.0,
                bottom: 6.0,
            })?;
            Ok(ControlNode::new(
                button.cast()?,
                Control::ContentButton {
                    label: String::new(),
                    content: None,
                },
            ))
        })?;
        let ControlNode {
            element,
            control,
            stale,
            ..
        } = self.control_mut(node);
        let stale = std::mem::take(stale);
        let Control::ContentButton { label: written, .. } = control else {
            unreachable!("content button keys hold content buttons")
        };
        if must_write(stale, written.as_str(), label) {
            let target = element.cast::<DependencyObject>()?;
            AutomationProperties::SetName(&target, label)?;
            ToolTipService::SetToolTip(&target, &boxed(label))?;
            written.clear();
            written.push_str(label);
        }
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(ContentButtonResponse(Response { ui: self, node }))
    }

    /// A circle with a person's `initials`, `size` view pixels wide, as
    /// WinUI's `PersonPicture` draws it.
    pub fn person_picture(
        &mut self,
        id: impl Hash,
        initials: &str,
        size: f64,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let picture = PersonPicture::new()?;
            Ok(ControlNode::new(
                picture.cast()?,
                Control::PersonPicture {
                    picture,
                    initials: String::new(),
                    size: f64::NAN,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::PersonPicture {
            picture,
            initials: written_initials,
            size: written_size,
        } = &mut control.control
        else {
            unreachable!("person picture keys hold person pictures")
        };
        if must_write(stale, written_initials.as_str(), initials) {
            picture.SetInitials(initials)?;
            written_initials.clear();
            written_initials.push_str(initials);
        }
        if stale || *written_size != size {
            let frame = picture.cast::<FrameworkElement>()?;
            frame.SetWidth(size)?;
            frame.SetHeight(size)?;
            *written_size = size;
        }
        Ok(Response { ui: self, node })
    }

    /// A progress bar that runs with no end, for work of unknown length.
    pub fn progress_bar(&mut self, id: impl Hash) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let bar = ProgressBar::new()?;
            bar.SetIsIndeterminate(true)?;
            Ok(ControlNode::new(bar.cast()?, Control::ProgressBar))
        })?;
        assert!(
            matches!(self.control(node).control, Control::ProgressBar),
            "progress bar keys hold progress bars"
        );
        Ok(Response { ui: self, node })
    }

    /// A spinning ring `size` view pixels wide, for work of unknown length.
    pub fn progress_ring(&mut self, id: impl Hash, size: f64) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let ring = ProgressRing::new()?;
            ring.SetIsActive(true)?;
            ring.SetIsIndeterminate(true)?;
            let frame = ring.cast::<FrameworkElement>()?;
            frame.SetWidth(size)?;
            frame.SetHeight(size)?;
            Ok(ControlNode::new(ring.cast()?, Control::ProgressRing))
        })?;
        assert!(
            matches!(self.control(node).control, Control::ProgressRing),
            "progress ring keys hold progress rings"
        );
        Ok(Response { ui: self, node })
    }

    /// A small pill in the critical color with `glyph` and `label`, such
    /// as an Offline tag, and `tooltip` under the pointer.
    pub fn tag(
        &mut self,
        id: impl Hash,
        glyph: char,
        label: &str,
        tooltip: &str,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let panel = StackPanel::new()?;
            panel.SetOrientation(XamlOrientation::Horizontal)?;
            panel.SetSpacing(6.0)?;
            panel.SetPadding(Thickness {
                left: 10.0,
                top: 4.0,
                right: 12.0,
                bottom: 4.0,
            })?;
            panel.SetCornerRadius(CornerRadius {
                top_left: 12.0,
                top_right: 12.0,
                bottom_right: 12.0,
                bottom_left: 12.0,
            })?;
            crate::theme::set_brush(
                &panel,
                &Panel::BackgroundProperty()?,
                "SystemFillColorCriticalBackgroundBrush",
            )?;
            panel
                .cast::<FrameworkElement>()?
                .SetVerticalAlignment(VerticalAlignment::Center)?;
            let icon = FontIcon::new()?;
            icon.SetFontSize(12.0)?;
            crate::theme::set_brush(
                &icon,
                &crate::bindings::IconElement::ForegroundProperty()?,
                "SystemFillColorCriticalBrush",
            )?;
            let text = TextBlock::new()?;
            text.SetFontSize(12.0)?;
            text.cast::<FrameworkElement>()?
                .SetVerticalAlignment(VerticalAlignment::Center)?;
            let children = panel.cast::<Panel>()?.Children()?;
            children.Append(&icon)?;
            children.Append(&text)?;
            Ok(ControlNode::new(
                panel.cast()?,
                Control::Tag {
                    text,
                    icon,
                    label: String::new(),
                    glyph: None,
                    tooltip: String::new(),
                },
            ))
        })?;
        let ControlNode {
            element,
            control,
            stale,
            ..
        } = self.control_mut(node);
        let stale = std::mem::take(stale);
        let Control::Tag {
            text,
            icon,
            label: written_label,
            glyph: written_glyph,
            tooltip: written_tooltip,
        } = control
        else {
            unreachable!("tag keys hold tags")
        };
        if must_write(stale, written_label.as_str(), label) {
            text.SetText(label)?;
            written_label.clear();
            written_label.push_str(label);
        }
        if must_write(stale, written_glyph, &Some(glyph)) {
            icon.SetGlyph(glyph.encode_utf8(&mut [0; 4]) as &str)?;
            *written_glyph = Some(glyph);
        }
        if must_write(stale, written_tooltip.as_str(), tooltip) {
            ToolTipService::SetToolTip(&element.cast::<DependencyObject>()?, &boxed(tooltip))?;
            written_tooltip.clear();
            written_tooltip.push_str(tooltip);
        }
        Ok(Response { ui: self, node })
    }
}

/// What [`Ui::content_button`] returns.
pub struct ContentButtonResponse<'u>(Response<'u>);

impl ContentButtonResponse<'_> {
    /// Whether the button was clicked since the last pass. The first call
    /// subscribes it.
    pub fn clicked(&mut self) -> Result<bool> {
        self.0.button_clicked()
    }
}

impl<'u> Deref for ContentButtonResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for ContentButtonResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}
