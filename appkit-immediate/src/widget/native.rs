use super::{Control, Response};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::NSView;
use std::hash::Hash;

impl Ui {
    /// A view this crate has no widget for. `create` runs on the first pass
    /// that declares it; `update` runs on every pass, including the first.
    /// The crate owns the view's place among its siblings and its lifetime;
    /// `update` owns its properties. A `native` view cannot hold declared
    /// children.
    pub fn native(
        &mut self,
        id: impl Hash,
        create: impl FnOnce(MainThreadMarker) -> Retained<NSView>,
        update: impl FnOnce(&NSView),
    ) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            ControlNode::payload(create(mtm), Control::Native)
        });
        update(&self.control(node).view);
        Response { ui: self, node }
    }
}
