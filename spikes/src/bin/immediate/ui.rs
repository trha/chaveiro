//! An immediate-mode layer over the WinUI retained tree.
//!
//! The app calls `build` on every event. Each widget call finds its control in
//! a cache by key, creates it on first use, and sets only the properties whose
//! value differs from the last value this layer set. Controls not touched in a
//! build are detached and dropped. WinUI events become data: a handler queues
//! `(key, event)` and asks for a rebuild, and the widget reads it during the
//! next build.
//!
//! Values the user edits (text, password, scroll offset) are owned by the
//! control. The layer reads them back and pushes an app value only when the app
//! changed it, so caret, selection and IME composition survive rebuilds.

use crate::winui::*;
use std::{
    cell::{Cell, RefCell},
    hash::{BuildHasherDefault, Hash, Hasher},
    rc::Rc,
};
use windows_core::{Interface, Result};

/// Identifies a widget: a hash of its parent's key and its local id.
pub type Key = u64;

/// Counts of the work one build did on WinUI.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub widgets: usize,
    pub created: usize,
    pub dropped: usize,
    /// Property writes and reads on WinUI controls, including child list edits.
    pub calls: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Event {
    Click,
    TextChanged,
    Scrolled,
}

enum Control {
    Stack(StackPanel, UIElementCollection),
    Canvas(Canvas, UIElementCollection),
    Scroll(ScrollViewer),
    Text(TextBlock),
    Button(Button),
    TextBox(TextBox),
    Password(PasswordBox),
    Repeater(ItemsRepeater),
}

/// Row bookkeeping for one list.
///
/// Pool mode: `slot_of` maps an entry key to the slot whose row controls show
/// it, and `free` holds slots whose rows are collapsed and waiting for reuse.
/// Repeater mode: `realized` maps a row key to the item index WinUI realized it
/// for, `element_rows` maps the row's element pointer back to its row key, and
/// `free_rows` holds row keys WinUI recycled.
#[derive(Default)]
struct ListState {
    slot_of: rustc_hash::FxHashMap<u64, u32>,
    free: Vec<u32>,
    next_slot: u32,
    realized: rustc_hash::FxHashMap<Key, usize>,
    element_rows: rustc_hash::FxHashMap<usize, Key>,
    free_rows: Vec<Key>,
    source: Option<windows_collections::IObservableVector<windows_core::IInspectable>>,
    row_count: usize,
    revision: u64,
}

/// Calls from WinUI's `ItemsRepeater` back into the app, set by the runner.
/// `realize(list, index)` builds row `index` and returns its element;
/// `recycle(list, element)` returns a row to the list's pool.
pub struct RepeaterHooks {
    pub realize: Rc<dyn Fn(Key, usize) -> Result<UIElement>>,
    pub recycle: Rc<dyn Fn(Key, &UIElement) -> Result<()>>,
}

thread_local! {
    pub static REPEATER_HOOKS: RefCell<Option<RepeaterHooks>> = const { RefCell::new(None) };
}

windows_core::implement_decl! {
    impl RowFactory as RowFactory_Impl: [IElementFactory]
}

struct RowFactory {
    list: Key,
}

impl IElementFactory_Impl for RowFactory_Impl {
    fn GetElement(&self, args: windows_core::Ref<ElementFactoryGetArgs>) -> Result<UIElement> {
        let data = args.ok()?.Data()?;
        let index = data.cast::<windows_reference::IReference<i32>>()?.Value()?;
        let realize = REPEATER_HOOKS
            .with(|hooks| {
                hooks
                    .borrow()
                    .as_ref()
                    .map(|hooks| Rc::clone(&hooks.realize))
            })
            .expect("hooks are set before the first build");
        realize(
            self.list,
            usize::try_from(index).expect("indexes are created non-negative"),
        )
    }

    fn RecycleElement(&self, args: windows_core::Ref<ElementFactoryRecycleArgs>) -> Result<()> {
        let element = args.ok()?.Element()?;
        let recycle = REPEATER_HOOKS
            .with(|hooks| {
                hooks
                    .borrow()
                    .as_ref()
                    .map(|hooks| Rc::clone(&hooks.recycle))
            })
            .expect("hooks are set before the first build");
        recycle(self.list, &element)
    }
}

struct Node {
    control: Control,
    element: UIElement,
    /// Last text, label or password this layer wrote, or last read back.
    text: String,
    /// Last position or size this layer wrote (`Canvas.Top`, height, spacing).
    number: f64,
    /// Child keys in collection order, as of the end of the last build.
    children: Vec<Key>,
    touched: u64,
    /// Whether this layer collapsed the element (pooled rows).
    hidden: bool,
    _revoker: Option<windows_core::EventRevoker>,
}

/// One open container during a build.
struct Frame {
    key: Key,
    children: Vec<Key>,
    /// Whether child order matters. Canvas children are placed by `Canvas.Top`.
    ordered: bool,
}

type NodeMap = std::collections::HashMap<Key, Node, BuildHasherDefault<PassThroughHasher>>;

/// Keys are already hashes, so the map uses them as they are.
#[derive(Default)]
struct PassThroughHasher(u64);

impl Hasher for PassThroughHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _bytes: &[u8]) {
        unreachable!("only u64 keys are hashed");
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

/// Events queued by WinUI handlers, shared with them. Handlers only push and
/// wake, so they never re-enter a build.
#[derive(Default)]
pub struct Inbox {
    events: RefCell<Vec<(Key, Event)>>,
    wake: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Inbox {
    fn push(&self, key: Key, event: Event) {
        self.events.borrow_mut().push((key, event));
        if let Some(wake) = self.wake.borrow().clone() {
            wake();
        }
    }
}

pub struct Ui {
    nodes: NodeMap,
    build: u64,
    frames: Vec<Frame>,
    /// Spare child lists, reused so a build allocates none in steady state.
    spare_lists: Vec<Vec<Key>>,
    inbox: Rc<Inbox>,
    events: Vec<(Key, Event)>,
    scratch: String,
    root: Option<Key>,
    pub stats: Stats,
    /// Bench hook: replaces the scroll offset that lists read from WinUI.
    pub scroll_override: Option<f64>,
    /// Offset and viewport height the last list read, for logging.
    pub last_scroll: (f64, f64),
    /// Canvas lists reuse row controls through a slot pool instead of
    /// creating and dropping them.
    pub pool_rows: bool,
    lists: rustc_hash::FxHashMap<Key, ListState>,
    /// Reused scratch list of `(index, entry key)` or `(row key, index)`.
    scratch_rows: Vec<(usize, u64)>,
    /// `ItemsSource` replacements to apply after the build, outside any
    /// borrow, because WinUI may call the factory synchronously.
    pending_sources: Vec<(ItemsRepeater, Key, usize)>,
}

impl Ui {
    pub fn new(inbox: Rc<Inbox>) -> Self {
        Self {
            nodes: NodeMap::default(),
            build: 0,
            frames: Vec::new(),
            spare_lists: Vec::new(),
            inbox,
            events: Vec::new(),
            scratch: String::new(),
            root: None,
            stats: Stats::default(),
            scroll_override: None,
            last_scroll: (0.0, 0.0),
            pool_rows: false,
            lists: rustc_hash::FxHashMap::default(),
            scratch_rows: Vec::new(),
            pending_sources: Vec::new(),
        }
    }

    /// Runs one build. `content` must open exactly one root container, which
    /// becomes the window content on the first build.
    ///
    /// Returns whether the build consumed events. Widgets built before the
    /// widget that read an event still show the old state, so the caller must
    /// then build once more.
    pub fn run(
        &mut self,
        window: &Window,
        content: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<bool> {
        self.build += 1;
        self.stats = Stats::default();
        self.events.clear();
        std::mem::swap(&mut self.events, &mut *self.inbox.events.borrow_mut());
        self.frames.push(Frame {
            key: 0,
            children: self.spare_lists.pop().unwrap_or_default(),
            ordered: true,
        });
        content(self)?;
        let top = self.frames.pop().expect("the root frame is pushed above");
        let root = *top
            .children
            .first()
            .expect("a build must open a root container");
        if self.root != Some(root) {
            window.SetContent(&self.nodes[&root].element)?;
            self.root = Some(root);
        }
        self.recycle(top.children);
        self.prune();
        Ok(!self.events.is_empty())
    }

    fn recycle(&mut self, mut list: Vec<Key>) {
        list.clear();
        self.spare_lists.push(list);
    }

    fn key(&self, id: impl Hash) -> Key {
        let mut hasher = rustc_hash::FxHasher::default();
        self.frames
            .last()
            .expect("widgets are built inside a container")
            .key
            .hash(&mut hasher);
        id.hash(&mut hasher);
        hasher.finish()
    }

    fn event(&self, key: Key, event: Event) -> bool {
        self.events.iter().any(|queued| *queued == (key, event))
    }

    /// Finds or creates the node for `key`, marks it touched and records it as
    /// a child of the open container.
    fn node(
        &mut self,
        key: Key,
        create: impl FnOnce(&Rc<Inbox>, Key) -> Result<(Control, Option<windows_core::EventRevoker>)>,
    ) -> Result<&mut Node> {
        self.stats.widgets += 1;
        self.frames
            .last_mut()
            .expect("widgets are built inside a container")
            .children
            .push(key);
        if !self.nodes.contains_key(&key) {
            let (control, revoker) = create(&self.inbox, key)?;
            let element: UIElement = match &control {
                Control::Stack(control, _) => control.cast()?,
                Control::Canvas(control, _) => control.cast()?,
                Control::Scroll(control) => control.cast()?,
                Control::Text(control) => control.cast()?,
                Control::Button(control) => control.cast()?,
                Control::TextBox(control) => control.cast()?,
                Control::Password(control) => control.cast()?,
                Control::Repeater(control) => control.cast()?,
            };
            self.stats.created += 1;
            self.nodes.insert(
                key,
                Node {
                    control,
                    element,
                    text: String::new(),
                    number: f64::NAN,
                    children: Vec::new(),
                    touched: 0,
                    hidden: false,
                    _revoker: revoker,
                },
            );
        }
        let node = self.nodes.get_mut(&key).expect("inserted above");
        node.touched = self.build;
        Ok(node)
    }

    fn open(&mut self, key: Key, ordered: bool) {
        let children = self.spare_lists.pop().unwrap_or_default();
        self.frames.push(Frame {
            key,
            children,
            ordered,
        });
    }

    /// Closes the open container and edits its WinUI child list to match.
    fn close(&mut self) -> Result<()> {
        let frame = self.frames.pop().expect("close matches open");
        let mut old = std::mem::take(
            &mut self
                .nodes
                .get_mut(&frame.key)
                .expect("container node exists")
                .children,
        );
        if old != frame.children {
            let collection = match &self.nodes[&frame.key].control {
                Control::Stack(_, children) | Control::Canvas(_, children) => children.clone(),
                _ => unreachable!("only panels open frames"),
            };
            if frame.ordered {
                // Keep the common prefix and suffix, replace the middle.
                let prefix = old
                    .iter()
                    .zip(&frame.children)
                    .take_while(|(a, b)| a == b)
                    .count();
                let suffix = old[prefix..]
                    .iter()
                    .rev()
                    .zip(frame.children[prefix..].iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();
                for _ in prefix..old.len() - suffix {
                    collection.RemoveAt(prefix as u32)?;
                    self.stats.calls += 1;
                }
                for (offset, key) in frame.children[prefix..frame.children.len() - suffix]
                    .iter()
                    .enumerate()
                {
                    collection.InsertAt((prefix + offset) as u32, &self.nodes[key].element)?;
                    self.stats.calls += 1;
                }
            } else {
                // Order is free: detach the keys that left, append the new ones.
                for key in old.iter().filter(|key| !frame.children.contains(key)) {
                    if let Some(node) = self.nodes.get(key) {
                        let mut index = 0;
                        if collection.IndexOf(&node.element, &mut index)? {
                            collection.RemoveAt(index)?;
                        }
                        self.stats.calls += 2;
                    }
                }
                for key in frame.children.iter().filter(|key| !old.contains(key)) {
                    collection.Append(&self.nodes[key].element)?;
                    self.stats.calls += 1;
                }
            }
        }
        old.clear();
        old.extend_from_slice(&frame.children);
        self.nodes
            .get_mut(&frame.key)
            .expect("container node exists")
            .children = old;
        self.recycle(frame.children);
        Ok(())
    }

    /// Drops every node not touched in this build. Their parents already
    /// detached them in `close`.
    fn prune(&mut self) {
        let build = self.build;
        let before = self.nodes.len();
        self.nodes.retain(|_, node| node.touched == build);
        self.stats.dropped = before - self.nodes.len();
    }

    /// A vertical or horizontal stack. Children keep the order they are built in.
    pub fn stack(
        &mut self,
        id: impl Hash,
        horizontal: bool,
        spacing: f64,
        content: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<()> {
        let key = self.key(id);
        let node = self.node(key, |_, _| {
            let panel = StackPanel::new()?;
            if horizontal {
                panel.SetOrientation(Orientation::Horizontal)?;
            }
            let children = panel.cast::<Panel>()?.Children()?;
            Ok((Control::Stack(panel, children), None))
        })?;
        if node.number != spacing {
            let Control::Stack(panel, _) = &node.control else {
                unreachable!("key {key} is a stack")
            };
            panel.SetSpacing(spacing)?;
            node.number = spacing;
            self.stats.calls += 1;
        }
        self.open(key, true);
        content(self)?;
        self.close()
    }

    /// A text label. `text` is formatted into a reused buffer, so an unchanged
    /// label costs no allocation and no WinUI call.
    pub fn label(&mut self, id: impl Hash, text: std::fmt::Arguments<'_>) -> Result<()> {
        let key = self.key(id);
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        std::fmt::Write::write_fmt(&mut scratch, text).expect("writing to a String cannot fail");
        let node = self.node(key, |_, _| Ok((Control::Text(TextBlock::new()?), None)))?;
        if node.text != scratch {
            let Control::Text(block) = &node.control else {
                unreachable!("key {key} is a label")
            };
            block.SetText(&scratch)?;
            node.text.clear();
            node.text.push_str(&scratch);
            self.stats.calls += 1;
        }
        self.scratch = scratch;
        Ok(())
    }

    /// A push button. Returns whether it was clicked since the last build.
    pub fn button(&mut self, id: impl Hash, label: &str) -> Result<bool> {
        let key = self.key(id);
        let node = self.node(key, |inbox, key| {
            let button = Button::new()?;
            let inbox = Rc::clone(inbox);
            let revoker = button
                .cast::<ButtonBase>()?
                .Click(move |_, _| inbox.push(key, Event::Click))?;
            Ok((Control::Button(button), Some(revoker)))
        })?;
        if node.text != label {
            let Control::Button(button) = &node.control else {
                unreachable!("key {key} is a button")
            };
            let block = TextBlock::new()?;
            block.SetText(label)?;
            button.cast::<ContentControl>()?.SetContent(&block)?;
            node.text.clear();
            node.text.push_str(label);
            self.stats.calls += 2;
        }
        Ok(self.event(key, Event::Click))
    }

    /// A text box bound to `value`. Returns whether the user changed it.
    ///
    /// The control owns the text while the user edits. The layer writes
    /// `value` into it only when the app changed `value` since the last build.
    pub fn text_box(
        &mut self,
        id: impl Hash,
        placeholder: &str,
        value: &mut String,
    ) -> Result<bool> {
        let key = self.key(id);
        let typed = self.event(key, Event::TextChanged);
        let mut calls = 0;
        let node = self.node(key, |inbox, key| {
            let text_box = TextBox::new()?;
            text_box.SetPlaceholderText(placeholder)?;
            let inbox = Rc::clone(inbox);
            let revoker = text_box.TextChanged(move |_, _| inbox.push(key, Event::TextChanged))?;
            Ok((Control::TextBox(text_box), Some(revoker)))
        })?;
        let Control::TextBox(text_box) = &node.control else {
            unreachable!("key {key} is a text box")
        };
        let mut changed = false;
        if typed {
            let text = text_box.Text()?;
            calls += 1;
            // An echo of our own write reads back the value we already hold.
            if text != node.text {
                value.clone_from(&text);
                node.text = text;
                changed = true;
            }
        } else if *value != node.text {
            text_box.SetText(value)?;
            node.text.clone_from(value);
            calls += 1;
        }
        self.stats.calls += calls;
        Ok(changed)
    }

    /// A password box whose text stays in the control. Returns its key, which
    /// `take_password` reads on submit. The app never holds a live copy.
    pub fn password_box(&mut self, id: impl Hash, placeholder: &str) -> Result<Key> {
        let key = self.key(id);
        self.node(key, |_, _| {
            let password_box = PasswordBox::new()?;
            password_box.SetPlaceholderText(placeholder)?;
            Ok((Control::Password(password_box), None))
        })?;
        Ok(key)
    }

    /// Reads the password from the box built this build under `key`, then
    /// clears the box.
    pub fn take_password(&mut self, key: Key) -> Result<String> {
        let Control::Password(password_box) = &self.nodes[&key].control else {
            unreachable!("key {key} is a password box")
        };
        let password = password_box.Password()?;
        password_box.SetPassword("")?;
        self.stats.calls += 2;
        Ok(password)
    }

    /// A vertical list of `row_count` rows of fixed height inside a scroll
    /// viewer. Only rows in view (plus `OVERSCAN` on each side) are built.
    /// `row_key` gives each row a stable identity, and `row` builds row `index`.
    pub fn list(
        &mut self,
        id: impl Hash,
        height: f64,
        row_count: usize,
        row_height: f64,
        row_key: impl Fn(usize) -> u64,
        mut row: impl FnMut(&mut Ui, usize) -> Result<()>,
    ) -> Result<()> {
        const OVERSCAN: usize = 4;
        let scroll_key = self.key(id);
        let node = self.node(scroll_key, |inbox, key| {
            let scroll = ScrollViewer::new()?;
            let inbox = Rc::clone(inbox);
            let revoker = scroll.ViewChanged(move |_, _| inbox.push(key, Event::Scrolled))?;
            Ok((Control::Scroll(scroll), Some(revoker)))
        })?;
        let Control::Scroll(scroll) = &node.control else {
            unreachable!("key {scroll_key} is a list")
        };
        let scroll = scroll.clone();
        if node.number != height {
            scroll.cast::<FrameworkElement>()?.SetHeight(height)?;
            node.number = height;
            self.stats.calls += 1;
        }
        let (offset, viewport) = match self.scroll_override {
            Some(offset) => (offset, height),
            None => {
                self.stats.calls += 2;
                (scroll.VerticalOffset()?, scroll.ViewportHeight()?)
            }
        };
        self.last_scroll = (offset, viewport);
        let viewport = if viewport > 0.0 { viewport } else { height };

        // The canvas is the scroll viewer's content, not a panel child.
        self.frames.push(Frame {
            key: scroll_key,
            children: self.spare_lists.pop().unwrap_or_default(),
            ordered: true,
        });
        let canvas_key = self.key("canvas");
        let canvas_node = self.node(canvas_key, |_, _| {
            let canvas = Canvas::new()?;
            let children = canvas.cast::<Panel>()?.Children()?;
            Ok((Control::Canvas(canvas, children), None))
        })?;
        let content_height = row_count as f64 * row_height;
        let resized = canvas_node.number != content_height;
        if resized {
            canvas_node
                .element
                .cast::<FrameworkElement>()?
                .SetHeight(content_height)?;
            canvas_node.number = content_height;
        }
        let canvas_element = canvas_node.element.clone();
        self.stats.calls += usize::from(resized);
        let top = self.frames.pop().expect("pushed above");
        if self.nodes[&scroll_key].children.first() != Some(&canvas_key) {
            scroll
                .cast::<ContentControl>()?
                .SetContent(&canvas_element)?;
            self.nodes
                .get_mut(&scroll_key)
                .expect("scroll node exists")
                .children = vec![canvas_key];
            self.stats.calls += 1;
        }
        self.recycle(top.children);

        let first = ((offset / row_height).floor() as usize).saturating_sub(OVERSCAN);
        let last = (((offset + viewport) / row_height).ceil() as usize + OVERSCAN).min(row_count);
        self.open(canvas_key, false);
        if !self.pool_rows {
            for index in first..last {
                let key = self.key(row_key(index));
                self.canvas_row(key, index as f64 * row_height, index, &mut row)?;
            }
            return self.close();
        }

        // Pool mode: each entry in view owns a slot, and the slot's controls
        // are reused when an entry scrolls out and another scrolls in.
        let mut wanted = std::mem::take(&mut self.scratch_rows);
        wanted.clear();
        wanted.extend((first..last).map(|index| (index, row_key(index))));
        let state = self.lists.entry(canvas_key).or_default();
        let ListState {
            slot_of,
            free,
            next_slot,
            ..
        } = state;
        slot_of.retain(|entry, slot| {
            let keep = wanted.iter().any(|(_, wanted_entry)| wanted_entry == entry);
            if !keep {
                free.push(*slot);
            }
            keep
        });
        for (_, entry) in &wanted {
            if !slot_of.contains_key(entry) {
                let slot = free.pop().unwrap_or_else(|| {
                    *next_slot += 1;
                    *next_slot - 1
                });
                slot_of.insert(*entry, slot);
            }
        }
        for &(index, entry) in &wanted {
            let slot = self.lists[&canvas_key].slot_of[&entry];
            let key = self.key(("slot", slot));
            self.canvas_row(key, index as f64 * row_height, index, &mut row)?;
        }
        // Free slots stay in the canvas, collapsed, so reuse costs no detach.
        for position in 0..self.lists[&canvas_key].free.len() {
            let slot = self.lists[&canvas_key].free[position];
            let key = self.key(("slot", slot));
            if self.nodes.contains_key(&key) {
                self.keep_alive(key);
                let node = self.nodes.get_mut(&key).expect("checked above");
                if !node.hidden {
                    node.element.SetVisibility(Visibility::Collapsed)?;
                    node.hidden = true;
                    self.stats.calls += 1;
                }
            }
        }
        self.scratch_rows = wanted;
        self.close()
    }

    /// Builds one row of a canvas list at `top`, showing it if it was pooled.
    fn canvas_row(
        &mut self,
        key: Key,
        top: f64,
        index: usize,
        row: &mut impl FnMut(&mut Ui, usize) -> Result<()>,
    ) -> Result<()> {
        let row_node = self.node(key, new_row)?;
        let mut calls = 0;
        if row_node.hidden {
            row_node.element.SetVisibility(Visibility::Visible)?;
            row_node.hidden = false;
            calls += 1;
        }
        if row_node.number != top {
            Canvas::SetTop(&row_node.element, top)?;
            row_node.number = top;
            calls += 1;
        }
        self.stats.calls += calls;
        self.open(key, true);
        row(self, index)?;
        self.close()
    }

    /// Keeps a node and its whole subtree alive through this build without
    /// rebuilding it, and records it as a child of the open container.
    fn keep_alive(&mut self, key: Key) {
        self.frames
            .last_mut()
            .expect("inside a container")
            .children
            .push(key);
        self.touch_tree(key);
    }

    fn touch_tree(&mut self, key: Key) {
        let Some(node) = self.nodes.get_mut(&key) else {
            return;
        };
        node.touched = self.build;
        for position in 0..node.children.len() {
            let child = self.nodes[&key].children[position];
            self.touch_tree(child);
        }
    }

    /// A vertical list backed by WinUI's `ItemsRepeater`. WinUI decides which
    /// rows exist: it calls `realize` (through `REPEATER_HOOKS`) to build a row
    /// and `recycle` to return one. This build rebuilds every realized row so
    /// they show current state, and keeps recycled rows alive for reuse.
    ///
    /// `revision` must change whenever the order or identity of rows changes;
    /// WinUI then recycles every row and asks for the visible ones again.
    pub fn repeater_list(
        &mut self,
        id: impl Hash,
        height: f64,
        row_count: usize,
        revision: u64,
        mut row: impl FnMut(&mut Ui, usize) -> Result<()>,
    ) -> Result<()> {
        let scroll_key = self.key(id);
        let node = self.node(scroll_key, |inbox, key| {
            let scroll = ScrollViewer::new()?;
            let inbox = Rc::clone(inbox);
            let revoker = scroll.ViewChanged(move |_, _| inbox.push(key, Event::Scrolled))?;
            Ok((Control::Scroll(scroll), Some(revoker)))
        })?;
        let Control::Scroll(scroll) = &node.control else {
            unreachable!("key {scroll_key} is a list")
        };
        let scroll = scroll.clone();
        if node.number != height {
            scroll.cast::<FrameworkElement>()?.SetHeight(height)?;
            node.number = height;
            self.stats.calls += 1;
        }

        self.frames.push(Frame {
            key: scroll_key,
            children: self.spare_lists.pop().unwrap_or_default(),
            ordered: true,
        });
        let repeater_key = self.key("repeater");
        let repeater_node = self.node(repeater_key, |_, key| {
            let repeater = ItemsRepeater::new()?;
            let factory: IElementFactory =
                windows_core::ComObject::new(RowFactory { list: key }).into_interface();
            repeater.SetItemTemplate(&factory.cast::<windows_core::IInspectable>()?)?;
            Ok((Control::Repeater(repeater), None))
        })?;
        let Control::Repeater(repeater) = &repeater_node.control else {
            unreachable!("key {repeater_key} is a repeater")
        };
        let repeater = repeater.clone();
        let repeater_element = repeater_node.element.clone();
        let top = self.frames.pop().expect("pushed above");
        self.recycle(top.children);
        if self.nodes[&scroll_key].children.first() != Some(&repeater_key) {
            scroll
                .cast::<ContentControl>()?
                .SetContent(&repeater_element)?;
            self.nodes
                .get_mut(&scroll_key)
                .expect("scroll node exists")
                .children = vec![repeater_key];
            self.stats.calls += 1;
        }

        let state = self.lists.entry(repeater_key).or_default();
        if state.source.is_none() || state.row_count != row_count || state.revision != revision {
            state.row_count = row_count;
            state.revision = revision;
            self.pending_sources
                .push((repeater, repeater_key, row_count));
        }

        // Rebuild realized rows in place. Their elements belong to the
        // repeater, so this frame's child list is not applied to any panel.
        self.frames.push(Frame {
            key: repeater_key,
            children: self.spare_lists.pop().unwrap_or_default(),
            ordered: false,
        });
        let mut realized = std::mem::take(&mut self.scratch_rows);
        realized.clear();
        realized.extend(
            self.lists[&repeater_key]
                .realized
                .iter()
                .map(|(key, index)| (*index, *key)),
        );
        for &(index, key) in &realized {
            if index < row_count && self.nodes.contains_key(&key) {
                self.node(key, new_row)?;
                self.open(key, true);
                row(self, index)?;
                self.close()?;
            } else {
                self.keep_alive(key);
            }
        }
        for position in 0..self.lists[&repeater_key].free_rows.len() {
            let key = self.lists[&repeater_key].free_rows[position];
            self.keep_alive(key);
        }
        self.scratch_rows = realized;
        let pseudo = self.frames.pop().expect("pushed above");
        self.recycle(pseudo.children);
        Ok(())
    }

    /// `ItemsSource` updates queued by this build. Apply them after releasing
    /// every borrow of `Ui`, because WinUI may call the factory at once.
    pub fn take_pending_sources(&mut self) -> Vec<(ItemsRepeater, Key, usize)> {
        std::mem::take(&mut self.pending_sources)
    }

    /// Stores a new source for `list`, after `apply_source` built it.
    pub fn set_source(
        &mut self,
        list: Key,
        source: windows_collections::IObservableVector<windows_core::IInspectable>,
    ) {
        self.lists.entry(list).or_default().source = Some(source);
    }

    /// Builds row `index` for repeater `list`, reusing a recycled row's
    /// controls when one is free. Called from `IElementFactory::GetElement`.
    pub fn realize(
        &mut self,
        list: Key,
        index: usize,
        row: impl FnOnce(&mut Ui, usize) -> Result<()>,
    ) -> Result<UIElement> {
        self.frames.push(Frame {
            key: list,
            children: self.spare_lists.pop().unwrap_or_default(),
            ordered: false,
        });
        let reused = self.lists.entry(list).or_default().free_rows.pop();
        let key = match reused {
            Some(key) => key,
            None => {
                let state = self.lists.get_mut(&list).expect("inserted above");
                state.next_slot += 1;
                let slot = state.next_slot - 1;
                self.key(("slot", slot))
            }
        };
        let element = self.node(key, new_row)?.element.clone();
        self.open(key, true);
        row(self, index)?;
        self.close()?;
        let pseudo = self.frames.pop().expect("pushed above");
        self.recycle(pseudo.children);
        let state = self.lists.get_mut(&list).expect("inserted above");
        state.realized.insert(key, index);
        state.element_rows.insert(element.as_raw() as usize, key);
        Ok(element)
    }

    /// Returns a row WinUI recycled to `list`'s pool. Called from
    /// `IElementFactory::RecycleElement`.
    pub fn recycle_row(&mut self, list: Key, element: &UIElement) {
        let state = self.lists.entry(list).or_default();
        if let Some(key) = state.element_rows.remove(&(element.as_raw() as usize)) {
            state.realized.remove(&key);
            state.free_rows.push(key);
        }
    }
}

fn new_row(_: &Rc<Inbox>, _: Key) -> Result<(Control, Option<windows_core::EventRevoker>)> {
    let panel = StackPanel::new()?;
    panel.SetOrientation(Orientation::Horizontal)?;
    panel.SetSpacing(12.0)?;
    let children = panel.cast::<Panel>()?.Children()?;
    Ok((Control::Stack(panel, children), None))
}

/// Builds the `ItemsSource` for a repeater of `row_count` rows: each item is
/// its own index, boxed, because rows are built from the index.
pub fn index_source(
    row_count: usize,
) -> Result<windows_collections::IObservableVector<windows_core::IInspectable>> {
    let values = (0..row_count)
        .map(|index| {
            let index = i32::try_from(index).expect("row counts fit in i32");
            Some(windows_reference::IReference::<i32>::from(index).into())
        })
        .collect::<Vec<Option<windows_core::IInspectable>>>();
    Ok(values.into())
}

/// Coalesces rebuild requests into one `DispatcherQueue` callback.
pub struct Scheduler {
    pending: Cell<bool>,
    dispatcher: DispatcherQueue,
}

impl Scheduler {
    pub fn new() -> Result<Rc<Self>> {
        Ok(Rc::new(Self {
            pending: Cell::new(false),
            dispatcher: DispatcherQueue::GetForCurrentThread()?,
        }))
    }

    /// Queues `build` unless a build is already queued.
    pub fn request(self: &Rc<Self>, build: &Rc<dyn Fn()>) {
        if self.pending.replace(true) {
            return;
        }
        let scheduler = Rc::clone(self);
        let build = Rc::clone(build);
        let handler = DispatcherQueueHandler::new(move || {
            scheduler.pending.set(false);
            build();
        });
        // `false` means the queue is shutting down, so there is nothing to build for.
        let _ = self.dispatcher.TryEnqueue(&handler);
    }
}

/// Connects the inbox to a scheduler, so every queued event requests a build.
pub fn connect(inbox: &Inbox, wake: Rc<dyn Fn()>) {
    *inbox.wake.borrow_mut() = Some(wake);
}
