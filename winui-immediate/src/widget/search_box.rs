use super::{Control, Response, boxed, resource};
use crate::{
    bindings::{
        Control as XamlControl, FontIcon, FrameworkElement, Grid, HorizontalAlignment,
        IKeyboardAccelerator, KeyboardAccelerator, KeyboardAcceleratorPlacementMode, Panel,
        TextBox, Thickness, UIElement, VerticalAlignment, VirtualKey, VirtualKeyModifiers,
    },
    events::{Delegate, EventKind, Key},
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_collections::IMap;
use windows_core::{IInspectable, Interface, Result};

impl Ui {
    /// A search box bound to `value`, in the look of the Microsoft Store's
    /// title bar: at rest and under the pointer it is drawn as a button,
    /// with a search icon on its right, and once clicked it is a text box.
    /// Binding works as for [`Ui::text_box`]. Give it a key with
    /// [`SearchBoxResponse::shortcut`].
    pub fn search_box(
        &mut self,
        id: impl Hash,
        placeholder: &str,
        value: &mut String,
    ) -> Result<SearchBoxResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let text_box = TextBox::new()?;
            button_look(&text_box)?;
            // Room for the icon, which sits over the box's right end.
            text_box.cast::<XamlControl>()?.SetPadding(Thickness {
                left: 11.0,
                top: 5.0,
                right: 36.0,
                bottom: 6.0,
            })?;
            let icon = FontIcon::new()?;
            icon.SetGlyph("\u{E721}")?;
            icon.SetFontSize(14.0)?;
            let icon_frame = icon.cast::<FrameworkElement>()?;
            icon_frame.SetHorizontalAlignment(HorizontalAlignment::Right)?;
            icon_frame.SetVerticalAlignment(VerticalAlignment::Center)?;
            icon_frame.SetMargin(Thickness {
                left: 0.0,
                top: 0.0,
                right: 12.0,
                bottom: 0.0,
            })?;
            // A click on the icon reaches the box under it.
            icon.cast::<UIElement>()?.SetIsHitTestVisible(false)?;
            let grid = Grid::new()?;
            let children = grid.cast::<Panel>()?.Children()?;
            children.Append(&text_box)?;
            children.Append(&icon)?;

            Ok(ControlNode::new(
                grid.cast()?,
                Control::SearchBox {
                    text_box,
                    text: String::new(),
                    placeholder: String::new(),
                    composing: false,
                    shortcut: None,
                },
            ))
        })?;
        let changed = self.bind_text(node, placeholder, value)?;
        Ok(SearchBoxResponse {
            response: Response { ui: self, node },
            changed,
        })
    }
}

/// Makes Ctrl+`letter`, pressed anywhere in the window of `element`, focus
/// `text_box` and select its text, in place of any key set before.
/// `letter` is an uppercase ASCII letter.
fn set_shortcut(element: &UIElement, text_box: &TextBox, letter: char) -> Result<()> {
    let accelerator = KeyboardAccelerator::new()?;
    // A virtual key code of a letter is its uppercase ASCII code.
    accelerator.SetKey(VirtualKey(letter as i32))?;
    accelerator.SetModifiers(VirtualKeyModifiers::Control)?;
    // The handler holds the box weakly, because the box's grid holds the
    // accelerator, which holds the handler.
    let target = text_box.downgrade()?;
    Delegate::focus(target).register(&accelerator.cast::<IKeyboardAccelerator>()?, |table| {
        table.Invoked
    })?;
    let accelerators = element.KeyboardAccelerators()?;
    accelerators.Clear()?;
    accelerators.Append(&accelerator)?;
    // The placeholder names the key, so no tooltip shows it too.
    element.SetKeyboardAcceleratorPlacementMode(KeyboardAcceleratorPlacementMode::Hidden)
}

/// Gives `text_box` a button's border and fills while it is not focused.
/// The focused state keeps the text box's own look.
fn button_look(text_box: &TextBox) -> Result<()> {
    let resources = text_box
        .cast::<FrameworkElement>()?
        .Resources()?
        .cast::<IMap<IInspectable, IInspectable>>()?;
    for (key, brush) in [
        ("TextControlBackground", "ControlFillColorDefaultBrush"),
        (
            "TextControlBackgroundPointerOver",
            "ControlFillColorSecondaryBrush",
        ),
        ("TextControlBorderBrush", "ControlElevationBorderBrush"),
        (
            "TextControlBorderBrushPointerOver",
            "ControlElevationBorderBrush",
        ),
        (
            "TextControlPlaceholderForeground",
            "TextFillColorPrimaryBrush",
        ),
        (
            "TextControlPlaceholderForegroundPointerOver",
            "TextFillColorPrimaryBrush",
        ),
    ] {
        resources.Insert(&boxed(key), &resource::<IInspectable>(brush)?)?;
    }
    Ok(())
}

/// What [`Ui::search_box`] returns.
pub struct SearchBoxResponse<'u> {
    response: Response<'u>,
    changed: bool,
}

impl SearchBoxResponse<'_> {
    /// Whether the user changed the text since the last pass; `value` then
    /// holds the new text.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Makes Ctrl+`letter`, pressed anywhere in the box's window, focus the
    /// box and select its text, such as Ctrl+F. No tooltip shows the key,
    /// so name it in the placeholder. Declare one shortcut per window.
    ///
    /// Panics when `letter` is not an ASCII letter.
    pub fn shortcut(&mut self, letter: char) -> Result<&mut Self> {
        assert!(
            letter.is_ascii_alphabetic(),
            "a shortcut is Ctrl and a letter"
        );
        let letter = letter.to_ascii_uppercase();
        let ControlNode {
            element, control, ..
        } = self.response.ui.control_mut(self.response.node);
        let Control::SearchBox {
            text_box, shortcut, ..
        } = control
        else {
            unreachable!("search box keys hold search boxes")
        };
        if *shortcut != Some(letter) {
            set_shortcut(element, text_box, letter)?;
            *shortcut = Some(letter);
        }
        Ok(self)
    }

    /// The [`Key`]s the user pressed in the box since the last pass, in
    /// order, seen before the box acted on them, such as Down to move into
    /// the results. The first call subscribes the box, so its first result
    /// is empty.
    pub fn keys(&mut self) -> Result<impl Iterator<Item = Key>> {
        let Response { ui, node } = &mut self.response;
        ui.subscribe(*node, &[EventKind::Key], |control| match &control.control {
            Control::SearchBox { text_box, .. } => text_box,
            _ => unreachable!("search box keys hold search boxes"),
        })?;
        Ok(ui.events.keys(*node))
    }
}

impl<'u> Deref for SearchBoxResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for SearchBoxResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}
