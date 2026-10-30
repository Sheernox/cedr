//! HTTP for telemetry and self-update: https only (loopback http for tests), on every redirect hop,
//! with bounded reads. Errors name no URL, since a test override may carry anything.

use std::io::Read;
use std::time::Duration;

fn allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}

/// A base URL from a test override, refused unless https or loopback http.
pub(crate) fn checked_base(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid URL".to_string())?;
    if !allowed(&parsed) {
        return Err("the URL must be https://".into());
    }
    Ok(url.trim_end_matches('/').to_string())
}

pub(crate) fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if allowed(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("refused a redirect to a URL that is not https://")
        }
    });
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(5).min(timeout))
        .redirect(policy)
        .user_agent(concat!("cedr/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(error)
}

/// A request error without its URL, with its causes, for messages.
pub(crate) fn error(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut parts = vec![e.to_string()];
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

/// A GET's body, refused past `max` bytes or on a non-2xx status.
pub(crate) fn get(
    client: &reqwest::blocking::Client,
    url: &str,
    max: u64,
) -> Result<Vec<u8>, String> {
    let response = client.get(url).send().map_err(error)?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let mut body = Vec::new();
    response
        .take(max + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() as u64 > max {
        return Err(format!("response over {max} bytes"));
    }
    Ok(body)
}
