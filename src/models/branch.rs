//! The carrier branch model.

use crate::codes::CountryCode;

/// A carrier branch or pickup point.
#[derive(Debug, Clone, PartialEq)]
pub struct Branch {
    /// The branch identifier used by the carrier.
    pub id: String,
    /// The provider branch type, for example "branch" or "box".
    pub r#type: String,
    /// The display name. It falls back to the postal code when the provider
    /// sends no name.
    pub name: String,
    /// The street part of the address.
    pub street: String,
    /// The city part of the address.
    pub city: String,
    /// The postal code.
    pub zip: String,
    /// The ISO 3166-1 alpha-2 country code. It is empty when the provider
    /// omits it for a domestic branch.
    pub country: CountryCode,
    /// The GPS latitude when the provider sent a valid coordinate pair.
    pub latitude: Option<f64>,
    /// The GPS longitude when the provider sent a valid coordinate pair.
    pub longitude: Option<f64>,
}
