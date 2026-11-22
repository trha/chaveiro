use super::{Control, Response, must_write};
use crate::{
    events::{Command, EventKind},
    pass::{ChildKind, ControlNode, Ui},
    tree::Visit,
};
use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::ProtocolObject, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSColor, NSFont, NSFontWeightSemibold, NSStackView, NSTextAlignment,
    NSTextField, NSTextFieldBezelStyle, NSTextFieldCell, NSUserInterfaceLayoutOrientation,
};
use objc2_foundation::{NSRect, NSString};
use std::{
    array,
    hash::Hash,
    ops::{Deref, DerefMut},
};

/// The number of boxes [`Ui::code_boxes`] shows.
pub const CODE_LENGTH: usize = 6;

/// The digits of a one-time code, one per box; [`None`] for an empty box.
pub type CodeDigits = [Option<u8>; CODE_LENGTH];

impl Ui {
    /// Six one-digit boxes for a one-time code, bound to `digits`, in two
    /// groups of three with a dash between them.
    ///
    /// Typing a digit fills its box and moves focus to the next one, and
    /// Backspace in an empty box moves back. Left and Right move one box,
    /// Home and End (or Command-Left and Command-Right) to the first and
    /// last. Pasting several digits into any box fills that box and the
    /// ones after it. Other characters are dropped. Screen readers name
    /// each box "Digit n of 6".
    ///
    /// The boxes are one field: [`header`](Response::header),
    /// [`error`](Response::error), [`hint`](Response::hint) and
    /// [`enabled`](Response::enabled) act on the whole row, and
    /// [`make_first_responder`](Response::make_first_responder) focuses the
    /// first empty box.
    pub fn code_boxes(&mut self, id: impl Hash, digits: &mut CodeDigits) -> CodeBoxesResponse<'_> {
        let (node, visit) = self.visit(id, ChildKind::Visual, |mtm| {
            let boxes: [Retained<NSTextField>; CODE_LENGTH] =
                array::from_fn(|index| digit_box(mtm, index));
            let row = NSStackView::new(mtm);
            row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
            row.setSpacing(6.0);
            let (first, second) = boxes.split_at(CODE_LENGTH / 2);
            for field in first {
                row.addArrangedSubview(field);
            }
            let dash = NSTextField::labelWithString(&NSString::from_str("–"), mtm);
            dash.setTextColor(Some(&NSColor::tertiaryLabelColor()));
            row.addArrangedSubview(&dash);
            for field in second {
                row.addArrangedSubview(field);
            }
            ControlNode::field(
                mtm,
                &row,
                Control::CodeBoxes {
                    boxes,
                    digits: [None; CODE_LENGTH],
                },
            )
        });
        if visit == Visit::Created {
            let Control::CodeBoxes { boxes, .. } = &self.control(node).control else {
                unreachable!("code boxes keys hold code boxes")
            };
            for field in boxes {
                self.events.register(field, node);
                // AppKit holds the delegate and target weakly; `Events` keeps
                // the shared target alive for as long as the boxes exist.
                unsafe {
                    field.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
                    field.setTarget(Some(self.events.target()));
                    field.setAction(Some(sel!(fire:)));
                }
            }
        }

        let typed = self.events.happened(node, EventKind::TextDidChange);
        let enter = self.events.happened(node, EventKind::Action);
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::CodeBoxes {
            boxes,
            digits: written,
        } = &mut control.control
        else {
            unreachable!("code boxes keys hold code boxes")
        };

        // Commands come from the box being edited.
        let editing = boxes
            .iter()
            .position(|field| field.currentEditor().is_some());
        // The first box whose text differs from what it last showed.
        let edit = typed
            .then(|| {
                boxes.iter().enumerate().find_map(|(index, field)| {
                    let text = field.stringValue().to_string();
                    (text != shown(written[index])).then_some((index, text))
                })
            })
            .flatten();
        // The edited box and whether a command found it empty.
        let editing = editing.map(|index| {
            let empty =
                written[index].is_none() && edit.as_ref().is_none_or(|(at, _)| *at != index);
            (index, empty)
        });

        let mut next = None;
        if let Some((index, text)) = &edit {
            let (merged, after) = merge(written, *index, text);
            *digits = merged;
            next = after;
        }

        for (index, field) in boxes.iter().enumerate() {
            let declared = digits[index];
            let write = match &edit {
                // The edited box shows what the user typed, which may be a
                // letter or two digits, so its text is compared.
                Some((at, text)) if *at == index => {
                    must_write(node, stale, text.as_str(), shown(declared).as_str())
                }
                _ => must_write(node, stale, &written[index], &declared),
            };
            if write {
                field.setStringValue(&NSString::from_str(&shown(declared)));
            }
            written[index] = declared;
        }
        let changed = edit.is_some();

        // A command moves focus before a typed digit does; the last command
        // wins.
        let moved = editing.and_then(|(index, empty)| {
            self.events
                .commands(node)
                .filter_map(|command| step(index, command, empty))
                .last()
        });
        if let Some(index) = moved.or(next) {
            let Control::CodeBoxes { boxes, .. } = &self.control(node).control else {
                unreachable!("code boxes keys hold code boxes")
            };
            if let Some(window) = boxes[index].window() {
                // A field that becomes first responder selects its text, so
                // the next digit replaces the old one.
                window.makeFirstResponder(Some(&boxes[index]));
            }
        }
        CodeBoxesResponse {
            response: Response { ui: self, node },
            changed,
            enter,
        }
    }
}

/// A box for the digit at `index`, 40 points wide, its digit centered in
/// a large semibold font.
fn digit_box(mtm: MainThreadMarker, index: usize) -> Retained<NSTextField> {
    let field = NSTextField::new(mtm);
    // A plain cell draws its text at the top of a tall field.
    let cell: Retained<CenteredCell> =
        unsafe { msg_send![CenteredCell::alloc(mtm), initTextCell: &*NSString::new()] };
    field.setCell(Some(&cell));
    field.setEditable(true);
    field.setSelectable(true);
    field.setBezeled(true);
    field.setDrawsBackground(true);
    field.setTranslatesAutoresizingMaskIntoConstraints(false);
    field.setBezelStyle(NSTextFieldBezelStyle::SquareBezel);
    field.setAlignment(NSTextAlignment::Center);
    field.setFont(Some(&NSFont::systemFontOfSize_weight(22.0, unsafe {
        NSFontWeightSemibold
    })));
    field
        .widthAnchor()
        .constraintEqualToConstant(BOX_WIDTH)
        .setActive(true);
    field
        .heightAnchor()
        .constraintEqualToConstant(BOX_HEIGHT)
        .setActive(true);
    field.setAccessibilityLabel(Some(&NSString::from_str(&format!(
        "Digit {} of {CODE_LENGTH}",
        index + 1
    ))));
    field
}

define_class!(
    /// A text field cell that centers its one line of text vertically, for
    /// drawing and for the field editor alike.
    #[unsafe(super(NSTextFieldCell))]
    #[thread_kind = MainThreadOnly]
    struct CenteredCell;

    impl CenteredCell {
        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect(&self, bounds: NSRect) -> NSRect {
            let rect: NSRect = unsafe { msg_send![super(self), drawingRectForBounds: bounds] };
            // One line of the font. `cellSizeForBounds:` counts the bezel
            // too when linked against the macOS 26 SDK or later, so it
            // centers the text a few points high.
            let Some(font) = self.font() else {
                return rect;
            };
            let height = (font.ascender() - font.descender() + font.leading()).ceil();
            if height >= rect.size.height {
                return rect;
            }
            let mut centered = rect;
            centered.origin.y += (rect.size.height - height) / 2.0;
            centered.size.height = height;
            centered
        }
    }
);

/// The design's box width: six boxes, a dash and six gaps fit the 324
/// points right of a form's header column.
const BOX_WIDTH: f64 = 40.0;

/// The design's box height. A field keeps its 22-point control height,
/// which clips a 22-point digit, so the height is set; only the square
/// bezel draws at any height.
const BOX_HEIGHT: f64 = 44.0;

/// The text a box shows for `digit`.
fn shown(digit: Option<u8>) -> String {
    digit.map(|digit| digit.to_string()).unwrap_or_default()
}

/// What [`Ui::code_boxes`] returns.
pub struct CodeBoxesResponse<'u> {
    response: Response<'u>,
    changed: bool,
    enter: bool,
}

impl CodeBoxesResponse<'_> {
    /// Whether the user changed a digit since the last pass; `digits` then
    /// holds the new code.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Whether the user pressed Return in a box since the last pass.
    pub fn return_pressed(&self) -> bool {
        self.enter
    }
}

impl<'u> Deref for CodeBoxesResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for CodeBoxesResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}

/// The box to focus after `command` ran in box `index`, if focus moves.
/// `empty` says the command found the box empty and changed nothing.
///
/// Left and Right move one box, Home and End to the first and last, and
/// Backspace in an empty box moves back. Focus stays at either end.
fn step(index: usize, command: Command, empty: bool) -> Option<usize> {
    let target = match command {
        Command::Left => index.checked_sub(1)?,
        Command::Backspace if empty => index.checked_sub(1)?,
        Command::Backspace | Command::Up | Command::Down => return None,
        Command::Right => index + 1,
        Command::Home => 0,
        Command::End => CODE_LENGTH - 1,
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
