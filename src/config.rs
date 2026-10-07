//! Client configuration.

use std::time::Duration;

use url::{Host, Url};

use crate::{Error, Result};

/// The production Balíkobot API v2 endpoint.
pub const DEFAULT_BASE_URL: &str = "https://apiv2.balikobot.cz";
/// The request timeout used when the configured timeout is zero.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The JSON response size limit used when the configured limit is zero.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 << 20;

pub(crate) const MAX_RESPONSE_BYTES_LIMIT: usize = 1 << 30;
pub(crate) const USER_LIMIT: usize = 100;
pub(crate) const API_KEY_LIMIT: usize = 4096;

/// Configures a [`Client`](crate::Client).
#[derive(Clone)]
pub struct Config {
    pub(crate) base_url: String,
    pub(crate) user: String,
    pub(crate) api_key: String,
    pub(crate) timeout: Duration,
    pub(crate) max_response_bytes: usize,
    pub(crate) label_hosts: Vec<String>,
    pub(crate) live_account: Option<bool>,
}

impl Config {
    /// Returns a configuration for the given API user and key.
    pub fn new(user: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            user: user.into(),
            api_key: api_key.into(),
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            label_hosts: Vec::new(),
            live_account: None,
        }
    }

    /// Sets the API root. The default is [`DEFAULT_BASE_URL`].
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Sets the API user.
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// Sets the API key.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    /// Sets the whole-request timeout. Zero selects [`DEFAULT_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the hard byte limit for a JSON response body. Zero selects
    /// [`DEFAULT_MAX_RESPONSE_BYTES`].
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// Restricts label downloads to these hosts. A leading dot matches the
    /// suffix, so `.balikobot.cz` covers every subdomain.
    pub fn with_label_hosts<I, S>(mut self, label_hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.label_hosts = label_hosts.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the account mode expected before every mutating call.
    pub fn with_live_account(mut self, live_account: bool) -> Self {
        self.live_account = Some(live_account);
        self
    }
}

pub(crate) fn resolve_base_url(raw: &str) -> Result<(String, String, bool)> {
    let trimmed = raw.trim();
    let base = if trimmed.is_empty() {
        DEFAULT_BASE_URL
    } else {
        trimmed
    };
    let base = base.strip_suffix('/').unwrap_or(base);
    let parsed = Url::parse(base).map_err(|_| base_url_absolute_error())?;
    if parsed.cannot_be_a_base()
        || parsed.host().is_none()
        || raw_authority_has_userinfo(base)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some_and(|query| !query.is_empty())
        || parsed
            .fragment()
            .is_some_and(|fragment| !fragment.is_empty())
        || parsed.path() != "/"
    {
        return Err(base_url_absolute_error());
    }
    let loopback = host_is_loopback(&parsed);
    match parsed.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => {
            return Err(Error::Config(
                "base URL must use https unless the host is loopback".to_owned(),
            ));
        }
        _ => {
            return Err(Error::Config("base URL must use http or https".to_owned()));
        }
    }
    let origin = format!("{}://{}", parsed.scheme(), host_with_port(&parsed));
    Ok((base.to_owned(), origin, loopback))
}

/// Reports whether the raw URL carries a userinfo section before the host.
/// An empty userinfo such as `https://@example.com` has no parsed username or
/// password, so only the raw authority exposes it.
pub(crate) fn raw_authority_has_userinfo(raw: &str) -> bool {
    let Some((_, remainder)) = raw.split_once("://") else {
        return false;
    };
    let end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    remainder[..end].contains('@')
}

pub(crate) fn normalize_label_hosts(hosts: &[String]) -> Result<Vec<String>> {
    hosts
        .iter()
        .map(|host| {
            let host = host.trim().to_lowercase();
            if host.is_empty()
                || host
                    .chars()
                    .any(|character| matches!(character, '/' | '\\' | '@' | '?' | '#'))
            {
                return Err(Error::Config("label hosts must be host names".to_owned()));
            }
            Ok(host)
        })
        .collect()
}

pub(crate) fn host_with_port(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

pub(crate) fn host_is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

fn base_url_absolute_error() -> Error {
    Error::Config("base URL must be absolute without path, query or fragment".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_applies_defaults() {
        let config = Config::new("user", "key");
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.user, "user");
        assert_eq!(config.api_key, "key");
        assert_eq!(config.timeout, DEFAULT_TIMEOUT);
        assert_eq!(config.max_response_bytes, DEFAULT_MAX_RESPONSE_BYTES);
        assert!(config.label_hosts.is_empty());
        assert_eq!(config.live_account, None);
    }
}
