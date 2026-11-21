use super::{Control, Response, must_write};
use crate::{
    bindings::{TitleBar, UIElement},
    events::EventKind,
    pass::{ChildKind, ControlNode, Payload, Ui},
};
use std::hash::Hash;
use windows_core::{Interface, Result};

impl Ui {
    /// The window's title bar: `title`, and a back button while `back` is
    /// true. It becomes the window's drag area, with the back button left
    /// clickable, so use it in a window made [`compact`] or
    /// [`extends_into_title_bar`], and declare at most one per window.
    ///
    /// [`compact`]: crate::WindowResponse::compact
    /// [`extends_into_title_bar`]: crate::WindowResponse::extends_into_title_bar
    pub fn title_bar(
        &mut self,
        id: impl Hash,
        title: &str,
        back: bool,
    ) -> Result<TitleBarResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let title_bar = TitleBar::new()?;
            // The bar itself is a tab stop, so it would take the window's
            // first focus from the content; only its back button should.
            title_bar.cast::<UIElement>()?.SetIsTabStop(false)?;
            Ok(ControlNode::new(
                title_bar.cast()?,
                Control::TitleBar {
                    title_bar,
                    title: String::new(),
                    back: None,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::TitleBar {
            title_bar,
            title: written_title,
            back: written_back,
        } = &mut control.control
        else {
            unreachable!("title bar keys hold title bars")
        };
        if must_write(stale, written_title.as_str(), title) {
            title_bar.SetTitle(title)?;
            written_title.clear();
            written_title.push_str(title);
        }
        if must_write(stale, written_back, &Some(back)) {
            title_bar.SetIsBackButtonVisible(back)?;
            title_bar.SetIsBackButtonEnabled(back)?;
            *written_back = Some(back);
        }
        let element = control.element.clone();
        let window = self.enclosing_window();
        let Payload::Window(window) = &mut self.tree.node_mut(window).payload else {
            unreachable!("enclosing_window returns a window")
        };
        if window.title_bar != Some(node) {
            window.window.SetTitleBar(&element)?;
            window.title_bar = Some(node);
        }
        Ok(TitleBarResponse(Response { ui: self, node }))
    }
}

/// What [`Ui::title_bar`] returns.
pub struct TitleBarResponse<'u>(Response<'u>);

impl TitleBarResponse<'_> {
    /// Whether the user pressed the back button since the last pass. The
    /// first call subscribes it.
    pub fn back_requested(&mut self) -> Result<bool> {
        let Response { ui, node } = &mut self.0;
        let control = ui.control_mut(*node);
        let element = control.element.clone();
        let mut subscribed = control.subscribed;
        ui.events
            .subscribe(*node, &mut subscribed, &element, EventKind::BackRequested)?;
        ui.control_mut(*node).subscribed = subscribed;
        Ok(ui.events.happened(*node, EventKind::BackRequested))
    }
}

impl<'u> std::ops::Deref for TitleBarResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl std::ops::DerefMut for TitleBarResponse<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
