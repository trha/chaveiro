//! Search inside the list: which items match a query, and how well.
//!
//! Matching ignores case and accents. A query is split on whitespace, and
//! an item matches when every word matches its name, its subtitle or one of
//! its website addresses.

use crate::vault::CipherSummary;
use icu_normalizer::DecomposingNormalizerBorrowed;
use zeroize::{Zeroize, Zeroizing};

/// How well an item matches a query. The list shows the items of a
/// better tier first, and keeps index order inside a tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The name is the query, word for word.
    Exact,
    /// The name starts with each word.
    Prefix,
    /// A word of the name starts with each word, such as "hub" in
    /// "GitHub" or "fi" in "Wi-Fi".
    WordStart,
    /// The name contains each word.
    Contains,
    /// The subtitle or a website address contains a word.
    Detail,
    /// The letters of a word appear in order in the name, close together,
    /// such as "gthb" in "GitHub".
    Fuzzy,
}

/// A query, folded once, with the buffers that fold each item.
#[derive(Default)]
pub struct Query {
    /// The folded characters of every word, back to back. Not wiped: the
    /// text box and its binding keep the typed text unwiped anyway.
    chars: Vec<char>,
    /// The end of each word in `chars`.
    ends: Vec<usize>,
    name: Folded,
    detail: Folded,
}

impl Query {
    /// Reads `text` as the query, reusing the buffers.
    pub fn set(&mut self, text: &str) {
        self.chars.clear();
        self.ends.clear();
        for word in text.split_whitespace() {
            fold_into(word, |char, _| self.chars.push(char));
            self.ends.push(self.chars.len());
        }
    }

    /// Whether the query has no words, so every item matches.
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// How well `cipher` matches; [`None`] when some word matches none of
    /// its fields. Every item is [`Tier::Exact`] for an empty query, and
    /// an item that did not decrypt matches no other.
    pub fn rank(&mut self, cipher: &CipherSummary) -> Option<Tier> {
        if self.is_empty() {
            return Some(Tier::Exact);
        }
        if !cipher.readable {
            return None;
        }
        self.name.fold(&cipher.name);
        if self.is_exact() {
            return Some(Tier::Exact);
        }
        let mut worst = Tier::Exact;
        for word in 0..self.ends.len() {
            let start = match word {
                0 => 0,
                _ => self.ends[word - 1],
            };
            let word = &self.chars[start..self.ends[word]];
            let name = &self.name;
            let tier = if name.chars.starts_with(word) {
                Tier::Prefix
            } else if name
                .word_starts()
                .any(|at| name.chars[at..].starts_with(word))
            {
                Tier::WordStart
            } else if contains(&name.chars, word) {
                Tier::Contains
            } else if std::iter::once(&cipher.subtitle)
                .chain(&cipher.uris)
                .any(|field| {
                    self.detail.fold(field);
                    contains(&self.detail.chars, word)
                })
            {
                Tier::Detail
            } else if close_in_order(&name.chars, word) {
                Tier::Fuzzy
            } else {
                return None;
            };
            worst = worst.max(tier);
        }
        Some(worst)
    }

    /// Whether the folded name's words are the query's words, in order.
    fn is_exact(&self) -> bool {
        let mut names = self
            .name
            .chars
            .split(|char| char.is_whitespace())
            .filter(|word| !word.is_empty());
        let mut start = 0;
        for &end in &self.ends {
            if names.next() != Some(&self.chars[start..end]) {
                return false;
            }
            start = end;
        }
        names.next().is_none()
    }
}

/// One field, folded: each character, and whether it starts a word. It
/// holds decrypted vault text, so no copy of it is freed unwiped: on drop
/// and when a buffer grows, the old contents are zeroed.
///
/// Contract: `chars` and `starts` have the same length.
#[derive(Default)]
struct Folded {
    chars: Zeroizing<Vec<char>>,
    starts: Zeroizing<Vec<bool>>,
}

impl Folded {
    fn fold(&mut self, text: &str) {
        self.chars.clear();
        self.starts.clear();
        fold_into(text, |char, start| {
            push_wiping(&mut self.chars, char);
            push_wiping(&mut self.starts, start);
        });
    }

    /// The positions where a word starts.
    fn word_starts(&self) -> impl Iterator<Item = usize> {
        self.starts
            .iter()
            .enumerate()
            .filter(|(_, start)| **start)
            .map(|(at, _)| at)
    }
}

/// Pushes `value`. A full `buffer` moves to one twice as big by hand,
/// because `Vec`'s own growth would free the old buffer unwiped.
fn push_wiping<T: Copy + Zeroize>(buffer: &mut Zeroizing<Vec<T>>, value: T) {
    if buffer.len() == buffer.capacity() {
        let mut bigger = Vec::with_capacity((2 * buffer.capacity()).max(64));
        bigger.extend_from_slice(buffer);
        // The old buffer is wiped as its wrapper drops here.
        drop(std::mem::replace(buffer, Zeroizing::new(bigger)));
    }
    buffer.push(value);
}

/// Gives `push` each character of `text` in lowercase, with accents taken
/// off, and whether it starts a word. A word starts after a character that
/// is not a letter or digit, and at a capital after a small letter, as in
/// "GitHub".
fn fold_into(text: &str, mut push: impl FnMut(char, bool)) {
    const NFD: DecomposingNormalizerBorrowed<'static> = DecomposingNormalizerBorrowed::new_nfd();
    let mut previous: Option<char> = None;
    for char in text.chars() {
        let mut start = match previous {
            None => true,
            Some(previous) => {
                !previous.is_alphanumeric() || (previous.is_lowercase() && char.is_uppercase())
            }
        };
        previous = Some(char);
        // The decomposition puts an accent after its letter as a combining
        // mark, which this drops. Only the common marks are dropped, which
        // covers the Latin, Greek and Cyrillic accents.
        for part in NFD.normalize_iter(std::iter::once(char)) {
            if ('\u{0300}'..='\u{036F}').contains(&part) {
                continue;
            }
            for lower in part.to_lowercase() {
                push(lower, start);
                start = false;
            }
        }
    }
}

fn contains(haystack: &[char], word: &[char]) -> bool {
    haystack.windows(word.len()).any(|window| window == word)
}

/// Whether the characters of `word` appear in order in `haystack` within a
/// span at most twice the word's length. A wider span would let a short
/// word match most names.
fn close_in_order(haystack: &[char], word: &[char]) -> bool {
    let Some(&first) = word.first() else {
        return true;
    };
    let limit = 2 * word.len();
    haystack
        .iter()
        .enumerate()
        .filter(|(_, char)| **char == first)
        .any(|(begin, _)| {
            // Each search goes on from where the last letter was found.
            let mut rest = haystack[begin..].iter().take(limit);
            word.iter().all(|wanted| rest.any(|char| char == wanted))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crypto::SecretString, vault::CipherKind};
    use proptest::prelude::*;
    use uuid::Uuid;

    fn item(name: &str, subtitle: &str, uris: &[&str]) -> CipherSummary {
        CipherSummary {
            id: Uuid::nil(),
            kind: CipherKind::Login,
            name: SecretString::new(name.to_owned()),
            subtitle: SecretString::new(subtitle.to_owned()),
            uris: uris
                .iter()
                .map(|uri| SecretString::new((*uri).to_owned()))
                .collect(),
            folder_id: None,
            organization_id: None,
            favorite: false,
            reprompt: false,
            deleted: false,
            readable: true,
        }
    }

    fn rank(query: &str, cipher: &CipherSummary) -> Option<Tier> {
        let mut parsed = Query::default();
        parsed.set(query);
        parsed.rank(cipher)
    }

    #[test]
    fn each_tier_has_an_example() {
        let github = item("GitHub", "alex-dev", &["https://github.com/login"]);
        let wifi = item("Home Wi-Fi", "", &[]);
        assert_eq!(rank("github", &github), Some(Tier::Exact));
        assert_eq!(rank("git", &github), Some(Tier::Prefix));
        assert_eq!(rank("hub", &github), Some(Tier::WordStart));
        assert_eq!(rank("fi", &wifi), Some(Tier::WordStart));
        assert_eq!(rank("ome", &wifi), Some(Tier::Contains));
        assert_eq!(rank("dev", &github), Some(Tier::Detail));
        assert_eq!(rank("login", &github), Some(Tier::Detail));
        assert_eq!(rank("gthb", &github), Some(Tier::Fuzzy));
        assert_eq!(rank("gb", &github), None);
        assert_eq!(rank("gitlab", &github), None);
    }

    #[test]
    fn case_and_accents_do_not_matter() {
        let cafe = item("Café São João", "", &[]);
        assert_eq!(rank("cafe sao joao", &cafe), Some(Tier::Exact));
        assert_eq!(rank("CAFÉ", &cafe), Some(Tier::Prefix));
        assert_eq!(rank("JOÃO", &cafe), Some(Tier::WordStart));
    }

    #[test]
    fn an_unreadable_item_matches_only_the_empty_query() {
        let mut unreadable = item("", "", &[]);
        unreadable.readable = false;
        assert_eq!(rank("", &unreadable), Some(Tier::Exact));
        assert_eq!(rank("a", &unreadable), None);
    }

    /// A name of one to three words of small letters.
    fn name() -> impl Strategy<Value = String> {
        prop::collection::vec("[a-z]{1,8}", 1..4).prop_map(|words| words.join(" "))
    }

    proptest! {
        #[test]
        fn the_empty_query_matches_everything(name in name(), blank in "[ \t]{0,3}") {
            prop_assert_eq!(rank(&blank, &item(&name, "", &[])), Some(Tier::Exact));
        }

        #[test]
        fn the_name_itself_is_exact_in_any_case(name in name()) {
            let cipher = item(&name, "", &[]);
            prop_assert_eq!(rank(&name.to_uppercase(), &cipher), Some(Tier::Exact));
        }

        #[test]
        fn a_start_of_the_name_is_at_least_a_prefix(name in name(), length in 1usize..8) {
            let first = name.split(' ').next().expect("names have a word");
            let query = &first[..length.min(first.len())];
            let tier = rank(query, &item(&name, "", &[]));
            prop_assert!(tier.is_some_and(|tier| tier <= Tier::Prefix), "{:?}", tier);
        }

        #[test]
        fn a_piece_of_the_name_matches_by_name(name in name(), start in 0usize..20, length in 1usize..5) {
            let start = start % name.len();
            let end = (start + length).min(name.len());
            let piece = name[start..end].trim();
            prop_assume!(!piece.is_empty() && !piece.contains(' '));
            let tier = rank(piece, &item(&name, "", &[]));
            prop_assert!(tier.is_some_and(|tier| tier <= Tier::Contains), "{:?}", tier);
        }

        #[test]
        fn a_piece_of_an_address_matches_as_detail(host in "[a-z]{3,10}") {
            let uri = format!("https://{host}.example");
            let tier = rank(&host, &item("Zz", "", &[&uri]));
            prop_assert_eq!(tier, Some(Tier::Detail));
        }

        #[test]
        fn a_letter_in_no_field_matches_nothing(name in name(), word in "[a-z]{0,4}") {
            let query = format!("{word}9");
            prop_assert_eq!(rank(&query, &item(&name, "x", &["y"])), None);
        }

        #[test]
        fn another_word_never_ranks_better(name in name(), first in "[a-z]{1,4}", second in "[a-z]{1,4}") {
            let cipher = item(&name, "", &[]);
            let alone = rank(&first, &cipher);
            let both = rank(&format!("{first} {second}"), &cipher);
            // Two words can still be the exact name when one is not.
            if let Some(both) = both.filter(|tier| *tier != Tier::Exact) {
                prop_assert!(alone.is_some_and(|alone| alone <= both), "{:?} {:?}", alone, both);
            }
        }
    }
}
