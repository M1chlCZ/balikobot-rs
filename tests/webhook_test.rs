use balikobot::{Error, TrackingNotification, parse_tracking_notifications};

#[test]
fn single_object_is_parsed() {
    let notifications = parse_tracking_notifications(
        r#"{
            "package_id": "add-ppl-8728035",
            "eid": "order-2026-000123-S1",
            "carrier_id": "DR1536622512M",
            "status_id": 1,
            "status_id_v2": 1.2,
            "name": "Zásilka byla doručena příjemci.",
            "status_text": "Doručeno"
        }"#
        .as_bytes(),
    )
    .expect("parse");
    assert_eq!(
        notifications,
        vec![TrackingNotification {
            package_id: "add-ppl-8728035".to_owned(),
            eid: "order-2026-000123-S1".to_owned(),
            carrier_id: "DR1536622512M".to_owned(),
            status_id: "1.2".to_owned(),
            status_text: "Zásilka byla doručena příjemci.".to_owned(),
        }]
    );
}

#[test]
fn array_is_parsed() {
    let notifications = parse_tracking_notifications(
        r#"[
            {
                "carrier_id": "TRACK-1",
                "status_id": 2,
                "status_text": "Zásilka v přepravě."
            },
            {
                "package_id": 42,
                "eid": 12345678,
                "carrier_id": "TRACK-2",
                "status_id_v2": 2.2,
                "name": "Zásilka byla převzata do přepravy."
            },
            {
                "carrier_id": "TRACK-3",
                "status_id": -1,
                "name": "Zásilka zatím nebyla předána dopravci."
            }
        ]"#
        .as_bytes(),
    )
    .expect("parse");
    assert_eq!(notifications.len(), 3);
    assert_eq!(notifications[0].carrier_id, "TRACK-1");
    assert_eq!(notifications[0].package_id, "");
    assert_eq!(notifications[0].eid, "");
    assert_eq!(notifications[0].status_id, "2");
    assert_eq!(notifications[0].status_text, "Zásilka v přepravě.");
    assert_eq!(notifications[1].package_id, "42");
    assert_eq!(notifications[1].eid, "12345678");
    assert_eq!(notifications[1].carrier_id, "TRACK-2");
    assert_eq!(notifications[1].status_id, "2.2");
    assert_eq!(
        notifications[1].status_text,
        "Zásilka byla převzata do přepravy."
    );
    assert_eq!(notifications[2].status_id, "-1");
    assert_eq!(
        notifications[2].status_text,
        "Zásilka zatím nebyla předána dopravci."
    );
}

#[test]
fn null_identifiers_are_tolerated() {
    let notifications = parse_tracking_notifications(
        r#"{
            "package_id": null,
            "eid": null,
            "carrier_id": "TRACK-1",
            "status_id": 2,
            "status_text": "Zásilka v přepravě."
        }"#
        .as_bytes(),
    )
    .expect("parse");
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].package_id, "");
    assert_eq!(notifications[0].eid, "");
    assert_eq!(notifications[0].carrier_id, "TRACK-1");
}

#[test]
fn empty_array_is_parsed() {
    let notifications = parse_tracking_notifications(b"[]").expect("parse");
    assert!(notifications.is_empty());
}

#[test]
fn malformed_input_is_rejected() {
    let cases = [
        ("empty body", ""),
        ("whitespace only", " \t\n "),
        ("null body", "null"),
        ("scalar body", "\"status\""),
        ("malformed json", "{\"carrier_id\":"),
        ("missing identity", r#"{"status_id":1,"name":"Delivered"}"#),
        (
            "empty identity",
            r#"{"package_id":"","eid":"","carrier_id":"","status_id":1,"name":"Delivered"}"#,
        ),
        (
            "null identity",
            r#"{"package_id":null,"eid":null,"carrier_id":null,"status_id":1,"name":"Delivered"}"#,
        ),
        (
            "missing status",
            r#"{"carrier_id":"TRACK-1","name":"Delivered"}"#,
        ),
        (
            "quoted status",
            r#"{"carrier_id":"TRACK-1","status_id":"1.2","name":"Delivered"}"#,
        ),
        (
            "too precise status",
            r#"{"carrier_id":"TRACK-1","status_id":1.2345,"name":"Delivered"}"#,
        ),
        (
            "null trailing status",
            r#"{"carrier_id":"TRACK-1","status_id":1,"status_id_v2":null,"name":"Delivered"}"#,
        ),
        ("missing text", r#"{"carrier_id":"TRACK-1","status_id":1}"#),
        (
            "empty text",
            r#"{"carrier_id":"TRACK-1","status_id":1,"name":"","status_text":""}"#,
        ),
        (
            "control character",
            r#"{"carrier_id":"TRACK-1","status_id":1,"name":"Delivered\nNow"}"#,
        ),
        (
            "boolean identity",
            r#"{"carrier_id":true,"status_id":1,"name":"Delivered"}"#,
        ),
        (
            "object identity",
            r#"{"carrier_id":{},"status_id":1,"name":"Delivered"}"#,
        ),
        ("array entry not object", "[1]"),
        ("null array entry", "[null]"),
    ];
    for (name, body) in cases {
        assert!(
            matches!(
                parse_tracking_notifications(body.as_bytes()),
                Err(Error::InvalidResponse)
            ),
            "{name}"
        );
    }
}

#[test]
fn overlong_identities_are_rejected() {
    let overlong = format!(
        r#"{{"carrier_id":"{}","status_id":1,"name":"Delivered"}}"#,
        "x".repeat(101)
    );
    assert!(matches!(
        parse_tracking_notifications(overlong.as_bytes()),
        Err(Error::InvalidResponse)
    ));
    let numeric = format!(
        r#"{{"carrier_id":{},"status_id":1,"name":"Delivered"}}"#,
        "1".repeat(101)
    );
    assert!(matches!(
        parse_tracking_notifications(numeric.as_bytes()),
        Err(Error::InvalidResponse)
    ));
}

#[test]
fn one_bad_entry_fails_the_whole_array() {
    let body = r#"[
        {"carrier_id":"TRACK-1","status_id":1,"name":"Delivered"},
        {"status_id":1,"name":"Delivered"}
    ]"#;
    assert!(matches!(
        parse_tracking_notifications(body.as_bytes()),
        Err(Error::InvalidResponse)
    ));
}

#[test]
fn more_than_one_thousand_entries_are_rejected() {
    let entries: Vec<String> = (0..=1_000)
        .map(|index| format!(r#"{{"carrier_id":"TRACK-{index}","status_id":2,"name":"Transit"}}"#))
        .collect();
    let body = format!("[{}]", entries.join(","));
    assert!(matches!(
        parse_tracking_notifications(body.as_bytes()),
        Err(Error::InvalidResponse)
    ));
}
