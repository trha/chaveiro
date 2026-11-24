//! Menus: a `MenuFlyout` the app declares only while it is open, and its
//! items.
//!
//! A menu opens when the app first declares it, at a [`MenuAnchor`], and
//! the app keeps declaring it until [`MenuResponse::closed`]. Its items are
//! nodes like any control, so a click reads like a button's.

use super::{Control, Response, must_write, resource};
use crate::{
    bindings::{
        Control as XamlControl, DependencyObject, DesktopAcrylicBackdrop, Flyout, FlyoutBase,
        FlyoutPlacementMode, FlyoutShowOptions, FontIcon, IconElement, MenuFlyout, MenuFlyoutItem,
        MenuFlyoutSeparator, MenuFlyoutSubItem, Point, Setter, SetterBase, Style,
        ToggleMenuFlyoutItem, TypeKind, TypeName,
    },
    events::EventKind,
    pass::{ChildKind, ControlNode, FlyoutNode, MenuNode, NativeNode, NativeTree, Payload, Ui},
    tree::NodeId,
};
use immediate_tree::Node;
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{IInspectable, Interface, Result};

impl Ui {
    /// A menu of the items `body` declares, opened at `anchor` on the pass
    /// that first declares it. Keep declaring it, with the same `id`, until
    /// [`MenuResponse::closed`]; a pass that stops declaring it closes it.
    /// An anchor whose control is gone shows nothing and reads as closed.
    pub fn menu(
        &mut self,
        id: impl Hash,
        anchor: MenuAnchor,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<MenuResponse> {
        let (node, _) = self.visit(id, ChildKind::Logical, || {
            let flyout = MenuFlyout::new()?;
            // The menu shows at once, with no slide or fade.
            flyout
                .cast::<FlyoutBase>()?
                .SetAreOpenCloseAnimationsEnabled(false)?;
            let items = flyout.Items()?;
            Ok(Payload::Menu(Box::new(MenuNode {
                flyout,
                items,
                visual: Vec::new(),
                shown: false,
                subscribed: Default::default(),
            })))
        })?;
        self.subscribe_native(node, &[EventKind::FlyoutClosed], |payload| match payload {
            Payload::Menu(menu) => (&menu.flyout, &mut menu.subscribed),
            _ => unreachable!("menu keys hold menus"),
        })?;
        self.open(node);
        body(self)?;
        self.close()?;
        let mut closed = self.events.happened(node, EventKind::FlyoutClosed);
        let Payload::Menu(menu) = self.core.tree.native(node) else {
            unreachable!("menu keys hold menus")
        };
        if !menu.shown {
            if !self.show_at(&menu.flyout.cast()?, anchor)? {
                closed = true;
            }
            let Payload::Menu(menu) = self.core.tree.native_mut(node) else {
                unreachable!("menu keys hold menus")
            };
            menu.shown = true;
        }
        Ok(MenuResponse { closed })
    }

    /// A flyout around the one control `body` declares, such as an account
    /// card, opened at `anchor` on the pass that first declares it. It
    /// opens and closes as [`menu`](Self::menu) does.
    pub fn flyout(
        &mut self,
        id: impl Hash,
        anchor: MenuAnchor,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<MenuResponse> {
        let (node, _) = self.visit(id, ChildKind::Logical, || {
            let flyout = Flyout::new()?;
            let base = flyout.cast::<FlyoutBase>()?;
            base.SetAreOpenCloseAnimationsEnabled(false)?;
            // Acrylic, as a menu has by default. A system backdrop draws
            // only on a popup of its own window, and the presenter's own fill
            // would cover it, so its style makes the fill transparent.
            base.SetShouldConstrainToRootBounds(false)?;
            base.cast::<crate::bindings::IFlyoutBase2>()?
                .SetSystemBackdrop(&DesktopAcrylicBackdrop::new()?)?;
            flyout.SetFlyoutPresenterStyle(&transparent_presenter()?)?;
            Ok(Payload::Flyout(Box::new(FlyoutNode {
                flyout,
                content: None,
                shown: false,
                subscribed: Default::default(),
            })))
        })?;
        self.subscribe_native(node, &[EventKind::FlyoutClosed], |payload| match payload {
            Payload::Flyout(popup) => (&popup.flyout, &mut popup.subscribed),
            _ => unreachable!("flyout keys hold flyouts"),
        })?;
        self.open(node);
        body(self)?;
        self.close()?;
        let mut closed = self.events.happened(node, EventKind::FlyoutClosed);
        let Payload::Flyout(popup) = self.core.tree.native(node) else {
            unreachable!("flyout keys hold flyouts")
        };
        if !popup.shown {
            if !self.show_at(&popup.flyout.cast()?, anchor)? {
                closed = true;
            }
            let Payload::Flyout(popup) = self.core.tree.native_mut(node) else {
                unreachable!("flyout keys hold flyouts")
            };
            popup.shown = true;
        }
        Ok(MenuResponse { closed })
    }

    /// Shows `flyout` at `anchor`. Gives false when the anchor's control
    /// is gone, and nothing shows.
    fn show_at(&self, flyout: &FlyoutBase, anchor: MenuAnchor) -> Result<bool> {
        let Some(Node::Native(NativeNode(Payload::Control(control)))) =
            self.core.tree.get(anchor.node).map(|node| &node.payload)
        else {
            return Ok(false);
        };
        let options = FlyoutShowOptions::new()?;
        match (anchor.position, anchor.right) {
            // At the pointer, as a right-click menu opens.
            (Some(position), _) => options.SetPosition(Some(position))?,
            // Under the control, as a button's menu opens.
            (None, false) => options.SetPlacement(FlyoutPlacementMode::BottomEdgeAlignedLeft)?,
            (None, true) => options.SetPlacement(FlyoutPlacementMode::BottomEdgeAlignedRight)?,
        }
        flyout.ShowAtWithOptions(&control.element.cast::<DependencyObject>()?, &options)?;
        Ok(true)
    }

    /// A menu item: `glyph`, a Segoe Fluent Icons glyph, and `label`.
    pub fn menu_item(
        &mut self,
        id: impl Hash,
        glyph: char,
        label: &str,
    ) -> Result<MenuItemResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let item = MenuFlyoutItem::new()?;
            Ok(ControlNode::new(
                item.cast()?,
                Control::MenuItem {
                    item,
                    look: Look::default(),
                    critical: false,
                    hint: String::new(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = control.stale;
        let Control::MenuItem { item, look, .. } = &mut control.control else {
            unreachable!("menu item keys hold menu items")
        };
        look.write(stale, item, Some(glyph), label)?;
        Ok(MenuItemResponse(Response { ui: self, node }))
    }

    /// A menu item with a check mark while `checked`, and `glyph` if any. A
    /// click does not change `checked`: the app does, as for a button.
    pub fn menu_toggle(
        &mut self,
        id: impl Hash,
        glyph: Option<char>,
        label: &str,
        checked: bool,
    ) -> Result<MenuItemResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let item = ToggleMenuFlyoutItem::new()?;
            Ok(ControlNode::new(
                item.cast()?,
                Control::MenuToggle {
                    item,
                    look: Look::default(),
                    checked: None,
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = control.stale;
        let Control::MenuToggle {
            item,
            look,
            checked: written,
        } = &mut control.control
        else {
            unreachable!("menu toggle keys hold menu toggles")
        };
        look.write(stale, item, glyph, label)?;

        if must_write(stale, written, &Some(checked)) {
            item.SetIsChecked(checked)?;
            *written = Some(checked);
        }
        Ok(MenuItemResponse(Response { ui: self, node }))
    }

    /// An item that opens a menu of the items `body` declares.
    pub fn submenu(
        &mut self,
        id: impl Hash,
        glyph: char,
        label: &str,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let item = MenuFlyoutSubItem::new()?;
            let items = item.Items()?;
            Ok(ControlNode::new(
                item.cast()?,
                Control::SubMenu {
                    item,
                    items,
                    look: Look::default(),
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = control.stale;
        let Control::SubMenu { item, look, .. } = &mut control.control else {
            unreachable!("submenu keys hold submenus")
        };
        if must_write(stale, &look.glyph, &Some(glyph)) {
            item.SetIcon(&icon(glyph)?)?;
            look.glyph = Some(glyph);
        }
        if must_write(stale, look.label.as_str(), label) {
            item.SetText(label)?;
            look.label.clear();
            look.label.push_str(label);
        }
        self.open(node);
        body(self)?;
        self.close()?;
        Ok(Response { ui: self, node })
    }

    /// A line between groups of menu items.
    pub fn menu_separator(&mut self, id: impl Hash) -> Result<()> {
        self.visit(id, ChildKind::Visual, || {
            let separator = MenuFlyoutSeparator::new()?;
            Ok(ControlNode::new(separator.cast()?, Control::MenuSeparator))
        })?;
        Ok(())
    }
}

/// A `FlyoutPresenter` style with a transparent background, so a system
/// backdrop shows through it.
fn transparent_presenter() -> Result<Style> {
    let style = Style::CreateInstance(&TypeName {
        name: "Microsoft.UI.Xaml.Controls.FlyoutPresenter".into(),
        kind: TypeKind::Metadata,
    })?;
    let setter = Setter::CreateInstance(
        &XamlControl::BackgroundProperty()?,
        &resource::<IInspectable>("SubtleFillColorTransparentBrush")?,
    )?;
    style
        .Setters()?
        .cast::<windows_collections::IVector<SetterBase>>()?
        .Append(&setter.cast::<SetterBase>()?)?;
    Ok(style)
}

/// Where a [`Ui::menu`] opens: from [`Response::context_requested`], at the
/// pointer, or from [`Response::anchor`], under a control.
#[derive(Clone, Copy)]
pub struct MenuAnchor {
    node: NodeId,
    /// In the control's element; [`None`] opens under it.
    position: Option<Point>,
    /// Under the control, line up the right edges instead of the left.
    right: bool,
}

impl Response<'_> {
    /// A request for the control's context menu since the last pass, by
    /// right-click, the Menu key or Shift+F10, as the anchor to open it at:
    /// the pointer, or under the control from the keyboard. The first call
    /// subscribes the control, so its first result is [`None`].
    pub fn context_requested(&mut self) -> Result<Option<MenuAnchor>> {
        self.ui
            .subscribe(self.node, &[EventKind::ContextRequested], |control| {
                &control.element
            })?;
        Ok(self
            .ui
            .events
            .context(self.node)
            .map(|position| MenuAnchor {
                node: self.node,
                position,
                right: false,
            }))
    }

    /// The anchor that opens a menu under this control, as for a button.
    pub fn anchor(&self) -> MenuAnchor {
        MenuAnchor {
            node: self.node,
            position: None,
            right: false,
        }
    }

    /// As [`anchor`](Self::anchor), with the right edges lined up, for a
    /// control at a window's right end.
    pub fn anchor_right(&self) -> MenuAnchor {
        MenuAnchor {
            right: true,
            ..self.anchor()
        }
    }
}

/// What [`Ui::menu`] returns.
pub struct MenuResponse {
    closed: bool,
}

impl MenuResponse {
    /// Whether the menu closed since the last pass, by a pick, a click
    /// outside it or Escape. Stop declaring it then; a pick's click reads in
    /// the same pass.
    pub fn closed(&self) -> bool {
        self.closed
    }
}

/// What [`Ui::menu_item`] and [`Ui::menu_toggle`] return.
pub struct MenuItemResponse<'u>(Response<'u>);

impl MenuItemResponse<'_> {
    /// Whether the item was picked since the last pass. The first call
    /// subscribes it.
    pub fn clicked(&mut self) -> Result<bool> {
        let clicked = self.0.button_clicked()?;
        if clicked
            && let Control::MenuToggle { checked, .. } =
                &mut self.0.ui.control_mut(self.0.node).control
        {
            // WinUI flipped the check, so the next pass writes the app's.
            *checked = None;
        }
        Ok(clicked)
    }

    /// Shows `hint` at the item's right end, the name of a key that does
    /// the same, such as `Ctrl+U` for a [`Ui::shortcut`]. Menu items only.
    pub fn key_hint(&mut self, hint: &str) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let stale = control.stale;
        let Control::MenuItem {
            item,
            hint: written,
            ..
        } = &mut control.control
        else {
            panic!("only menu items take a key hint");
        };
        if must_write(stale, written.as_str(), hint) {
            item.SetKeyboardAcceleratorTextOverride(hint)?;
            written.clear();
            written.push_str(hint);
        }
        Ok(self)
    }

    /// Draws the item in the critical color, for an action such as Move to
    /// trash.
    pub fn critical(&mut self, on: bool) -> Result<&mut Self> {
        let ControlNode {
            element, control, ..
        } = self.0.ui.control_mut(self.0.node);
        if let Control::MenuItem { critical, .. } = control
            && *critical != on
        {
            let property = XamlControl::ForegroundProperty()?;
            match on {
                true => {
                    crate::theme::set_brush(element, &property, "SystemFillColorCriticalBrush")?
                }
                false => crate::theme::clear_brush(element, &property)?,
            }
            *critical = on;
        }
        Ok(self)
    }
}

impl<'u> Deref for MenuItemResponse<'u> {
    type Target = Response<'u>;

    fn deref(&self) -> &Response<'u> {
        &self.0
    }
}

impl<'u> DerefMut for MenuItemResponse<'u> {
    fn deref_mut(&mut self) -> &mut Response<'u> {
        &mut self.0
    }
}

/// The glyph and label of a menu item, as last written.
#[derive(Default)]
pub(crate) struct Look {
    glyph: Option<char>,
    label: String,
}

impl Look {
    /// Writes `glyph` and `label` where they changed, on `item`, a
    /// `MenuFlyoutItem` or a subclass. An item starts with no icon, so
    /// [`None`] needs no first write.
    fn write(
        &mut self,
        stale: bool,
        item: &impl Interface,
        glyph: Option<char>,
        label: &str,
    ) -> Result<()> {
        // Cast only for a write, because a cast is a COM call.
        let item = || item.cast::<MenuFlyoutItem>();
        if must_write(stale, &self.glyph, &glyph) {
            let item = item()?;
            match glyph {
                Some(glyph) => item.SetIcon(&icon(glyph)?)?,
                None => item.SetIcon(None::<&IconElement>)?,
            }
            self.glyph = glyph;
        }
        if must_write(stale, self.label.as_str(), label) {
            item()?.SetText(label)?;
            self.label.clear();
            self.label.push_str(label);
        }
        Ok(())
    }
}

fn icon(glyph: char) -> Result<IconElement> {
    let icon = FontIcon::new()?;
    icon.SetGlyph(glyph.encode_utf8(&mut [0; 4]) as &str)?;
    icon.cast()
}
