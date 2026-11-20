use super::{Control, Response, must_write};
use crate::{
    bindings::PasswordBox,
    pass::{ChildKind, ControlNode, Ui},
    tree::NodeId,
};
use std::{hash::Hash, ops::Deref};
use windows_core::{Interface, Result};

impl Ui {
    /// A password box. The password stays in the control: the app reads it
    /// once, with [`PasswordField::take`], and holds no copy while the user
    /// types. The box registers no event handler.
    pub fn password_box(&mut self, id: impl Hash, placeholder: &str) -> Result<PasswordField<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let password_box = PasswordBox::new()?;
            Ok(ControlNode::new(
                password_box.cast()?,
                Control::PasswordBox {
                    password_box,
                    placeholder: String::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::PasswordBox {
            password_box,
            placeholder: written,
        } = &mut control.control
        else {
            unreachable!("password box keys hold password boxes")
        };
        if must_write(stale, written.as_str(), placeholder) {
            password_box.SetPlaceholderText(placeholder)?;
            written.clear();
            written.push_str(placeholder);
        }
        Ok(PasswordField(Response { ui: self, node }))
    }
}

/// What [`Ui::password_box`] returns.
pub struct PasswordField<'u>(Response<'u>);

impl PasswordField<'_> {
    /// Reads the password and clears the box. The returned string is the
    /// only copy this crate makes.
    pub fn take(&mut self) -> Result<String> {
        self.0.ui.take_password(self.handle())
    }

    /// A handle for [`Ui::take_password`], for when the button that submits
    /// the password is declared after the box.
    pub fn handle(&self) -> PasswordHandle {
        PasswordHandle(self.0.node)
    }
}

/// Names a password box for [`Ui::take_password`] during the pass that
/// declared it.
#[derive(Clone, Copy, Debug)]
pub struct PasswordHandle(NodeId);

impl Ui {
    /// Reads the password from the box `handle` names and clears the box.
    /// The returned string is the only copy this crate makes.
    ///
    /// Panics when the box is no longer declared, which only happens when the
    /// handle outlives the pass that made it.
    pub fn take_password(&mut self, handle: PasswordHandle) -> Result<String> {
        let Control::PasswordBox { password_box, .. } = &self.control(handle.0).control else {
            unreachable!("password handles name password boxes")
        };
        let password = password_box.Password()?;
        password_box.SetPassword("")?;
        Ok(password)
    }
}

impl<'u> Deref for PasswordField<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}
