//! [`Ui::button`] rebuilt on [`Ui::native`], as an app outside the crate
//! would write it: a response type with chained methods, and a click
//! handler registered on the first ask, to test that `native` can carry a
//! widget with an open set of options.

use crate::{
    NativeResponse, Response, Ui,
    bindings::{
        AutomationProperties, Button, ContentControl, DependencyObject, FrameworkElement,
        IButtonBase, Style, TextBlock,
    },
    events::Delegate,
    widget::resource,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

/// The values last written into the button.
struct Written {
    content: TextBlock,
    label: String,
    accent: bool,
    /// Whether the click handler is registered.
    clicks: bool,
}

struct Clicked;

/// As [`Ui::button`].
#[expect(dead_code, reason = "a compile test of the native API")]
pub(crate) fn button<'u>(ui: &'u mut Ui, id: impl Hash, label: &str) -> Result<MyButton<'u>> {
    let mut native = ui.native(id, |_| {
        let button = Button::new()?;
        let content = TextBlock::new()?;
        button.cast::<ContentControl>()?.SetContent(&content)?;
        let written = Written {
            content,
            label: String::new(),
            accent: false,
            clicks: false,
        };
        Ok((button, written))
    })?;
    let stale = native.stale();
    let (button, written) = native.split();
    if stale || written.label != label {
        written.content.SetText(label)?;
        AutomationProperties::SetName(&button.cast::<DependencyObject>()?, label)?;
        written.label.clear();
        written.label.push_str(label);
    }
    Ok(MyButton(native))
}

/// What [`button`] returns.
pub(crate) struct MyButton<'u>(NativeResponse<'u, Button, Written, Clicked>);

impl MyButton<'_> {
    /// Whether the button was clicked since the last pass. The first call
    /// registers the handler, so it returns false.
    #[expect(dead_code, reason = "a compile test of the native API")]
    pub(crate) fn clicked(&mut self) -> Result<bool> {
        if !self.0.state().clicks {
            let sender = self.0.sender().clone();
            Delegate::routed(Box::new(move || sender.send(Clicked)))
                .register(&self.0.element().cast::<IButtonBase>()?, |table| {
                    table.Click
                })?;
            self.0.state().clicks = true;
        }
        Ok(!self.0.events().is_empty())
    }

    /// Gives the button the accent style, or the standard style.
    #[expect(dead_code, reason = "a compile test of the native API")]
    pub(crate) fn accent(&mut self, on: bool) -> Result<&mut Self> {
        let stale = self.0.stale();
        let (button, written) = self.0.split();
        if stale || written.accent != on {
            let frame = button.cast::<FrameworkElement>()?;
            match on {
                true => frame.SetStyle(&resource::<Style>("AccentButtonStyle")?)?,
                false => frame.SetStyle(None::<&Style>)?,
            }
            written.accent = on;
        }
        Ok(self)
    }
}

impl<'u> Deref for MyButton<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'u> DerefMut for MyButton<'u> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
