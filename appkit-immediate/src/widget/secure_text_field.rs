use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::{NodeId, Visit},
};
use objc2::{rc::Retained, runtime::ProtocolObject, sel};
use objc2_app_kit::NSSecureTextField;
use objc2_foundation::{NSRange, NSString};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};
use zeroize::{Zeroize, Zeroizing};

impl Ui {
    /// A password field. The password stays in the field: the app reads it
    /// once, with [`SecureTextFieldResponse::take`], and holds no copy while
    /// the user types. While the field has focus, AppKit turns on secure
    /// keyboard input.
    ///
    /// AppKit keeps copies this crate cannot wipe: the field's own string,
    /// and the window's field editor buffer while the field is edited.
    pub fn secure_text_field(
        &mut self,
        id: impl Hash,
        placeholder: &str,
    ) -> SecureTextFieldResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let field = NSSecureTextField::new(mtm);
            let view = Retained::clone(&field);
            ControlNode::field(
                mtm,
                &view,
                Control::SecureTextField {
                    field,
                    placeholder: String::new(),
                },
            )
        });
        if visit == Visit::Created {
            let Control::SecureTextField { field, .. } = &self.control(node).control else {
                unreachable!("secure text field keys hold secure text fields")
            };
            self.events.register(field, node);
            // AppKit holds the delegate and target weakly; `Events` keeps the
            // shared target alive for as long as the field exists.
            unsafe {
                field.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
                field.setTarget(Some(self.events.target()));
                field.setAction(Some(sel!(fire:)));
            }
        }
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::SecureTextField {
            field,
            placeholder: written,
        } = &mut control.control
        else {
            unreachable!("secure text field keys hold secure text fields")
        };
        if must_write(node, stale, written.as_str(), placeholder) {
            field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            written.clear();
            written.push_str(placeholder);
        }
        SecureTextFieldResponse(Response { ui: self, node })
    }

    /// Reads the password from the field `handle` names and clears the
    /// field. The returned string is the only copy this crate makes.
    ///
    /// Panics when the field is no longer declared, which only happens when
    /// the handle outlives the pass that made it.
    pub fn take_password(&mut self, handle: SecureTextFieldHandle) -> Zeroizing<String> {
        let Control::SecureTextField { field, .. } = &self.control(handle.0).control else {
            unreachable!("secure text field handles name secure text fields")
        };
        let password = field.stringValue();
        let mut units = Zeroizing::new(vec![0u16; password.length()]);
        unsafe {
            password.getCharacters_range(
                NonNull::new(units.as_mut_ptr()).expect("a vector's buffer is never null"),
                NSRange::new(0, units.len()),
            );
        }
        // Cleared before decoding, so the field no longer holds its text.
        field.setStringValue(&NSString::new());
        drop(password);
        let mut text = Zeroizing::new(String::with_capacity(units.len() * 3));
        for character in char::decode_utf16(units.iter().copied()) {
            text.push(character.unwrap_or(char::REPLACEMENT_CHARACTER));
        }
        units.zeroize();
        text
    }

    /// Whether the field `handle` names is empty, without copying its text
    /// out.
    pub fn password_is_empty(&self, handle: SecureTextFieldHandle) -> bool {
        let Control::SecureTextField { field, .. } = &self.control(handle.0).control else {
            unreachable!("secure text field handles name secure text fields")
        };
        field.stringValue().length() == 0
    }
}

/// What [`Ui::secure_text_field`] returns.
pub struct SecureTextFieldResponse<'u>(Response<'u>);

impl SecureTextFieldResponse<'_> {
    /// Reads the password and clears the field. The returned string is the
    /// only copy this crate makes.
    pub fn take(&mut self) -> Zeroizing<String> {
        let handle = self.handle();
        self.0.ui.take_password(handle)
    }

    /// A handle for [`Ui::take_password`], for when the button that submits
    /// the password is declared after the field.
    pub fn handle(&self) -> SecureTextFieldHandle {
        SecureTextFieldHandle(self.0.node)
    }

    /// Whether the user changed the password since the last pass.
    pub fn changed(&self) -> bool {
        self.0
            .ui
            .events
            .happened(self.0.node, EventKind::TextDidChange)
    }

    /// Whether the field is empty.
    pub fn is_empty(&self) -> bool {
        self.0.ui.password_is_empty(self.handle())
    }

    /// Whether the user pressed Return in the field since the last pass.
    pub fn return_pressed(&self) -> bool {
        self.0.ui.events.happened(self.0.node, EventKind::Action)
    }
}

/// Names a secure text field for [`Ui::take_password`] during the pass that
/// declared it.
#[derive(Clone, Copy, Debug)]
pub struct SecureTextFieldHandle(NodeId);

impl<'u> Deref for SecureTextFieldResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for SecureTextFieldResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}
