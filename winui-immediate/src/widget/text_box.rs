use super::{Control, Response, must_write};
use crate::{
    bindings::{FrameworkElement, ITextBox, KeyRoutedEventArgs, TextBox, TextWrapping, VirtualKey},
    events::Delegate,
    events::{EventKind, Key},
    pass::{ChildKind, ControlNode, Ui},
    tree::NodeId,
    tree::Visit,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

impl Ui {
    /// A one-line text box bound to `value`.
    ///
    /// The control owns its text while the user edits: after the user types,
    /// the pass copies the control's text into `value`. The pass writes
    /// `value` into the control only when the app changed it since, and never
    /// while an IME composition is open, so the caret, the selection and the
    /// composition survive every pass. A write held back by a composition
    /// happens on the pass after it ends.
    pub fn text_box(
        &mut self,
        id: impl Hash,
        placeholder: &str,
        value: &mut String,
    ) -> Result<TextBoxResponse<'_>> {
        let capacity = value.capacity();
        let (node, visit) = self.visit(id, ChildKind::Visual, || {
            let text_box = TextBox::new()?;
            Ok(ControlNode::new(
                text_box.cast()?,
                Control::TextBox {
                    text_box,
                    // As roomy as the app's buffer, so the layer's copy
                    // does not grow on a keystroke either.
                    text: String::with_capacity(capacity),
                    placeholder: String::new(),
                    composing: false,
                },
            ))
        })?;
        let changed = self.bind_text(node, placeholder, value)?;
        Ok(TextBoxResponse {
            response: Response { ui: self, node },
            changed,
            created: visit == Visit::Created,
        })
    }

    /// A text box for several lines, such as a note: Enter starts a new
    /// line, so [`TextBoxResponse::enter_pressed`] never fires, and long
    /// lines wrap. Otherwise it binds `value` as [`text_box`](Self::text_box)
    /// does.
    pub fn text_area(
        &mut self,
        id: impl Hash,
        placeholder: &str,
        value: &mut String,
    ) -> Result<TextBoxResponse<'_>> {
        let capacity = value.capacity();
        let (node, visit) = self.visit(id, ChildKind::Visual, || {
            let text_box = TextBox::new()?;
            text_box.SetAcceptsReturn(true)?;
            text_box.SetTextWrapping(TextWrapping::Wrap)?;
            text_box.cast::<FrameworkElement>()?.SetMinHeight(80.0)?;
            Ok(ControlNode::new(
                text_box.cast()?,
                Control::TextBox {
                    text_box,
                    // As roomy as the app's buffer, so the layer's copy
                    // does not grow on a keystroke either.
                    text: String::with_capacity(capacity),
                    placeholder: String::new(),
                    composing: false,
                },
            ))
        })?;
        let changed = self.bind_text(node, placeholder, value)?;
        Ok(TextBoxResponse {
            response: Response { ui: self, node },
            changed,
            created: visit == Visit::Created,
        })
    }

    /// Syncs the text box of `node`, a [`Control::TextBox`] or
    /// [`Control::SearchBox`], with `placeholder` and `value`, as
    /// [`Ui::text_box`] describes. Returns whether the user changed the
    /// text.
    pub(crate) fn bind_text(
        &mut self,
        node: NodeId,
        placeholder: &str,
        value: &mut String,
    ) -> Result<bool> {
        self.subscribe(
            node,
            &[
                EventKind::TextChanged,
                EventKind::CompositionStarted,
                EventKind::CompositionEnded,
            ],
            |control| match &control.control {
                Control::TextBox { text_box, .. } | Control::SearchBox { text_box, .. } => text_box,
                _ => unreachable!("text keys hold text boxes"),
            },
        )?;
        let typed = self.events.happened(node, EventKind::TextChanged);
        let composition = self.events.last_of(
            node,
            EventKind::CompositionStarted,
            EventKind::CompositionEnded,
        );
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let (Control::TextBox {
            text_box,
            text,
            placeholder: written_placeholder,
            composing,
            ..
        }
        | Control::SearchBox {
            text_box,
            text,
            placeholder: written_placeholder,
            composing,
            ..
        }) = &mut control.control
        else {
            unreachable!("text keys hold text boxes")
        };
        if let Some(kind) = composition {
            *composing = kind == EventKind::CompositionStarted;
        }
        if must_write(stale, written_placeholder.as_str(), placeholder) {
            text_box.SetPlaceholderText(placeholder)?;
            written_placeholder.clear();
            written_placeholder.push_str(placeholder);
        }
        let mut changed = false;
        if typed {
            let current = text_box.cast::<ITextBox>()?.text_string()?;
            // An echo of this layer's own write reads back the stored text.
            // The compare and the copies reuse the strings' buffers, so a
            // keystroke allocates only when the text outgrows them.
            let units = || char::decode_utf16(current.iter().copied());
            let same = units()
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .eq(text.chars());
            if !same {
                for target in [&mut *value, &mut *text] {
                    target.clear();
                    target.extend(units().map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)));
                }
                changed = true;
            }
        } else if !*composing && must_write(stale, text.as_str(), value.as_str()) {
            text_box.SetText(value)?;
            text.clone_from(value);
        }
        Ok(changed)
    }
}

/// A pattern of digit groups for [`TextBoxResponse::mask`]: the box keeps
/// only digits, at most the groups' total, and puts `separator` between
/// full groups as the user types.
///
/// Contract: the groups' total plus one separator between each pair of
/// groups fits in [`MASK_LENGTH`] bytes, which [`Mask::new`] checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mask {
    groups: &'static [u8],
    separator: u8,
}

/// The most bytes a masked text can take.
pub(crate) const MASK_LENGTH: usize = 64;

impl Mask {
    /// A card number: up to 19 digits in groups of 4, split by spaces.
    pub const CARD_NUMBER: Mask = Mask::new(&[4, 4, 4, 4, 3], b' ');
    /// A month and two-digit year: `MM/YY`.
    pub const MONTH_YEAR: Mask = Mask::new(&[2, 2], b'/');

    /// Groups of `groups` digits, split by the ASCII `separator`. Panics,
    /// at compile time for a constant, when the text cannot fit
    /// [`MASK_LENGTH`].
    pub const fn new(groups: &'static [u8], separator: u8) -> Mask {
        assert!(separator.is_ascii(), "the separator is one ASCII byte");
        let mut length = 0;
        let mut index = 0;
        while index < groups.len() {
            length += groups[index] as usize + (index > 0) as usize;
            index += 1;
        }
        assert!(length <= MASK_LENGTH, "the masked text fits MASK_LENGTH");
        Mask { groups, separator }
    }

    /// The handlers that keep a box to this mask: on `TextChanging`, and on
    /// `PreviewKeyDown`, so Backspace and Delete can step over a separator
    /// before the box acts on them.
    pub(crate) fn delegates(self) -> [Delegate; 2] {
        // Only text boxes register these, so a sender that is no text box
        // cannot come; the cast just keeps the hook safe.
        [
            Delegate::text_changing(Box::new(move |text_box| self.apply(text_box))),
            Delegate::preview_key_down(Box::new(move |sender, key| {
                self.step_over_separator(&sender.cast::<TextBox>()?, key)
            })),
        ]
    }

    /// Rewrites the text of `text_box` to this mask, keeping the caret after
    /// the same digit. Writing the text raises `TextChanging` again, which
    /// then finds it formatted and stops.
    fn apply(self, text_box: &TextBox) -> Result<()> {
        let text = text_box.cast::<ITextBox>()?.text_string()?;
        let caret = usize::try_from(text_box.SelectionStart()?).unwrap_or(0);
        let mut buffer = [0; MASK_LENGTH];
        let (formatted, caret) = self.format(&text, caret, &mut buffer);
        if !formatted.bytes().map(u16::from).eq(text.iter().copied()) {
            text_box.SetText(formatted)?;
            text_box.SetSelectionStart(caret as i32)?;
        }
        Ok(())
    }

    /// Before Backspace or Delete acts next to a separator, moves the caret
    /// past it, so the key deletes the digit beyond. Deleting only the
    /// separator would do nothing, because the mask puts it back.
    fn step_over_separator(self, text_box: &TextBox, key: &KeyRoutedEventArgs) -> Result<()> {
        // The offset of the character the key deletes, from the caret.
        let offset = match key.Key()? {
            VirtualKey::Back => -1,
            VirtualKey::Delete => 0,
            _ => return Ok(()),
        };
        if text_box.SelectionLength()? != 0 {
            return Ok(());
        }
        let caret = text_box.SelectionStart()?;
        let text = text_box.cast::<ITextBox>()?.text_string()?;
        let deleted = usize::try_from(caret + offset)
            .ok()
            .and_then(|index| text.get(index));
        if deleted == Some(&u16::from(self.separator)) {
            // Backspace deletes before the caret and Delete after it, so
            // each moves by one in its own direction.
            text_box.SetSelectionStart(caret + if offset < 0 { -1 } else { 1 })?;
        }
        Ok(())
    }

    /// `text` with only its digits, cut to the mask's total, in groups,
    /// written into `buffer`; and where the caret goes so it stays after
    /// the same digit it followed at `caret`, a UTF-16 offset into `text`.
    pub(crate) fn format<'b>(
        self,
        text: &[u16],
        caret: usize,
        buffer: &'b mut [u8; MASK_LENGTH],
    ) -> (&'b str, usize) {
        let is_digit = |unit: &u16| (u16::from(b'0')..=u16::from(b'9')).contains(unit);
        let total: usize = self.groups.iter().map(|&size| usize::from(size)).sum();
        // Digits past the total are dropped, so a caret after them goes to
        // the end.
        let before = text[..caret.min(text.len())]
            .iter()
            .filter(|unit| is_digit(unit))
            .count()
            .min(total);
        let mut length = 0;
        let mut new_caret = 0;
        let mut boundaries = self.groups.iter().scan(0, |end, &size| {
            *end += usize::from(size);
            Some(*end)
        });
        let mut boundary = boundaries.next();
        for (written, &unit) in text.iter().filter(|unit| is_digit(unit)).take(total).enumerate() {
            if Some(written) == boundary && written > 0 {
                buffer[length] = self.separator;
                length += 1;
                boundary = boundaries.next();
            }
            // A digit: the filter above keeps only ASCII digits.
            buffer[length] = unit as u8;
            length += 1;
            if written + 1 == before {
                new_caret = length;
            }
        }
        let formatted = std::str::from_utf8(&buffer[..length]).expect("digits and an ASCII separator");
        (formatted, new_caret)
    }
}

/// What [`Ui::text_box`] returns.
pub struct TextBoxResponse<'u> {
    response: Response<'u>,
    changed: bool,
    created: bool,
}

impl TextBoxResponse<'_> {
    /// Whether this pass made the box.
    pub(crate) fn created(&self) -> bool {
        self.created
    }

    /// Whether the user changed the text since the last pass; `value` then
    /// holds the new text.
    pub fn changed(&self) -> bool {
        self.changed
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

    /// Whether the user pressed Enter in the box since the last pass. The
    /// first call subscribes the box.
    pub fn enter_pressed(&mut self) -> Result<bool> {
        enter_pressed(&mut self.response)
    }

    /// Formats the box's text to `mask` as the user types or pastes, before
    /// the box draws it, so `value` holds the formatted text. Only the pass
    /// that makes the box sets it; a later mask is ignored.
    pub fn mask(&mut self, mask: Mask) -> Result<&mut Self> {
        if self.created {
            let Response { ui, node } = &mut self.response;
            let Control::TextBox { text_box, .. } = &ui.control(*node).control else {
                unreachable!("text box responses hold text boxes")
            };
            let text_box = text_box.clone();
            ui.events.mask(&text_box, mask)?;
        }
        Ok(self)
    }

    /// The [`Key`]s the user pressed in the box since the last pass, in
    /// order, seen before the box acted on them: a Backspace that deleted
    /// nothing is here too. The first call subscribes the box, so its first
    /// result is empty.
    pub fn keys(&mut self) -> Result<impl Iterator<Item = Key> + '_> {
        let Response { ui, node } = &mut self.response;
        ui.subscribe(*node, &[EventKind::Key], |control| &control.element)?;
        Ok(ui.events.keys(*node))
    }
}

/// Subscribes the control of `response` to Enter and reads it.
pub(crate) fn enter_pressed(response: &mut Response<'_>) -> Result<bool> {
    let Response { ui, node } = response;
    ui.subscribe(*node, &[EventKind::Enter], |control| &control.element)?;
    Ok(ui.events.happened(*node, EventKind::Enter))
}

impl<'u> Deref for TextBoxResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for TextBoxResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}

#[cfg(test)]
mod tests {
    use super::{MASK_LENGTH, Mask};
    use proptest::prelude::*;

    fn format(mask: Mask, text: &str, caret: usize) -> (String, usize) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut buffer = [0; MASK_LENGTH];
        let (formatted, caret) = mask.format(&wide, caret, &mut buffer);
        (formatted.to_owned(), caret)
    }

    fn masks() -> impl Strategy<Value = Mask> {
        prop_oneof![Just(Mask::CARD_NUMBER), Just(Mask::MONTH_YEAR)]
    }

    /// Typed text: digits mixed with spaces, slashes, letters and others.
    fn typed() -> impl Strategy<Value = String> {
        "[0-9 /a-zé.-]{0,40}"
    }

    proptest! {
        #[test]
        fn keeps_the_typed_digits_in_order_up_to_the_total(mask in masks(), text in typed()) {
            let total: usize = mask.groups.iter().map(|&size| usize::from(size)).sum();
            let (formatted, _) = format(mask, &text, 0);
            let digits: String = text.chars().filter(char::is_ascii_digit).take(total).collect();
            let kept: String = formatted.chars().filter(char::is_ascii_digit).collect();
            prop_assert_eq!(kept, digits);
        }

        #[test]
        fn separators_split_full_groups_only(mask in masks(), text in typed()) {
            let (formatted, _) = format(mask, &text, 0);
            if formatted.is_empty() {
                return Ok(());
            }
            let parts: Vec<&str> = formatted.split(char::from(mask.separator)).collect();
            prop_assert!(parts.len() <= mask.groups.len());
            for (index, part) in parts.iter().enumerate() {
                let size = usize::from(mask.groups[index]);
                prop_assert!(part.chars().all(|c| c.is_ascii_digit()));
                match index + 1 == parts.len() {
                    // The last group may be partly typed, never empty.
                    true => prop_assert!((1..=size).contains(&part.len())),
                    false => prop_assert_eq!(part.len(), size),
                }
            }
        }

        #[test]
        fn formatted_text_stays_as_it_is(mask in masks(), text in typed(), caret in 0usize..50) {
            let (once, at) = format(mask, &text, caret);
            prop_assert_eq!(format(mask, &once, at), (once.clone(), at));
        }

        #[test]
        fn the_caret_stays_after_the_same_digit(mask in masks(), text in typed(), caret in 0usize..50) {
            let (formatted, at) = format(mask, &text, caret);
            let total: usize = mask.groups.iter().map(|&size| usize::from(size)).sum();
            let before = text.chars().take(caret).filter(char::is_ascii_digit).count().min(total);
            prop_assert!(at <= formatted.len());
            prop_assert_eq!(formatted[..at].chars().filter(char::is_ascii_digit).count(), before);
            // Right after that digit, not after a separator that follows it.
            prop_assert!(before == 0 || formatted.as_bytes()[at - 1].is_ascii_digit());
        }
    }
}
