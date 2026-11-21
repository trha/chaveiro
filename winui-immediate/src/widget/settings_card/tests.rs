//! The layout states, checked against the toolkit's triggers in
//! `SettingsCard.xaml`.

use super::{ContentAlignment, Layout, SettingsCard, WRAP_NO_ICON_WIDTH, WRAP_WIDTH};
use proptest::prelude::*;

fn alignment() -> impl Strategy<Value = ContentAlignment> {
    prop_oneof![
        Just(ContentAlignment::Right),
        Just(ContentAlignment::Left),
        Just(ContentAlignment::Vertical),
    ]
}

/// A card with or without each text and the icon.
fn card(
    alignment: ContentAlignment,
    header: bool,
    description: bool,
    icon: bool,
) -> SettingsCard<'static> {
    SettingsCard {
        header: if header { "Header" } else { "" },
        description: if description { "Description" } else { "" },
        icon: if icon { "\u{E8EA}" } else { "" },
        clickable: false,
        alignment,
    }
}

proptest! {
    /// `Right` keeps the content beside the header from 476px up, and before
    /// the first layout.
    #[test]
    fn a_wide_right_card_keeps_content_beside_the_header(
        width in prop_oneof![Just(0.0), WRAP_WIDTH..2000.0],
        header: bool, description: bool, icon: bool, content: bool,
    ) {
        let layout = Layout::new(&card(ContentAlignment::Right, header, description, icon), width, content);
        prop_assert!(!layout.under);
        prop_assert!(!layout.spacing);
        prop_assert_eq!(layout.show_icon, icon);
    }

    /// `RightWrapped` and `RightWrappedNoIcon`: under the header, stretched,
    /// and without the icon below 286px.
    #[test]
    fn a_narrow_right_card_wraps_its_content(
        width in 1.0..WRAP_WIDTH, header: bool, description: bool, icon: bool, content: bool,
    ) {
        let layout = Layout::new(&card(ContentAlignment::Right, header, description, icon), width, content);
        prop_assert!(layout.under && layout.stretch);
        prop_assert_eq!(layout.show_icon, icon && width >= WRAP_NO_ICON_WIDTH);
    }

    /// `Left` shows the content alone, unstretched, whatever the width.
    #[test]
    fn a_left_card_shows_only_its_content(
        width in 0.0..2000.0, header: bool, description: bool, icon: bool, content: bool,
    ) {
        let layout = Layout::new(&card(ContentAlignment::Left, header, description, icon), width, content);
        prop_assert!(layout.under && !layout.stretch && layout.content_only);
        prop_assert!(!layout.show_icon && !layout.show_header && !layout.show_description);
        prop_assert!(!layout.spacing);
    }

    #[test]
    fn a_vertical_card_stretches_its_content_under_the_header(
        width in 0.0..2000.0, header: bool, description: bool, icon: bool, content: bool,
    ) {
        let layout = Layout::new(&card(ContentAlignment::Vertical, header, description, icon), width, content);
        prop_assert!(layout.under && layout.stretch && !layout.content_only);
        prop_assert_eq!(layout.show_icon, icon);
    }

    /// `ContentSpacing` needs content under some text; empty parts collapse.
    #[test]
    fn spacing_and_visibility_follow_what_the_card_holds(
        alignment in alignment(), width in 0.0..2000.0,
        header: bool, description: bool, icon: bool, content: bool,
    ) {
        let layout = Layout::new(&card(alignment, header, description, icon), width, content);
        prop_assert_eq!(layout.show_content, content);
        prop_assert!(!layout.show_header || header);
        prop_assert!(!layout.show_description || description);
        prop_assert!(!layout.show_icon || icon);
        let text = header || description;
        prop_assert_eq!(
            layout.spacing,
            layout.under && alignment != ContentAlignment::Left && content && text
        );
    }
}
