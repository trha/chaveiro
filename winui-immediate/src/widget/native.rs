use super::{Control, Response};
use crate::{
    bindings::UIElement,
    pass::{ChildKind, ControlNode, Ui},
};
use std::hash::Hash;
use windows_core::{IInspectable, Interface, Result};

impl Ui {
    /// A control this crate has no widget for. `create` runs on the first
    /// pass that declares it and must return a XAML `UIElement`; `update`
    /// runs on every pass, including the first. The crate owns the control's
    /// place among its siblings and its lifetime; `update` owns its
    /// properties. A `native` control cannot hold declared children.
    pub fn native(
        &mut self,
        id: impl Hash,
        create: impl FnOnce() -> Result<IInspectable>,
        update: impl FnOnce(&IInspectable) -> Result<()>,
    ) -> Result<Response<'_>> {
        let (node, _) = self.visit(id, ChildKind::Visual, || {
            Ok(ControlNode::new(create()?.cast()?, Control::Native))
        })?;
        let element = &self.control(node).element;
        // SAFETY: both types are a transparent interface pointer, and every
        // WinRT interface is also an `IInspectable`, so the borrow needs no
        // QueryInterface and no AddRef.
        update(unsafe { &*(element as *const UIElement).cast::<IInspectable>() })?;
        Ok(Response { ui: self, node })
    }
}
