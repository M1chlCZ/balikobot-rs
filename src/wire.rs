//! The HTTP transport shared by the API methods.

use std::io;
use std::time::Duration;

use serde_json::Value;
use ureq::http;
use url::Url;

use crate::client::Client;
use crate::config::{host_is_loopback, host_with_port};
use crate::{Error, Result};

const MAX_RETRY_AFTER_SECONDS: u64 = 3600;
const LABEL_QUERY_ZPL: &str = "zpl=1";

/// A provider response before any JSON interpretation.
#[derive(Debug)]
pub struct RawResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The response headers.
    pub headers: http::HeaderMap,
    /// The response body, read up to the configured limit.
    pub body: Vec<u8>,
}

/// Sends a JSON request and returns the response with its body read.
pub fn request(
    client: &Client,
    method: http::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<RawResponse> {
    let url = format!("{}/{}", client.base_url, path.trim_start_matches('/'));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(&url)
        .header("accept", "application/json")
        .header("authorization", client.authorization.as_str());
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)?
        }
        None => Vec::new(),
    };
    let request = builder.body(payload).map_err(|_| Error::InvalidRequest)?;
    let response = client
        .agent
        .run(request)
        .map_err(|error| dispatch_error(&error))?;
    let (parts, mut body) = response.into_parts();
    let body = match body
        .with_config()
        .limit(client.max_response_bytes as u64)
        .read_to_vec()
    {
        Ok(body) => body,
        Err(ureq::Error::BodyExceedsLimit(_)) => return Err(Error::InvalidResponse),
        Err(error) => return Err(dispatch_error(&error)),
    };
    Ok(RawResponse {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    })
}

/// Reports whether the response declares a JSON content type.
pub fn is_json(response: &RawResponse) -> bool {
    response
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

/// Returns the integer `Retry-After` hint, clamped to one hour.
pub fn retry_after(response: &RawResponse) -> Option<Duration> {
    let seconds = response
        .headers
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()?;
    if seconds < 1 {
        return None;
    }
    Some(Duration::from_secs(
        (seconds as u64).min(MAX_RETRY_AFTER_SECONDS),
    ))
}

/// Reports whether a provider label URL is allowed for this client.
pub fn valid_label_url(client: &Client, raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.fragment().is_some_and(|fragment| !fragment.is_empty()) {
        return false;
    }
    if url.path().is_empty() {
        return false;
    }
    if url
        .query()
        .is_some_and(|query| !query.is_empty() && query != LABEL_QUERY_ZPL)
    {
        return false;
    }
    if !client.label_hosts.is_empty() {
        return label_host_allowed(&url, &client.label_hosts);
    }
    if client.loopback {
        return format!("{}://{}", url.scheme(), host_with_port(&url)) == client.origin;
    }
    let host = host_with_port(&url);
    url.scheme() == "https" && (host == "pdf.balikobot.cz" || host.ends_with(".balikobot.cz"))
}

/// Maps a transport error to the public error set.
pub fn dispatch_error(error: &ureq::Error) -> Error {
    match error {
        ureq::Error::Timeout(_) => Error::Ambiguous,
        ureq::Error::HostNotFound => Error::Unavailable { retry_after: None },
        ureq::Error::ConnectionFailed => Error::Unavailable { retry_after: None },
        ureq::Error::Io(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            Error::Unavailable { retry_after: None }
        }
        _ => Error::Ambiguous,
    }
}

fn label_host_allowed(url: &Url, allowed_hosts: &[String]) -> bool {
    if !label_scheme_allowed(url) {
        return false;
    }
    let host = host_with_port(url).to_lowercase();
    let hostname = url.host_str().unwrap_or_default().to_lowercase();
    allowed_hosts.iter().any(|allowed| {
        if allowed.starts_with('.') {
            hostname.ends_with(allowed.as_str())
        } else {
            host == *allowed
        }
    })
}

fn label_scheme_allowed(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => host_is_loopback(url),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn response(headers: &[(&str, &str)]) -> RawResponse {
        let mut map = http::HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                name.parse::<http::HeaderName>().expect("header name"),
                value.parse().expect("header value"),
            );
        }
        RawResponse {
            status: 200,
            headers: map,
            body: Vec::new(),
        }
    }

    #[test]
    fn json_content_type_is_case_insensitive_and_parameterized() {
        assert!(is_json(&response(&[(
            "content-type",
            "application/json; charset=utf-8"
        )])));
        assert!(is_json(&response(&[("Content-Type", "Application/JSON")])));
        assert!(!is_json(&response(&[("content-type", "text/plain")])));
        assert!(!is_json(&response(&[])));
    }

    #[test]
    fn retry_after_parses_and_clamps() {
        assert_eq!(
            retry_after(&response(&[("retry-after", "12")])),
            Some(Duration::from_secs(12))
        );
        assert_eq!(retry_after(&response(&[("retry-after", "0")])), None);
        assert_eq!(retry_after(&response(&[("retry-after", "-3")])), None);
        assert_eq!(retry_after(&response(&[("retry-after", "nope")])), None);
        assert_eq!(
            retry_after(&response(&[("retry-after", "99999")])),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(retry_after(&response(&[])), None);
    }

    #[test]
    fn labels_follow_the_default_host_allowlist() {
        let client = Client::new(Config::new("user", "key")).expect("client");
        assert!(valid_label_url(
            &client,
            "https://pdf.balikobot.cz/label.pdf"
        ));
        assert!(valid_label_url(
            &client,
            "https://x.balikobot.cz/label.pdf?zpl=1"
        ));
        assert!(!valid_label_url(&client, "https://evil.example/label.pdf"));
        assert!(!valid_label_url(
            &client,
            "http://pdf.balikobot.cz/label.pdf"
        ));
        assert!(!valid_label_url(
            &client,
            "https://pdf.balikobot.cz/label.pdf?x=1"
        ));
        assert!(!valid_label_url(
            &client,
            "https://pdf.balikobot.cz/label.pdf#f"
        ));
    }

    #[test]
    fn dispatch_maps_transport_errors() {
        assert!(matches!(
            dispatch_error(&ureq::Error::HostNotFound),
            Error::Unavailable { retry_after: None }
        ));
        assert!(matches!(
            dispatch_error(&ureq::Error::Timeout(ureq::Timeout::Global)),
            Error::Ambiguous
        ));
        let refused = ureq::Error::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"));
        assert!(matches!(
            dispatch_error(&refused),
            Error::Unavailable { retry_after: None }
        ));
        assert!(matches!(
            dispatch_error(&ureq::Error::ConnectionFailed),
            Error::Unavailable { retry_after: None }
        ));
    }
}
