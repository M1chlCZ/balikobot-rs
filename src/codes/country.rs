//! Country codes used by the Balíkobot API.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// An ISO 3166-1 alpha-2 country code.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CountryCode(Cow<'static, str>);

impl CountryCode {
    /// Austria.
    pub const AT: Self = Self(Cow::Borrowed("AT"));
    /// Belgium.
    pub const BE: Self = Self(Cow::Borrowed("BE"));
    /// Bulgaria.
    pub const BG: Self = Self(Cow::Borrowed("BG"));
    /// Croatia.
    pub const HR: Self = Self(Cow::Borrowed("HR"));
    /// Cyprus.
    pub const CY: Self = Self(Cow::Borrowed("CY"));
    /// Czechia.
    pub const CZ: Self = Self(Cow::Borrowed("CZ"));
    /// Denmark.
    pub const DK: Self = Self(Cow::Borrowed("DK"));
    /// Estonia.
    pub const EE: Self = Self(Cow::Borrowed("EE"));
    /// Finland.
    pub const FI: Self = Self(Cow::Borrowed("FI"));
    /// France.
    pub const FR: Self = Self(Cow::Borrowed("FR"));
    /// Germany.
    pub const DE: Self = Self(Cow::Borrowed("DE"));
    /// Greece.
    pub const GR: Self = Self(Cow::Borrowed("GR"));
    /// Hungary.
    pub const HU: Self = Self(Cow::Borrowed("HU"));
    /// Ireland.
    pub const IE: Self = Self(Cow::Borrowed("IE"));
    /// Italy.
    pub const IT: Self = Self(Cow::Borrowed("IT"));
    /// Latvia.
    pub const LV: Self = Self(Cow::Borrowed("LV"));
    /// Lithuania.
    pub const LT: Self = Self(Cow::Borrowed("LT"));
    /// Luxembourg.
    pub const LU: Self = Self(Cow::Borrowed("LU"));
    /// Malta.
    pub const MT: Self = Self(Cow::Borrowed("MT"));
    /// The Netherlands.
    pub const NL: Self = Self(Cow::Borrowed("NL"));
    /// Poland.
    pub const PL: Self = Self(Cow::Borrowed("PL"));
    /// Portugal.
    pub const PT: Self = Self(Cow::Borrowed("PT"));
    /// Romania.
    pub const RO: Self = Self(Cow::Borrowed("RO"));
    /// Slovakia.
    pub const SK: Self = Self(Cow::Borrowed("SK"));
    /// Slovenia.
    pub const SI: Self = Self(Cow::Borrowed("SI"));
    /// Spain.
    pub const ES: Self = Self(Cow::Borrowed("ES"));
    /// Sweden.
    pub const SE: Self = Self(Cow::Borrowed("SE"));
    /// The United Kingdom.
    pub const GB: Self = Self(Cow::Borrowed("GB"));
    /// Switzerland.
    pub const CH: Self = Self(Cow::Borrowed("CH"));
    /// Norway.
    pub const NO: Self = Self(Cow::Borrowed("NO"));
    /// Iceland.
    pub const IS: Self = Self(Cow::Borrowed("IS"));
    /// Liechtenstein.
    pub const LI: Self = Self(Cow::Borrowed("LI"));
    /// Ukraine.
    pub const UA: Self = Self(Cow::Borrowed("UA"));
    /// Serbia.
    pub const RS: Self = Self(Cow::Borrowed("RS"));
    /// Bosnia and Herzegovina.
    pub const BA: Self = Self(Cow::Borrowed("BA"));
    /// Montenegro.
    pub const ME: Self = Self(Cow::Borrowed("ME"));
    /// North Macedonia.
    pub const MK: Self = Self(Cow::Borrowed("MK"));
    /// Albania.
    pub const AL: Self = Self(Cow::Borrowed("AL"));
    /// Türkiye.
    pub const TR: Self = Self(Cow::Borrowed("TR"));
    /// The United States.
    pub const US: Self = Self(Cow::Borrowed("US"));
    /// Canada.
    pub const CA: Self = Self(Cow::Borrowed("CA"));

    /// Normalizes a country code and returns it. The value is trimmed and
    /// uppercased, so custom countries work too. A malformed value returns
    /// [`Error::InvalidRequest`].
    pub fn new(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_uppercase();
        if !valid(&normalized) {
            return Err(Error::InvalidRequest);
        }
        Ok(Self(Cow::Owned(normalized)))
    }

    /// Reports whether the code is a well-formed country code.
    pub fn is_valid(&self) -> bool {
        valid(&self.0)
    }

    /// Returns the wire value of the code.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid(value: &str) -> bool {
    value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_uppercase())
}

impl fmt::Display for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CountryCode {
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
        assert_eq!(CountryCode::DE.as_str(), "DE");
    }

    #[test]
    fn new_normalizes_custom_value() {
        assert_eq!(CountryCode::new(" de ").unwrap().as_str(), "DE");
    }

    #[test]
    fn new_rejects_malformed_value() {
        assert_eq!(CountryCode::new("D3"), Err(Error::InvalidRequest));
    }

    #[test]
    fn is_valid_checks_value() {
        assert!(CountryCode::CZ.is_valid());
        assert!(!CountryCode(Cow::Borrowed("C3")).is_valid());
    }

    #[test]
    fn serde_round_trip_uses_plain_string() {
        let code = CountryCode::new(" de ").unwrap();
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, "\"DE\"");
        assert_eq!(serde_json::from_str::<CountryCode>(&json).unwrap(), code);
    }
}
