use super::{Control, Response, must_write};
use crate::{
    bindings::{
        AppBarButton, CommandBar, CommandBarDefaultLabelPosition, FontIcon, FrameworkElement,
        HorizontalAlignment, IconElement,
    },
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_collections::IVector;
use windows_core::{Interface, Result};

impl Ui {
    /// A WinUI `CommandBar` of the buttons `body` declares with
    /// [`app_bar_button`](Self::app_bar_button), in order, each with its
    /// label on the right of its icon, at the left of its container.
    /// `body` declares nothing else.
    pub fn command_bar(
        &mut self,
        id: impl Hash,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let bar = CommandBar::new()?;
            bar.SetDefaultLabelPosition(CommandBarDefaultLabelPosition::Right)?;
            // A bar stretched across its container puts its buttons at the
            // right end; the buttons lead here, as in a details pane.
            bar.cast::<FrameworkElement>()?
                .SetHorizontalAlignment(HorizontalAlignment::Left)?;
            let commands = bar.PrimaryCommands()?.cast::<IVector<_>>()?;
            Ok(ControlNode::new(
                bar.cast()?,
                Control::CommandBar { commands },
            ))
        })?;
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(Response { ui: self, node })
    }

    /// A button of the [`command_bar`](Self::command_bar) around it: the
    /// Segoe Fluent Icons `glyph` and `label`, which screen readers say.
    /// Declare it only inside a command bar.
    pub fn app_bar_button(
        &mut self,
        id: impl Hash,
        glyph: char,
        label: &str,
    ) -> Result<AppBarButtonResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let button = AppBarButton::new()?;
            let icon = FontIcon::new()?;
            button.SetIcon(&icon.cast::<IconElement>()?)?;
            Ok(ControlNode::new(
                button.cast()?,
                Control::AppBarButton {
                    button,
                    icon,
                    glyph: None,
                    label: String::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::AppBarButton {
            button,
            icon,
            glyph: written_glyph,
            label: written_label,
        } = &mut control.control
        else {
            unreachable!("app bar button keys hold app bar buttons")
        };
        if must_write(stale, written_glyph, &Some(glyph)) {
            icon.SetGlyph(glyph.encode_utf8(&mut [0; 4]) as &str)?;
            *written_glyph = Some(glyph);
        }
        if must_write(stale, written_label.as_str(), label) {
            button.SetLabel(label)?;
            written_label.clear();
            written_label.push_str(label);
        }
        Ok(AppBarButtonResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::app_bar_button`] returns.
pub struct AppBarButtonResponse<'u>(Response<'u>);

impl AppBarButtonResponse<'_> {
    /// Whether the button was clicked since the last pass. The first call
    /// subscribes it.
    pub fn clicked(&mut self) -> Result<bool> {
        self.0.button_clicked()
    }
}

impl<'u> Deref for AppBarButtonResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for AppBarButtonResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}
