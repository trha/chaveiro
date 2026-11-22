use super::{Control, Response};
use crate::pass::{ChildKind, ControlNode, Ui};
use objc2::{MainThreadOnly, define_class, msg_send, rc::Retained};
use objc2_app_kit::{NSScrollView, NSView};
use objc2_foundation::NSObjectProtocol;
use std::hash::Hash;

impl Ui {
    /// A view that scrolls the one control `body` declares up and down.
    /// The control is as wide as the view and starts at its top; the view
    /// fills the space its container gives it, so put it in a
    /// [`Length::Star`](crate::Length::Star) grid row or a split view pane.
    pub fn scroll_view(&mut self, id: impl Hash, body: impl FnOnce(&mut Ui)) -> Response<'_> {
        let (node, _) = self.visit(id, ChildKind::Visual, |mtm| {
            let scroll = NSScrollView::new(mtm);
            scroll.setHasVerticalScroller(true);
            scroll.setDrawsBackground(false);
            let document: Retained<FlippedView> =
                unsafe { msg_send![FlippedView::alloc(mtm), init] };
            let document = document.into_super();
            document.setTranslatesAutoresizingMaskIntoConstraints(false);
            scroll.setDocumentView(Some(&document));
            let clip = scroll.contentView();
            for constraint in [
                document
                    .leadingAnchor()
                    .constraintEqualToAnchor(&clip.leadingAnchor()),
                document
                    .trailingAnchor()
                    .constraintEqualToAnchor(&clip.trailingAnchor()),
                document
                    .topAnchor()
                    .constraintEqualToAnchor(&clip.topAnchor()),
            ] {
                constraint.setActive(true);
            }
            ControlNode::payload(
                Retained::clone(&scroll).into_super(),
                Control::ScrollView {
                    document,
                    content: None,
                    pins: Vec::new(),
                },
            )
        });
        // A scroll view writes nothing after it is made but its content,
        // which its close sets when it changes.
        self.control_mut(node).stale = false;
        self.open(node);
        body(self);
        self.close();
        Response { ui: self, node }
    }
}

define_class!(
    /// A view whose origin is its top left, so a scroll view's document
    /// starts at the top.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    pub(crate) struct FlippedView;

    unsafe impl NSObjectProtocol for FlippedView {}

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);
