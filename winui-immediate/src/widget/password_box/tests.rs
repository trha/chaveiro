use super::{secret_from_wide, sole_owner};
use proptest::prelude::*;

proptest! {
    /// Any text WinUI hands over comes back unchanged.
    #[test]
    fn valid_utf16_round_trips(text in any::<String>()) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let secret = secret_from_wide(&wide);
        prop_assert_eq!(secret.as_str(), text.as_str());
    }

    /// Unpaired surrogates become U+FFFD, as in std's lossy decoding.
    #[test]
    fn any_utf16_decodes_like_std_lossy(wide in prop::collection::vec(any::<u16>(), 0..64)) {
        let secret = secret_from_wide(&wide);
        let lossy = String::from_utf16_lossy(&wide);
        prop_assert_eq!(secret.as_str(), lossy.as_str());
    }

    /// The UTF-8 text never needs more than the 3 bytes per unit the buffer
    /// starts with, so the buffer never grows and leaves no unwiped copy.
    #[test]
    fn text_fits_the_first_buffer(wide in prop::collection::vec(any::<u16>(), 0..64)) {
        let text = secret_from_wide(&wide);
        prop_assert!(text.len() <= 3 * wide.len());
        prop_assert!(text.capacity() >= 3 * wide.len());
    }
}

/// A new string owns its buffer, and a second handle to it shares it, as a
/// box that kept the string would.
#[test]
fn a_shared_string_is_no_sole_owner() {
    let string = windows_core::HSTRING::from("secret");
    assert!(sole_owner(&string));
    let kept = string.clone();
    assert!(!sole_owner(&string));
    assert!(!sole_owner(&kept));
    drop(kept);
    assert!(sole_owner(&string));
}

#[test]
fn the_empty_string_owns_nothing_to_share() {
    assert!(sole_owner(&windows_core::HSTRING::new()));
}
