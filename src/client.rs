//! The API client and its configuration validation.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ureq::Agent;
use ureq::http;

use crate::config::{
    API_KEY_LIMIT, Config, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT, MAX_RESPONSE_BYTES_LIMIT,
    USER_LIMIT, normalize_label_hosts, resolve_base_url,
};
use crate::wire::{self, RequestFailure};
use crate::{Branch, CarrierCode, CountryCode, Error, Result};

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

    /// Returns the branches of one carrier service in one country. The route
    /// depends on the carrier: the selected carriers use the combined service
    /// and country segments, Zásilkovna uses the country-only route, and the
    /// remaining carriers use the service-only route with a client-side
    /// country filter.
    pub fn branches(
        &self,
        carrier: &CarrierCode,
        service: &str,
        country: &CountryCode,
    ) -> Result<Vec<Branch>> {
        if !carrier.is_valid() || !valid_service(service) || !country.is_valid() {
            return Err(Error::InvalidRequest);
        }
        let (path, filter_country) = wire::branches_path(carrier, service, country);
        let response =
            wire::request(self, http::Method::GET, &path, None).map_err(
                |failure| match failure {
                    RequestFailure::Transport(_) | RequestFailure::BodyLimit => {
                        Error::Unavailable { retry_after: None }
                    }
                },
            )?;
        if response.status == 429 || response.status >= 500 {
            return Err(Error::Unavailable { retry_after: None });
        }
        if response.status != 200 || !wire::is_json(&response) {
            return Err(Error::InvalidResponse);
        }
        let Some(parsed) = wire::parse_branches(&response.body) else {
            return Err(Error::InvalidResponse);
        };
        match parsed.status {
            Some(200) => {}
            Some(426 | 503) => return Err(Error::Unavailable { retry_after: None }),
            _ => return Err(Error::InvalidResponse),
        }
        let mut branches = Vec::with_capacity(parsed.branches.len());
        for wire_branch in &parsed.branches {
            let Some(branch) = wire::sanitize_branch(wire_branch) else {
                continue;
            };
            if filter_country && !branch.country.as_str().is_empty() && branch.country != *country {
                continue;
            }
            branches.push(branch);
        }
        Ok(branches)
    }
}

fn valid_service(service: &str) -> bool {
    (1..=16).contains(&service.len()) && service.bytes().all(|byte| byte.is_ascii_alphanumeric())
}
