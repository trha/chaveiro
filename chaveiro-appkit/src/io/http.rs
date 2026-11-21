//! The core's HTTP requests over `NSURLSession`.

use appkit_executor::completion;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL,
    NSURLRequestCachePolicy, NSURLResponse, NSURLSession, NSURLSessionConfiguration,
};
use vault_core::core::{Body, HttpError, HttpRequest, HttpResponse, Method};
use zeroize::Zeroizing;

/// A session that keeps no state between requests: no cache, no cookies
/// and no credential store.
pub fn session() -> Retained<NSURLSession> {
    let configuration = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    configuration.setURLCache(None);
    configuration.setHTTPCookieStorage(None);
    configuration.setURLCredentialStorage(None);
    configuration.setRequestCachePolicy(NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData);
    NSURLSession::sessionWithConfiguration(&configuration)
}

/// Starts `request` and returns a future of its answer. Any status is an
/// answer; only a failure to reach the server is an [`HttpError`]. Dropping
/// the future cancels the request.
///
/// The request starts at once, and the future borrows nothing, so it can be
/// spawned.
pub fn send(
    session: &NSURLSession,
    request: HttpRequest,
) -> impl use<> + Future<Output = Result<HttpResponse, HttpError>> {
    let HttpRequest {
        method,
        url,
        headers,
        body,
    } = request;
    let address = NSURL::URLWithString(&NSString::from_str(url.as_str()))
        .expect("a parsed url::Url is a valid NSURL");
    let message = NSMutableURLRequest::requestWithURL(&address);
    message.setHTTPMethod(&NSString::from_str(match method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Delete => "DELETE",
    }));
    let header = |name: &str, value: &str| {
        message.setValue_forHTTPHeaderField(
            Some(&NSString::from_str(value)),
            &NSString::from_str(name),
        );
    };
    header("Accept", "application/json");
    for (name, value) in &headers {
        header(name, value);
    }
    let content = match body {
        Body::None => None,
        Body::Json(value) => Some((
            "application/json",
            Zeroizing::new(serde_json::to_vec(&value).expect("a JSON value always serializes")),
        )),
        Body::Form(pairs) => Some((
            "application/x-www-form-urlencoded; charset=utf-8",
            Zeroizing::new(form_encode(&pairs).as_bytes().to_vec()),
        )),
    };
    if let Some((content_type, bytes)) = content {
        header("Content-Type", content_type);
        // Foundation's copy is outside Rust's allocator, so it is not wiped.
        // At login it holds the password hash until the request is freed.
        message.setHTTPBody(Some(&NSData::with_bytes(&bytes)));
    }
    let (completer, completion) = completion();
    let handler = RcBlock::new(
        move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
            // SAFETY: Foundation passes each argument as null or as a valid
            // object that lives for this call.
            let (data, response, error) =
                unsafe { (data.as_ref(), response.as_ref(), error.as_ref()) };
            completer.complete(match (response, error) {
                (_, Some(error)) => Err(HttpError {
                    message: error.localizedDescription().to_string(),
                }),
                (Some(response), None) => {
                    let status = response
                        .downcast_ref::<NSHTTPURLResponse>()
                        .map_or(0, |response| {
                            u16::try_from(response.statusCode()).unwrap_or(0)
                        });
                    Ok(HttpResponse {
                        status,
                        body: data.map_or_else(Vec::new, NSData::to_vec),
                    })
                }
                (None, None) => Err(HttpError {
                    message: "The server sent no response.".to_owned(),
                }),
            });
        },
    );
    let task = unsafe { session.dataTaskWithRequest_completionHandler(&message, &handler) };
    task.resume();
    let completion = completion.cancel_with(move || task.cancel());
    async move {
        // Foundation freed the block without calling it, such as when the
        // session was invalidated.
        completion.await.unwrap_or_else(|| {
            Err(HttpError {
                message: "The request was dropped.".to_owned(),
            })
        })
    }
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
