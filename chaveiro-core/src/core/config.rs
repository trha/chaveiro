use url::Url;
use uuid::Uuid;

/// What the shell sets up once, before [`Input::Started`](super::Input::Started).
#[derive(Clone, Debug)]
pub struct Config {
    /// The OAuth `client_id` of this app, such as `desktop`.
    pub client_id: String,
    /// Sent as `Bitwarden-Client-Name`.
    pub client_name: String,
    /// Sent as `Bitwarden-Client-Version`.
    pub client_version: String,
    /// Bitwarden's device type number, sent as `deviceType` and
    /// `Device-Type`.
    pub device_type: u8,
    /// Stable per install; the shell creates and keeps it.
    pub device_id: Uuid,
    pub device_name: String,
    /// Milliseconds with no user action before a [`Input::Tick`](super::Input::Tick) locks.
    pub lock_after: u64,
}

impl Config {
    /// The base URLs for the server the user typed, such as
    /// `https://vault.example.com`. Bitwarden cloud (`vault.bitwarden.com`
    /// or `vault.bitwarden.eu`, with or without `vault.`) has its own
    /// identity and API hosts. Any other server serves both under the one
    /// base, at `/identity` and `/api`. A trailing slash is ignored. Fails
    /// for a URL that is not `http` or `https`.
    pub fn server_urls(server: &Url) -> Result<ServerUrls, ServerUrlError> {
        if !matches!(server.scheme(), "http" | "https") || server.cannot_be_a_base() {
            return Err(ServerUrlError);
        }
        let cloud = match server
            .host_str()
            .map(|host| host.strip_prefix("vault.").unwrap_or(host))
        {
            Some("bitwarden.com") => Some((
                "https://identity.bitwarden.com",
                "https://api.bitwarden.com",
            )),
            Some("bitwarden.eu") => {
                Some(("https://identity.bitwarden.eu", "https://api.bitwarden.eu"))
            }
            _ => None,
        };
        Ok(match cloud {
            Some((identity, api)) => ServerUrls {
                identity: Url::parse(identity).expect("a fixed, valid URL"),
                api: Url::parse(api).expect("a fixed, valid URL"),
            },
            None => ServerUrls {
                identity: endpoint(server, &["identity"], &[]),
                api: endpoint(server, &["api"], &[]),
            },
        })
    }
}

/// The identity and API base URLs of one server, from
/// [`Config::server_urls`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerUrls {
    pub identity: Url,
    pub api: Url,
}

/// The server URL is not an `http` or `https` URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerUrlError;

impl std::fmt::Display for ServerUrlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the server URL must start with http:// or https://")
    }
}

impl std::error::Error for ServerUrlError {}

/// `base` with `segments` appended to its path, each percent-encoded, and
/// `query` as its form-encoded query. A trailing slash on `base` is ignored.
/// `base` must be an `http` or `https` URL.
pub(super) fn endpoint<S: AsRef<str>>(base: &Url, segments: &[S], query: &[(&str, &str)]) -> Url {
    let mut url = base.clone();
    url.path_segments_mut()
        .expect("Config and server_urls admit only http and https URLs")
        .pop_if_empty()
        .extend(segments);
    url.set_query(None);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    url
}

/// One server: the URL the user gave, and the bases requests go to.
#[derive(Clone, Debug)]
pub(super) struct Server {
    pub(super) url: Url,
    pub(super) urls: ServerUrls,
}

impl Server {
    pub(super) fn new(url: Url) -> Result<Self, ServerUrlError> {
        Ok(Server {
            urls: Config::server_urls(&url)?,
            url,
        })
    }
}
