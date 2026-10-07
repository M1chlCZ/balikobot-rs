//! The package models shared by the shipment methods.

use serde::Serialize;

use crate::codes::{CountryCode, CurrencyCode};

/// One package for the ADD method. The external reference `eid` makes ADD
/// idempotent: a repeated request with an already stored `eid` returns the
/// original record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AddPackageRequest {
    /// The external package reference. It must contain 8 to 40 alphanumeric
    /// or dash characters.
    #[serde(rename = "eid")]
    pub eid: String,
    /// The carrier service code, for example "1" or "VMCZ".
    #[serde(rename = "service_type")]
    pub service_type: String,
    /// The recipient name. The name or the firm must be set.
    #[serde(rename = "rec_name", skip_serializing_if = "String::is_empty")]
    pub rec_name: String,
    /// The recipient company. The name or the firm must be set.
    #[serde(rename = "rec_firm", skip_serializing_if = "String::is_empty")]
    pub rec_firm: String,
    /// The recipient street.
    #[serde(rename = "rec_street")]
    pub rec_street: String,
    /// The recipient city.
    #[serde(rename = "rec_city")]
    pub rec_city: String,
    /// The recipient postal code.
    #[serde(rename = "rec_zip")]
    pub rec_zip: String,
    /// The ISO 3166-1 alpha-2 destination country.
    #[serde(rename = "rec_country")]
    pub rec_country: CountryCode,
    /// The recipient phone. The phone or the email must be set.
    #[serde(rename = "rec_phone", skip_serializing_if = "String::is_empty")]
    pub rec_phone: String,
    /// The recipient email. The phone or the email must be set.
    #[serde(rename = "rec_email", skip_serializing_if = "String::is_empty")]
    pub rec_email: String,
    /// The pickup branch reference for branch delivery.
    #[serde(rename = "branch_id", skip_serializing_if = "String::is_empty")]
    pub branch_id: String,
    /// The package weight in kilograms. It must be positive and at most
    /// 10000.
    #[serde(rename = "weight")]
    pub weight: f64,
    /// The package length in centimeters. It must be positive and at most
    /// 1000.
    #[serde(rename = "length")]
    pub length: f64,
    /// The package width in centimeters. It must be positive and at most
    /// 1000.
    #[serde(rename = "width")]
    pub width: f64,
    /// The package height in centimeters. It must be positive and at most
    /// 1000.
    #[serde(rename = "height")]
    pub height: f64,
    /// The declared value. It must not be negative and at most 100000000.
    #[serde(rename = "price")]
    pub price: f64,
    /// The cash-on-delivery amount. It must not be negative and at most
    /// 100000000. A positive amount requires `vs`.
    #[serde(rename = "cod_price", skip_serializing_if = "is_zero")]
    pub cod_price: f64,
    /// The cash-on-delivery currency. It must be "CZK" or "EUR".
    #[serde(rename = "cod_currency", skip_serializing_if = "currency_is_empty")]
    pub cod_currency: CurrencyCode,
    /// The cash-on-delivery variable symbol. It must be set exactly when
    /// `cod_price` is positive and must be below 10000000000.
    #[serde(rename = "vs", skip_serializing_if = "Option::is_none")]
    pub vs: Option<i64>,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

fn currency_is_empty(value: &CurrencyCode) -> bool {
    value.as_str().is_empty()
}

/// The accepted package record returned by ADD.
#[derive(Debug, Clone, PartialEq)]
pub struct AddPackageResult {
    /// The Balíkobot package reference used by labels, ORDER and DROP.
    pub package_id: String,
    /// The carrier tracking number.
    pub carrier_id: String,
    /// The provider label URL for this package.
    pub label_url: String,
}

/// One open package entry returned by OVERVIEW.
#[derive(Debug, Clone, PartialEq)]
pub struct OverviewPackage {
    /// The external package reference stored by the provider.
    pub eid: String,
    /// The Balíkobot package reference.
    pub package_id: String,
    /// The carrier tracking number.
    pub carrier_id: String,
    /// The provider label URL for this package.
    pub label_url: String,
}
