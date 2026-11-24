//! [`Ui::check_box`] rebuilt on [`Ui::native`], as an app outside the crate
//! would write it, with two events, to test that `native` can carry a real
//! widget.

use crate::{
    Ui,
    bindings::{CheckBox, ContentControl, IButtonBase, IUIElement, TextBlock, ToggleButton},
    events::Delegate,
};
use std::hash::Hash;
use windows_core::{Interface, Result};

/// The values last written into the box.
struct Written {
    content: TextBlock,
    label: String,
    checked: bool,
}

enum Event {
    Click,
    LostFocus,
}

/// As [`Ui::check_box`]; returns whether the user changed the box, and
/// whether focus left it.
#[expect(dead_code, reason = "a compile test of the native API")]
pub(crate) fn check_box(
    ui: &mut Ui,
    id: impl Hash,
    label: &str,
    checked: &mut bool,
) -> Result<(bool, bool)> {
    let mut native = ui.native(id, |sender| {
        let check_box = CheckBox::new()?;
        let content = TextBlock::new()?;
        check_box.cast::<ContentControl>()?.SetContent(&content)?;
        // An app writes `.Click(&RoutedEventHandler::new(...))` with these
        // closures; this crate's minimal bindings have no closure
        // delegates, so it uses its own.
        let click = sender.clone();
        Delegate::routed(Box::new(move || click.send(Event::Click)))
            .register(&check_box.cast::<IButtonBase>()?, |table| table.Click)?;
        let left = sender.clone();
        Delegate::routed(Box::new(move || left.send(Event::LostFocus)))
            .register(&check_box.cast::<IUIElement>()?, |table| table.LostFocus)?;
        let written = Written {
            content,
            label: String::new(),
            checked: false,
        };
        Ok((check_box.cast::<ToggleButton>()?, written))
    })?;
    let clicked = native.events().iter().any(|e| matches!(e, Event::Click));
    let left = native
        .events()
        .iter()
        .any(|e| matches!(e, Event::LostFocus));
    let stale = native.stale();
    let (toggle, written) = native.split();
    if stale || written.label != label {
        written.content.SetText(label)?;
        written.label.clear();
        written.label.push_str(label);
    }
    let mut changed = false;
    if clicked {
        let current = toggle.IsChecked()?;
        if current != written.checked {
            written.checked = current;
            *checked = current;
            changed = true;
        }
    } else if stale || written.checked != *checked {
        toggle.SetIsChecked(Some(*checked))?;
        written.checked = *checked;
    }
    Ok((changed, left))
}
