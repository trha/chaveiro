use super::{Control, Response, boxed, must_write};
use crate::{
    bindings::{ComboBox, FrameworkElement, HorizontalAlignment, ItemsControl, Selector},
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_collections::IVector;
use windows_core::{IInspectable, Interface, Result};

impl Ui {
    /// A drop-down list of `items`, bound to `selected`, as wide as its
    /// container allows.
    ///
    /// The user's pick is copied into `selected`, and a pick runs a pass.
    /// The pass writes `selected` into the control only when the app changed
    /// it. An index past the end shows no selection.
    ///
    /// `items` is read twice when it changed and once when it did not, so a
    /// pass can name labels it already holds, such as folder names, without
    /// collecting them: an unchanged list allocates nothing.
    pub fn combo_box<'a>(
        &mut self,
        id: impl Hash,
        items: impl IntoIterator<Item = &'a str, IntoIter: Clone>,
        selected: &mut usize,
    ) -> Result<ComboBoxResponse<'_>> {
        let items = items.into_iter();
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let combo_box = ComboBox::new()?;
            combo_box
                .cast::<FrameworkElement>()?
                .SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
            Ok(ControlNode::new(
                combo_box.cast()?,
                Control::ComboBox {
                    combo_box,
                    items: Vec::new(),
                    selected: -1,
                    text: None,
                },
            ))
        })?;
        self.subscribe(node, &[EventKind::SelectionChanged], combo_box_of)?;
        let picked = self.events.happened(node, EventKind::SelectionChanged);
        let control = self.control_mut(node);
        let stale = control.stale;
        let Control::ComboBox {
            combo_box,
            items: written_items,
            selected: written_selected,
            ..
        } = &mut control.control
        else {
            unreachable!("combo box keys hold combo boxes")
        };
        let combo_box = &*combo_box;
        // Cast only when read or written, because a cast is a COM call.
        let selector = || combo_box.cast::<Selector>();
        // Clearing the items clears the selection, which raises an echo that
        // the read below treats as no pick, since the index is written again
        // right after.
        let count = write_items(combo_box, stale, written_items, items, written_selected)?;
        let declared = i32::try_from(*selected)
            .ok()
            .filter(|&index| (index as usize) < count)
            .unwrap_or(-1);
        let mut changed = false;
        if picked {
            let current = selector()?.SelectedIndex()?;
            // An echo of this layer's own write reads back the stored index.
            if current != *written_selected {
                *written_selected = current;
                if let Ok(current) = usize::try_from(current) {
                    *selected = current;
                    changed = true;
                }
            }
        }
        if !changed && must_write(false, written_selected, &declared) {
            selector()?.SetSelectedIndex(declared)?;
            *written_selected = declared;
        }
        Ok(ComboBoxResponse {
            response: Response { ui: self, node },
            changed,
            picked: changed.then_some(*selected),
            closed: false,
        })
    }

    /// A combo box the user can also type into, bound to `text`, with
    /// `items` as suggestions, as wide as its container allows.
    ///
    /// Picking an item puts it in the box. Typed text reaches `text` when
    /// WinUI submits it: on Enter, or when focus leaves the box. The pass
    /// writes `text` into the box only when the app changed it.
    pub fn editable_combo_box(
        &mut self,
        id: impl Hash,
        items: &[&str],
        text: &mut String,
    ) -> Result<ComboBoxResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let combo_box = ComboBox::new()?;
            combo_box.SetIsEditable(true)?;
            combo_box
                .cast::<FrameworkElement>()?
                .SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
            Ok(ControlNode::new(
                combo_box.cast()?,
                Control::ComboBox {
                    combo_box,
                    items: Vec::new(),
                    selected: -1,
                    text: Some(String::new()),
                },
            ))
        })?;
        self.subscribe(
            node,
            &[
                EventKind::SelectionChanged,
                EventKind::TextSubmitted,
                EventKind::DropDownClosed,
            ],
            combo_box_of,
        )?;
        let selection_changed = self.events.happened(node, EventKind::SelectionChanged);
        let submitted = self.events.happened(node, EventKind::TextSubmitted);
        let closed = self.events.happened(node, EventKind::DropDownClosed);
        let control = self.control_mut(node);
        let stale = control.stale;
        let Control::ComboBox {
            combo_box,
            items: written_items,
            selected: written_selected,
            text: Some(written_text),
            ..
        } = &mut control.control
        else {
            unreachable!("editable combo box keys hold editable combo boxes")
        };
        let combo_box = &*combo_box;
        write_items(
            combo_box,
            stale,
            written_items,
            items.iter().copied(),
            written_selected,
        )?;
        let mut picked = None;
        if selection_changed {
            let current = combo_box.cast::<Selector>()?.SelectedIndex()?;
            if current != *written_selected {
                *written_selected = current;
                picked = usize::try_from(current).ok();
            }
        }
        let mut changed = false;
        if selection_changed || submitted {
            let current = combo_box.Text()?;
            // An echo of this layer's own write reads back the stored text.
            if current != *written_text {
                text.clone_from(&current);
                *written_text = current;
                changed = true;
            }
        }
        if !changed && must_write(stale, written_text.as_str(), text.as_str()) {
            // A selected item whose text differs would come back into the
            // box when focus leaves it, so the selection goes first.
            let selector = combo_box.cast::<Selector>()?;
            let selected = usize::try_from(selector.SelectedIndex()?).ok();
            if selected.and_then(|index| items.get(index)) != Some(&text.as_str()) {
                selector.SetSelectedIndex(-1)?;
                *written_selected = -1;
            }
            combo_box.SetText(text.as_str())?;
            written_text.clone_from(text);
        }
        Ok(ComboBoxResponse {
            response: Response { ui: self, node },
            changed,
            picked,
            closed,
        })
    }
}

fn combo_box_of(control: &ControlNode) -> &ComboBox {
    match &control.control {
        Control::ComboBox { combo_box, .. } => combo_box,
        _ => unreachable!("combo box keys hold combo boxes"),
    }
}

/// Writes `items` into `combo_box` when they differ from `written`, which
/// clears the selection. Returns how many items there are.
fn write_items<'a>(
    combo_box: &ComboBox,
    stale: bool,
    written: &mut Vec<String>,
    items: impl Clone + Iterator<Item = &'a str>,
    selected: &mut i32,
) -> Result<usize> {
    if !stale && written.iter().map(String::as_str).eq(items.clone()) {
        return Ok(written.len());
    }
    let list = combo_box
        .cast::<ItemsControl>()?
        .Items()?
        .cast::<IVector<IInspectable>>()?;
    list.Clear()?;
    written.clear();
    for item in items {
        list.Append(&boxed(item))?;
        written.push(item.to_owned());
    }
    *selected = -1;
    Ok(written.len())
}

/// What [`Ui::combo_box`] and [`Ui::editable_combo_box`] return.
pub struct ComboBoxResponse<'u> {
    response: Response<'u>,
    changed: bool,
    picked: Option<usize>,
    closed: bool,
}

impl ComboBoxResponse<'_> {
    /// Whether the user changed the value since the last pass: picked an
    /// item, or submitted typed text.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// The index of the item the user picked since the last pass, if any.
    pub fn picked(&self) -> Option<usize> {
        self.picked
    }

    /// Whether the list closed since the last pass, for an editable box.
    /// Focus moved while the list was open comes back to the box when it
    /// closes, so move focus after this.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// Shows `text` on the line under the box while it has no error, such
    /// as how to use it. An error replaces it.
    pub fn hint(&mut self, text: &str) -> Result<&mut Self> {
        self.response.set_hint(text)?;
        Ok(self)
    }

    /// Writes the text shown above the box.
    pub fn header(&mut self, text: &str) -> Result<&mut Self> {
        self.response.set_header(text)?;
        Ok(self)
    }

    /// Marks the field as required: ` *` after the header, and screen
    /// readers say so.
    pub fn required(&mut self, on: bool) -> Result<&mut Self> {
        self.response.set_required(on)?;
        Ok(self)
    }
    /// Shows `message` on the line under the box, or keeps the line empty;
    /// see [`Response`]'s error line. The first call reserves the line.
    pub fn error(&mut self, message: Option<&str>) -> Result<&mut Self> {
        self.response.set_error(message)?;
        Ok(self)
    }
}

impl<'u> Deref for ComboBoxResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for ComboBoxResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
