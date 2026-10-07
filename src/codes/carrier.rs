//! Carrier codes used by the Balíkobot API.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A carrier code used in Balíkobot request paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CarrierCode(Cow<'static, str>);

impl CarrierCode {
    /// The PPL code.
    pub const PPL: Self = Self(Cow::Borrowed("ppl"));
    /// The DPD code.
    pub const DPD: Self = Self(Cow::Borrowed("dpd"));
    /// The DPD Czech Republic code.
    pub const DPDCZ: Self = Self(Cow::Borrowed("dpdcz"));
    /// The DPD Slovakia code.
    pub const DPDSK: Self = Self(Cow::Borrowed("dpdsk"));
    /// The Geis code.
    pub const GEIS: Self = Self(Cow::Borrowed("geis"));
    /// The GLS code.
    pub const GLS: Self = Self(Cow::Borrowed("gls"));
    /// The InTime code.
    pub const INTIME: Self = Self(Cow::Borrowed("intime"));
    /// The Česká pošta code.
    pub const CP: Self = Self(Cow::Borrowed("cp"));
    /// The alternative Česká pošta code.
    pub const CESKAPOSTA: Self = Self(Cow::Borrowed("ceskaposta"));
    /// The Balíkovna code.
    pub const BALIKOVNA: Self = Self(Cow::Borrowed("balikovna"));
    /// The Zásilkovna code.
    pub const ZASILKOVNA: Self = Self(Cow::Borrowed("zasilkovna"));
    /// The Slovenská pošta code.
    pub const SP: Self = Self(Cow::Borrowed("sp"));
    /// The Uloženka code.
    pub const ULOZENKA: Self = Self(Cow::Borrowed("ulozenka"));

    /// Normalizes a carrier code and returns it. The value is trimmed and
    /// lowercased, so custom carriers work too. A malformed value returns
    /// [`Error::InvalidCode`].
    pub fn new(value: &str) -> Result<Self> {
        let normalized = value.trim().to_lowercase();
        if !valid(&normalized) {
            return Err(Error::InvalidCode {
                kind: "carrier",
                value: value.to_owned(),
            });
        }
        Ok(Self(Cow::Owned(normalized)))
    }

    /// Reports whether the code is a well-formed carrier code.
    pub fn is_valid(&self) -> bool {
        valid(&self.0)
    }

    /// Returns the wire value of the code.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Builds a code from a provider capability answer without normalizing
    /// or validating it.
    pub(crate) fn from_capability(value: &str) -> Self {
        Self(Cow::Owned(value.to_owned()))
    }
}

fn valid(value: &str) -> bool {
    (2..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

impl fmt::Display for CarrierCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CarrierCode {
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
        assert_eq!(CarrierCode::CESKAPOSTA.as_str(), "ceskaposta");
    }

    #[test]
    fn new_normalizes_custom_value() {
        assert_eq!(
            CarrierCode::new(" MyCarrier99 ").unwrap().as_str(),
            "mycarrier99"
        );
    }

    #[test]
    fn new_rejects_malformed_value() {
        assert_eq!(
            CarrierCode::new("bad code!"),
            Err(Error::InvalidCode {
                kind: "carrier",
                value: "bad code!".to_owned(),
            })
        );
    }

    #[test]
    fn is_valid_checks_value() {
        assert!(CarrierCode::PPL.is_valid());
        assert!(!CarrierCode(Cow::Borrowed("Bad Code!")).is_valid());
    }

    #[test]
    fn serde_round_trip_uses_plain_string() {
        let code = CarrierCode::new(" MyCarrier99 ").unwrap();
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, "\"mycarrier99\"");
        assert_eq!(serde_json::from_str::<CarrierCode>(&json).unwrap(), code);
    }
}
