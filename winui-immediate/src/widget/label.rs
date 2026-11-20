use super::{Control, Response, must_write};
use crate::{
    bindings::TextBlock,
    pass::{ChildKind, ControlNode, Payload, Ui},
};
use std::{fmt::Write as _, hash::Hash};
use windows_core::{Interface, Result};

impl Ui {
    /// A line of text. An unchanged label costs no allocation and no WinUI
    /// call.
    pub fn label(&mut self, id: impl Hash, text: std::fmt::Arguments<'_>) -> Result<Response<'_>> {
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
                },
            ))
        })?;
        let Payload::Control(control) = &mut self.tree.node_mut(node).payload else {
            unreachable!("label keys hold labels")
        };
        let stale = std::mem::take(&mut control.stale);
        let Control::Label { block, text } = &mut control.control else {
            unreachable!("label keys hold labels")
        };
        if must_write(stale, text.as_str(), self.scratch.as_str()) {
            block.SetText(&self.scratch)?;
            text.clear();
            text.push_str(&self.scratch);
        }
        Ok(Response { ui: self, node })
    }
}
