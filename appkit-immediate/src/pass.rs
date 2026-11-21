//! One build pass over the declared UI: finding nodes, opening and closing
//! containers, and applying their AppKit children.

use crate::{
    children::{Edit, edits},
    events::{EventKind, Events},
    tree::{Key, NodeId, Tree, Visit},
    widget::{
        Align, Control, Field, GridRule as Rule, Style, TextColor, grid_constraints, grid_rules,
    },
};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSApplication, NSLayoutConstraint, NSLayoutConstraintOrientation, NSStackView, NSTableView,
    NSUserInterfaceLayoutOrientation, NSView, NSVisualEffectView, NSWindow,
};
use objc2_foundation::NSArray;
use std::{any::Any, hash::Hash, rc::Rc};

/// Builds the UI. The app gets it in [`App::view`](crate::App::view).
pub struct Ui {
    pub(crate) tree: Tree<Payload>,
    /// Mirrors the call stack of open containers: the root frame at the
    /// bottom during a pass, and no frame between passes.
    frames: Vec<Frame>,
    pub(crate) events: Events,
    pub(crate) mtm: MainThreadMarker,
    /// Reused buffer for formatting labels before comparing them.
    pub(crate) scratch: String,
    /// Windows created this pass, shown when it ends.
    pub(crate) new_windows: Vec<NodeId>,
    /// Table views declared this pass, whose rows are built after the view.
    pub(crate) tables: Vec<NodeId>,
    /// Table views to reload after the pass, with no borrow held, because
    /// AppKit asks for rows inside `reloadData`.
    pub(crate) reloads: Vec<Retained<NSTableView>>,
    /// Whether this pass called [`Response::invalidate`](crate::Response::invalidate),
    /// so the next pass writes by request and is no idle pass.
    pub(crate) invalidated: bool,
    /// The header column's width while the body of a [`Ui::form`] runs.
    pub(crate) form_header_width: Option<f64>,
}

impl Ui {
    pub(crate) fn new(mtm: MainThreadMarker, wake: Rc<dyn Fn()>) -> Self {
        Ui {
            tree: Tree::new(Payload::Root),
            frames: Vec::new(),
            events: Events::new(mtm, wake),
            mtm,
            scratch: String::new(),
            new_windows: Vec::new(),
            tables: Vec::new(),
            reloads: Vec::new(),
            invalidated: false,
            form_header_width: None,
        }
    }

    /// Starts a pass. Returns whether events arrived since the last one.
    pub(crate) fn begin(&mut self) -> bool {
        debug_assert!(self.frames.is_empty(), "the last pass closed its frames");
        self.tables.clear();
        self.invalidated = false;
        self.tree.begin_pass();
        let consumed = self.events.begin_pass(&mut self.tree);
        let root = self.tree.root();
        self.open(root);
        consumed
    }

    /// Ends a pass, closing every window it did not declare. Returns the
    /// number of open windows.
    pub(crate) fn end(&mut self) -> usize {
        self.close();
        self.show_new_windows();
        self.tree.children(self.tree.root()).len()
    }

    /// Closes every window, as a pass that declares none would. Used after a
    /// pass panicked, which may have left containers open: closing them
    /// first attaches every node they visited to its parent, so no node is
    /// left live but unreachable.
    pub(crate) fn close_all(&mut self) {
        while !self.frames.is_empty() {
            self.close();
        }
        self.tree.begin_pass();
        let root = self.tree.root();
        let mut nothing = self.tree.take_list();
        let events = &self.events;
        self.tree
            .close(root, &mut nothing, |payload| release(events, payload));
        self.tree.recycle_list(nothing);
    }

    /// Whether the user asked to quit (Cmd+Q, the Dock, logout) since the
    /// last pass. The app quits by declaring no window.
    pub fn should_terminate(&self) -> bool {
        self.events
            .happened(self.tree.root(), EventKind::ShouldTerminate)
    }

    /// Routes the application's delegate calls to the root node and makes
    /// the shared target the application's delegate.
    pub(crate) fn adopt_application(&self, application: &NSApplication) {
        self.events.register(application, self.tree.root());
        application.setDelegate(Some(ProtocolObject::from_ref(self.events.target())));
    }

    /// Adds the controls a cache emitted on its last run to the open
    /// container, for a pass that skips the cache's body.
    pub(crate) fn emit_cached(&mut self, node: NodeId) {
        let Payload::Cache(cache) = &self.tree.node(node).payload else {
            unreachable!("cache keys hold caches")
        };
        self.frames
            .last_mut()
            .expect("a cache is declared inside a container")
            .visual
            .extend_from_slice(&cache.emitted);
    }

    /// Opens `node` as the container for the widgets declared next.
    pub(crate) fn open(&mut self, node: NodeId) {
        let key = self.tree.node(node).key();
        let logical = self.tree.take_list();
        let visual = self.tree.take_list();
        self.frames.push(Frame {
            node,
            key,
            logical,
            visual,
        });
    }

    /// Finds or creates the node `id` in the open container and records it
    /// as a child of the kind `kind` says.
    pub(crate) fn visit(
        &mut self,
        id: impl Hash,
        kind: ChildKind,
        create: impl FnOnce(MainThreadMarker) -> Payload,
    ) -> (NodeId, Visit) {
        let mtm = self.mtm;
        let frame = self
            .frames
            .last_mut()
            .expect("widgets are declared inside a pass");
        let key = Key::child(frame.key, id);
        let (node, visit) = self.tree.visit(frame.node, key, || create(mtm));
        frame.logical.push(node);
        if kind == ChildKind::Visual {
            frame.visual.push(node);
        }
        (node, visit)
    }

    /// Closes the innermost container: applies its AppKit children, then
    /// drops the children it no longer declares.
    pub(crate) fn close(&mut self) {
        let mut frame = self.frames.pop().expect("close matches open");
        self.apply_visual(frame.node, &mut frame.visual);
        let events = &self.events;
        self.tree.close(frame.node, &mut frame.logical, |payload| {
            release(events, payload)
        });
        frame.visual.clear();
        self.tree.recycle_list(frame.logical);
        self.tree.recycle_list(frame.visual);
    }

    pub(crate) fn control(&self, node: NodeId) -> &ControlNode {
        match &self.tree.node(node).payload {
            Payload::Control(control) => control,
            _ => unreachable!("visual children and widget keys are controls"),
        }
    }

    pub(crate) fn control_mut(&mut self, node: NodeId) -> &mut ControlNode {
        match &mut self.tree.node_mut(node).payload {
            Payload::Control(control) => control,
            _ => unreachable!("widget keys hold controls"),
        }
    }

    /// Rebuilds the constraints across `stack` of each child of `node`
    /// whose `cross_stale` is set. The stack places children along its axis
    /// only; each child carries its own constraints across it, by its
    /// `align`.
    pub(crate) fn place_across(&mut self, node: NodeId, stack: &NSStackView) {
        for position in 0..self.control(node).visual.len() {
            let child = self.control(node).visual[position];
            if self.control(child).cross_stale {
                crate::write_check::record(child);
                let child = self.control_mut(child);
                child.cross_stale = false;
                NSLayoutConstraint::deactivateConstraints(&NSArray::from_retained_slice(
                    &std::mem::take(&mut child.cross),
                ));
                let cross = cross_constraints(stack, &child.view, child.align);
                NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&cross));
                child.cross = cross;
            }
        }
    }

    /// Makes `declared` the AppKit children of `node`. Leaves the previous
    /// list in `declared`.
    fn apply_visual(&mut self, node: NodeId, declared: &mut Vec<NodeId>) {
        match &mut self.tree.node_mut(node).payload {
            Payload::Root => {}
            Payload::Cache(cache) => {
                // A cache has no view: its controls belong to the parent.
                std::mem::swap(&mut cache.emitted, declared);
                let Payload::Cache(cache) = &self.tree.node(node).payload else {
                    unreachable!("matched above")
                };
                self.frames
                    .last_mut()
                    .expect("a cache is declared inside a container")
                    .visual
                    .extend_from_slice(&cache.emitted);
            }
            Payload::State(_) => unreachable!("state nodes are never opened"),
            Payload::Window(window) => {
                assert!(
                    declared.len() <= 1,
                    "a window's body declares at most one control"
                );
                let content = declared.first().copied();
                if window.content == content {
                    return;
                }
                crate::write_check::record(node);
                window.content = content;
                if let Some(old) = window.pinned.take() {
                    old.removeFromSuperview();
                }
                NSLayoutConstraint::deactivateConstraints(&NSArray::from_retained_slice(
                    &std::mem::take(&mut window.pins),
                ));
                let container = window.container.clone();
                let Some(content) = content else {
                    return;
                };
                let view = self.control(content).view.clone();
                // The content fills the window, and its size bounds the
                // window's, so a window cannot shrink below its content.
                let pins = pin_to_edges(&container, &view);
                let Payload::Window(window) = &mut self.tree.node_mut(node).payload else {
                    unreachable!("matched above")
                };
                window.pins = pins;
                window.pinned = Some(view);
            }
            Payload::Control(ControlNode {
                control:
                    Control::Row {
                        shell,
                        content,
                        pins,
                    },
                ..
            }) => {
                assert!(declared.len() <= 1, "a table row declares one control");
                let declared_content = declared.first().copied();
                if *content == declared_content {
                    return;
                }
                crate::write_check::record(node);
                let old = std::mem::replace(content, declared_content);
                NSLayoutConstraint::deactivateConstraints(&NSArray::from_retained_slice(
                    &std::mem::take(pins),
                ));
                let shell = shell.clone();
                if let Some(old) = old.and_then(|old| self.tree.get(old)) {
                    let Payload::Control(old) = &old.payload else {
                        unreachable!("visual children are controls")
                    };
                    old.view.removeFromSuperview();
                }
                let Some(declared_content) = declared_content else {
                    return;
                };
                let view = self.control(declared_content).view.clone();
                // The content's constraints give the row its height.
                let built = pin_to_edges(&shell, &view);
                let Control::Row { pins, .. } = &mut self.control_mut(node).control else {
                    unreachable!("matched above")
                };
                *pins = built;
            }
            Payload::Control(ControlNode {
                control:
                    Control::Grid {
                        container,
                        rows,
                        constraints,
                        placed,
                        stretched,
                    },
                visual,
                ..
            }) => {
                // Afterwards `declared` holds the old list.
                std::mem::swap(visual, declared);
                if *placed && declared == visual {
                    return;
                }
                crate::write_check::record(node);
                let container = container.clone();
                let rules = grid_rules(rows, visual.len());
                NSLayoutConstraint::deactivateConstraints(&NSArray::from_retained_slice(
                    &std::mem::take(constraints),
                ));
                *placed = true;
                let mut lowered = std::mem::take(stretched);
                let new = visual.clone();
                for (child, priority) in lowered.drain(..) {
                    if let Some(child) = self.tree.get(child) {
                        let Payload::Control(child) = &child.payload else {
                            unreachable!("visual children are controls")
                        };
                        child.view.setContentHuggingPriority_forOrientation(
                            priority,
                            NSLayoutConstraintOrientation::Vertical,
                        );
                    }
                }
                for rule in &rules {
                    if let Rule::Stretch { row } = *rule {
                        let view = &self.control(new[row]).view;
                        lowered.push((
                            new[row],
                            view.contentHuggingPriorityForOrientation(
                                NSLayoutConstraintOrientation::Vertical,
                            ),
                        ));
                        view.setContentHuggingPriority_forOrientation(
                            1.0,
                            NSLayoutConstraintOrientation::Vertical,
                        );
                    }
                }
                // Order among subviews does not matter: constraints place
                // them.
                for old in declared.iter().filter(|old| !new.contains(old)) {
                    if let Some(old) = self.tree.get(*old) {
                        let Payload::Control(old) = &old.payload else {
                            unreachable!("visual children are controls")
                        };
                        old.view.removeFromSuperview();
                    }
                }
                let children: Vec<Retained<NSView>> = new
                    .iter()
                    .map(|child| self.control(*child).view.clone())
                    .collect();
                for (child, view) in new.iter().zip(&children) {
                    if !declared.contains(child) {
                        container.addSubview(view);
                    }
                }
                let built = grid_constraints(&container, &children, &rules);
                NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&built));
                let Control::Grid {
                    constraints,
                    stretched,
                    ..
                } = &mut self.control_mut(node).control
                else {
                    unreachable!("matched above")
                };
                *constraints = built;
                *stretched = lowered;
            }
            Payload::Control(control) => {
                let Some(stack) = control.control.arranged() else {
                    assert!(
                        declared.is_empty(),
                        "only stack views, box views and grids have children"
                    );
                    return;
                };
                let stack = stack.clone();
                // Afterwards `declared` holds the old list.
                std::mem::swap(&mut control.visual, declared);
                let changes: Vec<Edit> =
                    edits(declared, self.control(node).visual.as_slice()).collect();
                for edit in changes {
                    crate::write_check::record(node);
                    match edit {
                        Edit::RemoveAt(index) => {
                            let view = stack.arrangedSubviews().objectAtIndex(index as usize);
                            stack.removeArrangedSubview(&view);
                            // Removing the view also removes its cross-axis
                            // constraints, which name the stack.
                            view.removeFromSuperview();
                        }
                        Edit::InsertAt(index, child) => {
                            stack.insertArrangedSubview_atIndex(
                                &self.control(child).view,
                                index as _,
                            );
                            self.control_mut(child).cross_stale = true;
                        }
                    }
                }
                self.place_across(node, &stack);
            }
        }
    }
}

/// How a declared node joins its container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildKind {
    /// A control: it joins the container's AppKit children as well.
    Visual,
    /// A node without a view of its own (a window): it joins only the tree.
    Logical,
}

pub(crate) enum Payload {
    Root,
    Window(WindowNode),
    Control(ControlNode),
    Cache(CacheNode),
    /// UI-only data from [`Ui::state`].
    State(Box<dyn Any>),
}

pub(crate) struct CacheNode {
    /// The dependencies of the last run, reassigned in place while their type
    /// stays the same.
    pub(crate) deps: Option<Box<dyn Any>>,
    /// The controls the last run declared, in order, which a skipped run
    /// declares again for the parent.
    pub(crate) emitted: Vec<NodeId>,
}

/// Contract: `pinned` is the view of `content`, a subview of `container`,
/// and `pins` are the active constraints that tie it to `container`'s
/// edges; all three are empty together.
pub(crate) struct WindowNode {
    pub(crate) window: Retained<NSWindow>,
    /// The window's content view, which keeps its autoresizing mask so the
    /// window sizes it.
    pub(crate) container: Retained<NSView>,
    /// The control declared as the window's body.
    pub(crate) content: Option<NodeId>,
    pub(crate) pinned: Option<Retained<NSView>>,
    pub(crate) pins: Vec<Retained<NSLayoutConstraint>>,
    /// The visual effect view behind the content, made on first use.
    pub(crate) effect: Option<Retained<NSVisualEffectView>>,
    pub(crate) compact: bool,
    /// The fitting size [`size_to_fit`](crate::WindowResponse::size_to_fit)
    /// last applied, in points.
    pub(crate) fitted: Option<(f64, f64)>,
    /// The title last written.
    pub(crate) title: String,
}

pub(crate) struct ControlNode {
    pub(crate) view: Retained<NSView>,
    pub(crate) control: Control,
    /// AppKit children as last applied (stacks only).
    pub(crate) visual: Vec<NodeId>,
    /// Set by [`Response::invalidate`](crate::Response::invalidate): the next
    /// pass writes every managed property, then clears it.
    pub(crate) stale: bool,
    /// Where the view sits across its stack view.
    pub(crate) align: Align,
    /// The active constraints that place the view across its stack view,
    /// rebuilt by the stack's close while `cross_stale` is set.
    pub(crate) cross: Vec<Retained<NSLayoutConstraint>>,
    pub(crate) cross_stale: bool,
    pub(crate) width: Option<Retained<NSLayoutConstraint>>,
    /// The options last written; [`None`] until first written.
    pub(crate) enabled: Option<bool>,
    pub(crate) style: Option<Style>,
    pub(crate) padding: Option<[f64; 4]>,
    pub(crate) text_color: Option<TextColor>,
    /// The header and error line of a form field.
    pub(crate) field: Option<Box<Field>>,
}

impl ControlNode {
    /// A control node, wrapped as the payload the tree stores.
    /// Turns off the view's autoresizing mask, because its container places
    /// it with constraints.
    pub(crate) fn payload(view: Retained<NSView>, control: Control) -> Payload {
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        let align = match control {
            Control::Button { .. } => Align::Start,
            _ => Align::Stretch,
        };
        Payload::Control(ControlNode {
            view,
            control,
            visual: Vec::new(),
            stale: false,
            align,
            cross: Vec::new(),
            cross_stale: true,
            width: None,
            enabled: None,
            style: None,
            padding: None,
            text_color: None,
            field: None,
        })
    }

    /// A form field node: `control` wrapped in a field stack, which becomes
    /// the node's view.
    pub(crate) fn field(mtm: MainThreadMarker, control_view: &NSView, control: Control) -> Payload {
        control_view.setTranslatesAutoresizingMaskIntoConstraints(false);
        let field = Field::new(mtm, control_view);
        let Payload::Control(mut node) =
            ControlNode::payload(Retained::clone(&field.stack).into_super(), control)
        else {
            unreachable!("payload makes control payloads")
        };
        node.field = Some(field);
        Payload::Control(node)
    }
}

/// Adds `view` to `container` and pins it to all four edges. Returns the
/// active constraints.
fn pin_to_edges(container: &NSView, view: &NSView) -> Vec<Retained<NSLayoutConstraint>> {
    container.addSubview(view);
    let pins = vec![
        view.leadingAnchor()
            .constraintEqualToAnchor(&container.leadingAnchor()),
        view.trailingAnchor()
            .constraintEqualToAnchor(&container.trailingAnchor()),
        view.topAnchor()
            .constraintEqualToAnchor(&container.topAnchor()),
        view.bottomAnchor()
            .constraintEqualToAnchor(&container.bottomAnchor()),
    ];
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&pins));
    pins
}

/// The constraints that place `view` across `stack` by `align`, inside the
/// stack's edge insets.
fn cross_constraints(
    stack: &NSStackView,
    view: &NSView,
    align: Align,
) -> Vec<Retained<NSLayoutConstraint>> {
    let insets = stack.edgeInsets();
    // The same rules for both axes; the anchors differ in type, so a macro
    // writes them once.
    macro_rules! place {
        ($start:ident, $end:ident, $center:ident, $start_inset:expr, $end_inset:expr) => {{
            let pin_start = || {
                view.$start()
                    .constraintEqualToAnchor_constant(&stack.$start(), $start_inset)
            };
            let pin_end = || {
                view.$end()
                    .constraintEqualToAnchor_constant(&stack.$end(), -$end_inset)
            };
            let within_start = || {
                view.$start()
                    .constraintGreaterThanOrEqualToAnchor_constant(&stack.$start(), $start_inset)
            };
            let within_end = || {
                view.$end()
                    .constraintLessThanOrEqualToAnchor_constant(&stack.$end(), -$end_inset)
            };
            match align {
                Align::Stretch => vec![pin_start(), pin_end()],
                Align::Start => vec![pin_start(), within_end()],
                Align::End => vec![within_start(), pin_end()],
                Align::Center => vec![
                    view.$center().constraintEqualToAnchor(&stack.$center()),
                    within_start(),
                    within_end(),
                ],
            }
        }};
    }
    let vertical = stack.orientation() == NSUserInterfaceLayoutOrientation::Vertical;
    let mut constraints = if vertical {
        place!(
            leadingAnchor,
            trailingAnchor,
            centerXAnchor,
            insets.left,
            insets.right
        )
    } else {
        place!(
            topAnchor,
            bottomAnchor,
            centerYAnchor,
            insets.top,
            insets.bottom
        )
    };
    // A view that is not stretched is as small as its content allows. A
    // view with no width of its own, such as a stack view, is otherwise
    // free to grow and has an ambiguous layout.
    if align != Align::Stretch {
        let shrink = match vertical {
            true => view.widthAnchor().constraintEqualToConstant(0.0),
            false => view.heightAnchor().constraintEqualToConstant(0.0),
        };
        shrink.setPriority(SHRINK);
        constraints.push(shrink);
    }
    constraints
}

/// Weaker than every content hugging and compression resistance AppKit
/// sets, so it only settles what nothing else decides.
const SHRINK: f32 = 1.0;

/// One open container during a pass.
struct Frame {
    node: NodeId,
    key: Key,
    /// Nodes declared in it so far, in order.
    logical: Vec<NodeId>,
    /// The subset of `logical` that are AppKit children.
    visual: Vec<NodeId>,
}

/// Releases what a dropped node holds outside the tree. A dropped control
/// left its parent's view in that parent's close; dropping the payload
/// releases the view.
fn release(events: &Events, payload: Payload) {
    match payload {
        Payload::Root | Payload::Cache(_) | Payload::State(_) => {}
        Payload::Window(window) => {
            events.forget(&window.window);
            window.window.setDelegate(None);
            window.window.close();
        }
        // A field registers its control, which is not the node's view.
        Payload::Control(control) => {
            if let Some(sender) = control.control.sender() {
                events.forget(sender);
            }
        }
    }
}
