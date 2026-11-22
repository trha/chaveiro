//! Menus: a `MenuFlyout` the app declares only while it is open, and its
//! items.
//!
//! A menu opens when the app first declares it, at a [`MenuAnchor`], and
//! the app keeps declaring it until [`MenuResponse::closed`]. Its items are
//! nodes like any control, so a click reads like a button's.

use super::{Control, Response, must_write, resource};
use crate::{
    bindings::{
        Brush, Control as XamlControl, DependencyObject, FlyoutBase, FlyoutPlacementMode,
        FlyoutShowOptions, FontIcon, IconElement, MenuFlyout, MenuFlyoutItem, MenuFlyoutSeparator,
        MenuFlyoutSubItem, Point, ToggleMenuFlyoutItem,
    },
    events::EventKind,
    pass::{ChildKind, ControlNode, MenuNode, Payload, Ui},
    tree::NodeId,
};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use windows_core::{Interface, Result};

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
        let Payload::Menu(menu) = &mut self.tree.node_mut(node).payload else {
            unreachable!("menu keys hold menus")
        };
        let flyout = menu.flyout.clone();
        self.events
            .subscribe(node, &mut menu.subscribed, &flyout, EventKind::FlyoutClosed)?;
        self.open(node);
        body(self)?;
        self.close()?;
        let mut closed = self.events.happened(node, EventKind::FlyoutClosed);
        let Payload::Menu(menu) = &self.tree.node(node).payload else {
            unreachable!("menu keys hold menus")
        };
        if !menu.shown {
            let target = match self.tree.get(anchor.node).map(|node| &node.payload) {
                Some(Payload::Control(control)) => Some(control.element.clone()),
                _ => None,
            };
            match target {
                None => closed = true,
                Some(target) => {
                    let options = FlyoutShowOptions::new()?;
                    match anchor.position {
                        // At the pointer, as a right-click menu opens.
                        Some(position) => options.SetPosition(Some(position))?,
                        // Under the control, as a button's menu opens.
                        None => options.SetPlacement(FlyoutPlacementMode::BottomEdgeAlignedLeft)?,
                    }
                    flyout
                        .cast::<FlyoutBase>()?
                        .ShowAtWithOptions(&target.cast::<DependencyObject>()?, &options)?;
                }
            }
            let Payload::Menu(menu) = &mut self.tree.node_mut(node).payload else {
                unreachable!("menu keys hold menus")
            };
            menu.shown = true;
        }
        Ok(MenuResponse { closed })
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
                },
            ))
        })?;
        let control = self.control_mut(node);
        let stale = std::mem::take(&mut control.stale);
        let Control::MenuItem { item, look, .. } = &mut control.control else {
            unreachable!("menu item keys hold menu items")
        };
        look.write(stale, &item.clone(), Some(glyph), label)?;
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
        let stale = std::mem::take(&mut control.stale);
        let Control::MenuToggle {
            item,
            look,
            checked: written,
        } = &mut control.control
        else {
            unreachable!("menu toggle keys hold menu toggles")
        };
        let item = item.clone();
        look.write(stale, &item.cast()?, glyph, label)?;
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
        let stale = std::mem::take(&mut control.stale);
        let Control::SubMenu { item, look, .. } = &mut control.control else {
            unreachable!("submenu keys hold submenus")
        };
        let item = item.clone();
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

/// Where a [`Ui::menu`] opens: from [`Response::context_requested`], at the
/// pointer, or from [`Response::anchor`], under a control.
#[derive(Clone, Copy)]
pub struct MenuAnchor {
    node: NodeId,
    /// In the control's element; [`None`] opens under it.
    position: Option<Point>,
}

impl Response<'_> {
    /// A request for the control's context menu since the last pass, by
    /// right-click, the Menu key or Shift+F10, as the anchor to open it at:
    /// the pointer, or under the control from the keyboard. The first call
    /// subscribes the control, so its first result is [`None`].
    pub fn context_requested(&mut self) -> Result<Option<MenuAnchor>> {
        let control = self.ui.control_mut(self.node);
        let element = control.element.clone();
        let mut subscribed = control.subscribed;
        self.ui.events.subscribe(
            self.node,
            &mut subscribed,
            &element,
            EventKind::ContextRequested,
        )?;
        self.ui.control_mut(self.node).subscribed = subscribed;
        Ok(self
            .ui
            .events
            .context(self.node)
            .map(|position| MenuAnchor {
                node: self.node,
                position,
            }))
    }

    /// The anchor that opens a menu under this control, as for a button.
    pub fn anchor(&self) -> MenuAnchor {
        MenuAnchor {
            node: self.node,
            position: None,
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

    /// Draws the item in the critical color, for an action such as Move to
    /// trash.
    pub fn critical(&mut self, on: bool) -> Result<&mut Self> {
        let control = self.0.ui.control_mut(self.0.node);
        let element = control.element.clone();
        if let Control::MenuItem { critical, .. } = &mut control.control
            && *critical != on
        {
            let control = element.cast::<XamlControl>()?;
            match on {
                true => {
                    control.SetForeground(&resource::<Brush>("SystemFillColorCriticalBrush")?)?
                }
                false => control
                    .cast::<DependencyObject>()?
                    .ClearValue(&crate::bindings::Control::ForegroundProperty()?)?,
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
    /// Writes `glyph` and `label` where they changed. An item starts with
    /// no icon, so [`None`] needs no first write.
    fn write(
        &mut self,
        stale: bool,
        item: &MenuFlyoutItem,
        glyph: Option<char>,
        label: &str,
    ) -> Result<()> {
        if must_write(stale, &self.glyph, &glyph) {
            match glyph {
                Some(glyph) => item.SetIcon(&icon(glyph)?)?,
                None => item.SetIcon(None::<&IconElement>)?,
            }
            self.glyph = glyph;
        }
        if must_write(stale, self.label.as_str(), label) {
            item.SetText(label)?;
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
