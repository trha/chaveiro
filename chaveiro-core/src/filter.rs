//! The vault's sidebar filters and how many items each one shows. The UIs
//! own the labels and icons.

use crate::vault::{CipherKind, CipherSummary, FolderSummary, VaultIndex};
use std::collections::HashMap;
use uuid::Uuid;

/// One sidebar entry's view of the vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Every item not in the trash.
    All,
    Favorites,
    /// The items of one type, from [`KINDS`]. Items of a type this client
    /// does not model show only under [`All`](Filter::All) and their
    /// folder.
    Kind(CipherKind),
    Folder(Uuid),
    /// Items with no folder, or with a folder the vault does not have.
    NoFolder,
    /// Items in the trash, which no other filter shows.
    Trash,
}

/// The types the sidebar lists, in its order.
pub const KINDS: [CipherKind; 5] = [
    CipherKind::Login,
    CipherKind::Card,
    CipherKind::Identity,
    CipherKind::SecureNote,
    CipherKind::SshKey,
];

impl Filter {
    /// Whether `cipher` shows under this filter. `folders` are the vault's
    /// folders, which decide whether a folder id counts as no folder.
    pub fn matches(self, cipher: &CipherSummary, folders: &[FolderSummary]) -> bool {
        if cipher.deleted {
            return self == Filter::Trash;
        }
        match self {
            Filter::All => true,
            Filter::Favorites => cipher.favorite,
            Filter::Kind(kind) => KINDS.contains(&kind) && cipher.kind == kind,
            Filter::Folder(id) => {
                cipher.folder_id == Some(id) && folders.iter().any(|folder| folder.id == id)
            }
            Filter::NoFolder => !cipher
                .folder_id
                .is_some_and(|id| folders.iter().any(|folder| folder.id == id)),
            Filter::Trash => false,
        }
    }
}

/// How many items each filter shows, counted in one pass over the index.
/// The default counts an empty vault and allocates nothing.
#[derive(Default)]
pub struct Counts {
    all: usize,
    favorites: usize,
    /// Indexed like [`KINDS`].
    kinds: [usize; KINDS.len()],
    folders: HashMap<Uuid, usize>,
    no_folder: usize,
    trash: usize,
}

impl Counts {
    pub fn of(index: &VaultIndex) -> Self {
        let mut counts = Counts::default();
        counts.recount(index);
        counts
    }

    /// Counts `index` again in place, reusing the folder map's capacity.
    pub fn recount(&mut self, index: &VaultIndex) {
        self.clear();
        self.folders
            .extend(index.folders.iter().map(|folder| (folder.id, 0)));
        for cipher in &index.ciphers {
            if cipher.deleted {
                self.trash += 1;
                continue;
            }
            self.all += 1;
            self.favorites += usize::from(cipher.favorite);
            if let Some(position) = KINDS.iter().position(|kind| *kind == cipher.kind) {
                self.kinds[position] += 1;
            }
            match cipher.folder_id.and_then(|id| self.folders.get_mut(&id)) {
                Some(count) => *count += 1,
                None => self.no_folder += 1,
            }
        }
    }

    /// Counts an empty vault again. Keeps the folder map's capacity.
    pub fn clear(&mut self) {
        self.all = 0;
        self.favorites = 0;
        self.kinds = [0; KINDS.len()];
        self.folders.clear();
        self.no_folder = 0;
        self.trash = 0;
    }

    /// The number of items `filter` shows; 0 for a folder the index does
    /// not have, or a type outside [`KINDS`].
    pub fn get(&self, filter: Filter) -> usize {
        match filter {
            Filter::All => self.all,
            Filter::Favorites => self.favorites,
            Filter::Kind(kind) => KINDS
                .iter()
                .position(|listed| *listed == kind)
                .map_or(0, |position| self.kinds[position]),
            Filter::Folder(id) => self.folders.get(&id).copied().unwrap_or(0),
            Filter::NoFolder => self.no_folder,
            Filter::Trash => self.trash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::SecretString;
    use proptest::prelude::*;

    fn kind() -> impl Strategy<Value = CipherKind> {
        prop_oneof![
            Just(CipherKind::Login),
            Just(CipherKind::SecureNote),
            Just(CipherKind::Card),
            Just(CipherKind::Identity),
            Just(CipherKind::SshKey),
            (6u8..9).prop_map(CipherKind::Unknown),
        ]
    }

    /// One item as the strategy draws it: type, folder number, favorite,
    /// trashed. Folders 1 to 3 may exist; 0 is none and 9 never exists.
    type Item = (CipherKind, u128, bool, bool);

    /// A vault of 0 to 3 folders and up to 30 items, as plain data because
    /// the summaries hide their contents from `Debug`.
    fn vault() -> impl Strategy<Value = (u128, Vec<Item>)> {
        let folder = prop_oneof![Just(0u128), Just(9), 1u128..=3];
        (
            0u128..=3,
            prop::collection::vec((kind(), folder, any::<bool>(), any::<bool>()), 0..30),
        )
    }

    fn index((folder_count, items): (u128, Vec<Item>)) -> VaultIndex {
        VaultIndex {
            folders: (1..=folder_count)
                .map(|id| FolderSummary {
                    id: Uuid::from_u128(id),
                    name: SecretString::default(),
                    readable: true,
                })
                .collect(),
            ciphers: items
                .into_iter()
                .enumerate()
                .map(
                    |(position, (kind, folder, favorite, deleted))| CipherSummary {
                        id: Uuid::from_u128(1000 + position as u128),
                        kind,
                        name: SecretString::default(),
                        subtitle: SecretString::default(),
                        folder_id: (folder != 0).then(|| Uuid::from_u128(folder)),
                        organization_id: None,
                        favorite,
                        deleted,
                        readable: true,
                    },
                )
                .collect(),
            ssh_keys: Vec::new(),
        }
    }

    /// Every filter the sidebar of `index` lists, plus a folder it lacks.
    fn filters(index: &VaultIndex) -> Vec<Filter> {
        let mut filters = vec![
            Filter::All,
            Filter::Favorites,
            Filter::NoFolder,
            Filter::Trash,
            Filter::Folder(Uuid::from_u128(9)),
            Filter::Kind(CipherKind::Unknown(7)),
        ];
        filters.extend(KINDS.map(Filter::Kind));
        filters.extend(index.folders.iter().map(|folder| Filter::Folder(folder.id)));
        filters
    }

    proptest! {
        #[test]
        fn each_count_is_the_number_of_items_the_filter_shows(vault in vault()) {
            let index = index(vault);
            let counts = Counts::of(&index);
            for filter in filters(&index) {
                let shown = index
                    .ciphers
                    .iter()
                    .filter(|cipher| filter.matches(cipher, &index.folders))
                    .count();
                prop_assert_eq!(counts.get(filter), shown, "{:?}", filter);
            }
        }

        #[test]
        fn a_recount_keeps_nothing_of_the_last_vault(first in vault(), second in vault()) {
            let (first, second) = (index(first), index(second));
            let mut counts = Counts::of(&first);
            counts.recount(&second);
            let fresh = Counts::of(&second);
            let mut every = filters(&first);
            every.extend(filters(&second));
            for filter in every {
                prop_assert_eq!(counts.get(filter), fresh.get(filter), "{:?}", filter);
            }
        }

        #[test]
        fn a_trashed_item_shows_only_in_trash(vault in vault()) {
            let index = index(vault);
            for cipher in index.ciphers.iter().filter(|cipher| cipher.deleted) {
                let shown: Vec<Filter> = filters(&index)
                    .into_iter()
                    .filter(|filter| filter.matches(cipher, &index.folders))
                    .collect();
                prop_assert_eq!(shown, vec![Filter::Trash]);
            }
        }

        #[test]
        fn a_live_item_shows_in_all_and_in_exactly_one_folder_entry(vault in vault()) {
            let index = index(vault);
            let folder_entries: Vec<Filter> = index
                .folders
                .iter()
                .map(|folder| Filter::Folder(folder.id))
                .chain([Filter::NoFolder])
                .collect();
            for cipher in index.ciphers.iter().filter(|cipher| !cipher.deleted) {
                prop_assert!(Filter::All.matches(cipher, &index.folders));
                let homes = folder_entries
                    .iter()
                    .filter(|filter| filter.matches(cipher, &index.folders))
                    .count();
                prop_assert_eq!(homes, 1);
            }
        }

        #[test]
        fn a_live_item_shows_in_one_type_entry_unless_its_type_is_unknown(vault in vault()) {
            let index = index(vault);
            for cipher in index.ciphers.iter().filter(|cipher| !cipher.deleted) {
                let types = KINDS
                    .iter()
                    .filter(|kind| Filter::Kind(**kind).matches(cipher, &index.folders))
                    .count();
                let expected = match cipher.kind {
                    CipherKind::Unknown(_) => 0,
                    _ => 1,
                };
                prop_assert_eq!(types, expected);
            }
        }
    }
}
