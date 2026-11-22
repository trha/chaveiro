use super::{Control, Response, must_write};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSFontWeightRegular, NSImage, NSImageSymbolConfiguration, NSImageView,
    NSLayoutConstraintOrientation,
};
use objc2_foundation::NSString;
use std::hash::Hash;

impl Ui {
    /// The SF Symbols image `name`, such as `"globe"`, drawn at the size of
    /// `size`-point text in the label color; a name the system does not
    /// have shows no image. Tint it with
    /// [`text_color`](Response::text_color).
    pub fn symbol(&mut self, id: impl Hash, name: &str, size: f64) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let image = NSImageView::new(mtm);
            // A symbol keeps its own size in a stack view.
            for orientation in [
                NSLayoutConstraintOrientation::Horizontal,
                NSLayoutConstraintOrientation::Vertical,
            ] {
                image.setContentHuggingPriority_forOrientation(750.0, orientation);
            }
            ControlNode::payload(
                Retained::clone(&image).into_super().into_super(),
                Control::Symbol {
                    image,
                    name: String::new(),
                    size: f64::NAN,
                },
            )
        });
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::Symbol {
            image,
            name: written_name,
            size: written_size,
        } = &mut control.control
        else {
            unreachable!("symbol keys hold symbols")
        };
        if must_write(node, stale, written_size, &size) {
            let configuration = unsafe {
                NSImageSymbolConfiguration::configurationWithPointSize_weight(
                    size,
                    NSFontWeightRegular,
                )
            };
            image.setSymbolConfiguration(Some(&configuration));
            *written_size = size;
        }
        if must_write(node, stale, written_name.as_str(), name) {
            image.setImage(
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(name),
                    None,
                )
                .as_deref(),
            );
            written_name.clear();
            written_name.push_str(name);
        }
        Response { ui: self, node }
    }
}
