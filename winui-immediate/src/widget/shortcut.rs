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
        let owner = self.enclosing_control();
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
            owner.KeyboardAccelerators()?.Append(&accelerator)?;
            owner.SetKeyboardAcceleratorPlacementMode(KeyboardAcceleratorPlacementMode::Hidden)?;
            Ok(Payload::Shortcut(ShortcutNode {
                accelerator,
                owner,
                subscribed: Default::default(),
            }))
        })?;
        let Payload::Shortcut(shortcut) = self.core.tree.native_mut(node) else {
            unreachable!("shortcut keys hold shortcuts")
        };
        let accelerator = shortcut.accelerator.clone();
        let mut subscribed = shortcut.subscribed;
        self.events
            .subscribe(node, &mut subscribed, &accelerator, EventKind::Invoked)?;
        let Payload::Shortcut(shortcut) = self.core.tree.native_mut(node) else {
            unreachable!("shortcut keys hold shortcuts")
        };
        shortcut.subscribed = subscribed;
        Ok(self.events.happened(node, EventKind::Invoked))
    }
}

/// Takes `shortcut`'s key off its control, whose node is being dropped.
pub(crate) fn release(shortcut: &ShortcutNode) {
    let removed = shortcut
        .owner
        .KeyboardAccelerators()
        .and_then(|accelerators| {
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
