use crate::{
    bindings::{
        KeyboardAccelerator, KeyboardAcceleratorPlacementMode, VirtualKey, VirtualKeyModifiers,
    },
    events::EventKind,
    pass::{ChildKind, NativeTree, Payload, ShortcutNode, Ui},
};
use std::hash::Hash;
use windows_core::Result;

impl Ui {
    /// A key that works anywhere in the window while this is declared: Ctrl
    /// and `letter`, with Shift too when `shift`. Gives whether the user
    /// pressed it since the last pass. A focused text box gets the key
    /// first, so it keeps its own Ctrl keys, such as Ctrl+C. No tooltip
    /// shows it, so name it in the control it acts for, such as a menu item.
    ///
    /// Declare it inside a control, which holds the key. Panics when
    /// `letter` is not an ASCII letter, or with no control open.
    pub fn shortcut(&mut self, id: impl Hash, shift: bool, letter: char) -> Result<bool> {
        assert!(
            letter.is_ascii_alphabetic(),
            "a shortcut is Ctrl and a letter"
        );
        let (node, _) = self.visit(id, ChildKind::Logical, || {
            let accelerator = KeyboardAccelerator::new()?;
            // A virtual key code of a letter is its uppercase ASCII code.
            accelerator.SetKey(VirtualKey(letter.to_ascii_uppercase() as i32))?;
            let modifiers = match shift {
                true => VirtualKeyModifiers(
                    VirtualKeyModifiers::Control.0 | VirtualKeyModifiers::Shift.0,
                ),
                false => VirtualKeyModifiers::Control,
            };
            accelerator.SetModifiers(modifiers)?;
            Ok(Payload::Shortcut(ShortcutNode {
                accelerator,
                owner: None,
                subscribed: Default::default(),
            }))
        })?;
        // The owner is found here, not in `visit`'s create, because that
        // cannot read the tree, and finding it every pass would clone its
        // element. An attach that failed is tried again on the next pass.
        let attached = match self.core.tree.native(node) {
            Payload::Shortcut(shortcut) => shortcut.owner.is_some(),
            _ => unreachable!("shortcut keys hold shortcuts"),
        };
        if !attached {
            let owner = self.enclosing_control();
            let Payload::Shortcut(shortcut) = self.core.tree.native_mut(node) else {
                unreachable!("shortcut keys hold shortcuts")
            };
            owner
                .KeyboardAccelerators()?
                .Append(&shortcut.accelerator)?;
            owner.SetKeyboardAcceleratorPlacementMode(KeyboardAcceleratorPlacementMode::Hidden)?;
            shortcut.owner = Some(owner);
        }
        self.subscribe_native(node, &[EventKind::Invoked], |payload| match payload {
            Payload::Shortcut(shortcut) => (&shortcut.accelerator, &mut shortcut.subscribed),
            _ => unreachable!("shortcut keys hold shortcuts"),
        })?;
        Ok(self.events.happened(node, EventKind::Invoked))
    }
}

/// Takes `shortcut`'s key off its control, whose node is being dropped.
pub(crate) fn release(shortcut: &ShortcutNode) {
    // A key never attached is on no control.
    let Some(owner) = &shortcut.owner else { return };
    let removed = owner.KeyboardAccelerators().and_then(|accelerators| {
        let mut index = 0;
        if accelerators.IndexOf(&shortcut.accelerator, &mut index)? {
            accelerators.RemoveAt(index)?;
        }
        Ok(())
    });
    if let Err(error) = removed {
        tracing::warn!(%error, "a dropped shortcut still works");
    }
}
