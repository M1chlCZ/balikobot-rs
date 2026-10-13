//! The tracking models shared by the tracking methods.

/// The latest provider tracking status of one package.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackStatusResult {
    /// The raw provider status code, for example "1", "1.2" or "-1".
    pub status_id: String,
    /// The provider status description.
    pub status_text: String,
}

/// One package status change delivered to a Balíkobot tracking webhook. At
/// least one of `package_id`, `eid` and `carrier_id` identifies the package;
/// `status_id` and `status_text` describe the new provider status.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackingNotification {
    /// The Balíkobot package reference. It can be empty when the notification
    /// identifies the package another way.
    pub package_id: String,
    /// The external package reference. It can be empty when the notification
    /// identifies the package another way.
    pub eid: String,
    /// The carrier tracking number. It can be empty when the notification
    /// identifies the package another way.
    pub carrier_id: String,
    /// The raw provider status code, for example "1", "1.2" or "-1". The
    /// detailed `status_id_v2` value wins when both are present.
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
