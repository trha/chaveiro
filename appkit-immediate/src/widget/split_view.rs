use super::{Control, Response};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSSplitViewController, NSSplitViewItem, NSView, NSViewController};
use std::hash::Hash;

impl Ui {
    /// Panes side by side, one per control `body` declares, from an
    /// `NSSplitViewController`. The first pane is its sidebar item: it
    /// takes the system's sidebar look (Liquid Glass on macOS 26 and
    /// later, in an app linked against that SDK), reaches up under a
    /// [`full_size_content`](crate::WindowResponse::full_size_content)
    /// title bar, keeps its width when the window resizes, and the user
    /// can drag it between the system's sidebar widths. With three panes,
    /// the middle one is a content list, such as a mail app's list of
    /// messages: it keeps its width when the window resizes too. Every pane
    /// reaches up under a full-size title bar, so the toolbar shows the
    /// content through glass; a scroll view insets its content below the
    /// bar by itself, but put nothing else at a pane's top.
    pub fn split_view(&mut self, id: impl Hash, body: impl FnOnce(&mut Ui)) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let controller = NSSplitViewController::new(mtm);
            // Loads the split view the controller manages.
            let view = controller.view();
            ControlNode::payload(
                view,
                Control::SplitView {
                    controller,
                    panes: Vec::new(),
                },
            )
        });
        // A split view writes nothing after it is made but its panes, which
        // its close rebuilds when they change.
        self.control_mut(node).stale = false;
        self.open(node);
        body(self);
        self.close();
        Response { ui: self, node }
    }
}

/// The item that holds a split view's pane at `position`, and its view
/// controller's view, which the split view sizes and the pane's control is
/// pinned inside. The item keeps the view.
/// `count` is the number of panes.
pub(crate) fn pane_item(
    mtm: MainThreadMarker,
    position: usize,
    count: usize,
) -> (Retained<NSSplitViewItem>, Retained<NSView>) {
    let container = NSView::new(mtm);
    let controller = NSViewController::new(mtm);
    controller.setView(&container);
    let item = match (position, count) {
        (0, _) => NSSplitViewItem::sidebarWithViewController(&controller),
        (1, 3) => NSSplitViewItem::contentListWithViewController(&controller),
        _ => NSSplitViewItem::splitViewItemWithViewController(&controller),
    };
    (item, container)
}
