use super::{Control, Response, must_write};
use crate::{
    bindings::{InfoBar, InfoBarSeverity},
    pass::{ChildKind, ControlNode, Ui},
};
use std::hash::Hash;
use windows_core::{Interface, Result};

impl Ui {
    /// An open message bar with a bold `title` and a `message`, which the
    /// user cannot close. To hide it, stop declaring it.
    pub fn info_bar(
        &mut self,
        id: impl Hash,
        severity: Severity,
        title: &str,
        message: &str,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let info_bar = InfoBar::new()?;
            info_bar.SetIsClosable(false)?;
            info_bar.SetIsOpen(true)?;
            Ok(ControlNode::new(
                info_bar.cast()?,
                Control::InfoBar {
                    info_bar,
                    severity: None,
                    title: String::new(),
                    message: String::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::InfoBar {
            info_bar,
            severity: written_severity,
            title: written_title,
            message: written_message,
        } = &mut control.control
        else {
            unreachable!("info bar keys hold info bars")
        };
        if must_write(stale, written_severity, &Some(severity)) {
            info_bar.SetSeverity(match severity {
                Severity::Informational => InfoBarSeverity::Informational,
                Severity::Success => InfoBarSeverity::Success,
                Severity::Warning => InfoBarSeverity::Warning,
                Severity::Error => InfoBarSeverity::Error,
            })?;
            *written_severity = Some(severity);
        }
        if must_write(stale, written_title.as_str(), title) {
            info_bar.SetTitle(title)?;
            written_title.clear();
            written_title.push_str(title);
        }
        if must_write(stale, written_message.as_str(), message) {
            info_bar.SetMessage(message)?;
            written_message.clear();
            written_message.push_str(message);
        }
        Ok(Response { ui: self, node })
    }
}

/// How an [`info_bar`](Ui::info_bar) looks: its icon and its ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Informational,
    Success,
    Warning,
    Error,
}
