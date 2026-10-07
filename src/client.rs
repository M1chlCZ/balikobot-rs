//! The API client and its configuration validation.

use std::io::Read;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use ureq::Agent;
use ureq::http;

use crate::config::{
    API_KEY_LIMIT, Config, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT, MAX_RESPONSE_BYTES_LIMIT,
    USER_LIMIT, normalize_label_hosts, resolve_base_url,
};
use crate::models::{AddPackageRequest, AddPackageResult, OverviewPackage};
use crate::wire::{self, RequestFailure};
use crate::{Branch, CarrierCode, CountryCode, CurrencyCode, Error, Result};

const ADD_FIELD_LIMIT: usize = 255;
const LABEL_RESPONSE_LIMIT: usize = 4 << 20;
const TRACK_REFERENCE_MODULUS: i64 = 10_000_000_000;
const ZPL_MAGIC_PREFIX: &str = "^X";

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

    /// Calls the ADD method with one package. ADD is idempotent on `eid`: a
    /// repeated request with an already stored `eid` returns status 208
    /// together with the original record, which maps to a successful result.
    pub fn add_package(
        &self,
        carrier: &CarrierCode,
        request: &AddPackageRequest,
    ) -> Result<AddPackageResult> {
        if !carrier.is_valid() || !valid_add_package(request) {
            return Err(Error::InvalidRequest);
        }
        let body = json!({
            "packages": [serde_json::to_value(request).map_err(|_| Error::Ambiguous)?]
        });
        let response = wire::request(
            self,
            http::Method::POST,
            &format!("/{carrier}/add"),
            Some(&body),
        )
        .map_err(ambiguous_request_failure)?;
        let status = response.status;
        if status == 429 {
            return Err(Error::Unavailable {
                retry_after: wire::retry_after(&response),
            });
        }
        if status >= 500 {
            return Err(Error::Unavailable { retry_after: None });
        }
        if status != 200 {
            return Err(if (200..300).contains(&status) {
                Error::Ambiguous
            } else if (400..500).contains(&status) {
                Error::Rejected
            } else {
                Error::InvalidResponse
            });
        }
        if !wire::is_json(&response) {
            return Err(Error::Ambiguous);
        }
        let Some(parsed) = wire::parse_add(&response.body) else {
            return Err(Error::Ambiguous);
        };
        if parsed.status.is_none() {
            return Err(Error::Ambiguous);
        }
        if let Err(error) = wire::top_level_status_error(parsed.status) {
            return Err(if error == Error::InvalidResponse {
                Error::Ambiguous
            } else {
                error
            });
        }
        if parsed.packages.len() != 1 || parsed.packages[0].eid != request.eid {
            return Err(Error::Ambiguous);
        }
        let entry = &parsed.packages[0];
        let Some(entry_status) = entry.status else {
            return Err(Error::Ambiguous);
        };
        match entry_status {
            200 | 208 => {
                let Some(package_id) = &entry.package_id else {
                    return Err(Error::Ambiguous);
                };
                if entry.carrier_id.is_empty()
                    || !wire::valid_branch_field(&entry.carrier_id, wire::IDENTIFIER_LIMIT)
                    || !wire::valid_label_url(self, &entry.label_url)
                {
                    return Err(Error::Ambiguous);
                }
                Ok(AddPackageResult {
                    package_id: package_id.clone(),
                    carrier_id: entry.carrier_id.clone(),
                    label_url: entry.label_url.clone(),
                })
            }
            426 | 503 => Err(Error::Unavailable { retry_after: None }),
            400 | 403 | 404 | 405 | 406 | 409 | 413 | 423 | 501 => Err(Error::Rejected),
            _ => Err(Error::Ambiguous),
        }
    }

    /// Calls the OVERVIEW method, which lists the packages of a carrier that
    /// have not yet been closed by ORDER. `match_eid` names the single entry
    /// whose integrity is required for reconciliation: a malformed entry with
    /// a different EID is skipped, while a malformed matching entry fails the
    /// call.
    pub fn overview(&self, carrier: &CarrierCode, match_eid: &str) -> Result<Vec<OverviewPackage>> {
        if !carrier.is_valid() {
            return Err(Error::InvalidRequest);
        }
        let response = wire::request(
            self,
            http::Method::GET,
            &format!("/{carrier}/overview"),
            None,
        )
        .map_err(ambiguous_request_failure)?;
        if response.status == 429 {
            return Err(Error::Unavailable {
                retry_after: wire::retry_after(&response),
            });
        }
        if response.status >= 500 {
            return Err(Error::Unavailable { retry_after: None });
        }
        if response.status != 200 || !wire::is_json(&response) {
            return Err(if (400..500).contains(&response.status) {
                Error::Rejected
            } else {
                Error::InvalidResponse
            });
        }
        let Some(parsed) = wire::parse_overview(&response.body) else {
            return Err(Error::InvalidResponse);
        };
        wire::top_level_status_error(parsed.status)?;
        let mut packages = Vec::with_capacity(parsed.packages.len());
        for entry in &parsed.packages {
            match sanitize_overview_entry(self, entry) {
                Some(package) => packages.push(package),
                None if entry.eid == match_eid => return Err(Error::InvalidResponse),
                None => {}
            }
        }
        Ok(packages)
    }

    /// Asks for a fresh aggregate label URL of one package that has not
    /// entered ORDER yet.
    pub fn labels(&self, carrier: &CarrierCode, package_id: &str) -> Result<String> {
        if !carrier.is_valid() || !wire::valid_package_id(package_id) {
            return Err(Error::InvalidRequest);
        }
        let body = json!({"package_ids": [package_id]});
        let response = wire::request(
            self,
            http::Method::POST,
            &format!("/{carrier}/labels"),
            Some(&body),
        )
        .map_err(|_| Error::Unavailable { retry_after: None })?;
        wire::label_lookup_status(&response)?;
        let Some(parsed) = wire::parse_labels(&response.body) else {
            return Err(Error::InvalidResponse);
        };
        if parsed.status.is_none() {
            return Err(Error::InvalidResponse);
        }
        wire::top_level_status_error(parsed.status)?;
        if !wire::valid_label_url(self, &parsed.labels_url) {
            return Err(Error::InvalidResponse);
        }
        Ok(parsed.labels_url)
    }

    /// Retrieves the label URL of an already closed ORDER. The returned URL
    /// is accepted only when the response confirms both the requested order
    /// id and the membership of the requested package id.
    pub fn order_view_labels(
        &self,
        carrier: &CarrierCode,
        order_id: &str,
        package_id: &str,
    ) -> Result<String> {
        if !carrier.is_valid()
            || !wire::valid_package_id(order_id)
            || !wire::valid_package_id(package_id)
        {
            return Err(Error::InvalidRequest);
        }
        let path = format!("/{carrier}/orderview/{}", encode_path_segment(order_id));
        let response = wire::request(self, http::Method::GET, &path, None)
            .map_err(|_| Error::Unavailable { retry_after: None })?;
        wire::label_lookup_status(&response)?;
        let Some(parsed) = wire::parse_order_view(&response.body) else {
            return Err(Error::InvalidResponse);
        };
        wire::top_level_status_error(parsed.status)?;
        if parsed.order_id != order_id
            || !parsed.package_ids.iter().any(|id| id == package_id)
            || !wire::valid_label_url(self, &parsed.labels_url)
        {
            return Err(Error::InvalidResponse);
        }
        Ok(parsed.labels_url)
    }

    /// Fetches a provider label URL server-to-server. The body is read once
    /// with a hard byte limit and validated against the declared label media
    /// type. The returned body is owned by the caller.
    pub fn download_label(&self, label_url: &str) -> Result<(Vec<u8>, String)> {
        if !wire::valid_label_url(self, label_url) {
            return Err(Error::InvalidRequest);
        }
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri(label_url)
            .body(Vec::new())
            .map_err(|_| Error::InvalidRequest)?;
        let response = self
            .agent
            .run(request)
            .map_err(|_| Error::Unavailable { retry_after: None })?;
        let (parts, body) = response.into_parts();
        let status = parts.status.as_u16();
        if status == 429 || status >= 500 {
            return Err(Error::Unavailable { retry_after: None });
        }
        if status != 200 {
            return Err(if (400..500).contains(&status) {
                Error::Rejected
            } else {
                Error::InvalidResponse
            });
        }
        let media_type = parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let mut reader = body.into_reader();
        let mut bytes = Vec::new();
        if reader
            .by_ref()
            .take(LABEL_RESPONSE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return Err(Error::Unavailable { retry_after: None });
        }
        if bytes.len() > LABEL_RESPONSE_LIMIT || bytes.is_empty() {
            return Err(Error::InvalidResponse);
        }
        match media_type.as_str() {
            "application/pdf" if bytes.starts_with(b"%PDF-") => {}
            "application/zpl" if bytes.starts_with(ZPL_MAGIC_PREFIX.as_bytes()) => {}
            _ => return Err(Error::InvalidResponse),
        }
        Ok((bytes, media_type))
    }
}

/// Derives the `branch_id` that ADD expects from a stored branch of a
/// carrier. Česká pošta and Slovenská pošta use the branch ZIP without
/// spaces, the Uloženka `CP_NP` service does the same, PPL strips the `KM`
/// prefix, and every other carrier uses the stored branch id unchanged.
pub fn resolve_branch_id(
    carrier: &CarrierCode,
    service: &str,
    branch_id: &str,
    branch_zip: &str,
) -> String {
    match carrier.as_str() {
        "cp" | "sp" => strip_spaces(branch_zip),
        "ulozenka" if service == "CP_NP" => strip_spaces(branch_zip),
        "ppl" => branch_id.strip_prefix("KM").unwrap_or(branch_id).to_owned(),
        _ => branch_id.to_owned(),
    }
}

fn strip_spaces(value: &str) -> String {
    value.replace(' ', "")
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'$'
            | b'&'
            | b'+'
            | b':'
            | b'='
            | b'@' => encoded.push(char::from(byte)),
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{byte:02X}"));
            }
        }
    }
    encoded
}

fn ambiguous_request_failure(failure: RequestFailure) -> Error {
    match failure {
        RequestFailure::BodyLimit => Error::Ambiguous,
        failure => failure.into(),
    }
}

fn sanitize_overview_entry(
    client: &Client,
    entry: &wire::OverviewPackageStatus,
) -> Option<OverviewPackage> {
    if entry.eid.is_empty()
        || entry.carrier_id.is_empty()
        || !wire::valid_branch_field(&entry.carrier_id, wire::IDENTIFIER_LIMIT)
        || !wire::valid_label_url(client, &entry.label_url)
    {
        return None;
    }
    Some(OverviewPackage {
        eid: entry.eid.clone(),
        package_id: entry.package_id.clone()?,
        carrier_id: entry.carrier_id.clone(),
        label_url: entry.label_url.clone(),
    })
}

fn valid_add_package(request: &AddPackageRequest) -> bool {
    valid_add_identity(request) && valid_add_recipient(request) && valid_add_parcel(request)
}

fn valid_add_identity(request: &AddPackageRequest) -> bool {
    valid_eid(&request.eid)
        && valid_service(&request.service_type)
        && (!request.rec_name.is_empty() || !request.rec_firm.is_empty())
        && (!request.rec_phone.is_empty() || !request.rec_email.is_empty())
        && (request.cod_currency == CurrencyCode::CZK || request.cod_currency == CurrencyCode::EUR)
        && !(request.cod_price > 0.0 && request.vs.is_none())
        && !(request.cod_price == 0.0 && request.vs.is_some())
}

fn valid_add_recipient(request: &AddPackageRequest) -> bool {
    [
        &request.rec_name,
        &request.rec_firm,
        &request.rec_street,
        &request.rec_city,
        &request.rec_zip,
        &request.rec_phone,
        &request.rec_email,
    ]
    .into_iter()
    .all(|field| wire::valid_branch_field(field, ADD_FIELD_LIMIT))
        && !request.rec_street.is_empty()
        && !request.rec_city.is_empty()
        && !request.rec_zip.is_empty()
        && request.rec_country.is_valid()
        && (request.branch_id.is_empty() || wire::valid_branch_id(&request.branch_id))
}

fn valid_add_parcel(request: &AddPackageRequest) -> bool {
    request.weight > 0.0
        && request.weight <= 10_000.0
        && request.length > 0.0
        && request.length <= 1_000.0
        && request.width > 0.0
        && request.width <= 1_000.0
        && request.height > 0.0
        && request.height <= 1_000.0
        && request.price >= 0.0
        && request.price <= 100_000_000.0
        && request.cod_price >= 0.0
        && request.cod_price <= 100_000_000.0
        && request
            .vs
            .is_none_or(|symbol| (0..TRACK_REFERENCE_MODULUS).contains(&symbol))
}

fn valid_eid(eid: &str) -> bool {
    (8..=40).contains(&eid.len())
        && eid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_service(service: &str) -> bool {
    (1..=16).contains(&service.len()) && service.bytes().all(|byte| byte.is_ascii_alphanumeric())
}
