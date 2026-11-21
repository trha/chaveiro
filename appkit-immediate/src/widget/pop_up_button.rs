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
                    enabled_items: Vec::new(),
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
            enabled_items,
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
            crate::write_check::record(node);
            button.removeAllItems();
            let titles: Vec<Retained<NSString>> =
                items.iter().map(|item| NSString::from_str(item)).collect();
            button.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
            written_items.clear();
            written_items.extend(items.iter().map(|item| (*item).to_owned()));
            // New items reset the selection and enable every item, so both
            // are written again.
            *written_selected = -2;
            enabled_items.clear();
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
        if must_write(node, stale, written_selected, &declared) {
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

    /// Sets which items the user can pick, one flag per item in order. A
    /// disabled item still shows, greyed out. Missing flags count as
    /// enabled.
    pub fn enabled_items(&mut self, enabled: &[bool]) -> &mut Self {
        let Response { ui, node } = &mut self.response;
        let node = *node;
        let control = ui.control_mut(node);
        let Control::PopUpButton {
            button,
            items,
            enabled_items,
            ..
        } = &mut control.control
        else {
            unreachable!("pop-up button keys hold pop-up buttons")
        };
        // Missing flags count as enabled.
        let declared = (0..items.len()).map(|index| enabled.get(index).copied().unwrap_or(true));
        let same = enabled_items.iter().copied().eq(declared.clone());
        if !must_write(node, false, &same, &true) {
            return self;
        }
        // AppKit would enable every item again on each menu update.
        button.setAutoenablesItems(false);
        enabled_items.clear();
        for (index, on) in declared.enumerate() {
            let item = button
                .itemAtIndex(index as isize)
                .expect("the button holds every written item");
            item.setEnabled(on);
            enabled_items.push(on);
        }
        self
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
