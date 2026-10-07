//! A Rust client for the Balikobot shipping API v2.
//!
//! The crate wraps the JSON API for packages, labels, tracking, pickups, and
//! carrier capabilities in a blocking client. Build a [`Client`] from a
//! [`Config`] and call its methods; every failure is returned as a typed
//! [`Error`] that separates local request validation, permanent refusals,
//! temporary unavailability, and ambiguous mutating outcomes.

#![forbid(unsafe_code)]

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
