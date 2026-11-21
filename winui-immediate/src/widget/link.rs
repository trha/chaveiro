use super::{Control, Response, must_write};
use crate::{
    bindings::{ContentControl, HyperlinkButton, TextBlock},
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A link-styled button showing `label`, for a secondary action such as
    /// "Send the code again".
    pub fn link(&mut self, id: impl Hash, label: &str) -> Result<LinkResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let link = HyperlinkButton::new()?;
            let content = TextBlock::new()?;
            link.cast::<ContentControl>()?.SetContent(&content)?;
            Ok(ControlNode::new(
                link.cast()?,
                Control::Link {
                    content,
                    label: String::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let element = control.element.clone();
        let mut subscribed = control.subscribed;
        self.events
            .subscribe(node, &mut subscribed, &element, EventKind::Click)?;
        let clicked = self.events.happened(node, EventKind::Click);
        let control = self.control_mut(node);
        control.subscribed = subscribed;
        let stale = std::mem::take(&mut control.stale);
        let Control::Link {
            content,
            label: written,
        } = &mut control.control
        else {
            unreachable!("link keys hold links")
        };
        if must_write(stale, written.as_str(), label) {
            content.SetText(label)?;
            written.clear();
            written.push_str(label);
        }
        Ok(LinkResponse {
            response: Response { ui: self, node },
            clicked,
        })
    }
}

/// What [`Ui::link`] returns.
pub struct LinkResponse<'u> {
    response: Response<'u>,
    clicked: bool,
}

impl LinkResponse<'_> {
    /// Whether the link was clicked since the last pass.
    pub fn clicked(&self) -> bool {
        self.clicked
    }
}

impl<'u> Deref for LinkResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for LinkResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
