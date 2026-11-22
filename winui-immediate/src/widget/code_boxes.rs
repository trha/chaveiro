use super::{ErrorLine, FieldHandle, mark_critical};
use crate::{
    bindings::{
        AutomationProperties, Control as XamlControl, DependencyObject, FrameworkElement,
        TextAlignment, TextBox,
    },
    events::Key,
    pass::Ui,
    widget::Orientation,
};
use std::{cell::RefCell, hash::Hash, rc::Rc};
use windows_core::{Interface, Result};

/// The number of boxes [`Ui::code_boxes`] shows.
pub const CODE_LENGTH: usize = 6;

/// The digits of a one-time code, one per box; [`None`] for an empty box.
pub type CodeDigits = [Option<u8>; CODE_LENGTH];

impl Ui {
    /// Six one-digit boxes for a one-time code, bound to `digits`, with
    /// `error` on the line under them.
    ///
    /// Typing a digit fills its box and moves focus to the next one, and
    /// Backspace in an empty box moves back. Left and Right move one box,
    /// Home and End to the first and last. Pasting several digits into
    /// any box fills that box and the ones after it. Other characters are
    /// dropped. While `error` is set every box has the critical border.
    /// Screen readers name each box "Digit n of 6".
    pub fn code_boxes(
        &mut self,
        id: impl Hash,
        digits: &mut CodeDigits,
        enabled: bool,
        error: Option<&str>,
    ) -> Result<CodeBoxes> {
        let mut changed = false;
        let mut enter = false;
        let mut fields = Vec::with_capacity(CODE_LENGTH);
        let mut focus = None;
        self.stack(id, Orientation::Vertical, 0.0, |ui| {
            ui.stack("boxes", Orientation::Horizontal, 8.0, |ui| {
                let mut typed = None;
                for (index, digit) in (*digits).into_iter().enumerate() {
                    let mut text: String = digit.map(|digit| digit.to_string()).unwrap_or_default();
                    let mut field = ui.text_box(index, "", &mut text)?;
                    let changed = field.changed();
                    if changed {
                        typed = Some((index, text.clone()));
                    }
                    enter |= field.enter_pressed()?;
                    let empty = !changed && text.is_empty();
                    for key in field.keys()? {
                        focus = step(index, key, empty).or(focus);
                    }
                    field.width(BOX_WIDTH)?.enabled(enabled)?;
                    fields.push(field.field());
                    // A node keeps its control, so only a new box needs these.
                    if field.created() {
                        let element = field.cast::<TextBox>()?;
                        element.SetTextAlignment(TextAlignment::Center)?;
                        ui.events.select_all_on_focus(&element)?;
                        let control = element.cast::<XamlControl>()?;
                        control.SetFontSize(20.0)?;
                        // The theme's minimum is wider than a digit box.
                        element.cast::<FrameworkElement>()?.SetMinWidth(0.0)?;
                        control.SetFontWeight(crate::bindings::FontWeight { weight: 600 })?;
                        AutomationProperties::SetName(
                            &element.cast::<DependencyObject>()?,
                            &format!("Digit {} of {CODE_LENGTH}", index + 1),
                        )?;
                    }
                }
                if let Some((index, text)) = typed {
                    let (merged, next) = merge(digits, index, &text);
                    *digits = merged;
                    focus = focus.or(next);
                    changed = true;
                }
                Ok(())
            })?;
            let slot = ui.state("error line", || None::<Rc<RefCell<ErrorLine>>>)?;
            if slot.is_none() {
                *slot = Some(Rc::new(RefCell::new(ErrorLine::new()?)));
            }
            // An `Rc` clone is a count, so it is cheap; the row is cast only
            // on the pass that makes its node.
            let line = slot.clone().expect("made above");
            ui.native("error", || line.borrow().row.cast(), |_| Ok(()))?;
            let mut line = line.borrow_mut();
            if line.message.as_deref() != error {
                line.message = error.map(str::to_owned);
                line.render()?;
                for &field in &fields {
                    mark_critical(&ui.control(field.0).element, error.is_some())?;
                }
            }
            Ok(())
        })?;
        let fields: [FieldHandle; CODE_LENGTH] =
            fields.try_into().expect("the loop declares every box");
        if let Some(index) = focus {
            self.focus(fields[index])?;
        }
        Ok(CodeBoxes {
            changed,
            enter,
            fields,
        })
    }
}

/// Six boxes and five 8px gaps fit the 312px a 360px card leaves.
const BOX_WIDTH: f64 = 44.0;

/// What [`Ui::code_boxes`] returns.
pub struct CodeBoxes {
    changed: bool,
    enter: bool,
    fields: [FieldHandle; CODE_LENGTH],
}

impl CodeBoxes {
    /// Whether the user changed a digit since the last pass.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Whether the user pressed Enter in a box since the last pass.
    pub fn enter_pressed(&self) -> bool {
        self.enter
    }

    /// The box at `index`, for [`Ui::focus`].
    pub fn field(&self, index: usize) -> FieldHandle {
        self.fields[index]
    }
}

/// The box to focus after `key` went down in box `index`, if focus moves.
/// `empty` says the key found the box empty and the key changed nothing.
///
/// Left and Right move one box, Home and End to the first and last, and
/// Backspace in an empty box moves back. Focus stays at either end.
fn step(index: usize, key: Key, empty: bool) -> Option<usize> {
    let target = match key {
        Key::Left => index.checked_sub(1)?,
        Key::Backspace if empty => index.checked_sub(1)?,
        Key::Right => index + 1,
        Key::Home => 0,
        Key::End => CODE_LENGTH - 1,
        _ => return None,
    };
    (target < CODE_LENGTH && target != index).then_some(target)
}

/// `digits` after the user changed box `index` so it holds `text`, and the
/// box to focus next, if focus moves.
///
/// Non-digits in `text` are dropped. No digit clears the box. One digit
/// fills it; so does a box that held a digit and gained one more, which is
/// a digit typed beside the old one. More digits fill the box and the ones
/// after it, as a paste does. Focus moves to the box after the last one
/// filled, and stays when that is past the end.
fn merge(digits: &CodeDigits, index: usize, text: &str) -> (CodeDigits, Option<usize>) {
    let typed: Vec<u8> = text
        .chars()
        .filter_map(|character| character.to_digit(10))
        .map(|digit| digit as u8)
        .collect();
    let mut merged = *digits;
    let fill: &[u8] = match (digits[index], typed.as_slice()) {
        (_, []) => {
            merged[index] = None;
            return (merged, None);
        }
        // Typed beside the old digit: keep the new one.
        (Some(old), [first, second]) if *first == old => &[*second],
        (Some(old), [first, second]) if *second == old => &[*first],
        (_, typed) => typed,
    };
    let count = fill.len().min(CODE_LENGTH - index);
    for (slot, &digit) in merged[index..index + count].iter_mut().zip(fill) {
        *slot = Some(digit);
    }
    let next = index + count;
    (merged, (next < CODE_LENGTH).then_some(next))
}

#[cfg(test)]
mod tests;
