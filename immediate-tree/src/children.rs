//! Turns a panel's old child list into its new one with few collection edits.

use crate::tree::NodeId;

/// Edits that turn `old` into `new`, leaving their common prefix and suffix
/// in place. Apply them in order: each index refers to the collection as the
/// earlier edits left it. Both lists must hold unique ids. Equal lists yield
/// no edits.
pub fn edits(old: &[NodeId], new: &[NodeId]) -> impl Iterator<Item = Edit> {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(old, new)| old == new)
        .count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let start = u32::try_from(prefix).expect("child lists are shorter than 2^32");
    let removed = old.len() - prefix - suffix;
    let inserted = &new[prefix..new.len() - suffix];
    std::iter::repeat_n(Edit::RemoveAt(start), removed).chain(
        inserted
            .iter()
            .zip(start..)
            .map(|(id, index)| Edit::InsertAt(index, *id)),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    RemoveAt(u32),
    InsertAt(u32, NodeId),
}

#[cfg(test)]
mod tests;
