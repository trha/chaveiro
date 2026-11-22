use super::{Control, Response, boxed, must_write, resource};
use crate::{
    bindings::{
        AutomationProperties, ContentControl, DependencyObject, FlyoutBase, FontIcon,
        FrameworkElement, IPanel, MenuFlyout, Orientation, SplitButton, StackPanel, TextBlock,
        ToolTipService, UIElement,
    },
    pass::{ChildKind, ControlNode, Ui},
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_collections::IMap;
use windows_core::{IInspectable, Interface, Result};

impl Ui {
    /// A split button: the main part shows `glyph`, a Segoe Fluent Icons
    /// glyph, and `label`, and the chevron opens a menu of the items `body`
    /// declares, as [`submenu`](Self::submenu) takes them. WinUI opens and
    /// closes that menu itself, so the app declares the items on every pass
    /// and reads their clicks as usual.
    pub fn split_button(
        &mut self,
        id: impl Hash,
        glyph: char,
        label: &str,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<SplitButtonResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let button = SplitButton::new()?;
            let row = StackPanel::new()?;
            row.SetOrientation(Orientation::Horizontal)?;
            row.SetSpacing(8.0)?;
            let icon = FontIcon::new()?;
            icon.SetFontSize(16.0)?;
            let content = TextBlock::new()?;
            let children = row.cast::<IPanel>()?.Children()?;
            children.Append(&icon.cast::<UIElement>()?)?;
            children.Append(&content.cast::<UIElement>()?)?;
            button.cast::<ContentControl>()?.SetContent(&row)?;
            let flyout = MenuFlyout::new()?;
            // The menu shows at once, as Ui::menu's does.
            flyout
                .cast::<FlyoutBase>()?
                .SetAreOpenCloseAnimationsEnabled(false)?;
            let items = flyout.Items()?;
            button.SetFlyout(&flyout.cast::<FlyoutBase>()?)?;
            Ok(ControlNode::new(
                button.cast()?,
                Control::SplitButton(Box::new(Parts {
                    button,
                    icon,
                    content,
                    items,
                    glyph: None,
                    label: String::new(),
                    tooltip: String::new(),
                    accent: false,
                })),
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::SplitButton(parts) = &mut control.control else {
            unreachable!("split button keys hold split buttons")
        };
        if must_write(stale, &parts.glyph, &Some(glyph)) {
            parts
                .icon
                .SetGlyph(glyph.encode_utf8(&mut [0; 4]) as &str)?;
            parts.glyph = Some(glyph);
        }
        if must_write(stale, parts.label.as_str(), label) {
            parts.content.SetText(label)?;
            parts.label.clear();
            parts.label.push_str(label);
        }
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(SplitButtonResponse(Response { ui: self, node }))
    }
}

/// The parts of a [`Ui::split_button`], with the values last written.
pub(crate) struct Parts {
    button: SplitButton,
    icon: FontIcon,
    content: TextBlock,
    /// The menu's items, which the pass fills from the body.
    pub(crate) items: windows_collections::IVector<crate::bindings::MenuFlyoutItemBase>,
    /// [`None`] until first written.
    glyph: Option<char>,
    label: String,
    /// Empty until first written; an empty tooltip is never written.
    tooltip: String,
    accent: bool,
}

/// The split button template's brush keys, each with the theme brush that
/// gives the accent look, as `AccentButtonStyle` colors a button.
const ACCENT_BRUSHES: [(&str, &str); 14] = [
    ("SplitButtonBackground", "AccentFillColorDefaultBrush"),
    (
        "SplitButtonBackgroundPointerOver",
        "AccentFillColorSecondaryBrush",
    ),
    (
        "SplitButtonBackgroundPressed",
        "AccentFillColorTertiaryBrush",
    ),
    (
        "SplitButtonBackgroundDisabled",
        "AccentFillColorDisabledBrush",
    ),
    ("SplitButtonForeground", "TextOnAccentFillColorPrimaryBrush"),
    (
        "SplitButtonForegroundPointerOver",
        "TextOnAccentFillColorPrimaryBrush",
    ),
    (
        "SplitButtonForegroundPressed",
        "TextOnAccentFillColorSecondaryBrush",
    ),
    (
        "SplitButtonForegroundDisabled",
        "TextOnAccentFillColorDisabledBrush",
    ),
    (
        "SplitButtonForegroundSecondary",
        "TextOnAccentFillColorPrimaryBrush",
    ),
    (
        "SplitButtonForegroundSecondaryPressed",
        "TextOnAccentFillColorSecondaryBrush",
    ),
    (
        "SplitButtonBorderBrush",
        "AccentControlElevationBorderBrush",
    ),
    (
        "SplitButtonBorderBrushPointerOver",
        "AccentControlElevationBorderBrush",
    ),
    (
        "SplitButtonBorderBrushPressed",
        "ControlFillColorTransparentBrush",
    ),
    (
        "SplitButtonBorderBrushDivider",
        "TextOnAccentFillColorDisabledBrush",
    ),
];

/// What [`Ui::split_button`] returns.
pub struct SplitButtonResponse<'u>(Response<'u>);

impl SplitButtonResponse<'_> {
    /// Whether the main part was clicked since the last pass. The chevron
    /// only opens the menu. The first call subscribes the button.
    pub fn clicked(&mut self) -> Result<bool> {
        self.0.button_clicked()
    }

    /// Gives the button the accent colors, or the standard ones. Set it on
    /// the pass that declares the button first: WinUI reads the colors when
    /// it builds the button's template, so a later change may show only
    /// after the button is rebuilt.
    pub fn accent(&mut self, on: bool) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::SplitButton(parts) = &mut control.control else {
            unreachable!("split button keys hold split buttons")
        };
        if must_write(stale, &parts.accent, &on) {
            // The Windows App SDK has no accent split button style, so this
            // sets the button's own copies of the brushes its template uses.
            let resources = parts
                .button
                .cast::<FrameworkElement>()?
                .Resources()?
                .cast::<IMap<IInspectable, IInspectable>>()?;
            for (key, brush) in ACCENT_BRUSHES {
                let key = boxed(key);
                if on {
                    resources.Insert(&key, &resource::<IInspectable>(brush)?)?;
                } else if resources.HasKey(&key)? {
                    resources.Remove(&key)?;
                }
            }
            parts.accent = on;
        }
        Ok(self)
    }

    /// Shows `text` as the tooltip, and screen readers say it as the
    /// button's name, such as "New login (Ctrl+N)".
    pub fn tooltip(&mut self, text: &str) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::SplitButton(parts) = &mut control.control else {
            unreachable!("split button keys hold split buttons")
        };
        if must_write(stale, parts.tooltip.as_str(), text) {
            let target = parts.button.cast::<DependencyObject>()?;
            AutomationProperties::SetName(&target, text)?;
            ToolTipService::SetToolTip(&target, &boxed(text))?;
            parts.tooltip.clear();
            parts.tooltip.push_str(text);
        }
        Ok(self)
    }
}

impl<'u> Deref for SplitButtonResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for SplitButtonResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}
