use super::{Value, rows, when};
use chaveiro_core::{
    crypto::SecretString,
    vault::{CipherContent, CipherKind, CipherView, CustomField, LoginView, UriView, VaultIndex},
};

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

fn login(password: &str, websites: &[&str]) -> CipherView {
    CipherView::new(
        CipherKind::Login,
        secret("GitHub"),
        CipherContent::Login(LoginView {
            username: Some(secret("alex-dev")),
            password: Some(secret(password)),
            uris: websites
                .iter()
                .map(|uri| UriView {
                    uri: Some(secret(uri)),
                    match_rule: None,
                })
                .collect(),
            ..LoginView::default()
        }),
    )
}

/// The labels of the rows where the copies differ.
fn differing(mine: &CipherView, theirs: &CipherView) -> Vec<String> {
    let index = index();
    rows(mine, theirs, &index)
        .into_iter()
        .filter(|row| row.mine != row.theirs)
        .map(|row| row.label.to_owned())
        .collect()
}

#[test]
fn equal_copies_differ_nowhere() {
    let mine = login("hunter2", &["https://github.com"]);
    assert_eq!(differing(&mine, &mine.clone()), Vec::<String>::new());
}

#[test]
fn a_changed_password_and_website_list_differ() {
    let mine = login(
        "hunter2",
        &["https://github.com", "https://gist.github.com"],
    );
    let theirs = login("hunter3", &["https://github.com"]);
    assert_eq!(differing(&mine, &theirs), ["Password", "Websites"]);
}

#[test]
fn a_field_only_the_server_has_comes_last() {
    let mine = login("hunter2", &[]);
    let mut theirs = mine.clone();
    theirs.notes = Some(secret("Recovery codes in the safe"));
    let index = index();
    let rows = rows(&mine, &theirs, &index);
    let last = rows.last().expect("rows");
    assert_eq!(last.label, "Notes");
    assert_eq!(last.mine, None);
}

#[test]
fn secrets_stay_secret() {
    let mut mine = login("hunter2", &[]);
    mine.fields.push(CustomField {
        kind: 1,
        name: Some(secret("API token")),
        value: Some(secret("ghp_8fKc2Q")),
        linked_id: None,
    });
    let index = index();
    let rows = rows(&mine, &mine, &index);
    let kind = |label: &str| {
        rows.iter()
            .find(|row| row.label == label)
            .and_then(|row| row.mine.as_ref())
            .map(|value| matches!(value, Value::Secret(_)))
    };
    assert_eq!(kind("Password"), Some(true));
    assert_eq!(kind("API token"), Some(true));
    assert_eq!(kind("Username"), Some(false));
}

#[test]
fn a_server_date_shows_day_and_minute() {
    assert_eq!(when("2026-11-21T14:02:33.123Z"), "2026-11-21 14:02 UTC");
    assert_eq!(when("2026-11"), "2026-11");
}
