//! The tracking models shared by the tracking methods.

/// The latest provider tracking status of one package.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackStatusResult {
    /// The raw provider status code, for example "1", "1.2" or "-1".
    pub status_id: String,
    /// The provider status description.
    pub status_text: String,
}

/// The batch reference returned by ORDER.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderResult {
    /// The provider batch identifier.
    pub order_id: String,
}
