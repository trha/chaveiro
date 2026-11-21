use super::{Control, Response, must_write};
use crate::{
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{MainThreadOnly, rc::Retained, sel};
use objc2_app_kit::NSPopUpButton;
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};

impl Ui {
    /// A pop-up button that picks one of `items`, bound to `selected`.
    ///
    /// The user's pick is copied into `selected`, and a pick runs a pass.
    /// The pass writes `selected` into the button only when the app changed
    /// it. An index past the end shows no selection.
    pub fn pop_up_button(
        &mut self,
        id: impl Hash,
        items: &[&str],
        selected: &mut usize,
    ) -> PopUpButtonResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let button = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
                false,
            );
            let view = Retained::clone(&button);
            ControlNode::field(
                mtm,
                &view,
                Control::PopUpButton {
                    button,
                    items: Vec::new(),
                    selected: -1,
                },
            )
        });
        if visit == Visit::Created {
            let Control::PopUpButton { button, .. } = &self.control(node).control else {
                unreachable!("pop-up button keys hold pop-up buttons")
            };
            self.events.register(button, node);
            // AppKit holds the target weakly; `Events` keeps it alive.
            unsafe {
                button.setTarget(Some(self.events.target()));
                button.setAction(Some(sel!(fire:)));
            }
        }
        let picked = self.events.happened(node, EventKind::Action);
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::PopUpButton {
            button,
            items: written_items,
            selected: written_selected,
        } = &mut control.control
        else {
            unreachable!("pop-up button keys hold pop-up buttons")
        };
        if stale
            || written_items
                .iter()
                .map(String::as_str)
                .ne(items.iter().copied())
        {
            button.removeAllItems();
            let titles: Vec<Retained<NSString>> =
                items.iter().map(|item| NSString::from_str(item)).collect();
            button.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
            written_items.clear();
            written_items.extend(items.iter().map(|item| (*item).to_owned()));
            // New items reset the selection, so it is written again below.
            *written_selected = -2;
        }
        let mut changed = false;
        if picked {
            let index = button.indexOfSelectedItem();
            if index != *written_selected {
                *written_selected = index;
                if let Ok(index) = usize::try_from(index) {
                    *selected = index;
                    changed = true;
                }
            }
        }
        let declared = isize::try_from(*selected)
            .ok()
            .filter(|index| (*index as usize) < items.len())
            .unwrap_or(-1);
        if must_write(stale, written_selected, &declared) {
            button.selectItemAtIndex(declared);
            *written_selected = declared;
        }
        PopUpButtonResponse {
            response: Response { ui: self, node },
            changed,
        }
    }
}

/// What [`Ui::pop_up_button`] returns.
pub struct PopUpButtonResponse<'u> {
    response: Response<'u>,
    changed: bool,
}

impl PopUpButtonResponse<'_> {
    /// Whether the user picked another item since the last pass; `selected`
    /// then holds its index.
    pub fn changed(&self) -> bool {
        self.changed
    }
}

impl<'u> Deref for PopUpButtonResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for PopUpButtonResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
