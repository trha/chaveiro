use super::{CLOUDS, EmailError, ServerError, email_error, server_text, server_url, with_scheme};
use proptest::prelude::*;
use url::Url;

const ENTER_EMAIL: Option<EmailError> = Some(EmailError::Empty);
const LIKE_EXAMPLE: Option<EmailError> = Some(EmailError::Malformed);

#[test]
fn server_url_errors_say_what_is_wrong() {
    assert_eq!(server_url("  ").err(), Some(ServerError::Empty));
    for wrong in [
        "http://vault.example.com",
        "http://localhost.example.com",
        "http://10.0.0.1",
        "ftp://localhost",
        "ftp://vault.example.com",
        "vault.example.com",
    ] {
        assert_eq!(
            server_url(wrong).err(),
            Some(ServerError::NotHttps),
            "{wrong}"
        );
    }
    for hostless in ["https://", "https://vault"] {
        assert_eq!(
            server_url(hostless).err(),
            Some(ServerError::NoHost),
            "{hostless}"
        );
    }
    assert_eq!(
        server_url(" https://vault.example.com ").map(String::from),
        Ok("https://vault.example.com/".to_owned())
    );
}

#[test]
fn http_is_allowed_on_loopback_only() {
    for loopback in [
        "http://localhost:8000",
        "http://LOCALHOST",
        "http://127.0.0.1:8000",
        "http://127.8.9.10",
        "http://[::1]:8000",
    ] {
        assert!(server_url(loopback).is_ok(), "{loopback}");
    }
}

#[test]
fn cloud_names_mean_their_vaults() {
    assert_eq!(
        server_url("bitwarden.com").map(String::from),
        Ok("https://vault.bitwarden.com/".to_owned())
    );
    assert_eq!(
        server_url("bitwarden.eu").map(String::from),
        Ok("https://vault.bitwarden.eu/".to_owned())
    );
}

#[test]
fn leaving_the_box_adds_https_to_a_bare_address() {
    assert_eq!(
        with_scheme(" vault.example.com ").as_deref(),
        Some("https://vault.example.com")
    );
    for kept in [
        "",
        "bitwarden.com",
        "http://vault.example.com",
        "https://vault.example.com",
    ] {
        assert_eq!(with_scheme(kept), None, "{kept}");
    }
}
/// A label of 1 to 8 letters or digits.
fn label() -> impl Strategy<Value = String> {
    "[a-z0-9]{1,8}"
}

proptest! {
    #[test]
    fn blank_email_asks_for_one(spaces in "[ \t]{0,4}") {
        prop_assert_eq!(email_error(&spaces), ENTER_EMAIL);
    }

    #[test]
    fn name_at_domain_dot_tld_passes(name in label(), domain in label(), tld in label()) {
        let address = format!("{name}@{domain}.{tld}");
        prop_assert_eq!(email_error(&address), None);
    }

    #[test]
    fn an_address_without_at_or_dot_fails(name in label(), domain in label()) {
        prop_assert_eq!(email_error(&format!("{name}{domain}")), LIKE_EXAMPLE);
        prop_assert_eq!(email_error(&format!("{name}@{domain}")), LIKE_EXAMPLE);
    }

    #[test]
    fn a_space_inside_fails(name in label(), domain in label(), tld in label()) {
        let address = format!("{name} x@{domain}.{tld}");
        prop_assert_eq!(email_error(&address), LIKE_EXAMPLE);
    }
}

#[test]
fn server_text_names_clouds_and_keeps_addresses() {
    for (name, url) in CLOUDS {
        assert_eq!(server_text(&Url::parse(url).unwrap()), name);
    }
    let own = Url::parse("https://vault.example.com/").unwrap();
    assert_eq!(server_text(&own), "https://vault.example.com/");
}
