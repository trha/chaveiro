//! The widgets, and the [`Response`] each returns for imperative access.
//!
//! Each widget visits its node, creates the control the first time, and
//! writes a managed property only when its value differs from the value this
//! layer last wrote, or when the app invalidated the control.

use crate::{
    bindings::{
        AppBarButton, Application, AutomationProperties, Button, ComboBox, Control as XamlControl,
        DependencyObject, FocusState, FontIcon, FrameworkElement, Grid, HorizontalAlignment,
        ICommandBarElement, IPanel, IconElement, InfoBar, ItemsRepeater, MenuFlyoutItem,
        MenuFlyoutItemBase, MenuFlyoutSubItem, PasswordBox, StackPanel, Style, TextBlock, TextBox,
        TextWrapping, Thickness, TitleBar, ToggleMenuFlyoutItem, UIElement, UIElementCollection,
        Visibility,
    },
    events::EventKind,
    pass::Ui,
    theme::{clear_brush, set_brush, style_resources},
    tree::NodeId,
};
use windows_collections::{IMap, IVector};
use windows_core::{HSTRING, IInspectable, Interface, Result};
use windows_reference::IReference;

mod button;
mod cache;
mod check_box;
mod code_boxes;
mod combo_box;
mod command_bar;
mod dialog;
mod drag_bar;
mod flow;
mod font_icon;
mod grid;
mod icon_button;
mod info_bar;
mod label;
mod link;
mod list;
mod menu;
mod morph;
mod native;
mod navigation_view;
mod navigator;
mod password_box;
mod pressable;
mod scroll;
mod search_box;
mod settings_card;
pub(crate) mod shortcut;
mod split_button;
mod stack;
mod status;
mod task;
pub(crate) mod text_box;
mod title_bar;
mod window;

pub use button::ButtonResponse;
pub use check_box::CheckBoxResponse;
pub use code_boxes::{CODE_LENGTH, CodeBoxes, CodeDigits};
pub use combo_box::ComboBoxResponse;
pub use command_bar::AppBarButtonResponse;
pub use dialog::{DialogAnswer, DialogButtons, DialogResponse};
pub use flow::{Flow, FlowResponse, Lend, Lent, Owned, OwnedFamily, owned};
pub use font_icon::FontIconResponse;
pub use grid::Length;
pub use icon_button::IconButtonResponse;
pub use info_bar::Severity;
pub use label::{LabelResponse, Overflow};
pub use link::LinkResponse;
pub(crate) use list::index_source;
pub use list::{ListId, RowFn};
pub use menu::{MenuAnchor, MenuItemResponse, MenuResponse};
pub use morph::MORPH_DURATION;
pub use native::{NativeResponse, NativeSender};
pub use navigation_view::{NavigationEntry, NavigationSelection, NavigationViewResponse};
pub(crate) use navigator::Navigator;
pub use navigator::{SLIDE_DURATION, Slide};
pub use password_box::{PasswordField, PasswordHandle};
pub use pressable::PressableResponse;
pub use search_box::SearchBoxResponse;
pub use settings_card::{ContentAlignment, SettingsCard, SettingsCardResponse};
pub use split_button::SplitButtonResponse;
pub use stack::Orientation;
pub use status::ContentButtonResponse;
pub use task::{StreamResponse, TaskResponse};
pub use text_box::{Mask, TextBoxResponse};
pub use title_bar::TitleBarResponse;
pub use window::WindowResponse;
pub(crate) use window::reveal;

/// Access to the control a widget declared, for what the widget does not
/// cover.
///
/// A property this crate manages (a label's text, a stack's spacing) is
/// written again only when its declared value changes. After writing one
/// imperatively, call [`invalidate`](Self::invalidate), or the next change
/// may be skipped because the layer still holds the old value.
pub struct Response<'u> {
    pub(crate) ui: &'u mut Ui,
    pub(crate) node: NodeId,
}

impl Response<'_> {
    /// The control. Cast it to the interface you need with your own WinUI
    /// bindings, or use [`cast`](Self::cast).
    pub fn element(&self) -> IInspectable {
        self.ui.control(self.node).element.clone().into()
    }

    /// The control as another interface, such as its concrete type in your
    /// own WinUI bindings.
    pub fn cast<T: Interface>(&self) -> Result<T> {
        self.ui.control(self.node).element.cast()
    }

    /// Makes the next pass write every managed property of this control.
    pub fn invalidate(&mut self) {
        let control = self.ui.control_mut(self.node);
        control.stale = true;
        control.enabled = None;
        control.header.clear();
        control.style.clear();
        control.align = None;
        control.width = f64::NAN;
        control.padding = None;
        control.margin = None;
        control.dividers = None;
    }

    /// Sets whether the user can interact with the control. Fails when the
    /// control is not a XAML `Control`, such as a stack or a label.
    pub fn enabled(&mut self, on: bool) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.enabled != Some(on) {
            control.element.cast::<XamlControl>()?.SetIsEnabled(on)?;
            control.enabled = Some(on);
        }
        Ok(self)
    }

    /// Gives the control the app resource style named `key`, such as
    /// `TitleTextBlockStyle` for a label. Fails when no such style exists.
    pub fn style(&mut self, key: &str) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.style != key {
            control
                .element
                .cast::<FrameworkElement>()?
                .SetStyle(&resource::<Style>(key)?)?;
            control.style.clear();
            control.style.push_str(key);
        }
        Ok(self)
    }

    /// Where the control sits across the space its container gives it.
    pub fn align(&mut self, align: Align) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.align != Some(align) {
            control
                .element
                .cast::<FrameworkElement>()?
                .SetHorizontalAlignment(match align {
                    Align::Start => HorizontalAlignment::Left,
                    Align::Center => HorizontalAlignment::Center,
                    Align::End => HorizontalAlignment::Right,
                    Align::Stretch => HorizontalAlignment::Stretch,
                })?;
            control.align = Some(align);
        }
        Ok(self)
    }

    /// Where the control sits down the space its container gives it, such
    /// as centered beside a taller button.
    pub fn valign(&mut self, align: Align) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.valign != Some(align) {
            control
                .element
                .cast::<FrameworkElement>()?
                .SetVerticalAlignment(match align {
                    Align::Start => crate::bindings::VerticalAlignment::Top,
                    Align::Center => crate::bindings::VerticalAlignment::Center,
                    Align::End => crate::bindings::VerticalAlignment::Bottom,
                    Align::Stretch => crate::bindings::VerticalAlignment::Stretch,
                })?;
            control.valign = Some(align);
        }
        Ok(self)
    }

    /// Sets the control's width in view pixels.
    pub fn width(&mut self, width: f64) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.width != width {
            control
                .element
                .cast::<FrameworkElement>()?
                .SetWidth(width)?;
            control.width = width;
        }
        Ok(self)
    }

    /// Sets the space outside the control's edges, in view pixels: left,
    /// top, right, bottom. In a stack it adds to the stack's spacing.
    pub fn margin(&mut self, margin: [f64; 4]) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.margin != Some(margin) {
            let [left, top, right, bottom] = margin;
            control
                .element
                .cast::<FrameworkElement>()?
                .SetMargin(Thickness {
                    left,
                    top,
                    right,
                    bottom,
                })?;
            control.margin = Some(margin);
        }
        Ok(self)
    }

    /// Sets the space inside a stack's edges, in view pixels: left, top,
    /// right, bottom.
    pub fn padding(&mut self, padding: [f64; 4]) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.padding != Some(padding) {
            let Control::Stack { panel, .. } = &control.control else {
                panic!("only stacks take padding");
            };
            let [left, top, right, bottom] = padding;
            panel.SetPadding(Thickness {
                left,
                top,
                right,
                bottom,
            })?;
            control.padding = Some(padding);
        }
        Ok(self)
    }

    /// Draws a 1px divider line on the control's edges where `edges` is
    /// true: left, top, right, bottom. The line takes space inside the
    /// control, like padding. Its color is the theme's when first drawn.
    ///
    /// It replaces the control's own border, so on a card, a settings card,
    /// or a field that shows an error, the two fight. A label, icon, or list
    /// has no border; wrap it in a grid or stack. Panics on those.
    pub fn dividers(&mut self, edges: [bool; 4]) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.dividers == Some(edges) {
            return Ok(self);
        }
        // Grid, StackPanel and Control each declare their own border
        // properties, with no shared interface.
        let first = control.dividers.is_none();
        let [left, top, right, bottom] = edges.map(|on| if on { 1.0 } else { 0.0 });
        let thickness = Thickness {
            left,
            top,
            right,
            bottom,
        };
        let element = &control.element;
        const DIVIDER: &str = "DividerStrokeColorDefaultBrush";
        if let Ok(grid) = element.cast::<Grid>() {
            if first {
                set_brush(&grid, &Grid::BorderBrushProperty()?, DIVIDER)?;
            }
            grid.SetBorderThickness(thickness)?;
        } else if let Ok(panel) = element.cast::<StackPanel>() {
            if first {
                set_brush(&panel, &StackPanel::BorderBrushProperty()?, DIVIDER)?;
            }
            panel.SetBorderThickness(thickness)?;
        } else if let Ok(xaml) = element.cast::<XamlControl>() {
            if first {
                set_brush(&xaml, &XamlControl::BorderBrushProperty()?, DIVIDER)?;
            }
            xaml.SetBorderThickness(thickness)?;
        } else {
            panic!("this control has no border to draw dividers on");
        }
        control.dividers = Some(edges);
        Ok(self)
    }

    /// Sets the key that, after Alt, focuses the control. Pressing Alt shows the
    /// key on the control. Keep keys unique within a window.
    pub fn access_key(&mut self, key: &str) -> Result<&mut Self> {
        let control = self.ui.control_mut(self.node);
        if control.access_key != key {
            control.element.SetAccessKey(key)?;
            control.access_key.clear();
            control.access_key.push_str(key);
        }
        Ok(self)
    }

    /// Moves keyboard focus to the control, as code does, so no focus
    /// rectangle shows. Returns whether the control took focus; one not
    /// yet in the window cannot, so call it again on a later pass.
    pub fn focus(&mut self) -> Result<bool> {
        self.ui
            .control(self.node)
            .element
            .Focus(FocusState::Programmatic)
    }

    /// Whether keyboard focus left the control since the last pass. The
    /// first call subscribes it.
    pub fn left(&mut self) -> Result<bool> {
        self.ui
            .subscribe(self.node, &[EventKind::LostFocus], |control| {
                &control.element
            })?;
        Ok(self.ui.events.happened(self.node, EventKind::LostFocus))
    }

    /// Writes the error line under a text box, password box or combo box:
    /// a 12px critical message with an error icon, and a critical border.
    /// The first call reserves the line, 16px tall, so a later error never
    /// moves the controls below; pass [`None`] for a field that is fine,
    /// or that cannot fail but sits in a form whose fields can. Screen
    /// readers read the message as the field's error.
    pub(crate) fn set_error(&mut self, message: Option<&str>) -> Result<()> {
        let made = self.error_line()?;
        let control = self.ui.control_mut(self.node);
        let line = control.error.as_mut().expect("made above");
        if !made && line.message.as_deref() == message {
            return Ok(());
        }
        line.message = message.map(str::to_owned);
        line.render()?;
        let element = &control.element;
        mark_critical(element, message.is_some())?;
        let target = element.cast::<DependencyObject>()?;
        AutomationProperties::SetFullDescription(&target, message.unwrap_or(""))?;
        AutomationProperties::SetIsDataValidForForm(&target, message.is_none())?;
        Ok(())
    }

    /// Writes the caption the error line shows while the field has no
    /// error.
    pub(crate) fn set_hint(&mut self, hint: &str) -> Result<()> {
        self.error_line()?;
        let line = self
            .ui
            .control_mut(self.node)
            .error
            .as_mut()
            .expect("made above");
        if line.hint != hint {
            line.hint.clear();
            line.hint.push_str(hint);
            line.render()?;
        }
        Ok(())
    }

    /// Makes the error line under the field, once. Returns whether it was
    /// made now.
    fn error_line(&mut self) -> Result<bool> {
        let control = self.ui.control_mut(self.node);
        if control.error.is_some() {
            return Ok(false);
        }
        let line = ErrorLine::new()?;
        let row: IInspectable = line.row.cast()?;
        match &control.control {
            Control::TextBox { text_box, .. } => text_box.SetDescription(&row)?,
            Control::PasswordBox { password_box, .. } => password_box.SetDescription(&row)?,
            Control::ComboBox { combo_box, .. } => combo_box.SetDescription(&row)?,
            _ => unreachable!("only fields have error lines"),
        }
        control.error = Some(line);
        Ok(true)
    }

    /// Writes the header of a text box, password box or combo box.
    pub(crate) fn set_header(&mut self, text: &str) -> Result<()> {
        let control = self.ui.control_mut(self.node);
        if control.header == text {
            return Ok(());
        }
        control.header.clear();
        control.header.push_str(text);
        write_header(control)
    }

    /// Marks a field as one the form needs: its header ends in ` *`, and
    /// screen readers say it is required. Call it with or without
    /// [`header`](TextBoxResponse::header), in any order.
    pub(crate) fn set_required(&mut self, on: bool) -> Result<()> {
        let control = self.ui.control_mut(self.node);
        if control.required == on {
            return Ok(());
        }
        control.required = on;
        AutomationProperties::SetIsRequiredForForm(
            &control.element.cast::<DependencyObject>()?,
            on,
        )?;
        write_header(control)
    }
}

/// Gives a field the critical border, or its theme border back.
fn mark_critical(element: &UIElement, on: bool) -> Result<()> {
    let property = XamlControl::BorderBrushProperty()?;
    match on {
        true => set_brush(element, &property, "SystemFillColorCriticalBrush"),
        false => clear_brush(element, &property),
    }
}

/// Writes the header of a field from its recorded text and required mark.
/// The accessible name stays the bare text, so the `*` is not read aloud.
fn write_header(control: &mut crate::pass::ControlNode) -> Result<()> {
    let shown = match control.required {
        true => format!("{} *", control.header),
        false => control.header.clone(),
    };
    let header = boxed(&shown);
    match &control.control {
        Control::TextBox { text_box, .. } => text_box.SetHeader(&header)?,
        Control::PasswordBox { password_box, .. } => password_box.SetHeader(&header)?,
        Control::ComboBox { combo_box, .. } => combo_box.SetHeader(&header)?,
        _ => unreachable!("only fields have headers"),
    }
    AutomationProperties::SetName(
        &control.element.cast::<DependencyObject>()?,
        &control.header,
    )?;
    Ok(())
}

/// Names a text box, password box or combo box during the pass that
/// declared it, for changes decided after later controls, such as an error
/// found when the form's button is read.
#[derive(Clone, Copy, Debug)]
pub struct FieldHandle(pub(crate) NodeId);

impl Ui {
    /// [`TextBoxResponse::error`] for the field `field` names.
    pub fn set_error(&mut self, field: FieldHandle, message: Option<&str>) -> Result<()> {
        Response {
            ui: self,
            node: field.0,
        }
        .set_error(message)
    }

    /// [`Response::focus`] for the field `field` names. Returns whether
    /// the field took focus; one not yet in the window cannot, so call it
    /// again on a later pass.
    pub fn focus(&mut self, field: FieldHandle) -> Result<bool> {
        self.control(field.0)
            .element
            .Focus(FocusState::Programmatic)
    }
}

impl Response<'_> {
    /// A handle to this control, for [`Ui::set_error`] and [`Ui::focus`].
    pub fn field(&self) -> FieldHandle {
        FieldHandle(self.node)
    }
}

/// The line under a field that shows its error.
pub(crate) struct ErrorLine {
    row: StackPanel,
    icon: UIElement,
    text: TextBlock,
    /// The error shown; [`None`] while the field is fine.
    message: Option<String>,
    /// The caption shown while there is no error; empty for none.
    hint: String,
}

impl ErrorLine {
    fn new() -> Result<Self> {
        const CRITICAL: &str = "SystemFillColorCriticalBrush";
        let row = StackPanel::new()?;
        row.SetOrientation(crate::bindings::Orientation::Horizontal)?;
        row.SetSpacing(4.0)?;
        let frame = row.cast::<FrameworkElement>()?;
        frame.SetMinHeight(16.0)?;
        frame.SetMargin(Thickness {
            left: 0.0,
            top: 4.0,
            right: 0.0,
            bottom: 0.0,
        })?;
        let glyph = FontIcon::new()?;
        glyph.SetGlyph("\u{EA39}")?;
        glyph.SetFontSize(12.0)?;
        set_brush(&glyph, &IconElement::ForegroundProperty()?, CRITICAL)?;
        let icon: UIElement = glyph.cast()?;
        icon.SetVisibility(Visibility::Collapsed)?;
        let text = TextBlock::new()?;
        text.SetFontSize(12.0)?;
        set_brush(&text, &TextBlock::ForegroundProperty()?, CRITICAL)?;
        text.SetTextWrapping(TextWrapping::Wrap)?;
        let children = row.cast::<IPanel>()?.Children()?;
        children.Append(&icon)?;
        children.Append(&text.cast::<UIElement>()?)?;
        Ok(ErrorLine {
            row,
            icon,
            text,
            message: None,
            hint: String::new(),
        })
    }

    /// Shows the error with its icon in the critical color, or else the
    /// hint in the secondary text color.
    fn render(&self) -> Result<()> {
        let (visibility, text, brush) = match &self.message {
            Some(message) => (
                Visibility::Visible,
                message.as_str(),
                "SystemFillColorCriticalBrush",
            ),
            None => (
                Visibility::Collapsed,
                self.hint.as_str(),
                "TextFillColorSecondaryBrush",
            ),
        };
        self.icon.SetVisibility(visibility)?;
        self.text.SetText(text)?;
        set_brush(&self.text, &TextBlock::ForegroundProperty()?, brush)
    }
}

/// Where a control sits across its container, for [`Response::align`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

/// `text` as a XAML object, for a header or a list item.
pub(crate) fn boxed(text: &str) -> IInspectable {
    IReference::<HSTRING>::from(text).into()
}

/// A resource of the app's merged dictionaries, such as a brush or a style,
/// by its XAML key. A theme brush is the one for the theme at lookup time,
/// so set a theme brush with [`set_brush`], which follows the theme.
pub(crate) fn resource<T: Interface>(key: &str) -> Result<T> {
    Application::Current()?
        .Resources()?
        .cast::<IMap<IInspectable, IInspectable>>()?
        .Lookup(&boxed(key))?
        .cast()
}

/// The concrete control behind a node, with the values this layer last wrote.
pub(crate) enum Control {
    Stack {
        panel: StackPanel,
        children: UIElementCollection,
        orientation: Option<Orientation>,
        /// `NaN` until first written.
        spacing: f64,
        /// `NaN` until first written; only cards write it. A card group
        /// writes infinity, because its width is its container's.
        width: f64,
        /// The brush and corners [`Response::fill`] wrote.
        fill: Option<(&'static str, [f64; 4])>,
    },
    /// From [`Ui::grid`], or from [`Ui::columns`] when `across`.
    Grid {
        grid: Grid,
        children: UIElementCollection,
        /// The children fill columns, not rows. Fixed at creation.
        across: bool,
        /// The row heights, or column widths when `across`, as last written.
        lengths: Vec<Length>,
        /// `NaN` until first written; only grids of columns write it.
        spacing: f64,
    },
    Label {
        block: TextBlock,
        text: String,
        look: label::Look,
    },
    /// From [`Ui::pressable`].
    Pressable(Box<pressable::Parts>),
    /// From [`Ui::morph`].
    Morph(Box<morph::Parts>),
    /// From [`Ui::scroll`].
    Scroll {
        content: Option<NodeId>,
    },
    /// From [`Ui::icon_button`].
    IconButton {
        button: Button,
        icon: FontIcon,
        /// [`None`] until first written.
        glyph: Option<char>,
        label: String,
    },
    /// From [`Ui::command_bar`].
    CommandBar {
        commands: IVector<ICommandBarElement>,
    },
    /// From [`Ui::app_bar_button`].
    AppBarButton {
        button: AppBarButton,
        icon: FontIcon,
        /// [`None`] until first written.
        glyph: Option<char>,
        label: String,
    },
    Button {
        button: Button,
        content: TextBlock,
        label: String,
        accent: bool,
        /// Set by [ButtonResponse::subtle]; never turns off.
        subtle: bool,
        icon_accent: bool,
        /// Made on the first busy or icon pass; then the content is a row
        /// of the icon, the ring and `content`.
        row: Option<button::ButtonRow>,
        /// [`None`] until an icon is written.
        glyph: Option<char>,
        /// Empty until a tooltip is written.
        tooltip: String,
        busy: bool,
    },
    ComboBox {
        combo_box: ComboBox,
        /// The items as last written.
        items: Vec<String>,
        /// The selected index as last written or read back; -1 for none.
        selected: i32,
        /// For an editable box, its text as last written or read back.
        text: Option<String>,
    },
    InfoBar {
        info_bar: InfoBar,
        severity: Option<Severity>,
        title: String,
        message: String,
    },
    TextBox {
        text_box: TextBox,
        /// The control's text as last written or read back.
        text: String,
        placeholder: String,
        /// Whether an IME composition is open; app writes wait until it ends.
        composing: bool,
    },
    /// From [`Ui::search_box`].
    /// Its fields mean what [`Control::TextBox`]'s do, so the two share
    /// one binding; the node's element is a grid around `text_box`.
    SearchBox {
        text_box: TextBox,
        /// The control's text as last written or read back.
        text: String,
        placeholder: String,
        /// Whether an IME composition is open; app writes wait until it ends.
        composing: bool,
        /// The letter of the Ctrl shortcut, as last set.
        shortcut: Option<char>,
    },
    PasswordBox {
        password_box: PasswordBox,
        placeholder: String,
    },
    /// From [`Ui::native`]: the element and the app's state, as a
    /// `native::Parts` of the types the app declared.
    Native(Box<dyn std::any::Any>),
    List {
        repeater: ItemsRepeater,
        data: Box<list::ListData>,
    },
    /// The `ContentControl` shell WinUI gets for one list row; the row's
    /// control is its content.
    Row {
        content: Option<NodeId>,
    },
    CheckBox {
        /// The toggle base, which holds `IsChecked`.
        check_box: crate::bindings::ToggleButton,
        content: TextBlock,
        label: String,
        checked: bool,
    },
    Link {
        content: TextBlock,
        label: String,
    },
    /// From [`Ui::settings_card`].
    SettingsCard(Box<settings_card::Parts>),
    /// From [`Ui::navigator`]: one screen, and the old ones sliding out.
    Navigator(Box<Navigator>),
    TitleBar {
        title_bar: TitleBar,
        title: String,
        /// Whether the back button shows, as last written.
        back: Option<bool>,
        /// The control in the bar's middle, from the body.
        content: Option<NodeId>,
        /// The control at the bar's right end, from the body.
        right: Option<NodeId>,
    },
    /// From [`Ui::drag_bar`].
    DragBar {
        children: UIElementCollection,
        clickable: std::rc::Rc<std::cell::RefCell<drag_bar::Clickable>>,
        _layout: windows_core::EventRevoker,
    },
    /// From [`Ui::content_button`]: a subtle button whose content is the
    /// one control its body declares.
    ContentButton {
        label: String,
        content: Option<NodeId>,
    },
    PersonPicture {
        picture: crate::bindings::PersonPicture,
        initials: String,
        /// NaN until first written.
        size: f64,
    },
    ProgressBar,
    ProgressRing,
    /// From [`Ui::tag`].
    Tag {
        text: TextBlock,
        icon: FontIcon,
        label: String,
        glyph: Option<char>,
        tooltip: String,
    },
    FontIcon {
        icon: FontIcon,
        /// [`None`] until first written.
        glyph: Option<char>,
        /// `NaN` until first written.
        size: f64,
        secondary: bool,
    },
    /// From [`Ui::navigation_view`]: the pane's items, and one content
    /// control.
    NavigationView(Box<navigation_view::Parts>),
    /// From [`Ui::menu_item`].
    MenuItem {
        item: MenuFlyoutItem,
        look: menu::Look,
        critical: bool,
        /// The shortcut text last written; empty until first written.
        hint: String,
    },
    /// From [`Ui::menu_toggle`].
    MenuToggle {
        item: ToggleMenuFlyoutItem,
        look: menu::Look,
        /// [`None`] until written, and after a click, when WinUI flipped it.
        checked: Option<bool>,
    },
    /// From [`Ui::submenu`]: its items.
    SubMenu {
        item: MenuFlyoutSubItem,
        items: IVector<MenuFlyoutItemBase>,
        look: menu::Look,
    },
    MenuSeparator,
    /// From [`Ui::split_button`]: its parts and menu items.
    SplitButton(Box<split_button::Parts>),
}

impl Control {
    /// The child collection of a panel, or [`None`] for other controls.
    pub(crate) fn children(&self) -> Option<&UIElementCollection> {
        match self {
            Control::Stack { children, .. }
            | Control::Grid { children, .. }
            | Control::DragBar { children, .. } => Some(children),
            Control::SettingsCard(parts) => Some(&parts.children),
            Control::Morph(parts) => Some(&parts.children),
            _ => None,
        }
    }

    /// The one content control of a `ContentControl` this layer fills with
    /// the control its body declares, or [`None`] for other controls.
    /// The control takes clicks or typing, so a drag bar keeps it clickable.
    pub(crate) fn takes_input(&self) -> bool {
        matches!(
            self,
            Control::Button { .. }
                | Control::IconButton { .. }
                | Control::ContentButton { .. }
                | Control::AppBarButton { .. }
                | Control::TextBox { .. }
                | Control::SearchBox { .. }
                | Control::PasswordBox { .. }
                | Control::ComboBox { .. }
                | Control::CheckBox { .. }
                | Control::Link { .. }
                | Control::Pressable(_)
                | Control::Tag { .. }
                | Control::SplitButton(_)
        )
    }

    pub(crate) fn content_slot(&mut self) -> Option<&mut Option<NodeId>> {
        match self {
            Control::Row { content, .. }
            | Control::Scroll { content }
            | Control::ContentButton { content, .. } => Some(content),
            Control::NavigationView(parts) => Some(&mut parts.content),
            _ => None,
        }
    }
}

impl Response<'_> {
    /// Whether the control, a `ButtonBase`, was clicked since the last
    /// pass. The first call subscribes it.
    pub(crate) fn button_clicked(&mut self) -> Result<bool> {
        self.ui
            .subscribe(self.node, &[EventKind::Click], |control| &control.element)?;
        Ok(self.ui.events.happened(self.node, EventKind::Click))
    }
}

/// Restyles `button` through its lightweight resources to the subtle look:
/// no fill or border until the pointer is over it, as for a toolbar button
/// or a list row.
pub(crate) fn make_subtle(button: &Button) -> Result<()> {
    style_resources(button, SUBTLE_BUTTON)
}

/// The subtle button's brushes for each state.
const SUBTLE_BUTTON: &[(&str, &str)] = &[
    ("ButtonBackground", "SubtleFillColorTransparentBrush"),
    (
        "ButtonBackgroundPointerOver",
        "SubtleFillColorSecondaryBrush",
    ),
    ("ButtonBackgroundPressed", "SubtleFillColorTertiaryBrush"),
    (
        "ButtonBackgroundDisabled",
        "SubtleFillColorTransparentBrush",
    ),
    ("ButtonForeground", "TextFillColorPrimaryBrush"),
    ("ButtonForegroundPointerOver", "TextFillColorPrimaryBrush"),
    ("ButtonForegroundPressed", "TextFillColorSecondaryBrush"),
    ("ButtonForegroundDisabled", "TextFillColorDisabledBrush"),
    ("ButtonBorderBrush", "SubtleFillColorTransparentBrush"),
    (
        "ButtonBorderBrushPointerOver",
        "SubtleFillColorTransparentBrush",
    ),
    (
        "ButtonBorderBrushPressed",
        "SubtleFillColorTransparentBrush",
    ),
    (
        "ButtonBorderBrushDisabled",
        "SubtleFillColorTransparentBrush",
    ),
];

/// Whether a managed value must be written: the control is stale, or the
/// declared value differs from the last one written.
fn must_write<T: ?Sized + PartialEq>(stale: bool, written: &T, declared: &T) -> bool {
    stale || written != declared
}
