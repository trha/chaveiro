use super::{Control, Response, must_write};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::rc::Retained;
use objc2_app_kit::NSTextField;
use objc2_foundation::NSString;
use std::{fmt::Write as _, hash::Hash};

impl Ui {
    /// A line of text. An unchanged label costs no allocation and no AppKit
    /// call.
    pub fn label(&mut self, id: impl Hash, text: std::fmt::Arguments<'_>) -> Response<'_> {
        self.scratch.clear();
        self.scratch
            .write_fmt(text)
            .expect("formatting into a String cannot fail");
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let field = NSTextField::labelWithString(&NSString::new(), mtm);
            ControlNode::payload(
                Retained::clone(&field).into_super().into_super(),
                Control::Label {
                    field,
                    text: String::new(),
                },
            )
        });
        let Ui { tree, scratch, .. } = self;
        let crate::pass::Payload::Control(control) = &mut tree.node_mut(node).payload else {
            unreachable!("label keys hold labels")
        };
        let stale = std::mem::take(&mut control.stale);
        let Control::Label { field, text } = &mut control.control else {
            unreachable!("label keys hold labels")
        };
        if must_write(stale, text, scratch) {
            field.setStringValue(&NSString::from_str(scratch));
            text.clone_from(scratch);
        }
        Response { ui: self, node }
    }
}
