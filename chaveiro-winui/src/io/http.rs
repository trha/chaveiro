//! The core's HTTP requests over `Windows.Web.Http`.

use crate::bindings::{
    CryptographicBuffer, HttpBaseProtocolFilter, HttpBufferContent, HttpCacheReadBehavior,
    HttpCacheWriteBehavior, HttpClient, HttpCookieUsageBehavior, HttpMethod, HttpRequestMessage,
    IHttpBaseProtocolFilter3, IHttpContent, IHttpFilter, Uri,
};
use chaveiro_core::core::{Body, HttpError, HttpRequest, HttpResponse, Method};
use windows_core::{Array, Interface};
use zeroize::{Zeroize, Zeroizing};

/// A client that keeps no state between requests: it reads and writes no
/// cache, sends and stores no cookies, and never shows a credential prompt.
pub fn client() -> windows_core::Result<HttpClient> {
    let filter = HttpBaseProtocolFilter::new()?;
    filter.SetAllowUI(false)?;
    filter
        .cast::<IHttpBaseProtocolFilter3>()?
        .SetCookieUsageBehavior(HttpCookieUsageBehavior::NoCookies)?;
    let cache = filter.CacheControl()?;
    cache.SetReadBehavior(HttpCacheReadBehavior::NoCache)?;
    cache.SetWriteBehavior(HttpCacheWriteBehavior::NoCache)?;
    HttpClient::Create(&filter.cast::<IHttpFilter>()?)
}

/// Sends `request`. Any status is an answer; only a failure to reach the
/// server or to read its body is an [`HttpError`].
pub async fn send(client: &HttpClient, request: HttpRequest) -> Result<HttpResponse, HttpError> {
    exchange(client, request).await.map_err(|error| HttpError {
        message: error.message(),
    })
}

async fn exchange(client: &HttpClient, request: HttpRequest) -> windows_core::Result<HttpResponse> {
    let HttpRequest {
        method,
        url,
        headers,
        body,
    } = request;
    let method = match method {
        Method::Get => HttpMethod::Get()?,
        Method::Post => HttpMethod::Post()?,
        Method::Put => HttpMethod::Put()?,
        Method::Delete => HttpMethod::Delete()?,
    };
    let message = HttpRequestMessage::Create(&method, &Uri::CreateUri(url.as_str())?)?;
    let request_headers = message.Headers()?;
    request_headers.TryAppendWithoutValidation("Accept", "application/json")?;
    for (name, value) in &headers {
        request_headers.TryAppendWithoutValidation(name, value.as_str())?;
    }
    let (content_type, bytes) = match body {
        Body::None => ("", None),
        Body::Json(value) => (
            "application/json",
            Some(Zeroizing::new(
                serde_json::to_vec(&value).expect("a JSON value always serializes"),
            )),
        ),
        Body::Form(pairs) => (
            "application/x-www-form-urlencoded; charset=utf-8",
            Some(Zeroizing::new(form_encode(&pairs).as_bytes().to_vec())),
        ),
    };
    if let Some(bytes) = bytes {
        // The buffer is a WinRT copy that the wiping allocator does not see.
        // The request body holds the password hash at login, so this copy
        // outlives the request until WinRT frees it.
        let content = HttpBufferContent::CreateFromBuffer(
            &CryptographicBuffer::CreateFromByteArray(&bytes)?,
        )?;
        content
            .cast::<IHttpContent>()?
            .Headers()?
            .TryAppendWithoutValidation("Content-Type", content_type)?;
        message.SetContent(&content.cast::<IHttpContent>()?)?;
    }
    let response = winui_executor::winrt(client.SendRequestAsync(&message)?).await?;
    let status = u16::try_from(response.StatusCode()?.0).unwrap_or(0);
    let buffer = winui_executor::winrt(response.Content()?.ReadAsBufferAsync()?).await?;
    let mut array = Array::<u8>::new();
    CryptographicBuffer::CopyToByteArray(&buffer, &mut array)?;
    // `Array` frees its block with CoTaskMemFree, which the wiping allocator
    // does not see, and the body can hold tokens.
    let body = array.to_vec();
    array.zeroize();
    Ok(HttpResponse { status, body })
}

/// `application/x-www-form-urlencoded`, built in a buffer that wipes itself.
fn form_encode(pairs: &[(&'static str, Zeroizing<String>)]) -> Zeroizing<String> {
    let capacity = pairs
        .iter()
        .map(|(name, value)| name.len() + 3 * value.len() + 2)
        .sum();
    // The capacity fits the worst case, so the buffer never grows and
    // leaves no unwiped copy behind.
    let mut serializer = url::form_urlencoded::Serializer::new(String::with_capacity(capacity));
    for (name, value) in pairs {
        serializer.append_pair(name, value);
    }
    Zeroizing::new(serializer.finish())
}
