//! The HTTP transport shared by the API methods.

use std::cmp::Ordering;
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
use crate::codes::CountryCode;
use crate::config::{host_is_loopback, host_with_port};
use crate::models::Branch;

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
}

impl From<RequestFailure> for Error {
    fn from(failure: RequestFailure) -> Self {
        match failure {
            RequestFailure::Transport(error) => dispatch_error(&error),
            RequestFailure::BodyLimit => Error::InvalidResponse,
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
    let limit = client.max_response_bytes;
    let mut reader = body.into_reader();
    let mut body = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| RequestFailure::Transport(ureq::Error::Io(error)))?;
    if body.len() > limit {
        return Err(RequestFailure::BodyLimit);
    }
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

/// The decoded body of an `ORDERV` response.
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

/// Decodes an `ORDERV` response body.
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
    !value.is_empty() && valid_branch_field(value, IDENTIFIER_LIMIT)
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
