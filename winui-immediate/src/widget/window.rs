use crate::{
    bindings::{IWindow2, Window},
    events::{EventKind, EventSet},
    pass::{ChildKind, Payload, Ui, WindowNode},
    tree::NodeId,
};
use std::hash::Hash;
use windows_core::{IInspectable, Interface, Result};

impl Ui {
    /// Declares an open window. It is created on its first pass and activated
    /// once; a window a pass does not declare is closed. `body` must declare
    /// at most one control, which becomes the window's content.
    pub fn window(
        &mut self,
        id: impl Hash,
        title: &str,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<WindowResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Logical, || {
            let window = Window::new()?;
            let app_window = window.cast::<IWindow2>()?.AppWindow()?;
            Ok(Payload::Window(WindowNode {
                window,
                app_window,
                content: None,
                title: String::new(),
                activated: false,
                subscribed: EventSet::default(),
            }))
        })?;
        let window = self.window_mut(node);
        if window.title != title {
            window.window.SetTitle(title)?;
            window.title.clear();
            window.title.push_str(title);
        }
        let Payload::Window(window) = &mut self.tree.node_mut(node).payload else {
            unreachable!("window keys hold windows")
        };
        self.events.subscribe(
            node,
            &mut window.subscribed,
            &window.app_window,
            EventKind::CloseRequested,
        )?;
        self.open(node);
        body(self)?;
        self.close()?;
        let window = self.window_mut(node);
        if !window.activated {
            window.window.Activate()?;
            window.activated = true;
        }
        Ok(WindowResponse { ui: self, node })
    }

    fn window_mut(&mut self, node: NodeId) -> &mut WindowNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Window(window) => window,
            _ => unreachable!("window keys hold windows"),
        }
    }
}

/// What [`Ui::window`] returns.
pub struct WindowResponse<'u> {
    ui: &'u Ui,
    node: NodeId,
}

impl WindowResponse<'_> {
    /// True on the pass after the user asked to close the window. The window
    /// stays open until a pass stops declaring it.
    pub fn close_requested(&self) -> bool {
        self.ui
            .events
            .happened(self.node, EventKind::CloseRequested)
    }

    /// The WinUI `Window`, for what this crate does not cover. Cast it with
    /// your own WinUI bindings.
    pub fn window(&self) -> IInspectable {
        match &self.ui.tree.node(self.node).payload {
            Payload::Window(window) => window.window.clone().into(),
            _ => unreachable!("window keys hold windows"),
        }
    }
}
