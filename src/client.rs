//! The API client and its configuration validation.

use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use ureq::Agent;
use ureq::http;

use crate::config::{
    API_KEY_LIMIT, Config, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT, MAX_RESPONSE_BYTES_LIMIT,
    USER_LIMIT, normalize_label_hosts, resolve_base_url,
};
use crate::models::{
    ActivatedServices, AddPackageRequest, AddPackageResult, Carrier, OrderResult, OverviewPackage,
    PickupRequest, PickupResult, ServiceCOD, ServiceCountries, TrackStatusResult, WhoAmI,
    WhoAmICarrier,
};
use crate::wire::{self, RequestFailure};
use crate::{Branch, CarrierCode, CountryCode, CurrencyCode, Error, Result};

const ACCOUNT_MODE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const ADD_FIELD_LIMIT: usize = 255;
const LABEL_RESPONSE_LIMIT: usize = 4 << 20;
const PICKUP_NOTE_LIMIT: usize = 255;
const PICKUP_PACKAGE_LIMIT: i32 = 10_000;
const PICKUP_WEIGHT_LIMIT: f64 = 100_000.0;
const TRACK_REFERENCE_MODULUS: i64 = 10_000_000_000;
const ZPL_MAGIC_PREFIX: &str = "^X";

struct AccountState {
    verified_at: Option<Instant>,
}

/// A Balíkobot API v2 client.
pub struct Client {
    pub(crate) agent: Agent,
    pub(crate) base_url: String,
    pub(crate) authorization: String,
    pub(crate) origin: String,
    pub(crate) loopback: bool,
    pub(crate) max_response_bytes: usize,
    pub(crate) label_hosts: Vec<String>,
    pub(crate) live_account: Option<bool>,
    account_state: Mutex<AccountState>,
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
            account_state: Mutex::new(AccountState { verified_at: None }),
        })
    }

    /// Calls the WHOAMI method and returns the account information.
    pub fn who_am_i(&self) -> Result<WhoAmI> {
        let answer = self
            .capability_get::<wire::WhoAmIWire>("/info/whoami", false)?
            .ok_or(Error::InvalidResponse)?;
        let carriers = answer
            .carriers
            .iter()
            .map(|entry| WhoAmICarrier {
                slug: CarrierCode::from_capability(&entry.slug),
                name: entry.name.clone(),
            })
            .collect();
        Ok(WhoAmI {
            status: answer.status.ok_or(Error::InvalidResponse)?,
            live_account: answer.live_account,
            carriers,
        })
    }

    /// Calls the ACTIVATEDSERVICES method of one carrier and returns the
    /// normalized activated services. When the provider reports that parcel
    /// shipping is inactive, the service list is empty.
    pub fn activated_services(&self, carrier: &CarrierCode) -> Result<ActivatedServices> {
        if !carrier.is_valid() {
            return Err(Error::InvalidRequest);
        }
        let answer = self
            .capability_get::<wire::ActivatedServicesResponse>(
                &format!("/{carrier}/activatedservices"),
                false,
            )?
            .ok_or(Error::InvalidResponse)?;
        let (services, _) = wire::normalize_activated_services(&answer)?;
        Ok(ActivatedServices {
            active_parcel: answer.active_parcel,
            services,
        })
    }

    /// Calls the COUNTRIES4SERVICE method of one carrier and returns the
    /// supported destination countries per service. Every country sent by
    /// the provider is kept.
    pub fn countries(&self, carrier: &CarrierCode) -> Result<Vec<ServiceCountries>> {
        if !carrier.is_valid() {
            return Err(Error::InvalidRequest);
        }
        let answer = self
            .capability_get::<wire::CountriesResponse>(
                &format!("/{carrier}/countries4service"),
                false,
            )?
            .ok_or(Error::InvalidResponse)?;
        if answer.service_types.len() > wire::CAPABILITY_SERVICE_LIMIT {
            return Err(Error::InvalidResponse);
        }
        let mut result = Vec::with_capacity(answer.service_types.len());
        for entry in &answer.service_types {
            let raw_code = entry
                .service_type
                .as_deref()
                .ok_or(Error::InvalidResponse)?;
            let code = raw_code.trim();
            if !wire::valid_capability_service_code(code)
                || entry.countries.len() > wire::CAPABILITY_SERVICE_LIMIT
            {
                return Err(Error::InvalidResponse);
            }
            let countries = entry
                .countries
                .iter()
                .map(|raw_country| CountryCode::from_capability(raw_country))
                .collect();
            result.push(ServiceCountries {
                service_type: code.to_owned(),
                countries,
            });
        }
        Ok(result)
    }

    /// Calls the COD4SERVICES method of one carrier and returns the
    /// normalized cash-on-delivery destinations per service. Every country
    /// sent by the provider is kept. A carrier without the optional
    /// dictionary returns an empty list.
    pub fn cod(&self, carrier: &CarrierCode) -> Result<Vec<ServiceCOD>> {
        if !carrier.is_valid() {
            return Err(Error::InvalidRequest);
        }
        let Some(answer) =
            self.capability_get::<wire::CodResponse>(&format!("/{carrier}/cod4services"), true)?
        else {
            return Ok(Vec::new());
        };
        if answer.service_types.len() > wire::CAPABILITY_SERVICE_LIMIT {
            return Err(Error::InvalidResponse);
        }
        let mut result = Vec::with_capacity(answer.service_types.len());
        for entry in &answer.service_types {
            let raw_code = entry
                .service_type
                .as_deref()
                .ok_or(Error::InvalidResponse)?;
            let code = raw_code.trim();
            if !wire::valid_capability_service_code(code)
                || entry.countries.len() > wire::CAPABILITY_SERVICE_LIMIT
            {
                return Err(Error::InvalidResponse);
            }
            let countries = wire::normalize_cod_countries(&entry.countries)?;
            result.push(ServiceCOD {
                service_type: code.to_owned(),
                countries,
            });
        }
        Ok(result)
    }

    /// Discovers the contracted carriers and their activated services in one
    /// run. Without a scope every carrier of the account is discovered; an
    /// explicit empty scope discovers none. Every requested carrier must
    /// belong to the account. The returned destinations are restricted to EU
    /// countries.
    pub fn carrier_capabilities(&self, scope: Option<&[CarrierCode]>) -> Result<Vec<Carrier>> {
        let whoami = self.verified_who_am_i(false)?;
        let mut carriers = wire::scoped_capability_carriers(&whoami.carriers, scope)?;
        for carrier in &mut carriers {
            let activated = self
                .capability_get::<wire::ActivatedServicesResponse>(
                    &format!("/{}/activatedservices", carrier.carrier_code),
                    false,
                )?
                .ok_or(Error::InvalidResponse)?;
            let countries = self
                .capability_get::<wire::CountriesResponse>(
                    &format!("/{}/countries4service", carrier.carrier_code),
                    false,
                )?
                .ok_or(Error::InvalidResponse)?;
            carrier.services = wire::normalize_capabilities(
                &activated,
                &countries,
                &wire::CodResponse::default(),
            )?;
        }
        Ok(carriers)
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
        let response = wire::request(self, http::Method::GET, &path, None)
            .map_err(|_| Error::Unavailable { retry_after: None })?;
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

    /// Calls the TRACKSTATUS method for one carrier tracking number and
    /// returns the raw provider status. The documented response wraps
    /// per-package entries in a `packages` array. A package entry or HTTP
    /// answer with status 404 means that the carrier has no tracking data yet
    /// and maps to [`Error::NotFound`].
    pub fn track_status(
        &self,
        carrier: &CarrierCode,
        carrier_id: &str,
    ) -> Result<TrackStatusResult> {
        if !carrier.is_valid() || !wire::valid_package_id(carrier_id) {
            return Err(Error::InvalidRequest);
        }
        let body = json!({"carrier_ids": [carrier_id]});
        let response = wire::request(
            self,
            http::Method::POST,
            &format!("/{carrier}/trackstatus"),
            Some(&body),
        )
        .map_err(|_| Error::Unavailable { retry_after: None })?;
        wire::track_http_status(&response)?;
        let Some(parsed) = wire::parse_track_status(&response.body) else {
            return Err(Error::InvalidResponse);
        };
        if let Some(status) = parsed.status {
            match status {
                200 => {}
                426 | 503 => return Err(Error::Unavailable { retry_after: None }),
                404 => return Err(Error::NotFound),
                _ => return Err(Error::InvalidResponse),
            }
        }
        if parsed.packages.len() != 1 || parsed.packages[0].carrier_id != carrier_id {
            return Err(Error::InvalidResponse);
        }
        let entry = &parsed.packages[0];
        if entry.status.is_none() && (parsed.status.is_none() || entry.name.is_empty()) {
            return Err(Error::InvalidResponse);
        }
        match entry.status.or(parsed.status).expect("one status is set") {
            200 => {
                let Some(id) = entry.status_id_v2.as_deref().or(entry.status_id.as_deref()) else {
                    return Err(Error::InvalidResponse);
                };
                let description = if entry.name.is_empty() {
                    &entry.status_text
                } else {
                    &entry.name
                };
                if description.is_empty()
                    || !wire::valid_branch_field(description, wire::BRANCH_FIELD_LIMIT)
                {
                    return Err(Error::InvalidResponse);
                }
                Ok(TrackStatusResult {
                    status_id: id.to_owned(),
                    status_text: description.clone(),
                })
            }
            404 => Err(Error::NotFound),
            426 | 503 => Err(Error::Unavailable { retry_after: None }),
            400 | 403 | 405 | 406 | 409 | 413 | 423 => Err(Error::Rejected),
            _ => Err(Error::InvalidResponse),
        }
    }

    /// Calls the ORDER method, which hands one package over to the carrier
    /// batch. ORDER is idempotent on `package_ids`: a repeated closure of the
    /// same dataset returns status 208 with the original order id, so an
    /// idempotent retry after an ambiguous answer replays the original record
    /// instead of closing the package twice.
    pub fn order_batch(&self, carrier: &CarrierCode, package_id: &str) -> Result<OrderResult> {
        if !carrier.is_valid() || !wire::valid_package_id(package_id) {
            return Err(Error::InvalidRequest);
        }
        let body = json!({"package_ids": [package_id]});
        let response = wire::request(
            self,
            http::Method::POST,
            &format!("/{carrier}/order"),
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
        if status != 200 || !wire::is_json(&response) {
            return Err(if (200..300).contains(&status) {
                Error::Ambiguous
            } else if (400..500).contains(&status) {
                Error::Rejected
            } else {
                Error::InvalidResponse
            });
        }
        let Some(parsed) = wire::parse_order(&response.body) else {
            return Err(Error::Ambiguous);
        };
        let Some(body_status) = parsed.status else {
            return Err(Error::Ambiguous);
        };
        match body_status {
            200 | 208 => {
                if parsed.order_id.is_empty()
                    || !wire::valid_branch_field(&parsed.order_id, wire::IDENTIFIER_LIMIT)
                {
                    return Err(Error::Ambiguous);
                }
                Ok(OrderResult {
                    order_id: parsed.order_id.clone(),
                })
            }
            426 | 503 => Err(Error::Unavailable { retry_after: None }),
            400 | 402 | 403 | 404 | 405 | 406 | 409 | 413 | 423 => Err(Error::Rejected),
            _ => Err(Error::Ambiguous),
        }
    }

    /// Calls the DROP method for one package that has not entered ORDER. A
    /// body status 404 means that the package is already gone and the call
    /// succeeds. A body status 405 marks a package that was already handed to
    /// the batch and maps to [`Error::Rejected`]. An ambiguous DROP answer
    /// must be reconciled through OVERVIEW before any retry.
    pub fn drop_package(&self, carrier: &CarrierCode, package_id: &str) -> Result<()> {
        if !carrier.is_valid() || !wire::valid_package_id(package_id) {
            return Err(Error::InvalidRequest);
        }
        let body = json!({"package_ids": [package_id]});
        let response = wire::request(
            self,
            http::Method::POST,
            &format!("/{carrier}/drop"),
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
        if status != 200 || !wire::is_json(&response) {
            return Err(if (200..300).contains(&status) {
                Error::Ambiguous
            } else if (400..500).contains(&status) {
                Error::Rejected
            } else {
                Error::InvalidResponse
            });
        }
        let Some(parsed) = wire::parse_drop(&response.body) else {
            return Err(Error::Ambiguous);
        };
        let Some(body_status) = parsed.status else {
            return Err(Error::Ambiguous);
        };
        match body_status {
            200 | 404 => Ok(()),
            426 | 503 => Err(Error::Unavailable { retry_after: None }),
            400 | 402 | 403 | 405 | 406 | 409 | 413 | 423 => Err(Error::Rejected),
            _ => Err(Error::Ambiguous),
        }
    }

    /// Calls the ORDERPICKUP method and books one physical collection,
    /// separately from the shipment data handover performed by ORDER. The
    /// call performs exactly one HTTP attempt. DPD and DPDCZ take the
    /// collection address from the carrier configuration; PPL also defaults
    /// its contact information to that configuration.
    pub fn order_pickup(
        &self,
        carrier: &CarrierCode,
        request: &PickupRequest,
    ) -> Result<PickupResult> {
        if !valid_pickup_request(carrier, request) {
            return Err(Error::Rejected);
        }
        let body = pickup_body(carrier, request);
        let response = wire::request(
            self,
            http::Method::POST,
            &format!("/{carrier}/orderpickup"),
            Some(&body),
        )
        .map_err(|failure| match failure {
            RequestFailure::AccountUnverified => Error::Rejected,
            _ => Error::Ambiguous,
        })?;
        if response.status != 200 {
            return Err(wire::pickup_status_error(response.status));
        }
        if !wire::is_json(&response) {
            return Err(Error::Ambiguous);
        }
        let Some(parsed) = wire::parse_pickup(&response.body) else {
            return Err(Error::Ambiguous);
        };
        let Some(body_status) = parsed.status else {
            return Err(Error::Ambiguous);
        };
        if body_status != 200 {
            return Err(u16::try_from(body_status)
                .map(wire::pickup_status_error)
                .unwrap_or(Error::Ambiguous));
        }
        if *carrier != CarrierCode::PPL {
            return Ok(PickupResult {
                provider_id: String::new(),
                confirmed: true,
            });
        }
        let Some(confirmed) = parsed.confirmed else {
            return Err(Error::Ambiguous);
        };
        if parsed.pickup_order_id.is_empty()
            || !wire::valid_branch_field(&parsed.pickup_order_id, wire::IDENTIFIER_LIMIT)
        {
            return Err(Error::Ambiguous);
        }
        Ok(PickupResult {
            provider_id: parsed.pickup_order_id.clone(),
            confirmed,
        })
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
        let Some(media_type) = parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(wire::parse_media_type)
        else {
            return Err(Error::InvalidResponse);
        };
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

    fn capability_get<T>(&self, path: &str, allow_unsupported: bool) -> Result<Option<T>>
    where
        T: serde::de::DeserializeOwned + wire::CapabilityStatus,
    {
        let response = wire::request(self, http::Method::GET, path, None)
            .map_err(|_| Error::Unavailable { retry_after: None })?;
        if allow_unsupported && response.status == 501 {
            return Ok(None);
        }
        if response.status == 429 || response.status >= 500 {
            return Err(Error::Unavailable { retry_after: None });
        }
        if response.status != 200 || !wire::is_json(&response) {
            return Err(Error::InvalidResponse);
        }
        let parsed: T =
            serde_json::from_slice(&response.body).map_err(|_| Error::InvalidResponse)?;
        match parsed.status_value() {
            Some(200) => Ok(Some(parsed)),
            Some(501) if allow_unsupported => Ok(None),
            _ => Err(Error::InvalidResponse),
        }
    }

    fn verified_who_am_i(&self, allow_cached: bool) -> Result<wire::WhoAmIWire> {
        if allow_cached && self.live_account.is_none() {
            return Ok(wire::WhoAmIWire::default());
        }
        let mut state = self
            .account_state
            .lock()
            .map_err(|_| Error::InvalidResponse)?;
        if allow_cached
            && state
                .verified_at
                .is_some_and(|at| at.elapsed() < ACCOUNT_MODE_CACHE_TTL)
        {
            return Ok(wire::WhoAmIWire::default());
        }
        state.verified_at = None;
        let whoami = self
            .capability_get::<wire::WhoAmIWire>("/info/whoami", false)?
            .ok_or(Error::InvalidResponse)?;
        if let Some(expected) = self.live_account {
            if whoami.live_account != Some(expected) {
                return Err(Error::InvalidResponse);
            }
            state.verified_at = Some(Instant::now());
        }
        Ok(whoami)
    }

    pub(crate) fn verify_write_allowed(&self) -> Result<()> {
        self.verified_who_am_i(true).map(|_| ())
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

fn valid_pickup_request(carrier: &CarrierCode, request: &PickupRequest) -> bool {
    if *carrier != CarrierCode::DPDCZ
        && *carrier != CarrierCode::DPD
        && *carrier != CarrierCode::PPL
    {
        return false;
    }
    valid_pickup_date(&request.date)
        && request.package_count > 0
        && request.package_count <= PICKUP_PACKAGE_LIMIT
        && request.weight_kg > 0.0
        && request.weight_kg <= PICKUP_WEIGHT_LIMIT
        && !request.weight_kg.is_nan()
        && wire::valid_branch_field(&request.note, PICKUP_NOTE_LIMIT)
}

fn valid_pickup_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = value[0..4].parse::<u32>().expect("four digits");
    let month = value[5..7].parse::<u32>().expect("two digits");
    let day = value[8..10].parse::<u32>().expect("two digits");
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days).contains(&day)
}

fn pickup_body(carrier: &CarrierCode, request: &PickupRequest) -> serde_json::Value {
    let mut body = json!({"date": request.date});
    if *carrier == CarrierCode::PPL {
        if !request.note.is_empty() {
            body["note"] = json!(request.note);
        }
    } else {
        body["weight"] = json!(request.weight_kg);
        body["package_count"] = json!(request.package_count);
        if !request.note.is_empty() {
            body["message"] = json!(request.note);
        }
    }
    body
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
