//! Tracking webhook parsing.

use crate::models::TrackingNotification;
use crate::wire::{self, TrackingNotificationWire};
use crate::{Error, Result};

const TRACKING_BATCH_LIMIT: usize = 1_000;

/// Parses the JSON body of a Balíkobot tracking webhook and returns one
/// notification per status change. The body can be a single notification
/// object or an array of objects; an empty array returns an empty result.
/// Every entry must identify the package through `package_id`, `eid` or
/// `carrier_id`. Every entry must also carry a decimal `status_id` or
/// `status_id_v2` and a status description in `name` or `status_text`. A
/// malformed body, a missing required field or more than 1000 entries returns
/// [`Error::InvalidResponse`].
pub fn parse_tracking_notifications(body: &[u8]) -> Result<Vec<TrackingNotification>> {
    let entries = wire::parse_tracking_notifications(body).ok_or(Error::InvalidResponse)?;
    if entries.len() > TRACKING_BATCH_LIMIT {
        return Err(Error::InvalidResponse);
    }
    entries
        .into_iter()
        .map(normalize_tracking_notification)
        .collect::<Option<Vec<_>>>()
        .ok_or(Error::InvalidResponse)
}

fn normalize_tracking_notification(
    entry: TrackingNotificationWire,
) -> Option<TrackingNotification> {
    if entry.package_id.is_none() && entry.eid.is_none() && entry.carrier_id.is_none() {
        return None;
    }
    let status_id = entry.status_id_v2.or(entry.status_id)?;
    let status_text = if entry.name.is_empty() {
        entry.status_text
    } else {
        entry.name
    };
    if status_text.is_empty() || !wire::valid_branch_field(&status_text, wire::BRANCH_FIELD_LIMIT) {
        return None;
    }
    Some(TrackingNotification {
        package_id: entry.package_id.unwrap_or_default(),
        eid: entry.eid.unwrap_or_default(),
        carrier_id: entry.carrier_id.unwrap_or_default(),
        status_id,
        status_text,
    })
}
