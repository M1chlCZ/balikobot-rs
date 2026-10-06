//! Currency codes used by the Balíkobot API.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// An ISO 4217 currency code.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CurrencyCode(Cow<'static, str>);

impl CurrencyCode {
    /// The Czech koruna.
    pub const CZK: Self = Self(Cow::Borrowed("CZK"));
    /// The euro.
    pub const EUR: Self = Self(Cow::Borrowed("EUR"));
    /// The United States dollar.
    pub const USD: Self = Self(Cow::Borrowed("USD"));
    /// The pound sterling.
    pub const GBP: Self = Self(Cow::Borrowed("GBP"));
    /// The Polish złoty.
    pub const PLN: Self = Self(Cow::Borrowed("PLN"));
    /// The Hungarian forint.
    pub const HUF: Self = Self(Cow::Borrowed("HUF"));
    /// The Romanian leu.
    pub const RON: Self = Self(Cow::Borrowed("RON"));
    /// The Bulgarian lev.
    pub const BGN: Self = Self(Cow::Borrowed("BGN"));
    /// The Croatian kuna.
    pub const HRK: Self = Self(Cow::Borrowed("HRK"));
    /// The Swiss franc.
    pub const CHF: Self = Self(Cow::Borrowed("CHF"));
    /// The Norwegian krone.
    pub const NOK: Self = Self(Cow::Borrowed("NOK"));
    /// The Swedish krona.
    pub const SEK: Self = Self(Cow::Borrowed("SEK"));
    /// The Danish krone.
    pub const DKK: Self = Self(Cow::Borrowed("DKK"));

    /// Normalizes a currency code and returns it. The value is trimmed and
    /// uppercased, so custom currencies work too. A malformed value returns
    /// [`Error::InvalidRequest`].
    pub fn new(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_uppercase();
        if !valid(&normalized) {
            return Err(Error::InvalidRequest);
        }
        Ok(Self(Cow::Owned(normalized)))
    }

    /// Reports whether the code is a well-formed currency code.
    pub fn is_valid(&self) -> bool {
        valid(&self.0)
    }

    /// Returns the wire value of the code.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid(value: &str) -> bool {
    value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase())
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CurrencyCode {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_uses_wire_value() {
        assert_eq!(CurrencyCode::CZK.as_str(), "CZK");
    }

    #[test]
    fn new_normalizes_custom_value() {
        assert_eq!(CurrencyCode::new(" eur ").unwrap().as_str(), "EUR");
    }

    #[test]
    fn new_rejects_malformed_value() {
        assert_eq!(CurrencyCode::new("eu"), Err(Error::InvalidRequest));
    }

    #[test]
    fn is_valid_checks_value() {
        assert!(CurrencyCode::USD.is_valid());
        assert!(!CurrencyCode(Cow::Borrowed("u$d")).is_valid());
    }

    #[test]
    fn serde_round_trip_uses_plain_string() {
        let code = CurrencyCode::new(" eur ").unwrap();
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, "\"EUR\"");
        assert_eq!(serde_json::from_str::<CurrencyCode>(&json).unwrap(), code);
    }
}
