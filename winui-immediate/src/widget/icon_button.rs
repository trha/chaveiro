use super::{Control, Response, boxed, make_subtle, must_write};
use crate::{
    bindings::{
        AutomationProperties, Button, ContentControl, Control as XamlControl, DependencyObject,
        FontIcon, Thickness, ToolTipService,
    },
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A subtle button showing only `glyph`, a Segoe Fluent Icons glyph,
    /// such as the Copy or Show button of a field. `label` is its tooltip
    /// and the name screen readers say, such as "Copy password".
    pub fn icon_button(
        &mut self,
        id: impl Hash,
        glyph: char,
        label: &str,
    ) -> Result<IconButtonResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let button = Button::new()?;
            make_subtle(&button)?;
            button.cast::<XamlControl>()?.SetPadding(Thickness {
                left: 8.0,
                top: 8.0,
                right: 8.0,
                bottom: 8.0,
            })?;
            let icon = FontIcon::new()?;
            icon.SetFontSize(16.0)?;
            button.cast::<ContentControl>()?.SetContent(&icon)?;
            Ok(ControlNode::new(
                button.cast()?,
                Control::IconButton {
                    button,
                    icon,
                    glyph: None,
                    label: String::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::IconButton {
            button,
            icon,
            glyph: written_glyph,
            label: written_label,
        } = &mut control.control
        else {
            unreachable!("icon button keys hold icon buttons")
        };
        if must_write(stale, written_glyph, &Some(glyph)) {
            icon.SetGlyph(glyph.encode_utf8(&mut [0; 4]) as &str)?;
            *written_glyph = Some(glyph);
        }
        if must_write(stale, written_label.as_str(), label) {
            let target = button.cast::<DependencyObject>()?;
            AutomationProperties::SetName(&target, label)?;
            ToolTipService::SetToolTip(&target, &boxed(label))?;
            written_label.clear();
            written_label.push_str(label);
        }
        Ok(IconButtonResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::icon_button`] returns.
pub struct IconButtonResponse<'u>(Response<'u>);

impl IconButtonResponse<'_> {
    /// Whether the button was clicked since the last pass. The first call
    /// subscribes it.
    pub fn clicked(&mut self) -> Result<bool> {
        self.0.button_clicked()
    }
}

impl<'u> Deref for IconButtonResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for IconButtonResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}
