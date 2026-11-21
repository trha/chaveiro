use super::{Control, Response, must_write, resource};
use crate::{
    bindings::{Brush, FontIcon, IconElement},
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// One glyph of the Segoe Fluent Icons font, such as `'\u{E774}'` for
    /// a globe. It takes the text color of its parent until
    /// [`secondary`](FontIconResponse::secondary) changes it. Screen readers
    /// skip it; name the control it sits in instead.
    pub fn font_icon(&mut self, id: impl Hash, glyph: char) -> Result<FontIconResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let icon = FontIcon::new()?;
            Ok(ControlNode::new(
                icon.cast()?,
                Control::FontIcon {
                    icon,
                    glyph: None,
                    size: f64::NAN,
                    secondary: false,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::FontIcon {
            icon,
            glyph: written,
            ..
        } = &mut control.control
        else {
            unreachable!("font icon keys hold font icons")
        };
        if must_write(stale, written, &Some(glyph)) {
            icon.SetGlyph(glyph.encode_utf8(&mut [0; 4]) as &str)?;
            *written = Some(glyph);
        }
        Ok(FontIconResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::font_icon`] returns.
pub struct FontIconResponse<'u>(Response<'u>);

impl FontIconResponse<'_> {
    /// Sets the glyph's size in view pixels; WinUI's default is 20.
    pub fn size(&mut self, size: f64) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let Control::FontIcon {
            icon,
            size: written,
            ..
        } = &mut control.control
        else {
            unreachable!("font icon keys hold font icons")
        };
        if *written != size {
            icon.SetFontSize(size)?;
            *written = size;
        }
        Ok(self)
    }

    /// Draws the glyph in the secondary text color, as list rows draw their
    /// type icons, or in its parent's color again.
    pub fn secondary(&mut self, on: bool) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let Control::FontIcon {
            icon,
            secondary: written,
            ..
        } = &mut control.control
        else {
            unreachable!("font icon keys hold font icons")
        };
        if *written != on {
            let element = icon.cast::<IconElement>()?;
            match on {
                true => {
                    element.SetForeground(&resource::<Brush>("TextFillColorSecondaryBrush")?)?
                }
                false => element
                    .cast::<crate::bindings::DependencyObject>()?
                    .ClearValue(&IconElement::ForegroundProperty()?)?,
            }
            *written = on;
        }
        Ok(self)
    }
}

impl<'u> Deref for FontIconResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl DerefMut for FontIconResponse<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
