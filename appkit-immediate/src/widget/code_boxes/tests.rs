use super::{CODE_LENGTH, CodeDigits, merge, step};
use crate::events::Command;
use proptest::prelude::*;

fn digits() -> impl Strategy<Value = CodeDigits> {
    prop::array::uniform6(prop::option::of(0u8..10))
}

fn index() -> impl Strategy<Value = usize> {
    0..CODE_LENGTH
}

fn text(digits: &[u8]) -> String {
    digits
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect()
}

proptest! {
    #[test]
    fn boxes_before_the_edited_one_never_change(
        before in digits(), index in index(), typed in "[0-9a-z ]{0,9}"
    ) {
        let (after, _) = merge(&before, index, &typed);
        prop_assert_eq!(&after[..index], &before[..index]);
    }

    #[test]
    fn every_box_holds_a_single_digit(
        before in digits(), index in index(), typed in "[0-9a-z ]{0,9}"
    ) {
        let (after, _) = merge(&before, index, &typed);
        prop_assert!(after.iter().flatten().all(|&digit| digit < 10));
    }

    #[test]
    fn text_without_digits_empties_only_its_box(
        before in digits(), index in index(), typed in "[a-z ]{0,4}"
    ) {
        let (after, focus) = merge(&before, index, &typed);
        let mut expected = before;
        expected[index] = None;
        prop_assert_eq!(after, expected);
        prop_assert_eq!(focus, None);
    }

    #[test]
    fn one_digit_in_an_empty_box_fills_it_and_moves_on(
        mut before in digits(), index in index(), digit in 0u8..10
    ) {
        before[index] = None;
        let (after, focus) = merge(&before, index, &text(&[digit]));
        prop_assert_eq!(after[index], Some(digit));
        prop_assert_eq!(&after[index + 1..], &before[index + 1..]);
        prop_assert_eq!(focus, (index + 1 < CODE_LENGTH).then(|| index + 1));
    }

    /// A digit typed before or after the one a box holds replaces it.
    #[test]
    fn a_digit_typed_beside_the_old_one_replaces_it(
        before in digits(), index in index(), old in 0u8..10, new in 0u8..10, caret_first in any::<bool>()
    ) {
        let mut before = before;
        before[index] = Some(old);
        let typed = match caret_first { true => text(&[new, old]), false => text(&[old, new]) };
        let (after, _) = merge(&before, index, &typed);
        prop_assert_eq!(after[index], Some(new));
        prop_assert_eq!(&after[index + 1..], &before[index + 1..]);
    }

    /// A paste of three or more digits lands in order from the box it went
    /// into, and what does not fit is dropped.
    #[test]
    fn a_paste_fills_boxes_from_its_own(
        before in digits(), index in index(), pasted in prop::collection::vec(0u8..10, 3..9)
    ) {
        let (after, focus) = merge(&before, index, &format!("{} -", text(&pasted)));
        let fits = pasted.len().min(CODE_LENGTH - index);
        for offset in 0..fits {
            prop_assert_eq!(after[index + offset], Some(pasted[offset]));
        }
        prop_assert_eq!(&after[index + fits..], &before[index + fits..]);
        let last = index + fits;
        prop_assert_eq!(focus, (last < CODE_LENGTH).then_some(last));
    }
}

fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        Just(Command::Backspace),
        Just(Command::Left),
        Just(Command::Right),
        Just(Command::Home),
        Just(Command::End),
    ]
}

proptest! {
    /// Focus lands on a different box that exists, or stays.
    #[test]
    fn a_key_moves_focus_to_another_real_box_or_nowhere(
        index in index(), command in command(), empty in any::<bool>()
    ) {
        if let Some(target) = step(index, command, empty) {
            prop_assert!(target < CODE_LENGTH && target != index);
        }
    }

    #[test]
    fn left_and_right_move_one_box(index in index()) {
        prop_assert_eq!(step(index, Command::Left, false), index.checked_sub(1));
        let right = step(index, Command::Right, false);
        prop_assert_eq!(right, (index + 1 < CODE_LENGTH).then_some(index + 1));
    }

    #[test]
    fn home_and_end_reach_the_ends(index in index()) {
        prop_assert_eq!(step(index, Command::Home, false).unwrap_or(index), 0);
        prop_assert_eq!(step(index, Command::End, false).unwrap_or(index), CODE_LENGTH - 1);
    }

    #[test]
    fn backspace_moves_back_only_from_an_empty_box(index in index()) {
        prop_assert_eq!(step(index, Command::Backspace, false), None);
        prop_assert_eq!(step(index, Command::Backspace, true), index.checked_sub(1));
    }
}
