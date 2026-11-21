//! The checks behind a login form, shared by every shell: which server a
//! typed name means, and what is wrong with each field.
//!
//! Errors are enums, so each UI crate writes and localizes its own text.

use url::Url;

/// The cloud servers, as a server box names them and the URL each means.
pub const CLOUDS: [(&str, &str); 2] = [
    ("bitwarden.com", "https://vault.bitwarden.com"),
    ("bitwarden.eu", "https://vault.bitwarden.eu"),
];

/// What is wrong with a server box's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerError {
    Empty,
    /// The address names a scheme other than `https`, or none.
    NotHttps,
    /// The address has no host with a dot in it, such as `https://vault`.
    NoHost,
}

/// What is wrong with an email box's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailError {
    Empty,
    /// Not `name@domain.tld`, or it holds a space.
    Malformed,
}

/// The server box's text as a server URL. A cloud name means that cloud.
/// Only a loopback host may use `http://`, because its traffic stays on this
/// machine.
pub fn server_url(text: &str) -> Result<Url, ServerError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ServerError::Empty);
    }
    if let Some((_, url)) = CLOUDS.iter().find(|(name, _)| *name == text) {
        return Ok(Url::parse(url).expect("a fixed, valid URL"));
    }
    match Url::parse(text) {
        Ok(url) if url.scheme() == "http" && is_loopback(&url) => Ok(url),
        Ok(url) if url.scheme() != "https" => Err(ServerError::NotHttps),
        // A dot in the host catches a half-typed name such as `https://vault`.
        Ok(url) if url.host_str().is_some_and(|host| host.contains('.')) => Ok(url),
        _ if !text.to_ascii_lowercase().starts_with("https://") => Err(ServerError::NotHttps),
        _ => Err(ServerError::NoHost),
    }
}

/// `localhost`, or an address in 127.0.0.0/8 or `::1`.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// The text a server box shows for `server`: a cloud by its name, or else
/// the address.
pub fn server_text(server: &Url) -> String {
    let host = server
        .host_str()
        .map(|host| host.strip_prefix("vault.").unwrap_or(host));
    match CLOUDS.iter().find(|(name, _)| Some(*name) == host) {
        Some((name, _)) => (*name).to_owned(),
        None => server.to_string(),
    }
}

/// `text` with `https://` in front when it names no scheme, as a server box
/// does when the user leaves it: `vault.example.com` becomes
/// `https://vault.example.com`. Cloud names and empty text give [`None`].
pub fn with_scheme(text: &str) -> Option<String> {
    let text = text.trim();
    let bare =
        !text.is_empty() && !text.contains("://") && CLOUDS.iter().all(|(name, _)| *name != text);
    bare.then(|| format!("https://{text}"))
}

/// What is wrong with an email box's text, if anything.
pub fn email_error(text: &str) -> Option<EmailError> {
    let text = text.trim();
    if text.is_empty() {
        return Some(EmailError::Empty);
    }
    // One `@` with text on both sides and a dot inside the domain, and no
    // spaces: the check only catches typos, the server decides the rest.
    let valid = !text.contains(char::is_whitespace)
        && text.split_once('@').is_some_and(|(name, domain)| {
            !name.is_empty()
                && !domain.contains('@')
                && domain
                    .split_once('.')
                    .is_some_and(|(left, right)| !left.is_empty() && !right.is_empty())
        });
    match valid {
        true => None,
        false => Some(EmailError::Malformed),
    }
}

#[cfg(test)]
mod tests;
