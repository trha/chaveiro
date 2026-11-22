//! The retained node tree.

use std::{
    collections::HashMap,
    hash::{BuildHasherDefault, Hash, Hasher},
    num::NonZeroU32,
};

/// Nodes in a slab, found by key, that live as long as a pass declares them.
///
/// Contract: every live slot's node is reachable from [`root`](Self::root)
/// through [`children`](Node::children) lists exactly once, and
/// [`by_key`](Self::by_key) maps each live node's key to it.
/// [`close`](Self::close) keeps this true by dropping, with its subtree, any old child its parent did not visit this pass.
/// A node visited without building its children (a cache hit) keeps them. A pass therefore
/// costs work in proportion to the containers it closes, not the tree's size.
pub struct Tree<P> {
    slots: Vec<Slot<P>>,
    free: Vec<u32>,
    by_key: HashMap<Key, NodeId, BuildHasherDefault<KeyHasher>>,
    root: NodeId,
    /// The current pass; 0 never matches, so it marks "not this pass".
    pass: u32,
    /// Child lists returned by dropped nodes and closed containers, reused so
    /// a steady pass allocates no list.
    spare_lists: Vec<Vec<NodeId>>,
    /// Work list for dropping subtrees, kept to reuse its capacity.
    drop_stack: Vec<NodeId>,
}

impl<P> Tree<P> {
    /// A tree holding only the root node, whose payload is `root`.
    pub fn new(root: P) -> Self {
        let mut tree = Tree {
            slots: Vec::new(),
            free: Vec::new(),
            by_key: HashMap::default(),
            root: NodeId {
                index: 0,
                generation: NonZeroU32::MIN,
            },
            pass: 0,
            spare_lists: Vec::new(),
            drop_stack: Vec::new(),
        };
        tree.root = tree.insert(Key::ROOT, None, root);
        tree
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Starts a pass and returns its number. The root counts as visited.
    pub fn begin_pass(&mut self) -> u32 {
        if self.pass == u32::MAX {
            // Restart the count so no stale stamp can equal a new pass.
            for node in self.slots.iter_mut().filter_map(|slot| slot.node.as_mut()) {
                node.stamp = 0;
                node.event_stamp = 0;
            }
            self.pass = 0;
        }
        self.pass += 1;
        let pass = self.pass;
        self.node_mut(self.root).stamp = pass;
        pass
    }

    /// Finds the node `key`, a child of `parent`, or creates it with
    /// `create`, and marks it visited this pass. The caller records the
    /// returned id in the list it later passes to [`close`](Self::close).
    ///
    /// Panics with "duplicate id" when `key` was already visited this pass,
    /// which happens when two siblings use the same id. Panics when `key`
    /// belongs to a node under another parent, which only a hash collision
    /// can cause.
    pub fn visit(
        &mut self,
        parent: NodeId,
        key: Key,
        create: impl FnOnce() -> P,
    ) -> (NodeId, Visit) {
        let pass = self.pass;
        match self.by_key.get(&key).copied() {
            Some(id) => {
                let node = self.node_mut(id);
                assert!(
                    node.stamp != pass,
                    "duplicate id: key {key:?} was already built this pass"
                );
                assert!(
                    node.parent == Some(parent),
                    "key {key:?} moved to another parent"
                );
                node.stamp = pass;
                (id, Visit::Existing)
            }
            None => {
                let payload = create();
                let id = self.insert(key, Some(parent), payload);
                (id, Visit::Created)
            }
        }
    }

    /// Makes `visited` the children of `parent` and drops every old child not
    /// visited this pass, with its subtree, passing each dropped payload to
    /// `on_drop`. Leaves `visited` empty, holding the old list's capacity.
    pub fn close(&mut self, parent: NodeId, visited: &mut Vec<NodeId>, mut on_drop: impl FnMut(P)) {
        std::mem::swap(&mut self.node_mut(parent).children, visited);
        let pass = self.pass;
        let mut stack = std::mem::take(&mut self.drop_stack);
        stack.extend(
            visited
                .iter()
                .copied()
                .filter(|id| self.node(*id).stamp != pass),
        );
        visited.clear();
        while let Some(id) = stack.pop() {
            let node = self.remove(id);
            stack.extend_from_slice(&node.children);
            let mut children = node.children;
            children.clear();
            self.spare_lists.push(children);
            on_drop(node.payload);
        }
        self.drop_stack = stack;
    }

    /// Creates the node `key` under `parent` outside the parent's build, and
    /// appends it to the parent's children at once, so it stays reachable
    /// until the parent's next [`close`](Self::close) lists it. Used for rows
    /// that a toolkit asks for during layout.
    ///
    /// Panics when `key` is already live.
    pub fn adopt(&mut self, parent: NodeId, key: Key, payload: P) -> NodeId {
        assert!(self.find(key).is_none(), "key {key:?} is already live");
        let id = self.insert(key, Some(parent), payload);
        self.node_mut(parent).children.push(id);
        id
    }

    /// Marks `id` and its ancestors as having an event this pass.
    pub fn mark_event(&mut self, id: NodeId) {
        let pass = self.pass;
        let mut current = Some(id);
        while let Some(id) = current {
            let node = self.node_mut(id);
            if node.event_stamp == pass {
                break;
            }
            node.event_stamp = pass;
            current = node.parent;
        }
    }

    /// Whether an event targeted `id` or a node under it this pass.
    pub fn has_event(&self, id: NodeId) -> bool {
        self.node(id).event_stamp == self.pass
    }

    /// An empty child list, reusing a spare one when there is one.
    pub fn take_list(&mut self) -> Vec<NodeId> {
        self.spare_lists.pop().unwrap_or_default()
    }

    /// Returns a list to the spare pool. It must be empty.
    pub fn recycle_list(&mut self, list: Vec<NodeId>) {
        debug_assert!(list.is_empty());
        self.spare_lists.push(list);
    }

    pub fn find(&self, key: Key) -> Option<NodeId> {
        self.by_key.get(&key).copied()
    }

    /// The node `id` names, or [`None`] when it was dropped.
    pub fn get(&self, id: NodeId) -> Option<&Node<P>> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.node.as_ref()
    }

    /// Panics when `id` was dropped; [`node_mut`](Self::node_mut) and
    /// [`children`](Self::children) do too.
    pub fn node(&self, id: NodeId) -> &Node<P> {
        self.get(id)
            .expect("node ids held by a platform crate stay live until dropped")
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node<P> {
        let slot = &mut self.slots[id.index as usize];
        assert!(slot.generation == id.generation, "stale node id");
        slot.node
            .as_mut()
            .expect("node ids held by a platform crate stay live until dropped")
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).children
    }

    fn insert(&mut self, key: Key, parent: Option<NodeId>, payload: P) -> NodeId {
        let node = Node {
            key,
            parent,
            stamp: self.pass,
            event_stamp: 0,
            children: self.spare_lists.pop().unwrap_or_default(),
            payload,
        };
        let id = match self.free.pop() {
            Some(index) => {
                let slot = &mut self.slots[index as usize];
                slot.node = Some(node);
                NodeId {
                    index,
                    generation: slot.generation,
                }
            }
            None => {
                let index = u32::try_from(self.slots.len()).expect("fewer than 2^32 live nodes");
                self.slots.push(Slot {
                    generation: NonZeroU32::MIN,
                    node: Some(node),
                });
                NodeId {
                    index,
                    generation: NonZeroU32::MIN,
                }
            }
        };
        self.by_key.insert(key, id);
        id
    }

    fn remove(&mut self, id: NodeId) -> Node<P> {
        let slot = &mut self.slots[id.index as usize];
        let node = slot
            .node
            .take()
            .expect("a dropped id is live until removed");
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(id.index);
        }
        self.by_key.remove(&node.key);
        node
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visit {
    Created,
    Existing,
}

/// Identifies a node across passes: a hash of the parent's key and the
/// widget's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(u64);

impl Key {
    /// The key of the tree's root node.
    pub const ROOT: Key = Key(0);

    /// The key of the child `id` of the node keyed `parent`.
    pub fn child(parent: Key, id: impl Hash) -> Key {
        // Fixed seeds: keys only need to be stable within one process, and
        // fixed seeds make them reproducible in tests.
        const HASHER: ahash::RandomState = ahash::RandomState::with_seeds(
            0x243f_6a88_85a3_08d3,
            0x1319_8a2e_0370_7344,
            0xa409_3822_299f_31d0,
            0x082e_fa98_ec4e_6c89,
        );
        Key(HASHER.hash_one((parent.0, id)))
    }
}

/// Names a node only while `generation` equals its slot's generation, so an
/// id kept after its node was dropped never reads a later node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId {
    index: u32,
    generation: NonZeroU32,
}

const _: () = assert!(size_of::<Option<NodeId>>() == size_of::<NodeId>());

pub struct Node<P> {
    key: Key,
    /// Only [`Tree::mark_event`] reads it, to stamp ancestors.
    parent: Option<NodeId>,
    /// Pass in which this node was last visited.
    stamp: u32,
    /// Pass in which an event targeted this node or a descendant.
    event_stamp: u32,
    /// Children in build order, as of the last pass that closed this node.
    children: Vec<NodeId>,
    pub payload: P,
}

impl<P> Node<P> {
    pub fn key(&self) -> Key {
        self.key
    }
}

struct Slot<P> {
    /// Starts at 1 and rises each time the slot is freed. A slot whose
    /// generation reaches [`u32::MAX`] is retired instead of reused.
    generation: NonZeroU32,
    node: Option<Node<P>>,
}

/// Hashes a [`Key`] to its own value: [`Key::child`] already mixed it, so
/// hashing it again would only cost time.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("a Key hashes as one u64, through write_u64")
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

#[doc(hidden)]
impl NodeId {
    /// An id for tests that never touch a tree, such as child-list diffs.
    pub fn for_test(index: u32) -> Self {
        NodeId {
            index,
            generation: NonZeroU32::MIN,
        }
    }
}

#[doc(hidden)]
impl<P> Tree<P> {
    /// Ids of every live node.
    pub fn live(&self) -> Vec<NodeId> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.node.is_some())
            .map(|(index, slot)| NodeId {
                index: index as u32,
                generation: slot.generation,
            })
            .collect()
    }

    pub fn key_count(&self) -> usize {
        self.by_key.len()
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }
}

#[cfg(test)]
mod tests;
