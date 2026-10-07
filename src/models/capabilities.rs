//! The account capability models returned by the discovery methods.

use std::collections::HashMap;

use crate::codes::{CarrierCode, CountryCode, CurrencyCode};

/// The account information returned by the WHOAMI method.
#[derive(Debug, Clone, PartialEq)]
pub struct WhoAmI {
    /// The top-level provider status.
    pub status: i64,
    /// Whether the credentials belong to a live account. It is `None` when
    /// the provider omits the flag.
    pub live_account: Option<bool>,
    /// The carriers contracted by the account.
    pub carriers: Vec<WhoAmICarrier>,
}

/// One contracted carrier of the account.
#[derive(Debug, Clone, PartialEq)]
pub struct WhoAmICarrier {
    /// The carrier code used in request paths.
    pub slug: CarrierCode,
    /// The carrier display name. It can be empty.
    pub name: String,
}

/// The discovered services of one contracted carrier.
#[derive(Debug, Clone, PartialEq)]
pub struct Carrier {
    /// The carrier code used in request paths.
    pub carrier_code: CarrierCode,
    /// The activated services of the carrier.
    pub services: Vec<Service>,
}

/// One activated carrier service.
#[derive(Debug, Clone, PartialEq)]
pub struct Service {
    /// The provider service code.
    pub code: String,
    /// The provider service name.
    pub name: String,
    /// Home delivery support. `None` means that the provider did not declare
    /// the flag.
    pub home_delivery: Option<bool>,
    /// Box delivery support. `None` means that the provider did not declare
    /// the flag.
    pub box_delivery: Option<bool>,
    /// Pickup point delivery support. `None` means that the provider did not
    /// declare the flag.
    pub pickup_points_delivery: Option<bool>,
    /// The destination country codes supported by the service. The combined
    /// discovery keeps only EU destinations.
    pub countries: HashMap<CountryCode, bool>,
    /// The supported cash-on-delivery destinations. The combined discovery
    /// leaves it empty because it does not request the optional dictionary.
    pub cod: Vec<CODCapability>,
}

/// One cash-on-delivery destination of a service.
#[derive(Debug, Clone, PartialEq)]
pub struct CODCapability {
    /// The ISO 3166-1 alpha-2 destination country.
    pub country: CountryCode,
    /// The three-letter currency code.
    pub currency: CurrencyCode,
    /// The maximum cash-on-delivery amount in minor units, for example
    /// 149995 for 1499.95 CZK.
    pub max_amount_minor: i64,
}

/// The ACTIVATEDSERVICES answer of one carrier.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivatedServices {
    /// The provider flag that marks active parcel shipping. It is `None`
    /// when the provider omits the flag.
    pub active_parcel: Option<bool>,
    /// The activated services. The `countries` and `cod` fields are empty;
    /// use [`crate::Client::carrier_capabilities`] for the combined discovery.
    pub services: Vec<Service>,
}

/// One entry of the COUNTRIES4SERVICE answer.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceCountries {
    /// The provider service code.
    pub service_type: String,
    /// The destination country codes exactly as sent, with surrounding
    /// whitespace trimmed and letters upper-cased.
    pub countries: Vec<CountryCode>,
}

/// One entry of the COD4SERVICES answer.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceCOD {
    /// The provider service code.
    pub service_type: String,
    /// The normalized cash-on-delivery destinations.
    pub countries: Vec<CODCapability>,
}
