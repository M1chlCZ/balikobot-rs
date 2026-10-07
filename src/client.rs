//! The API client and its configuration validation.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ureq::Agent;

use crate::config::{
    API_KEY_LIMIT, Config, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT, MAX_RESPONSE_BYTES_LIMIT,
    USER_LIMIT, normalize_label_hosts, resolve_base_url,
};
use crate::{Error, Result};

/// A Balíkobot API v2 client.
pub struct Client {
    pub(crate) agent: Agent,
    pub(crate) base_url: String,
    pub(crate) authorization: String,
    pub(crate) origin: String,
    pub(crate) loopback: bool,
    pub(crate) max_response_bytes: usize,
    pub(crate) label_hosts: Vec<String>,
    #[allow(dead_code)]
    pub(crate) live_account: Option<bool>,
}

impl Client {
    /// Validates `config` and returns a client.
    pub fn new(config: Config) -> Result<Self> {
        let user = config.user.trim();
        if user.is_empty() || user.len() > USER_LIMIT {
            return Err(Error::Config(
                "user is required and limited to 100 bytes".to_owned(),
            ));
        }
        if config.api_key.is_empty() || config.api_key.len() > API_KEY_LIMIT {
            return Err(Error::Config(
                "API key is required and limited to 4096 bytes".to_owned(),
            ));
        }
        if config.max_response_bytes > MAX_RESPONSE_BYTES_LIMIT {
            return Err(Error::Config(
                "response limit must be between 0 and 1073741824 bytes".to_owned(),
            ));
        }
        let (base_url, origin, loopback) = resolve_base_url(&config.base_url)?;
        let label_hosts = normalize_label_hosts(&config.label_hosts)?;
        let timeout = if config.timeout.is_zero() {
            DEFAULT_TIMEOUT
        } else {
            config.timeout
        };
        let max_response_bytes = if config.max_response_bytes == 0 {
            DEFAULT_MAX_RESPONSE_BYTES
        } else {
            config.max_response_bytes
        };
        let authorization = format!(
            "Basic {}",
            STANDARD.encode(format!("{user}:{}", config.api_key))
        );
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(timeout))
            .build()
            .new_agent();
        Ok(Self {
            agent,
            base_url,
            authorization,
            origin,
            loopback,
            max_response_bytes,
            label_hosts,
            live_account: config.live_account,
        })
    }
}
