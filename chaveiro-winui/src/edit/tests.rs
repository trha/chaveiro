use super::{Draft, Fields, Website, expiration, expiration_text};
use chaveiro_core::{
    crypto::SecretString,
    filter::Filter,
    vault::{
        CardView, CipherContent, CipherKind, CipherView, CustomField, LoginView,
        PasswordHistoryEntry, UriView, VaultIndex,
    },
};
use proptest::prelude::*;
use uuid::Uuid;

proptest! {
    #[test]
    fn every_written_form_of_a_date_reads_as_it(month in 1u8..=12, year in 2000u16..=2099, form in 0..5) {
        let short = year % 100;
        let text = match form {
            0 => format!("{month:02}/{short:02}"),
            1 => format!("{month}/{short:02}"),
            2 => format!("{month:02}{short:02}"),
            3 => format!("{month:02}/{year}"),
            _ => format!(" {month:02} - {year} "),
        };
        prop_assert_eq!(expiration(&text), Some(Some((month, year))));
    }

    #[test]
    fn a_month_past_12_or_0_is_unreadable(month in prop_oneof![Just(0u8), 13u8..=99], year in 0u16..100) {
        prop_assert_eq!(expiration(&format!("{month:02}/{year:02}")), None);
    }

    #[test]
    fn blank_text_is_no_date(spaces in " {0,4}") {
        prop_assert_eq!(expiration(&spaces), Some(None));
    }

    /// The form shows a stored expiration in a shape Save reads back as the
    /// same date.
    #[test]
    fn a_stored_expiration_reads_back(month in 1u8..=12, year in 2000u16..=2099, padded in any::<bool>()) {
        let card = CardView {
            exp_month: Some(secret(&match padded {
                true => format!("{month:02}"),
                false => month.to_string(),
            })),
            exp_year: Some(secret(&year.to_string())),
            ..CardView::default()
        };
        prop_assert_eq!(expiration(&expiration_text(&card)), Some(Some((month, year))));
    }
}

#[test]
fn a_three_digit_year_is_unreadable() {
    assert_eq!(expiration("01/202"), None);
}

fn secret(text: &str) -> SecretString {
    SecretString::new(text.to_owned())
}

fn index() -> VaultIndex {
    VaultIndex {
        folders: Vec::new(),
        ciphers: Vec::new(),
        ssh_keys: Vec::new(),
    }
}

/// A stored login with what the form does not show: a second website, a
/// match rule, a custom field and a password history.
fn stored_login() -> CipherView {
    let uri = |text: &str, match_rule| UriView {
        uri: Some(secret(text)),
        match_rule,
    };
    CipherView {
        id: Some(Uuid::from_u128(1)),
        organization_id: None,
        collection_ids: Vec::new(),
        kind: CipherKind::Login,
        name: secret("Mail"),
        notes: Some(secret("  spaced  ")),
        folder_id: None,
        favorite: true,
        reprompt: false,
        content: CipherContent::Login(LoginView {
            username: Some(secret("me")),
            password: Some(secret("hunter2")),
            uris: vec![
                uri("https://mail.test", Some(3)),
                uri("https://other.test", None),
            ],
            ..LoginView::default()
        }),
        fields: vec![CustomField {
            kind: 0,
            name: Some(secret("pin")),
            value: Some(secret("1234")),
            linked_id: None,
        }],
        password_history: vec![PasswordHistoryEntry {
            password: secret("old"),
            last_used_date: "2026-01-01T00:00:00.000Z".into(),
        }],
    }
}

fn login(view: &CipherView) -> &LoginView {
    match &view.content {
        CipherContent::Login(login) => login,
        _ => panic!("a login"),
    }
}

/// What the hidden box reads when the user left it as filled.
fn as_filled(draft: &Draft) -> SecretString {
    draft.fill.clone().unwrap_or_default()
}

fn stored_card(number: &str) -> CipherView {
    CipherView {
        kind: CipherKind::Card,
        content: CipherContent::Card(CardView {
            number: Some(secret(number)),
            ..CardView::default()
        }),
        ..stored_login()
    }
}

fn card(view: &CipherView) -> &CardView {
    match &view.content {
        CipherContent::Card(card) => card,
        _ => panic!("a card"),
    }
}

#[test]
fn a_card_number_shows_grouped_and_saves_as_stored() {
    let index = index();
    let draft = Draft::edit(&stored_card("4111111111111111"), &index);
    let Fields::Card { number, .. } = &draft.now.fields else {
        panic!("a card form");
    };
    assert_eq!(number.as_str(), "4111 1111 1111 1111");
    let saved = draft.view(&index, as_filled(&draft));
    assert_eq!(
        card(&saved).number.as_deref().map(String::as_str),
        Some("4111111111111111")
    );
}

#[test]
fn a_changed_card_number_saves_its_digits() {
    let index = index();
    let mut draft = Draft::edit(&stored_card("4111111111111111"), &index);
    let Fields::Card { number, .. } = &mut draft.now.fields else {
        panic!("a card form");
    };
    number.clear();
    number.push_str("5500 0000 0000 0004");
    let saved = draft.view(&index, as_filled(&draft));
    assert_eq!(
        card(&saved).number.as_deref().map(String::as_str),
        Some("5500000000000004")
    );
}

#[test]
fn a_fresh_form_has_no_changes() {
    let index = index();
    assert!(!Draft::new(CipherKind::SshKey, Filter::All, &index).changed());
    assert!(!Draft::edit(&stored_login(), &index).changed());
}

#[test]
fn an_untouched_edit_saves_the_item_as_stored() {
    let index = index();
    let stored = stored_login();
    let draft = Draft::edit(&stored, &index);
    let saved = draft.view(&index, as_filled(&draft));
    assert_eq!(saved.id, stored.id);
    assert_eq!(saved.name, stored.name);
    assert_eq!(saved.notes, stored.notes, "notes keep their spaces");
    assert!(saved.favorite);
    assert_eq!(saved.fields.len(), 1, "custom fields stay");
    assert_eq!(saved.password_history.len(), 1, "the history stays");
    let (saved, stored) = (login(&saved), login(&stored));
    assert_eq!(saved.password, stored.password);
    assert_eq!(saved.username, stored.username);
    let uris: Vec<_> = saved
        .uris
        .iter()
        .map(|uri| (uri.uri.clone(), uri.match_rule))
        .collect();
    let stored_uris: Vec<_> = stored
        .uris
        .iter()
        .map(|uri| (uri.uri.clone(), uri.match_rule))
        .collect();
    assert_eq!(uris, stored_uris);
}

fn websites(draft: &mut Draft) -> &mut Vec<Website> {
    match &mut draft.now.fields {
        Fields::Login { websites, .. } => websites,
        _ => panic!("a login form"),
    }
}

/// The saved websites as (address, rule) pairs.
fn saved_uris(view: &CipherView) -> Vec<(Option<&str>, Option<u8>)> {
    login(view)
        .uris
        .iter()
        .map(|uri| (uri.uri.as_deref().map(String::as_str), uri.match_rule))
        .collect()
}

#[test]
fn an_edited_website_keeps_its_rule_and_the_others() {
    let index = index();
    let mut draft = Draft::edit(&stored_login(), &index);
    let first = &mut websites(&mut draft)[0].uri;
    first.clear();
    first.push_str(" https://new.test ");
    let saved = draft.view(&index, as_filled(&draft));
    assert_eq!(
        saved_uris(&saved),
        [
            (Some("https://new.test"), Some(3)),
            (Some("https://other.test"), None),
        ]
    );
}

#[test]
fn a_removed_website_is_gone() {
    let index = index();
    let mut draft = Draft::edit(&stored_login(), &index);
    websites(&mut draft).remove(0);
    let saved = draft.view(&index, as_filled(&draft));
    assert_eq!(saved_uris(&saved), [(Some("https://other.test"), None)]);
}

#[test]
fn an_added_website_saves_with_its_rule_and_empty_rows_drop() {
    let index = index();
    let mut draft = Draft::new(CipherKind::Login, Filter::All, &index);
    let rows = websites(&mut draft);
    rows.push(Website::empty());
    rows[1].uri.push_str("https://added.test");
    rows[1].rule = Some(5);
    let saved = draft.view(&index, as_filled(&draft));
    assert_eq!(saved_uris(&saved), [(Some("https://added.test"), Some(5))]);
}

#[test]
fn an_emptied_hidden_box_clears_its_secret() {
    let index = index();
    let draft = Draft::edit(&stored_login(), &index);
    let saved = draft.view(&index, Default::default());
    assert_eq!(login(&saved).password, None);
}

#[test]
fn a_brand_the_list_lacks_stays_when_untouched() {
    let index = index();
    let stored = CipherView {
        kind: CipherKind::Card,
        content: CipherContent::Card(CardView {
            brand: Some(secret("Elo")),
            exp_month: Some(secret("1")),
            exp_year: Some(secret("2031")),
            ..CardView::default()
        }),
        ..stored_login()
    };
    let draft = Draft::edit(&stored, &index);
    let saved = draft.view(&index, as_filled(&draft));
    let CipherContent::Card(card) = &saved.content else {
        panic!("a card");
    };
    assert_eq!(card.brand.as_deref().map(String::as_str), Some("Elo"));
    assert_eq!(
        card.exp_month.as_deref().map(String::as_str),
        Some("1"),
        "kept as stored"
    );
}
