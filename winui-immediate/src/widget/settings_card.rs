//! A port of the Windows Community Toolkit's `SettingsCard`
//! (`CommunityToolkit.WinUI.Controls.SettingsControls`): an icon, a header
//! with a description under it, and the app's controls on the right, on a
//! card. Its sizes, brushes and layout states come from the toolkit's
//! `SettingsCard.xaml`.
//!
//! The toolkit derives the card from `ButtonBase` and drives its own visual
//! states. Here a clickable card is a `Button` restyled through its
//! lightweight resources, so it gets the button's pointer, keyboard, focus
//! and automation behavior. A card that is not clickable is a plain grid,
//! which screen readers do not announce as a button.

use super::{Control, Response, resource};
use crate::{
    bindings::{
        AutomationProperties, Brush, Button, ColumnDefinition, ContentControl,
        Control as XamlControl, CornerRadius, DependencyObject, FontIcon, FrameworkElement, Grid,
        GridLength, GridUnitType, HorizontalAlignment, IPanel, Orientation as XamlOrientation,
        RowDefinition, StackPanel, TextBlock, TextWrapping, Thickness, UIElement,
        UIElementCollection, VerticalAlignment, Visibility,
    },
    events::EventKind,
    pass::{ChildKind, ControlNode, Ui},
    widget::boxed,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_collections::IMap;
use windows_core::{IInspectable, Interface, Result};

/// What a [`Ui::settings_card`] shows.
#[derive(Clone, Copy, Debug, Default)]
pub struct SettingsCard<'a> {
    pub header: &'a str,
    /// The secondary line under the header; empty for none.
    pub description: &'a str,
    /// A Segoe Fluent Icons glyph, such as `"\u{E8EA}"`; empty for none.
    pub icon: &'a str,
    /// The whole card is a button with a chevron on the right. Fixed when
    /// the card is created: a card under the same id keeps its first value.
    pub clickable: bool,
    pub alignment: ContentAlignment,
}

/// Where a [`Ui::settings_card`] puts the controls its body declares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContentAlignment {
    /// On the right of the header. A card narrower than 476px moves them
    /// under the header, and one narrower than 286px also hides the icon.
    #[default]
    Right,
    /// Alone on the left, with the icon, header and description hidden, as
    /// for a check box.
    Left,
    /// Under the header, stretched.
    Vertical,
}

impl Ui {
    /// A card in the style of the toolkit's `SettingsCard`: the icon, the
    /// header and description, and the controls `body` declares, laid out as
    /// [`ContentAlignment`] says. Screen readers name the card by its header
    /// and read the description as its help text.
    ///
    /// The brushes are the theme's at creation, as for [`Ui::card`].
    pub fn settings_card(
        &mut self,
        id: impl Hash,
        card: SettingsCard<'_>,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<SettingsCardResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let parts = Parts::new(card.clickable)?;
            let element = match &parts.button {
                Some(button) => button.cast()?,
                None => parts.root.cast()?,
            };
            Ok(ControlNode::new(
                element,
                Control::SettingsCard(Box::new(parts)),
            ))
        })?;
        let control = self.control_mut(node);
        let element = control.element.clone();
        let clickable =
            matches!(&control.control, Control::SettingsCard(parts) if parts.button.is_some());
        let mut subscribed = control.subscribed;
        // A width change can cross a wrap threshold, so it needs a pass.
        self.events
            .subscribe(node, &mut subscribed, &element, EventKind::SizeChanged)?;
        if clickable {
            self.events
                .subscribe(node, &mut subscribed, &element, EventKind::Click)?;
        }
        self.control_mut(node).subscribed = subscribed;
        let clicked = clickable && self.events.happened(node, EventKind::Click);

        self.open(node);
        body(self)?;
        self.close()?;

        let width = element.cast::<FrameworkElement>()?.ActualWidth()?;
        let control = self.control_mut(node);
        let has_content = !control.visual.is_empty();
        let Control::SettingsCard(parts) = &mut control.control else {
            unreachable!("settings card keys hold settings cards")
        };
        parts.write(&element, &card)?;
        parts.lay_out(Layout::new(&card, width, has_content))?;
        Ok(SettingsCardResponse {
            response: Response { ui: self, node },
            clicked,
        })
    }
}

/// What [`Ui::settings_card`] returns.
pub struct SettingsCardResponse<'u> {
    response: Response<'u>,
    clicked: bool,
}

impl SettingsCardResponse<'_> {
    /// Whether a clickable card was clicked since the last pass; always
    /// false for one that is not clickable.
    pub fn clicked(&self) -> bool {
        self.clicked
    }
}

impl<'u> Deref for SettingsCardResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.response
    }
}

impl<'u> DerefMut for SettingsCardResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.response
    }
}

/// The toolkit's `SettingsCardWrapThreshold`.
const WRAP_WIDTH: f64 = 476.0;
/// The toolkit's `SettingsCardWrapNoIconThreshold`.
const WRAP_NO_ICON_WIDTH: f64 = 286.0;
/// The theme's `ControlCornerRadius`.
const CORNER: f64 = 4.0;

/// The card's elements and the values last written to them.
///
/// Contract: `root` is `button`'s content when `button` is set, and the
/// card's element otherwise.
pub(crate) struct Parts {
    button: Option<Button>,
    root: Grid,
    icon: FontIcon,
    header_panel: StackPanel,
    header: TextBlock,
    description: TextBlock,
    /// Holds the controls the body declares.
    content: StackPanel,
    pub(crate) children: UIElementCollection,
    written: Written,
}

#[derive(Default)]
struct Written {
    header: String,
    description: String,
    icon: String,
    layout: Option<Layout>,
}

/// The toolkit's visual states that move parts, as one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    /// The content sits under the header, in row 1.
    under: bool,
    /// The content stretches across its column.
    stretch: bool,
    /// The `Left` state: only the content shows.
    content_only: bool,
    show_icon: bool,
    show_header: bool,
    show_description: bool,
    show_content: bool,
    /// The `ContentSpacing` state: 8px between the header and content rows.
    spacing: bool,
}

impl Layout {
    /// The states the toolkit's triggers pick for `card` at `width`. A width
    /// of 0, before the first layout, counts as wide.
    fn new(card: &SettingsCard<'_>, width: f64, has_content: bool) -> Layout {
        let narrow = width > 0.0 && width < WRAP_WIDTH;
        let very_narrow = width > 0.0 && width < WRAP_NO_ICON_WIDTH;
        let (under, stretch, content_only) = match card.alignment {
            ContentAlignment::Right => (narrow, narrow, false),
            ContentAlignment::Left => (true, false, true),
            ContentAlignment::Vertical => (true, true, false),
        };
        let has_text = !card.header.is_empty() || !card.description.is_empty();
        Layout {
            under,
            stretch,
            content_only,
            show_icon: !card.icon.is_empty()
                && !content_only
                && !(card.alignment == ContentAlignment::Right && very_narrow),
            show_header: !card.header.is_empty() && !content_only,
            show_description: !card.description.is_empty() && !content_only,
            show_content: has_content,
            spacing: under && !content_only && has_content && has_text,
        }
    }
}

impl Parts {
    fn new(clickable: bool) -> Result<Self> {
        let root = Grid::new()?;
        let columns = root.ColumnDefinitions()?;
        for unit in [
            GridUnitType::Auto,
            GridUnitType::Star,
            GridUnitType::Auto,
            GridUnitType::Auto,
        ] {
            let column = ColumnDefinition::new()?;
            column.SetWidth(GridLength {
                value: 1.0,
                grid_unit_type: unit,
            })?;
            columns.Append(&column)?;
        }
        let rows = root.RowDefinitions()?;
        for unit in [GridUnitType::Star, GridUnitType::Auto] {
            let row = RowDefinition::new()?;
            row.SetHeight(GridLength {
                value: 1.0,
                grid_unit_type: unit,
            })?;
            rows.Append(&row)?;
        }
        let parts_of_root = root.cast::<IPanel>()?.Children()?;

        let icon = FontIcon::new()?;
        icon.SetFontSize(20.0)?;
        let frame = icon.cast::<FrameworkElement>()?;
        frame.SetMargin(thickness(2.0, 0.0, 20.0, 0.0))?;
        frame.SetVerticalAlignment(VerticalAlignment::Center)?;
        place(&frame, 0, 0)?;
        parts_of_root.Append(&icon.cast::<UIElement>()?)?;

        let header_panel = StackPanel::new()?;
        header_panel.SetOrientation(XamlOrientation::Vertical)?;
        let frame = header_panel.cast::<FrameworkElement>()?;
        frame.SetVerticalAlignment(VerticalAlignment::Center)?;
        place(&frame, 0, 1)?;
        let header = TextBlock::new()?;
        header.SetTextWrapping(TextWrapping::Wrap)?;
        let description = TextBlock::new()?;
        description.SetTextWrapping(TextWrapping::Wrap)?;
        description.SetFontSize(12.0)?;
        description.SetForeground(&resource::<Brush>("TextFillColorSecondaryBrush")?)?;
        let lines = header_panel.cast::<IPanel>()?.Children()?;
        lines.Append(&header.cast::<UIElement>()?)?;
        lines.Append(&description.cast::<UIElement>()?)?;
        parts_of_root.Append(&header_panel.cast::<UIElement>()?)?;

        let content = StackPanel::new()?;
        content.SetOrientation(XamlOrientation::Horizontal)?;
        content.SetSpacing(8.0)?;
        content
            .cast::<FrameworkElement>()?
            .SetVerticalAlignment(VerticalAlignment::Center)?;
        parts_of_root.Append(&content.cast::<UIElement>()?)?;
        let children = content.cast::<IPanel>()?.Children()?;

        let button = match clickable {
            true => {
                let action = FontIcon::new()?;
                action.SetGlyph("\u{E974}")?;
                action.SetFontSize(13.0)?;
                action.SetMirroredWhenRightToLeft(true)?;
                let frame = action.cast::<FrameworkElement>()?;
                frame.SetMargin(thickness(14.0, 0.0, 0.0, 0.0))?;
                frame.SetVerticalAlignment(VerticalAlignment::Center)?;
                place(&frame, 0, 3)?;
                Grid::SetRowSpan(&frame, 2)?;
                parts_of_root.Append(&action.cast::<UIElement>()?)?;
                Some(clickable_shell(&root)?)
            }
            false => {
                root.cast::<IPanel>()?
                    .SetBackground(&resource::<Brush>("CardBackgroundFillColorDefaultBrush")?)?;
                root.SetBorderBrush(&resource::<Brush>("CardStrokeColorDefaultBrush")?)?;
                root.SetBorderThickness(thickness(1.0, 1.0, 1.0, 1.0))?;
                root.SetCornerRadius(CornerRadius {
                    top_left: CORNER,
                    top_right: CORNER,
                    bottom_right: CORNER,
                    bottom_left: CORNER,
                })?;
                root.SetPadding(thickness(16.0, 16.0, 16.0, 16.0))?;
                size_like_a_card(&root.cast()?)?;
                None
            }
        };
        Ok(Parts {
            button,
            root,
            icon,
            header_panel,
            header,
            description,
            content,
            children,
            written: Written::default(),
        })
    }

    /// Writes the texts and the icon that changed.
    fn write(&mut self, element: &UIElement, card: &SettingsCard<'_>) -> Result<()> {
        let written = &mut self.written;
        let target = element.cast::<DependencyObject>()?;
        if written.header != card.header {
            self.header.SetText(card.header)?;
            AutomationProperties::SetName(&target, card.header)?;
            card.header.clone_into(&mut written.header);
        }
        if written.description != card.description {
            self.description.SetText(card.description)?;
            AutomationProperties::SetHelpText(&target, card.description)?;
            card.description.clone_into(&mut written.description);
        }
        if written.icon != card.icon {
            self.icon.SetGlyph(card.icon)?;
            card.icon.clone_into(&mut written.icon);
        }
        Ok(())
    }

    /// Moves and shows the parts as `layout` says, when it changed.
    fn lay_out(&mut self, layout: Layout) -> Result<()> {
        if self.written.layout == Some(layout) {
            return Ok(());
        }
        let content = self.content.cast::<FrameworkElement>()?;
        match layout.under {
            true => place(&content, 1, 1)?,
            false => place(&content, 0, 2)?,
        }
        // A horizontal stack never stretches its children, so stretched
        // content stacks them vertically, as the toolkit's single presenter
        // would stretch its one child.
        self.content.SetOrientation(match layout.stretch {
            true => XamlOrientation::Vertical,
            false => XamlOrientation::Horizontal,
        })?;
        content.SetHorizontalAlignment(match (layout.under, layout.stretch) {
            (false, _) => HorizontalAlignment::Right,
            (true, true) => HorizontalAlignment::Stretch,
            (true, false) => HorizontalAlignment::Left,
        })?;
        // The header keeps 24px from content on its right only.
        let right = match layout.under {
            true => 0.0,
            false => 24.0,
        };
        self.header_panel
            .cast::<FrameworkElement>()?
            .SetMargin(thickness(0.0, 0.0, right, 0.0))?;
        show(&self.icon.cast()?, layout.show_icon)?;
        show(&self.header.cast()?, layout.show_header)?;
        show(&self.description.cast()?, layout.show_description)?;
        show(&self.header_panel.cast()?, !layout.content_only)?;
        show(&self.content.cast()?, layout.show_content)?;
        self.root.SetRowSpacing(match layout.spacing {
            true => 8.0,
            false => 0.0,
        })?;
        self.written.layout = Some(layout);
        Ok(())
    }
}

/// A button whose content is `root`, restyled to the toolkit's card through
/// the button's lightweight resources.
fn clickable_shell(root: &Grid) -> Result<Button> {
    let button = Button::new()?;
    button.cast::<ContentControl>()?.SetContent(root)?;
    let control = button.cast::<XamlControl>()?;
    control.SetPadding(thickness(16.0, 16.0, 16.0, 16.0))?;
    control.SetHorizontalContentAlignment(HorizontalAlignment::Stretch)?;
    control.SetVerticalContentAlignment(VerticalAlignment::Center)?;
    let frame = button.cast::<FrameworkElement>()?;
    frame.SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
    size_like_a_card(&frame)?;
    let resources = frame
        .Resources()?
        .cast::<IMap<IInspectable, IInspectable>>()?;
    // The toolkit's SettingsCard brushes for each state. It leaves the
    // background and border alone when disabled.
    for (key, brush) in [
        ("ButtonBackground", "CardBackgroundFillColorDefaultBrush"),
        (
            "ButtonBackgroundPointerOver",
            "ControlFillColorSecondaryBrush",
        ),
        ("ButtonBackgroundPressed", "ControlFillColorTertiaryBrush"),
        (
            "ButtonBackgroundDisabled",
            "CardBackgroundFillColorDefaultBrush",
        ),
        ("ButtonForeground", "TextFillColorPrimaryBrush"),
        ("ButtonForegroundPointerOver", "TextFillColorPrimaryBrush"),
        ("ButtonForegroundPressed", "TextFillColorSecondaryBrush"),
        ("ButtonForegroundDisabled", "TextFillColorDisabledBrush"),
        ("ButtonBorderBrush", "CardStrokeColorDefaultBrush"),
        (
            "ButtonBorderBrushPointerOver",
            "ControlElevationBorderBrush",
        ),
        ("ButtonBorderBrushPressed", "ControlStrokeColorDefaultBrush"),
        ("ButtonBorderBrushDisabled", "CardStrokeColorDefaultBrush"),
    ] {
        resources.Insert(&boxed(key), &resource::<IInspectable>(brush)?)?;
    }
    Ok(button)
}

/// The toolkit's `SettingsCardMinHeight` and `SettingsCardMinWidth`.
fn size_like_a_card(frame: &FrameworkElement) -> Result<()> {
    frame.SetMinHeight(68.0)?;
    frame.SetMinWidth(148.0)
}

fn place(frame: &FrameworkElement, row: i32, column: i32) -> Result<()> {
    Grid::SetRow(frame, row)?;
    Grid::SetColumn(frame, column)
}

fn show(element: &UIElement, on: bool) -> Result<()> {
    element.SetVisibility(match on {
        true => Visibility::Visible,
        false => Visibility::Collapsed,
    })
}

fn thickness(left: f64, top: f64, right: f64, bottom: f64) -> Thickness {
    Thickness {
        left,
        top,
        right,
        bottom,
    }
}

#[cfg(test)]
mod tests;
