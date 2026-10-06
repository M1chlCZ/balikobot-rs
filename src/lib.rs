//! A Rust client for the Balikobot shipping API v2.

pub mod codes;
pub mod error;

pub use codes::{CarrierCode, CountryCode, CurrencyCode};
pub use error::{Error, Result};
