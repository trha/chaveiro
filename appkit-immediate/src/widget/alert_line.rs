use super::{Control, ErrorLine, Response};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::rc::Retained;
use std::hash::Hash;

impl Ui {
    /// An error that names no field, such as a refusal from the server: a
    /// red icon and `message`, wrapped to the container's width. With
    /// [`None`] the line is blank but keeps one line of height, so an error
    /// that fits on one line moves nothing and never resizes the window.
    /// In a horizontal stack it takes the width the other children leave.
    pub fn alert_line(&mut self, id: impl Hash, message: Option<&str>) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let line = ErrorLine::new(mtm, true);
            ControlNode::payload(
                Retained::clone(&line.row).into_super(),
                Control::AlertLine { line },
            )
        });
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::AlertLine { line } = &mut control.control else {
            unreachable!("alert line keys hold alert lines")
        };
        if stale || line.message.as_deref() != message {
            crate::write_check::record(node);
            line.message = message.map(str::to_owned);
            line.render();
        }
        Response { ui: self, node }
    }
}
