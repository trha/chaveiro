//! A title bar the app lays out itself: any controls, with the empty parts
//! dragging the window.

use super::{Control, Response};
use crate::{
    bindings::{
        AppWindow, FrameworkElement, Grid, InputNonClientPointerSource, NonClientRegionKind, Panel,
        Rect, RectInt32, Thickness, TitleBarHeightOption, UIElement,
    },
    pass::{ChildKind, ControlNode, NativeNode, NativeTree, Payload, Ui},
    tree::NodeId,
};
use immediate_tree::Node;
use std::{cell::RefCell, hash::Hash, rc::Rc};
use windows_core::{Interface, Result};

/// The height of a drag bar, as WinUI's title bar is tall.
const HEIGHT: f64 = 48.0;

impl Ui {
    /// The window's title bar, laid out by the one control `body` declares,
    /// such as a [`columns`](Self::columns) row. The empty parts drag the
    /// window, and every control in it that takes input stays clickable.
    /// It keeps room on its right for the window's minimize, maximize and
    /// close buttons. Use it in a window made
    /// [`extends_into_title_bar`](crate::WindowResponse::extends_into_title_bar),
    /// in place of [`title_bar`](Self::title_bar), one per window.
    pub fn drag_bar(
        &mut self,
        id: impl Hash,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<Response<'_>> {
        let window = self.enclosing_window();
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            let grid = Grid::new()?;
            grid.cast::<FrameworkElement>()?.SetHeight(HEIGHT)?;
            let children = grid.cast::<Panel>()?.Children()?;
            let state = Rc::new(RefCell::new(Clickable {
                window: None,
                elements: Vec::new(),
                nodes: Vec::new(),
                walk: Vec::new(),
                found: Vec::new(),
                rects: Vec::new(),
                written: None,
                inset: f64::NAN,
                tall: false,
            }));
            // Layout moves the controls, so their areas follow each layout.
            let layout = {
                let state = Rc::clone(&state);
                let target = grid.downgrade()?;
                grid.cast::<FrameworkElement>()?
                    .LayoutUpdated(move |_, _| {
                        // A layout inside a pass that is changing the list
                        // runs again after it.
                        let (Some(grid), Ok(mut state)) = (target.upgrade(), state.try_borrow_mut())
                        else {
                            return;
                        };
                        if let Err(error) = state.update(&grid) {
                            tracing::warn!(%error, "the title bar's clickable areas did not update");
                        }
                    })?
            };
            Ok(ControlNode::new(
                grid.cast()?,
                Control::DragBar {
                    children,
                    clickable: state,
                    _layout: layout,
                },
            ))
        })?;
        // The window is read here, not in `visit`'s create, because that
        // cannot read the tree, and reading it every pass would clone it. An
        // attach that failed is tried again on the next pass.
        let Payload::Window(window_node) = self.core.tree.native(window) else {
            unreachable!("enclosing_window returns a window")
        };
        let Control::DragBar { clickable, .. } = &self.control(node).control else {
            unreachable!("drag bar keys hold drag bars")
        };
        if clickable.borrow().window.is_none() {
            let app_window = window_node.app_window.clone();
            let source = InputNonClientPointerSource::GetForWindowId(app_window.Id()?)?;
            clickable.borrow_mut().window = Some((app_window, source));
        }
        self.open(node);
        body(self)?;
        self.close()?;
        self.collect_clickable(node);
        let Payload::Window(window_node) = self.core.tree.native(window) else {
            unreachable!("enclosing_window returns a window")
        };
        if window_node.title_bar != Some(node) {
            window_node
                .window
                .SetTitleBar(&self.control(node).element)?;
            let Payload::Window(window_node) = self.core.tree.native_mut(window) else {
                unreachable!("enclosing_window returns a window")
            };
            window_node.title_bar = Some(node);
        }
        Ok(Response { ui: self, node })
    }

    /// Writes the controls under the drag bar `bar` that take input into
    /// its list, in tree order, when they changed since the last pass.
    fn collect_clickable(&mut self, bar: NodeId) {
        let Control::DragBar { clickable, .. } = &self.control(bar).control else {
            unreachable!("drag bar keys hold drag bars")
        };
        let mut clickable = clickable.borrow_mut();
        let Clickable {
            nodes,
            elements,
            walk,
            found,
            ..
        } = &mut *clickable;
        walk.clear();
        found.clear();
        walk.push(bar);
        while let Some(node) = walk.pop() {
            if let Node::Native(NativeNode(Payload::Control(control))) =
                &self.core.tree.node(node).payload
                && node != bar
                && control.control.takes_input()
            {
                found.push(node);
                continue;
            }
            // Children in reverse, so the walk visits them in order.
            walk.extend(self.core.tree.children(node).iter().rev());
        }
        if *found != *nodes {
            std::mem::swap(found, nodes);
            elements.clear();
            elements.extend(nodes.iter().map(|node| self.control(*node).element.clone()));
        }
    }
}

/// The areas of a drag bar that stay clickable, kept in step with layout.
///
/// Contract: `elements` holds the elements of `nodes`, in the same order.
pub(crate) struct Clickable {
    /// The bar's window and its input source, [`None`] until the bar's first
    /// pass reads them.
    window: Option<(AppWindow, InputNonClientPointerSource)>,
    nodes: Vec<NodeId>,
    elements: Vec<UIElement>,
    /// Reused by each pass's walk of the bar's nodes.
    walk: Vec<NodeId>,
    found: Vec<NodeId>,
    /// Reused for each update, in physical pixels of the window.
    rects: Vec<RectInt32>,
    /// The areas last given to Windows.
    written: Option<Vec<RectInt32>>,
    /// The right padding last written, in view pixels.
    inset: f64,
    /// The caption buttons were made as tall as the bar.
    tall: bool,
}

impl Clickable {
    /// Gives Windows the controls' areas, and keeps the caption buttons'
    /// room free, when either changed.
    fn update(&mut self, grid: &Grid) -> Result<()> {
        let Ok(root) = grid.cast::<UIElement>()?.XamlRoot() else {
            // Not in a window yet.
            return Ok(());
        };
        let Some((app_window, source)) = &self.window else {
            return Ok(());
        };
        let scale = root.RasterizationScale()?;
        let title_bar = app_window.TitleBar()?;
        if !self.tall {
            // The caption buttons grow to the bar's 48px, as WinUI's own
            // title bar asks for, so they line up with its controls.
            // Windows takes it only once the content extends into the title
            // bar, which a window sets after its first pass, so this waits
            // for a layout.
            title_bar
                .cast::<crate::bindings::IAppWindowTitleBar2>()?
                .SetPreferredHeightOption(TitleBarHeightOption::Tall)?;
            self.tall = true;
        }
        let inset = f64::from(title_bar.RightInset()?) / scale;
        if inset != self.inset {
            grid.SetPadding(Thickness {
                left: 0.0,
                top: 0.0,
                right: inset,
                bottom: 0.0,
            })?;
            self.inset = inset;
        }
        self.rects.clear();
        for element in &self.elements {
            let size = element.cast::<FrameworkElement>()?;
            let (width, height) = (size.ActualWidth()?, size.ActualHeight()?);
            if width <= 0.0 || height <= 0.0 {
                continue;
            }
            let bounds = element
                .TransformToVisual(None::<&UIElement>)?
                .TransformBounds(Rect {
                    x: 0.0,
                    y: 0.0,
                    width: width as f32,
                    height: height as f32,
                })?;
            let physical = |value: f32| (f64::from(value) * scale).round() as i32;
            self.rects.push(RectInt32 {
                x: physical(bounds.x),
                y: physical(bounds.y),
                width: physical(bounds.width),
                height: physical(bounds.height),
            });
        }
        if self.written.as_deref() != Some(&self.rects[..]) {
            source.SetRegionRects(NonClientRegionKind::Passthrough, &self.rects)?;
            match &mut self.written {
                Some(written) => {
                    written.clear();
                    written.extend_from_slice(&self.rects);
                }
                None => self.written = Some(self.rects.clone()),
            }
        }
        Ok(())
    }
}
