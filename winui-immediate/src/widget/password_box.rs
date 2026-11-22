use super::{Control, Response, must_write};
use crate::{
    bindings::{IPasswordBox, PasswordBox},
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::NodeId,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{HSTRING, Interface, Result};
use zeroize::{Zeroize, Zeroizing};

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
    /// Reads the password and clears the box; see [`Ui::take_password`].
    pub fn take(&mut self) -> Result<Zeroizing<String>> {
        self.0.ui.take_password(self.handle())
    }

    /// A handle for [`Ui::take_password`], for when the button that submits
    /// the password is declared after the box.
    pub fn handle(&self) -> PasswordHandle {
        PasswordHandle(self.0.node)
    }

    /// Writes the text shown above the box.
    pub fn header(&mut self, text: &str) -> Result<&mut Self> {
        self.0.set_header(text)?;
        Ok(self)
    }

    /// Marks the field as required: ` *` after the header, and screen
    /// readers say so.
    pub fn required(&mut self, on: bool) -> Result<&mut Self> {
        self.0.set_required(on)?;
        Ok(self)
    }
    /// Shows `message` on the line under the box, or keeps the line empty;
    /// see [`Response`]'s error line. The first call reserves the line.
    pub fn error(&mut self, message: Option<&str>) -> Result<&mut Self> {
        self.0.set_error(message)?;
        Ok(self)
    }
    /// Whether the user changed the password since the last pass. The first
    /// call subscribes the box.
    pub fn changed(&mut self) -> Result<bool> {
        let Response { ui, node } = &mut self.0;
        ui.subscribe(*node, &[EventKind::PasswordChanged], |control| {
            &control.element
        })?;
        Ok(ui.events.happened(*node, EventKind::PasswordChanged))
    }

    /// Whether the box is empty. It reads the password and wipes the copy,
    /// as [`Ui::take_password`] does, but leaves the box as it is.
    pub fn is_empty(&self) -> Result<bool> {
        let Control::PasswordBox { password_box, .. } = &self.0.ui.control(self.0.node).control
        else {
            unreachable!("password box keys hold password boxes")
        };
        let password = password_box.cast::<IPasswordBox>()?.password_string()?;
        let empty = password.is_empty();
        wipe(password);
        Ok(empty)
    }

    /// Whether the user pressed Enter in the box since the last pass. The
    /// first call subscribes the box.
    pub fn enter_pressed(&mut self) -> Result<bool> {
        super::text_box::enter_pressed(&mut self.0)
    }
}

/// Names a password box for [`Ui::take_password`] during the pass that
/// declared it.
#[derive(Clone, Copy, Debug)]
pub struct PasswordHandle(NodeId);

impl PasswordHandle {
    /// The same box as a [`FieldHandle`](crate::FieldHandle).
    pub fn field(self) -> crate::FieldHandle {
        crate::FieldHandle(self.0)
    }
}

impl Ui {
    /// [`PasswordField::is_empty`] for the box `handle` names.
    pub fn password_is_empty(&mut self, handle: PasswordHandle) -> Result<bool> {
        PasswordField(Response {
            ui: self,
            node: handle.0,
        })
        .is_empty()
    }
}

impl Ui {
    /// Reads the password from the box `handle` names and clears the box.
    /// The returned string wipes itself on drop, and the copy WinUI handed
    /// over is wiped before this returns. The box's own buffer is out of
    /// reach; clearing the box is all this crate can do for it.
    ///
    /// Panics when the box is no longer declared, which only happens when the
    /// handle outlives the pass that made it.
    pub fn take_password(&mut self, handle: PasswordHandle) -> Result<Zeroizing<String>> {
        let Control::PasswordBox { password_box, .. } = &self.control(handle.0).control else {
            unreachable!("password handles name password boxes")
        };
        let password = password_box.cast::<IPasswordBox>()?.password_string()?;
        // Cleared first, so the box no longer holds the string wiped below.
        password_box.SetPassword("")?;
        let text = secret_from_wide(&password);
        wipe(password);
        Ok(text)
    }
}

/// Zeroes the buffer of `password`, a fresh `HSTRING` from the password
/// getter, then frees it.
fn wipe(password: HSTRING) {
    let wide: &[u16] = &password;
    // SAFETY: the getter returns a fresh HSTRING that the caller owned
    // alone, so nothing reads the buffer while it is zeroed and the HSTRING
    // drops right after. When the box still holds the same text, its own
    // buffer is a separate copy.
    let buffer = unsafe { std::slice::from_raw_parts_mut(wide.as_ptr().cast_mut(), wide.len()) };
    // Volatile writes, so the compiler cannot drop them as dead before the
    // free.
    buffer.zeroize();
    drop(password);
}

/// `wide` as UTF-8 in a string that wipes itself. The buffer is sized for
/// the worst case up front, so it never grows and leaves no unwiped copy. An
/// unpaired surrogate becomes U+FFFD.
pub(crate) fn secret_from_wide(wide: &[u16]) -> Zeroizing<String> {
    // One UTF-16 unit is at most 3 UTF-8 bytes; a surrogate pair is 2 units
    // and 4 bytes.
    let mut text = Zeroizing::new(String::with_capacity(wide.len() * 3));
    for character in char::decode_utf16(wide.iter().copied()) {
        text.push(character.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    text
}

impl<'u> Deref for PasswordField<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for PasswordField<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}

#[cfg(test)]
mod tests;
