## [0.2.0] - 2026-10-13

- Batch label download through `Client::download_batch_labels`: a single
  package id returns a PDF or ZPL body, several package ids return a ZIP
  archive, and the call accepts up to 1000 ids.
- Tracking webhook parsing through `parse_tracking_notifications` and the
  `TrackingNotification` model: single objects and arrays of up to 1000
  entries with strict validation.
- Continuous integration and crates.io release workflows.

## 0.1.1

- Documentation and internal cleanup.

## 0.1.0

- Parity port of balikobot-go v0.2.0.
- Six error variants: invalid request, rejected, unavailable, not found,
  ambiguous and invalid response.
- Typed carrier, currency and country codes.
- Account mode with a five-minute cache.
- JSON and label response limits.
- Label host allowlist.
