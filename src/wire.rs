//! The HTTP transport shared by the API methods.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{self, Read};
use std::time::Duration;

use serde::Deserialize;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use serde_json::value::RawValue;
use ureq::http;
use url::Url;

use crate::Error;
use crate::client::Client;
use crate::codes::{CountryCode, CurrencyCode};
use crate::config::{host_is_loopback, host_with_port, raw_authority_has_userinfo};
use crate::models::{Branch, CODCapability, Carrier, Service};

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

/// A transport-level request outcome that callers map to the public errors.
#[doc(hidden)]
#[derive(Debug)]
pub enum RequestFailure {
    /// The request could not be completed.
    Transport(ureq::Error),
    /// The decoded response body exceeds the configured limit.
    BodyLimit,
    /// The account mode could not be verified before the write.
    AccountUnverified,
}

impl From<RequestFailure> for Error {
    fn from(failure: RequestFailure) -> Self {
        match failure {
            RequestFailure::Transport(error) => dispatch_error(&error),
            RequestFailure::BodyLimit => Error::InvalidResponse,
            RequestFailure::AccountUnverified => Error::Unavailable { retry_after: None },
        }
    }
}

/// Sends a JSON request and returns the response with its body read.
pub fn request(
    client: &Client,
    method: http::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<RawResponse, RequestFailure> {
    if method != http::Method::GET {
        client
            .verify_write_allowed()
            .map_err(|_| RequestFailure::AccountUnverified)?;
    }
    let url = format!("{}/{}", client.base_url, path.trim_start_matches('/'));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(&url)
        .header("accept", "application/json")
        .header("authorization", client.authorization.as_str());
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            serde_json::to_vec(value).expect("JSON values serialize")
        }
        None => Vec::new(),
    };
    let request = builder.body(payload).expect("valid request");
    let response = client
        .agent
        .run(request)
        .map_err(RequestFailure::Transport)?;
    let (parts, body) = response.into_parts();
    let mut bytes = Vec::new();
    if parts.status.as_u16() == 200 {
        let limit = client.max_response_bytes;
        let mut reader = body.into_reader();
        reader
            .by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| RequestFailure::Transport(ureq::Error::Io(error)))?;
        if bytes.len() > limit {
            return Err(RequestFailure::BodyLimit);
        }
    }
    Ok(RawResponse {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: bytes,
    })
}

/// Reports whether the response declares a JSON content type.
pub fn is_json(response: &RawResponse) -> bool {
    response
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_media_type)
        .is_some_and(|media_type| media_type == "application/json")
}

/// Parses a content type into its lowercased media type. Every parameter after
/// the first must carry a non-empty name before an `=`, like Go's
/// `mime.ParseMediaType`.
pub(crate) fn parse_media_type(value: &str) -> Option<String> {
    let mut parts = value.split(';');
    let media_type = parts.next()?.trim().to_ascii_lowercase();
    if media_type.is_empty() || !parts.all(valid_media_parameter) {
        return None;
    }
    Some(media_type)
}

fn valid_media_parameter(parameter: &str) -> bool {
    parameter
        .split_once('=')
        .is_some_and(|(name, _)| !name.trim().is_empty())
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

/// Maps a top-level provider body status to the public error set. A missing
/// status is accepted, because some documented answers omit it.
pub fn top_level_status_error(status: Option<i64>) -> crate::Result<()> {
    match status {
        None | Some(200 | 208) => Ok(()),
        Some(426 | 503) => Err(Error::Unavailable { retry_after: None }),
        Some(400 | 402 | 403 | 404 | 405 | 406 | 409 | 413 | 423 | 501) => Err(Error::Rejected),
        Some(_) => Err(Error::InvalidResponse),
    }
}

/// Classifies the HTTP answer of a label lookup before its body is decoded.
pub fn label_lookup_status(response: &RawResponse) -> crate::Result<()> {
    if response.status == 429 {
        return Err(Error::Unavailable {
            retry_after: retry_after(response),
        });
    }
    if response.status >= 500 {
        return Err(Error::Unavailable { retry_after: None });
    }
    if response.status != 200 || !is_json(response) {
        return Err(if (400..500).contains(&response.status) {
            Error::Rejected
        } else {
            Error::InvalidResponse
        });
    }
    Ok(())
}

/// Classifies the HTTP answer of a `TRACKSTATUS` call before its body is
/// decoded. A `4xx` answer other than 404 fails the whole account, so it is
/// retryable instead of permanently rejected.
pub fn track_http_status(response: &RawResponse) -> crate::Result<()> {
    if response.status == 429 {
        return Err(Error::Unavailable {
            retry_after: retry_after(response),
        });
    }
    if response.status >= 500 {
        return Err(Error::Unavailable { retry_after: None });
    }
    if response.status == 404 {
        return Err(Error::NotFound);
    }
    if response.status != 200 || !is_json(response) {
        return Err(if (400..500).contains(&response.status) {
            Error::Unavailable { retry_after: None }
        } else {
            Error::InvalidResponse
        });
    }
    Ok(())
}

/// Maps an `ORDERPICKUP` provider status to the public error set. Every other
/// status stays ambiguous, because the booking may have been accepted.
pub fn pickup_status_error(status: u16) -> Error {
    match status {
        400 | 401 | 403 | 404 | 405 | 413 | 415 | 422 | 429 => Error::Rejected,
        _ => Error::Ambiguous,
    }
}

/// Reports whether a provider label URL is allowed for this client.
pub fn valid_label_url(client: &Client, raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    if raw_authority_has_userinfo(raw) || !raw_path_is_explicit(raw) {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.fragment().is_some_and(|fragment| !fragment.is_empty()) {
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

/// Reports whether the raw URL spells an explicit path after the authority.
/// The `url` crate normalizes a missing path to `/`, so the raw text is the
/// only place where an absent path is visible.
fn raw_path_is_explicit(raw: &str) -> bool {
    let Some((_, remainder)) = raw.split_once("://") else {
        return false;
    };
    let index = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    remainder.as_bytes().get(index) == Some(&b'/')
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

pub(crate) const BRANCH_FIELD_LIMIT: usize = 200;
const ZIP_LIMIT: usize = 16;
pub(crate) const IDENTIFIER_LIMIT: usize = 100;

/// The decoded body of a `BRANCHES` response.
#[derive(Debug, Deserialize)]
pub struct BranchesResponse {
    /// The body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The decoded branch entries.
    #[serde(default, deserialize_with = "deserialize_branch_list")]
    pub branches: Vec<BranchWire>,
}

/// One provider branch entry before sanitizing.
#[derive(Debug, Default, Deserialize)]
pub struct BranchWire {
    /// The provider branch type, for example "branch" or "box".
    #[serde(rename = "type", default, deserialize_with = "deserialize_string")]
    pub branch_type: String,
    /// The primary branch identifier.
    #[serde(
        rename = "branch_id",
        default,
        deserialize_with = "deserialize_branch_id"
    )]
    pub branch_id: Option<String>,
    /// The fallback branch identifier.
    #[serde(default, deserialize_with = "deserialize_branch_id")]
    pub id: Option<String>,
    /// The display name.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub name: String,
    /// The street part of the address.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub street: String,
    /// The city part of the address.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub city: String,
    /// The postal code.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub zip: String,
    /// The ISO 3166-1 alpha-2 country code.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub country: String,
    /// The `lat` GPS coordinate.
    #[serde(default, deserialize_with = "deserialize_coordinate")]
    pub lat: Option<f64>,
    /// The `lng` GPS coordinate.
    #[serde(default, deserialize_with = "deserialize_coordinate")]
    pub lng: Option<f64>,
    /// The `latitude` GPS coordinate.
    #[serde(default, deserialize_with = "deserialize_coordinate")]
    pub latitude: Option<f64>,
    /// The `longitude` GPS coordinate.
    #[serde(default, deserialize_with = "deserialize_coordinate")]
    pub longitude: Option<f64>,
}

/// Decodes a `BRANCHES` response body. The top-level body must be a JSON
/// object; arrays, scalars, and `null` are rejected.
pub fn parse_branches(body: &[u8]) -> Option<BranchesResponse> {
    parse_object(body)
}

/// Converts one parsed branch into the public model, or rejects it.
pub fn sanitize_branch(wire: &BranchWire) -> Option<Branch> {
    let id = wire.branch_id.as_ref().or(wire.id.as_ref()).cloned()?;
    if !valid_branch_field(&wire.name, BRANCH_FIELD_LIMIT)
        || !valid_branch_field(&wire.street, BRANCH_FIELD_LIMIT)
        || !valid_branch_field(&wire.city, BRANCH_FIELD_LIMIT)
        || !valid_branch_field(&wire.zip, ZIP_LIMIT)
    {
        return None;
    }
    let name = if wire.name.is_empty() {
        wire.zip.clone()
    } else {
        wire.name.clone()
    };
    if name.is_empty() {
        return None;
    }
    let country = CountryCode::from_wire(wire.country.clone())?;
    let (latitude, longitude) = branch_coordinates(wire);
    Some(Branch {
        id,
        r#type: wire.branch_type.clone(),
        name,
        street: wire.street.clone(),
        city: wire.city.clone(),
        zip: wire.zip.clone(),
        country,
        latitude,
        longitude,
    })
}

/// Builds the `BRANCHES` path and reports whether the country must be filtered
/// client-side.
pub fn branches_path(
    carrier: &crate::CarrierCode,
    service: &str,
    country: &CountryCode,
) -> (String, bool) {
    let path = format!("/{carrier}/branches/service/{service}");
    match carrier.as_str() {
        "ppl" | "dpd" | "dpdcz" | "dpdsk" | "geis" | "gls" | "intime" => {
            (format!("{path}/country/{country}"), false)
        }
        "cp" | "ceskaposta" | "balikovna" => (format!("{path}/country/{country}"), true),
        "zasilkovna" => (format!("/{carrier}/branches/country/{country}"), false),
        "sp" | "ulozenka" => (path, true),
        _ => (path, true),
    }
}

/// The decoded body of an `ADD` response.
#[derive(Debug, Deserialize)]
pub struct AddResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The decoded package entries.
    #[serde(default, deserialize_with = "deserialize_lenient_list")]
    pub packages: Vec<AddPackageStatus>,
}

/// One per-package entry of an `ADD` response.
#[derive(Debug, Default, Deserialize)]
pub struct AddPackageStatus {
    /// The external package reference.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub eid: String,
    /// The per-package provider status.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The Balíkobot package reference.
    #[serde(default, deserialize_with = "deserialize_package_id")]
    pub package_id: Option<String>,
    /// The carrier tracking number.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub carrier_id: String,
    /// The provider label URL.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub label_url: String,
}

/// The decoded body of an `OVERVIEW` response.
#[derive(Debug, Deserialize)]
pub struct OverviewResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The decoded package entries.
    #[serde(default, deserialize_with = "deserialize_lenient_list")]
    pub packages: Vec<OverviewPackageStatus>,
}

/// One open package entry of an `OVERVIEW` response.
#[derive(Debug, Default, Deserialize)]
pub struct OverviewPackageStatus {
    /// The external package reference.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub eid: String,
    /// The Balíkobot package reference.
    #[serde(default, deserialize_with = "deserialize_package_id")]
    pub package_id: Option<String>,
    /// The carrier tracking number.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub carrier_id: String,
    /// The provider label URL.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub label_url: String,
}

/// The decoded body of a `LABELS` response.
#[derive(Debug, Default, Deserialize)]
pub struct LabelsResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The aggregate label URL.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub labels_url: String,
}

/// The decoded body of an `ORDERVIEW` response.
#[derive(Debug, Default, Deserialize)]
pub struct OrderViewResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The provider order reference.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub order_id: String,
    /// The package references of the order.
    #[serde(default, deserialize_with = "deserialize_package_ids")]
    pub package_ids: Vec<String>,
    /// The aggregate label URL.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub labels_url: String,
}

/// The decoded body of a `TRACKSTATUS` response.
#[derive(Debug, Deserialize)]
pub struct TrackStatusResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The decoded package entries.
    #[serde(default, deserialize_with = "deserialize_lenient_list")]
    pub packages: Vec<TrackStatusPackage>,
}

/// One per-package entry of a `TRACKSTATUS` response.
#[derive(Debug, Default, Deserialize)]
pub struct TrackStatusPackage {
    /// The carrier tracking number.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub carrier_id: String,
    /// The coarse provider status code.
    #[serde(default, deserialize_with = "deserialize_track_status_id")]
    pub status_id: Option<String>,
    /// The detailed provider status code.
    #[serde(default, deserialize_with = "deserialize_track_status_id")]
    pub status_id_v2: Option<String>,
    /// The provider status description.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub name: String,
    /// The fallback provider status description.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub status_text: String,
    /// The per-package provider status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
}

/// The decoded body of an `ORDER` response.
#[derive(Debug, Deserialize)]
pub struct OrderResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The provider order reference.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub order_id: String,
}

/// The decoded body of a `DROP` response.
#[derive(Debug, Deserialize)]
pub struct DropResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
}

/// The decoded body of an `ORDERPICKUP` response.
#[derive(Debug, Deserialize)]
pub struct PickupResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The provider pickup reference.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub pickup_order_id: String,
    /// The provider confirmation, when the provider sent one.
    #[serde(default)]
    pub confirmed: Option<bool>,
}

/// Decodes an `ADD` response body.
pub fn parse_add(body: &[u8]) -> Option<AddResponse> {
    parse_object(body)
}

/// Decodes an `OVERVIEW` response body.
pub fn parse_overview(body: &[u8]) -> Option<OverviewResponse> {
    parse_object(body)
}

/// Decodes a `LABELS` response body.
pub fn parse_labels(body: &[u8]) -> Option<LabelsResponse> {
    parse_object(body)
}

/// Decodes an `ORDERVIEW` response body.
pub fn parse_order_view(body: &[u8]) -> Option<OrderViewResponse> {
    parse_object(body)
}

/// Decodes a `TRACKSTATUS` response body.
pub fn parse_track_status(body: &[u8]) -> Option<TrackStatusResponse> {
    parse_object(body)
}

/// Decodes an `ORDER` response body.
pub fn parse_order(body: &[u8]) -> Option<OrderResponse> {
    parse_object(body)
}

/// Decodes a `DROP` response body.
pub fn parse_drop(body: &[u8]) -> Option<DropResponse> {
    parse_object(body)
}

/// Decodes an `ORDERPICKUP` response body.
pub fn parse_pickup(body: &[u8]) -> Option<PickupResponse> {
    parse_object(body)
}

fn parse_object<T: serde::de::DeserializeOwned>(body: &[u8]) -> Option<T> {
    if body.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    serde_json::from_slice(body).ok()
}

fn deserialize_branch_list<'de, D>(deserializer: D) -> Result<Vec<BranchWire>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct BranchListVisitor;

    impl<'de> Visitor<'de> for BranchListVisitor {
        type Value = Vec<BranchWire>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a branch list")
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut branches = Vec::new();
            while let Some(entry) = sequence.next_element::<Option<BranchWire>>()? {
                branches.push(entry.unwrap_or_default());
            }
            Ok(branches)
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut entries = Vec::new();
            while let Some((key, entry)) = map.next_entry::<String, Option<BranchWire>>()? {
                entries.push((key, entry.unwrap_or_default()));
            }
            entries.sort_by(|left, right| compare_branch_keys(&left.0, &right.0));
            Ok(entries.into_iter().map(|(_, branch)| branch).collect())
        }
    }

    deserializer.deserialize_any(BranchListVisitor)
}

fn compare_branch_keys(left: &str, right: &str) -> Ordering {
    match (left.parse::<i64>(), right.parse::<i64>()) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => left.cmp(right),
    }
}

fn deserialize_status<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    let text = raw.get().trim();
    if text.starts_with('"') {
        let digits: String = serde_json::from_str(text)
            .map_err(|_| serde::de::Error::custom("invalid Balíkobot response status"))?;
        if digits.is_empty()
            || digits.len() > 3
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(
                "invalid Balíkobot response status",
            ));
        }
        return digits
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("invalid Balíkobot response status"));
    }
    if text == "null" {
        return Err(serde::de::Error::custom(
            "invalid Balíkobot response status",
        ));
    }
    text.parse()
        .map(Some)
        .map_err(|_| serde::de::Error::custom("invalid Balíkobot response status"))
}

fn deserialize_track_status_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    let text = raw.get();
    if !valid_track_status_id(text) {
        return Err(serde::de::Error::custom(
            "invalid Balíkobot track status id",
        ));
    }
    Ok(Some(text.to_owned()))
}

fn valid_track_status_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    if bytes.first() == Some(&b'-') {
        index += 1;
    }
    let digits_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if !(1..=3).contains(&(index - digits_start)) {
        return false;
    }
    if index == bytes.len() {
        return true;
    }
    if bytes[index] != b'.' {
        return false;
    }
    index += 1;
    let fraction_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    (1..=2).contains(&(index - fraction_start)) && index == bytes.len()
}

fn deserialize_branch_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    let text = raw.get();
    if text.starts_with('"') {
        let value: String = serde_json::from_str(text)
            .map_err(|_| serde::de::Error::custom("invalid Balíkobot branch id"))?;
        return Ok(valid_branch_id(&value).then_some(value));
    }
    if text == "null" {
        return Ok(None);
    }
    if text.contains(['.', 'e', 'E'])
        || !text.starts_with(|first: char| first.is_ascii_digit() || first == '-')
    {
        return Err(serde::de::Error::custom("invalid Balíkobot branch id"));
    }
    Ok(valid_branch_id(text).then(|| text.to_owned()))
}

fn deserialize_coordinate<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Ok(raw) = Box::<RawValue>::deserialize(deserializer) else {
        return Ok(None);
    };
    let text = raw.get();
    let encoded: String = if text.starts_with('"') {
        serde_json::from_str(text).unwrap_or_default()
    } else {
        text.to_owned()
    };
    Ok(encoded.trim().parse().ok())
}

fn deserialize_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_lenient_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<Vec<Option<T>>>::deserialize(deserializer)?
        .unwrap_or_default()
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect())
}

fn deserialize_package_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    package_id_from_raw(raw.get())
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("invalid Balíkobot package id"))
}

fn deserialize_package_ids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    if raw.get().trim() == "null" {
        return Ok(Vec::new());
    }
    let values: Vec<Box<RawValue>> = serde_json::from_str(raw.get())
        .map_err(|_| serde::de::Error::custom("invalid Balíkobot package id"))?;
    values
        .iter()
        .map(|value| {
            package_id_from_raw(value.get())
                .ok_or_else(|| serde::de::Error::custom("invalid Balíkobot package id"))
        })
        .collect()
}

fn package_id_from_raw(text: &str) -> Option<String> {
    let value = if text.starts_with('"') {
        serde_json::from_str::<String>(text).ok()?
    } else {
        if text.contains(['.', 'e', 'E'])
            || !text.starts_with(|first: char| first.is_ascii_digit() || first == '-')
        {
            return None;
        }
        text.to_owned()
    };
    valid_package_id(&value).then_some(value)
}

/// Reports whether a value is usable as a package or order reference.
pub(crate) fn valid_package_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= IDENTIFIER_LIMIT
        && valid_branch_field(value, IDENTIFIER_LIMIT)
}

pub(crate) fn valid_branch_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn valid_branch_field(value: &str, maximum: usize) -> bool {
    value.chars().count() <= maximum && !value.contains(['\r', '\n', '\0'])
}

fn branch_coordinates(wire: &BranchWire) -> (Option<f64>, Option<f64>) {
    for (latitude, longitude) in [(wire.lat, wire.lng), (wire.latitude, wire.longitude)] {
        if let (Some(latitude), Some(longitude)) = (latitude, longitude)
            && valid_coordinates(latitude, longitude)
        {
            return (Some(latitude), Some(longitude));
        }
    }
    (None, None)
}

fn valid_coordinates(latitude: f64, longitude: f64) -> bool {
    (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
        && (latitude != 0.0 || longitude != 0.0)
}

pub(crate) const CAPABILITY_CARRIER_LIMIT: usize = 128;
pub(crate) const CAPABILITY_SERVICE_LIMIT: usize = 512;
const CAPABILITY_NAME_LIMIT: usize = 512;
const DECIMAL_EXPONENT_LIMIT: i64 = 64;
const PRICE_TEXT_LIMIT: usize = 64;
const SERVICE_CODE_LIMIT: usize = 64;

/// A provider answer that carries a top-level body status.
pub(crate) trait CapabilityStatus {
    /// Returns the body status, when the provider sent a valid one.
    fn status_value(&self) -> Option<i64>;
}

/// The decoded body of a `WHOAMI` response.
#[derive(Debug, Default, Deserialize)]
pub struct WhoAmIWire {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The live account flag, when the provider sent one.
    #[serde(default)]
    pub live_account: Option<bool>,
    /// The contracted carriers.
    #[serde(default, deserialize_with = "deserialize_lenient_list")]
    pub carriers: Vec<CapabilityCarrierWire>,
}

impl CapabilityStatus for WhoAmIWire {
    fn status_value(&self) -> Option<i64> {
        self.status
    }
}

/// One contracted carrier entry of a `WHOAMI` response.
#[derive(Debug, Default, Deserialize)]
pub struct CapabilityCarrierWire {
    /// The carrier code used in request paths.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub slug: String,
    /// The carrier display name.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub name: String,
}

/// The decoded body of an `ACTIVATEDSERVICES` response.
#[derive(Debug, Default, Deserialize)]
pub struct ActivatedServicesResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The active parcel shipping flag, when the provider sent one.
    #[serde(default)]
    pub active_parcel: Option<bool>,
    /// The activated service entries.
    #[serde(default, deserialize_with = "deserialize_lenient_list")]
    pub service_types: Vec<ActivatedServiceWire>,
}

impl CapabilityStatus for ActivatedServicesResponse {
    fn status_value(&self) -> Option<i64> {
        self.status
    }
}

/// One activated service entry before normalizing.
#[derive(Debug, Default, Deserialize)]
pub struct ActivatedServiceWire {
    /// The provider service code.
    #[serde(default, deserialize_with = "deserialize_service_code")]
    pub service_type: Option<String>,
    /// The provider service name.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub name: String,
    /// The home delivery flag.
    #[serde(default)]
    pub home_delivery: Option<bool>,
    /// The box delivery flag.
    #[serde(default)]
    pub box_delivery: Option<bool>,
    /// The pickup point delivery flag.
    #[serde(default)]
    pub pickup_points_delivery: Option<bool>,
}

/// The decoded body of a `COUNTRIES4SERVICE` response.
#[derive(Debug, Default, Deserialize)]
pub struct CountriesResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The service country entries.
    #[serde(default, deserialize_with = "countries_service_types")]
    pub service_types: Vec<CountriesServiceWire>,
}

impl CapabilityStatus for CountriesResponse {
    fn status_value(&self) -> Option<i64> {
        self.status
    }
}

/// One service country entry before normalizing.
#[derive(Debug, Default, Deserialize)]
pub struct CountriesServiceWire {
    /// The provider service code.
    #[serde(default, deserialize_with = "deserialize_service_code")]
    pub service_type: Option<String>,
    /// The destination country codes.
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub countries: Vec<String>,
}

/// The decoded body of a `COD4SERVICES` response.
#[derive(Debug, Default, Deserialize)]
pub struct CodResponse {
    /// The top-level body status, when the provider sent a valid one.
    #[serde(default, deserialize_with = "deserialize_status")]
    pub status: Option<i64>,
    /// The service cash-on-delivery entries.
    #[serde(default, deserialize_with = "deserialize_lenient_list")]
    pub service_types: Vec<CodServiceWire>,
}

impl CapabilityStatus for CodResponse {
    fn status_value(&self) -> Option<i64> {
        self.status
    }
}

/// One cash-on-delivery service entry before normalizing.
#[derive(Debug, Default, Deserialize)]
pub struct CodServiceWire {
    /// The provider service code.
    #[serde(default, deserialize_with = "deserialize_service_code")]
    pub service_type: Option<String>,
    /// The cash-on-delivery country entries.
    #[serde(default, deserialize_with = "deserialize_lenient_list")]
    pub countries: Vec<CodCountryWire>,
}

/// One cash-on-delivery country entry before normalizing.
#[derive(Debug, Default, Deserialize)]
pub struct CodCountryWire {
    /// The destination country code.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub country: String,
    /// The currency code.
    #[serde(default, deserialize_with = "deserialize_string")]
    pub currency: String,
    /// The unparsed maximum price.
    #[serde(default)]
    pub max_price: Option<Box<RawValue>>,
}

/// Normalizes the activated service entries of one carrier and returns them
/// with a lookup by service code.
pub(crate) fn normalize_activated_services(
    activated: &ActivatedServicesResponse,
) -> crate::Result<(Vec<Service>, HashMap<String, usize>)> {
    if activated.service_types.len() > CAPABILITY_SERVICE_LIMIT {
        return Err(Error::InvalidResponse);
    }
    let mut services = Vec::with_capacity(activated.service_types.len());
    let mut index = HashMap::with_capacity(activated.service_types.len());
    for entry in &activated.service_types {
        let service = normalize_activated_service(entry)?;
        if activated.active_parcel == Some(false) {
            continue;
        }
        if let Some(previous) = index.get(&service.code) {
            if !same_service(&services[*previous], &service) {
                return Err(Error::InvalidResponse);
            }
            continue;
        }
        index.insert(service.code.clone(), services.len());
        services.push(service);
    }
    Ok((services, index))
}

/// Merges the activated services, the supported countries and the optional
/// cash-on-delivery dictionary into the combined capability answer.
pub(crate) fn normalize_capabilities(
    activated: &ActivatedServicesResponse,
    countries: &CountriesResponse,
    cod: &CodResponse,
) -> crate::Result<Vec<Service>> {
    let (mut services, index) = normalize_activated_services(activated)?;
    merge_capability_countries(&mut services, &index, countries)?;
    merge_capability_cod(&mut services, &index, cod)?;
    Ok(services)
}

/// Selects the contracted carriers to discover. Without a scope every
/// contracted carrier is selected; an empty scope selects none. A requested
/// carrier that is not contracted fails the call.
pub(crate) fn scoped_capability_carriers(
    contracted: &[CapabilityCarrierWire],
    scope: Option<&[crate::CarrierCode]>,
) -> crate::Result<Vec<Carrier>> {
    if contracted.len() > CAPABILITY_CARRIER_LIMIT {
        return Err(Error::InvalidResponse);
    }
    let mut available = HashMap::with_capacity(contracted.len());
    for entry in contracted {
        let code = crate::CarrierCode::new(&entry.slug).map_err(|_| Error::InvalidResponse)?;
        available.insert(code, true);
    }
    let mut requested = available.clone();
    if let Some(scope) = scope {
        requested = HashMap::with_capacity(scope.len());
        for candidate in scope {
            let code =
                crate::CarrierCode::new(candidate.as_str()).map_err(|_| Error::InvalidResponse)?;
            if !available.contains_key(&code) {
                return Err(Error::InvalidResponse);
            }
            requested.insert(code, true);
        }
    }
    let mut carriers = Vec::with_capacity(requested.len());
    for entry in contracted {
        let code = crate::CarrierCode::new(&entry.slug).map_err(|_| Error::InvalidResponse)?;
        if requested.remove(&code).is_some() {
            carriers.push(Carrier {
                carrier_code: code,
                services: Vec::new(),
            });
        }
    }
    Ok(carriers)
}

/// Normalizes the cash-on-delivery destinations of one service. Every country
/// sent by the provider is kept.
pub(crate) fn normalize_cod_countries(
    countries: &[CodCountryWire],
) -> crate::Result<Vec<CODCapability>> {
    let mut entries = Vec::with_capacity(countries.len());
    for entry in countries {
        let capability = normalize_cod_capability(entry)?;
        if let Some(existing) =
            find_cod_capability(&entries, &capability.country, &capability.currency)
        {
            if *existing != capability {
                return Err(Error::InvalidResponse);
            }
            continue;
        }
        entries.push(capability);
    }
    Ok(entries)
}

fn normalize_activated_service(entry: &ActivatedServiceWire) -> crate::Result<Service> {
    let raw_code = entry
        .service_type
        .as_deref()
        .ok_or(Error::InvalidResponse)?;
    let code = raw_code.trim();
    let name = entry.name.trim();
    if !valid_capability_service_code(code)
        || name.is_empty()
        || name.chars().count() > CAPABILITY_NAME_LIMIT
        || name.chars().any(char::is_control)
    {
        return Err(Error::InvalidResponse);
    }
    Ok(Service {
        code: code.to_owned(),
        name: name.to_owned(),
        home_delivery: entry.home_delivery,
        box_delivery: entry.box_delivery,
        pickup_points_delivery: entry.pickup_points_delivery,
        countries: HashMap::new(),
        cod: Vec::new(),
    })
}

/// Reports whether a provider service code is usable for the capability
/// answers.
pub(crate) fn valid_capability_service_code(code: &str) -> bool {
    !code.is_empty()
        && code.chars().count() <= CAPABILITY_SERVICE_LIMIT
        && !code.contains(['/', '\\', '\0', '\r', '\n'])
        && !code.chars().any(char::is_control)
}

fn same_service(left: &Service, right: &Service) -> bool {
    left.code == right.code
        && left.name == right.name
        && left.home_delivery == right.home_delivery
        && left.box_delivery == right.box_delivery
        && left.pickup_points_delivery == right.pickup_points_delivery
}

fn merge_capability_countries(
    services: &mut [Service],
    index: &HashMap<String, usize>,
    countries: &CountriesResponse,
) -> crate::Result<()> {
    if countries.service_types.len() > CAPABILITY_SERVICE_LIMIT {
        return Err(Error::InvalidResponse);
    }
    for entry in &countries.service_types {
        let raw_code = entry
            .service_type
            .as_deref()
            .ok_or(Error::InvalidResponse)?;
        let code = raw_code.trim();
        if !valid_capability_service_code(code) || entry.countries.len() > CAPABILITY_SERVICE_LIMIT
        {
            return Err(Error::InvalidResponse);
        }
        let Some(service_index) = index.get(code) else {
            continue;
        };
        for raw_country in &entry.countries {
            let country = CountryCode::from_capability(raw_country);
            if is_eu_country_code(&country) {
                services[*service_index].countries.insert(country, true);
            }
        }
    }
    Ok(())
}

fn merge_capability_cod(
    services: &mut [Service],
    index: &HashMap<String, usize>,
    cod: &CodResponse,
) -> crate::Result<()> {
    if cod.service_types.len() > CAPABILITY_SERVICE_LIMIT {
        return Err(Error::InvalidResponse);
    }
    for entry in &cod.service_types {
        let raw_code = entry
            .service_type
            .as_deref()
            .ok_or(Error::InvalidResponse)?;
        let code = raw_code.trim();
        if !valid_capability_service_code(code) || entry.countries.len() > CAPABILITY_SERVICE_LIMIT
        {
            return Err(Error::InvalidResponse);
        }
        let Some(service_index) = index.get(code) else {
            continue;
        };
        let merged = merge_cod_countries(&services[*service_index].cod, &entry.countries)?;
        services[*service_index].cod = merged;
    }
    Ok(())
}

fn merge_cod_countries(
    entries: &[CODCapability],
    countries: &[CodCountryWire],
) -> crate::Result<Vec<CODCapability>> {
    let mut merged = entries.to_vec();
    for entry in countries {
        let capability = normalize_cod_capability(entry)?;
        if !is_eu_country_code(&capability.country) {
            continue;
        }
        if let Some(existing) =
            find_cod_capability(&merged, &capability.country, &capability.currency)
        {
            if *existing != capability {
                return Err(Error::InvalidResponse);
            }
            continue;
        }
        merged.push(capability);
    }
    Ok(merged)
}

fn find_cod_capability<'a>(
    entries: &'a [CODCapability],
    country: &CountryCode,
    currency: &CurrencyCode,
) -> Option<&'a CODCapability> {
    entries
        .iter()
        .find(|entry| entry.country == *country && entry.currency == *currency)
}

fn normalize_cod_capability(entry: &CodCountryWire) -> crate::Result<CODCapability> {
    let country = CountryCode::new(&entry.country).map_err(|_| Error::InvalidResponse)?;
    let currency = CurrencyCode::new(&entry.currency).map_err(|_| Error::InvalidResponse)?;
    let max_amount_minor = entry
        .max_price
        .as_deref()
        .and_then(major_price_to_minor)
        .ok_or(Error::InvalidResponse)?;
    Ok(CODCapability {
        country,
        currency,
        max_amount_minor,
    })
}

/// Converts a major-unit price text into minor units without floats.
fn major_price_to_minor(raw: &RawValue) -> Option<i64> {
    let text = raw.get().trim();
    if text.is_empty() || text.len() > PRICE_TEXT_LIMIT {
        return None;
    }
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => {
            let exponent = text[index + 1..].parse::<i64>().ok()?;
            if !(-DECIMAL_EXPONENT_LIMIT..=DECIMAL_EXPONENT_LIMIT).contains(&exponent) {
                return None;
            }
            (&text[..index], exponent)
        }
        None => (text, 0),
    };
    let (negative, unsigned) = match mantissa.strip_prefix('-') {
        Some(unsigned) => (true, unsigned),
        None => (false, mantissa.strip_prefix('+').unwrap_or(mantissa)),
    };
    let (integer, fraction) = match unsigned.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (unsigned, ""),
    };
    if (integer.is_empty() && fraction.is_empty())
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    if integer
        .bytes()
        .chain(fraction.bytes())
        .all(|byte| byte == b'0')
    {
        return Some(0);
    }
    if negative {
        return None;
    }
    let mut digits = String::with_capacity(integer.len() + fraction.len());
    digits.push_str(integer);
    digits.push_str(fraction);
    let mut scale = exponent + 2 - i64::try_from(fraction.len()).ok()?;
    if scale < 0 {
        let need = usize::try_from(-scale).ok()?;
        if need > digits.len()
            || !digits[digits.len() - need..]
                .bytes()
                .all(|byte| byte == b'0')
        {
            return None;
        }
        digits.truncate(digits.len() - need);
        scale = 0;
    }
    let mut value: i128 = 0;
    for byte in digits.bytes() {
        value = value
            .checked_mul(10)?
            .checked_add(i128::from(byte - b'0'))?;
    }
    if scale > 0 {
        value = value.checked_mul(10i128.checked_pow(u32::try_from(scale).ok()?)?)?;
    }
    i64::try_from(value).ok()
}

fn is_eu_country_code(code: &CountryCode) -> bool {
    matches!(
        code.as_str(),
        "AT" | "BE"
            | "BG"
            | "HR"
            | "CY"
            | "CZ"
            | "DK"
            | "EE"
            | "FI"
            | "FR"
            | "DE"
            | "GR"
            | "HU"
            | "IE"
            | "IT"
            | "LV"
            | "LT"
            | "LU"
            | "MT"
            | "NL"
            | "PL"
            | "PT"
            | "RO"
            | "SK"
            | "SI"
            | "ES"
            | "SE"
    )
}

fn deserialize_service_code<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    let text = raw.get();
    if text.starts_with('"') {
        let value: String = serde_json::from_str(text)
            .map_err(|_| serde::de::Error::custom("invalid Balíkobot service code"))?;
        if !valid_service_code(&value) {
            return Err(serde::de::Error::custom("invalid Balíkobot service code"));
        }
        return Ok(Some(value));
    }
    if text.starts_with(|first: char| first.is_ascii_digit() || first == '-')
        && !text.contains(['.', 'e', 'E'])
        && valid_service_code(text)
    {
        return Ok(Some(text.to_owned()));
    }
    Err(serde::de::Error::custom("invalid Balíkobot service code"))
}

fn valid_service_code(value: &str) -> bool {
    !value.is_empty() && value.len() <= SERVICE_CODE_LIMIT && !value.contains(['\r', '\n', '\0'])
}

fn countries_service_types<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<CountriesServiceWire>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    let text = raw.get().trim();
    if text == "null" {
        return Ok(Vec::new());
    }
    if text.starts_with('[') {
        let services: Vec<CountriesServiceWire> = serde_json::from_str(text)
            .map_err(|_| serde::de::Error::custom("invalid Balíkobot countries answer"))?;
        if services.len() > CAPABILITY_SERVICE_LIMIT {
            return Err(serde::de::Error::custom(
                "invalid Balíkobot countries answer",
            ));
        }
        return Ok(services);
    }
    if !text.starts_with('{') {
        return Err(serde::de::Error::custom(
            "invalid Balíkobot countries answer",
        ));
    }
    let keyed: BTreeMap<String, CountriesServiceWire> = serde_json::from_str(text)
        .map_err(|_| serde::de::Error::custom("invalid Balíkobot countries answer"))?;
    if keyed.len() > CAPABILITY_SERVICE_LIMIT
        || keyed
            .keys()
            .any(|key| key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(serde::de::Error::custom(
            "invalid Balíkobot countries answer",
        ));
    }
    Ok(keyed.into_values().collect())
}

fn deserialize_string_list<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
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
    fn json_content_type_rejects_malformed_parameters() {
        assert!(!is_json(&response(&[(
            "content-type",
            "application/json; charset"
        )])));
        assert!(!is_json(&response(&[(
            "content-type",
            "application/json; =utf-8"
        )])));
        assert!(!is_json(&response(&[(
            "content-type",
            "application/json;"
        )])));
        assert!(is_json(&response(&[(
            "content-type",
            "application/json; charset=utf-8; profile=x"
        )])));
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
        assert!(valid_label_url(&client, "https://pdf.balikobot.cz/"));
        assert!(!valid_label_url(&client, "https://pdf.balikobot.cz"));
        assert!(!valid_label_url(&client, "https://pdf.balikobot.cz?zpl=1"));
        assert!(!valid_label_url(
            &client,
            "https://@pdf.balikobot.cz/label.pdf"
        ));
    }

    #[test]
    fn package_id_limit_counts_bytes_not_characters() {
        let multibyte = "ž".repeat(60);
        assert_eq!(multibyte.chars().count(), 60);
        assert!(multibyte.len() > IDENTIFIER_LIMIT);
        assert!(!valid_package_id(&multibyte));
        assert!(valid_package_id(&"a".repeat(IDENTIFIER_LIMIT)));
        assert!(!valid_package_id(&"a".repeat(IDENTIFIER_LIMIT + 1)));
    }

    #[test]
    fn major_price_converts_to_minor_units_without_floats() {
        let raw = |text: &str| RawValue::from_string(text.to_owned()).expect("raw");
        for rejected in [
            "1e999999999",
            "1e-999999999",
            "1.001",
            "-1",
            "\"NaN\"",
            "\"1\"",
        ] {
            assert_eq!(major_price_to_minor(&raw(rejected)), None, "{rejected}");
        }
        assert_eq!(major_price_to_minor(&raw("1499.95")), Some(149995));
        assert_eq!(major_price_to_minor(&raw(" 12 ")), Some(1200));
        assert_eq!(major_price_to_minor(&raw("0.01")), Some(1));
        assert_eq!(major_price_to_minor(&raw("1e2")), Some(10000));
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
