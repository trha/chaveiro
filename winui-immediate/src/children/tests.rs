use super::*;
use proptest::prelude::*;

proptest! {
    #[test]
    fn edits_turn_old_into_new(old in unique_list(), new in unique_list()) {
        prop_assert_eq!(apply(&old, edits(&old, &new)), new);
    }

    #[test]
    fn equal_lists_need_no_edits(list in unique_list()) {
        prop_assert_eq!(edits(&list, &list).count(), 0);
    }

    #[test]
    fn appending_only_inserts(list in unique_list(), split in 0usize..=12) {
        let split = split.min(list.len());
        let (old, appended) = list.split_at(split);
        let all: Vec<Edit> = edits(old, &list).collect();
        prop_assert_eq!(all.len(), appended.len());
        prop_assert!(all.iter().all(|edit| matches!(edit, Edit::InsertAt(..))));
    }

    #[test]
    fn removing_one_is_one_edit(list in unique_list().prop_filter("non-empty", |list| !list.is_empty()), position in any::<prop::sample::Index>()) {
        let index = position.index(list.len());
        let mut new = list.clone();
        new.remove(index);
        prop_assert_eq!(edits(&list, &new).collect::<Vec<_>>(), vec![Edit::RemoveAt(index as u32)]);
    }

    #[test]
    fn edit_count_is_bounded(old in unique_list(), new in unique_list()) {
        prop_assert!(edits(&old, &new).count() <= old.len() + new.len());
    }
}

/// Applies edits the way a WinUI collection does.
fn apply(list: &[NodeId], edits: impl Iterator<Item = Edit>) -> Vec<NodeId> {
    let mut list = list.to_vec();
    for edit in edits {
        match edit {
            Edit::RemoveAt(index) => {
                list.remove(index as usize);
            }
            Edit::InsertAt(index, id) => list.insert(index as usize, id),
        }
    }
    list
}

/// A list of distinct ids drawn from a small pool, so two lists share ids.
fn unique_list() -> impl Strategy<Value = Vec<NodeId>> {
    Just((0..12u32).collect::<Vec<_>>())
        .prop_shuffle()
        .prop_flat_map(|pool| (0..=pool.len()).prop_map(move |length| pool[..length].to_vec()))
        .prop_map(|indexes| indexes.into_iter().map(NodeId::for_test).collect())
}
