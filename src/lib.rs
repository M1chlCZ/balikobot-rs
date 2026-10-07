//! A Rust client for the Balikobot shipping API v2.

pub mod client;
pub mod codes;
pub mod config;
pub mod error;
pub mod models;
#[doc(hidden)]
pub mod wire;

pub use client::{Client, resolve_branch_id};
pub use codes::{CarrierCode, CountryCode, CurrencyCode};
pub use config::Config;
pub use error::{Error, Result};
pub use models::{
    ActivatedServices, AddPackageRequest, AddPackageResult, Branch, CODCapability, Carrier,
    OrderResult, OverviewPackage, PickupRequest, PickupResult, Service, ServiceCOD,
    ServiceCountries, TrackStatusResult, WhoAmI, WhoAmICarrier,
};
