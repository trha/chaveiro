//! The general pasteboard, for secrets.

use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardContentsOptions, NSPasteboardTypeString};
use objc2_foundation::{NSData, NSString};

/// Names the pasteboard content of one copy, so a later clear empties the
/// pasteboard only while it still holds that copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Copied(isize);

/// The type that clipboard managers read as "skip this item", from
/// nspasteboard.org. macOS itself has no such flag.
const CONCEALED: &str = "org.nspasteboard.ConcealedType";

/// Puts `text` on the general pasteboard, kept off other devices and
/// marked so clipboard managers skip it. [`None`] when the write failed,
/// such as when another app wrote between the clear and the write.
///
/// The pasteboard holds its own copy of `text`, which no wipe reaches, so
/// call [`clear`] after a while.
pub fn copy(text: &str) -> Option<Copied> {
    let pasteboard = general();
    pasteboard.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    let count = pasteboard.clearContents();
    // SAFETY: the type constant is a static NSString.
    let written = pasteboard
        .setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
        && pasteboard.setData_forType(Some(&NSData::new()), &NSString::from_str(CONCEALED));
    // Each write after the clear leaves the count as `clearContents` gave it.
    (written && pasteboard.changeCount() == count).then_some(Copied(count))
}

/// Empties the general pasteboard if it still holds `copied`. Reading the
/// change count does not ask the user for paste access, as reading the
/// content would.
pub fn clear(copied: Copied) {
    let pasteboard = general();
    if pasteboard.changeCount() == copied.0 {
        pasteboard.clearContents();
    }
}

fn general() -> Retained<NSPasteboard> {
    NSPasteboard::generalPasteboard()
}
