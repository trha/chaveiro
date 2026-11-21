//! The widgets, and the [`Response`] each returns for imperative access.
//!
//! Each widget visits its node, creates the control the first time, and
//! writes a managed property only when its value differs from the value this
//! layer last wrote, or when the app invalidated the control.

use crate::{
    bindings::{
        Application, AutomationProperties, Brush, Button, ComboBox, ContentControl,
        Control as XamlControl, DependencyObject, FocusState, FontIcon, FrameworkElement,
        HorizontalAlignment, IPanel, IconElement, InfoBar, ItemsRepeater, PasswordBox,
        ProgressRing, RowDefinitionCollection, StackPanel, Style, TextBlock, TextBox, TextWrapping,
        Thickness, UIElement, UIElementCollection, Visibility,
    },
    events::EventKind,
    pass::Ui,
    tree::NodeId,
};
use windows_collections::IMap;
use windows_core::{HSTRING, IInspectable, Interface, Result};
use windows_reference::IReference;

mod button;
mod cache;
mod combo_box;
mod grid;
mod info_bar;
mod label;
mod list;
mod native;
mod password_box;
mod stack;
mod text_box;
mod window;

pub use button::ButtonResponse;
pub use combo_box::ComboBoxResponse;
pub use grid::Length;
pub use info_bar::Severity;
pub(crate) use list::index_source;
pub use list::{ListId, RowFn};
pub use password_box::{PasswordField, PasswordHandle};
pub use stack::Orientation;
pub use text_box::TextBoxResponse;
pub use window::WindowResponse;
pub(crate) use window::cloak;

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
    /// rectangle shows.
    pub fn focus(&mut self) -> Result<&mut Self> {
        self.ui
            .control(self.node)
            .element
            .Focus(FocusState::Programmatic)?;
        Ok(self)
    }

    /// Whether keyboard focus left the control since the last pass. The
    /// first call subscribes it.
    pub fn left(&mut self) -> Result<bool> {
        let control = self.ui.control_mut(self.node);
        let element = control.element.clone();
        let mut subscribed = control.subscribed;
        self.ui
            .events
            .subscribe(self.node, &mut subscribed, &element, EventKind::LostFocus)?;
        self.ui.control_mut(self.node).subscribed = subscribed;
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
        match message {
            Some(_) => element
                .cast::<XamlControl>()?
                .SetBorderBrush(&resource::<Brush>("SystemFillColorCriticalBrush")?)?,
            None => element
                .cast::<DependencyObject>()?
                .ClearValue(&XamlControl::BorderBrushProperty()?)?,
        }
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

    /// [`Response::focus`] for the field `field` names.
    pub fn focus(&mut self, field: FieldHandle) -> Result<()> {
        Response {
            ui: self,
            node: field.0,
        }
        .focus()?;
        Ok(())
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
        let critical = resource::<Brush>("SystemFillColorCriticalBrush")?;
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
        glyph.cast::<IconElement>()?.SetForeground(&critical)?;
        let icon: UIElement = glyph.cast()?;
        icon.SetVisibility(Visibility::Collapsed)?;
        let text = TextBlock::new()?;
        text.SetFontSize(12.0)?;
        text.SetForeground(&critical)?;
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
        self.text.SetForeground(&resource::<Brush>(brush)?)
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
/// by its XAML key. A theme brush is the one for the theme at lookup time.
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
        /// `NaN` until first written; only cards write it.
        width: f64,
    },
    Grid {
        children: UIElementCollection,
        definitions: RowDefinitionCollection,
        rows: Vec<Length>,
    },
    Label {
        block: TextBlock,
        text: String,
    },
    Button {
        button: Button,
        content: TextBlock,
        label: String,
        accent: bool,
        /// Made on the first busy pass; then the content is a row of the
        /// ring and `content`.
        ring: Option<ProgressRing>,
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
    PasswordBox {
        password_box: PasswordBox,
        placeholder: String,
    },
    /// From [`Ui::native`]: only the element is known.
    Native,
    List {
        repeater: ItemsRepeater,
        data: Box<list::ListData>,
    },
    /// The shell WinUI gets for one list row; the row's control is its
    /// content.
    Row {
        shell: ContentControl,
        content: Option<NodeId>,
    },
}

impl Control {
    /// The child collection of a panel, or [`None`] for other controls.
    pub(crate) fn children(&self) -> Option<&UIElementCollection> {
        match self {
            Control::Stack { children, .. } | Control::Grid { children, .. } => Some(children),
            _ => None,
        }
    }
}

/// Whether a managed value must be written: the control is stale, or the
/// declared value differs from the last one written.
fn must_write<T: ?Sized + PartialEq>(stale: bool, written: &T, declared: &T) -> bool {
    stale || written != declared
}
