use super::{Control, Response};
use crate::{
    bindings::ScrollViewer,
    pass::{ChildKind, ControlNode, Ui},
};
use std::hash::Hash;
use windows_core::{Interface, Result};

impl Ui {
    /// A vertical scroll area around the one control `body` declares, which
    /// gets the area's width and any height it wants. It scrolls only when
    /// its parent gives it a definite height, such as a
    /// [`Length::Star`](crate::Length::Star) grid row; in a stack it grows
    /// to fit and never scrolls.
    pub fn scroll(
        &mut self,
        id: impl Hash,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let scroll = ScrollViewer::new()?;
            Ok(ControlNode::new(
                scroll.cast()?,
                Control::Scroll { content: None },
            ))
        })?;
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(Response { ui: self, node })
    }
}
