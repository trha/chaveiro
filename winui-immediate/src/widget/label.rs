use super::{Control, Response, must_write};
use crate::{
    bindings::{DependencyObject, FontFamily, FontStyle, TextBlock, TextTrimming, TextWrapping},
    pass::{ChildKind, ControlNode, NativeTree, Payload, Ui},
};
use std::{
    fmt::Write as _,
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A line of text. An unchanged label costs no allocation and no WinUI
    /// call.
    pub fn label(
        &mut self,
        id: impl Hash,
        text: std::fmt::Arguments<'_>,
    ) -> Result<LabelResponse<'_>> {
        self.scratch.clear();
        self.scratch
            .write_fmt(text)
            .expect("formatting into a String cannot fail");
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let block = TextBlock::new()?;
            Ok(ControlNode::new(
                block.cast()?,
                Control::Label {
                    block,
                    text: String::new(),
                    look: Look::default(),
                },
            ))
        })?;
        let Payload::Control(control) = self.core.tree.native_mut(node) else {
            unreachable!("label keys hold labels")
        };
        let stale = std::mem::take(&mut control.stale);
        let Control::Label { block, text, .. } = &mut control.control else {
            unreachable!("label keys hold labels")
        };
        if must_write(stale, text.as_str(), self.scratch.as_str()) {
            block.SetText(&self.scratch)?;
            text.clear();
            text.push_str(&self.scratch);
        }
        Ok(LabelResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::label`] returns. Each setter writes only when its value
/// changed, so a label that skips a setter keeps the value it last set.
pub struct LabelResponse<'u>(Response<'u>);

impl LabelResponse<'_> {
    /// What the label does with text longer than its width. The default is
    /// [`Overflow::Clip`].
    pub fn overflow(&mut self, overflow: Overflow) -> Result<&mut Self> {
        let (block, look) = self.parts();
        if look.overflow != overflow {
            let (wrapping, trimming) = match overflow {
                Overflow::Clip => (TextWrapping::NoWrap, TextTrimming::None),
                Overflow::Ellipsis => (TextWrapping::NoWrap, TextTrimming::CharacterEllipsis),
                Overflow::Wrap => (TextWrapping::Wrap, TextTrimming::None),
            };
            block.SetTextWrapping(wrapping)?;
            block.SetTextTrimming(trimming)?;
            look.overflow = overflow;
        }
        Ok(self)
    }

    /// Draws the text in a monospace font, for keys, card numbers and
    /// fingerprints, or in the theme's font.
    pub fn monospace(&mut self, on: bool) -> Result<&mut Self> {
        let (block, look) = self.parts();
        if look.monospace != on {
            match on {
                true => block.SetFontFamily(&FontFamily::CreateInstanceWithName(MONOSPACE)?)?,
                false => block
                    .cast::<DependencyObject>()?
                    .ClearValue(&TextBlock::FontFamilyProperty()?)?,
            }
            look.monospace = on;
        }
        Ok(self)
    }

    /// Draws the text in the theme's secondary text color, for captions
    /// and hints, or in the color it inherits.
    pub fn secondary(&mut self, on: bool) -> Result<&mut Self> {
        let (block, look) = self.parts();
        if look.secondary != on {
            let property = TextBlock::ForegroundProperty()?;
            match on {
                true => crate::theme::set_brush(block, &property, "TextFillColorSecondaryBrush")?,
                false => crate::theme::clear_brush(block, &property)?,
            }
            look.secondary = on;
        }
        Ok(self)
    }

    pub fn italic(&mut self, on: bool) -> Result<&mut Self> {
        let (block, look) = self.parts();
        if look.italic != on {
            block.SetFontStyle(match on {
                true => FontStyle::Italic,
                false => FontStyle::Normal,
            })?;
            look.italic = on;
        }
        Ok(self)
    }

    fn parts(&mut self) -> (&TextBlock, &mut Look) {
        let Control::Label { block, look, .. } = &mut self.0.ui.control_mut(self.0.node).control
        else {
            unreachable!("label keys hold labels")
        };
        (block, look)
    }
}

impl<'u> Deref for LabelResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for LabelResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}

/// What a [`Ui::label`] does with text longer than its width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    /// One line, cut at the edge.
    #[default]
    Clip,
    /// One line, ending in `…`.
    Ellipsis,
    /// As many lines as the text needs, keeping its line breaks.
    Wrap,
}

/// The look of a label as last written. The default is WinUI's, so a
/// label whose app never asks for a look gets no write.
#[derive(Default)]
pub(crate) struct Look {
    overflow: Overflow,
    monospace: bool,
    secondary: bool,
    italic: bool,
}

/// The fonts of [`LabelResponse::monospace`], from the VaultScreen design.
const MONOSPACE: &str = "Consolas, Cascadia Mono";
