use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use balikobot::{CarrierCode, Client, Config, Error};

struct Canned {
    port: u16,
    request: Receiver<String>,
}

fn json_serve(status_line: &str, payload: Vec<u8>) -> Canned {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local address").port();
    let mut response = format!(
        "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    )
    .into_bytes();
    response.extend_from_slice(&payload);
    let (sender, request) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut raw = Vec::new();
        let mut buffer = [0u8; 1024];
        while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).expect("read");
            if count == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..count]);
        }
        let header_end = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .unwrap_or(raw.len());
        let content_length = raw[..header_end]
            .split(|byte| *byte == b'\n')
            .find_map(|line| {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let mut parts = line.splitn(2, |byte| *byte == b':');
                let name = parts.next()?;
                let value = parts.next()?;
                name.eq_ignore_ascii_case(b"content-length")
                    .then(|| std::str::from_utf8(value).ok()?.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while raw.len() < header_end + content_length {
            let count = stream.read(&mut buffer).expect("read");
            if count == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..count]);
        }
        sender.send(String::from_utf8_lossy(&raw).into_owned()).ok();
        stream.write_all(&response).expect("write");
        stream.flush().expect("flush");
    });
    Canned { port, request }
}

fn loopback_client(port: u16) -> Client {
    Client::new(Config::new("user", "key").with_base_url(format!("http://127.0.0.1:{port}")))
        .expect("client")
}

#[test]
fn track_status_returns_the_detailed_status() {
    let server = json_serve(
        "200 OK",
        r#"{"status":200,"packages":[{"carrier_id":"TRACK-1","status_id":1,"status_id_v2":1.2,"name":"Zásilka byla doručena příjemci."}]}"#
            .as_bytes()
            .to_vec(),
    );
    let client = loopback_client(server.port);
    let result = client
        .track_status(&CarrierCode::PPL, "TRACK-1")
        .expect("track status");
    assert_eq!(result.status_id, "1.2");
    assert_eq!(result.status_text, "Zásilka byla doručena příjemci.");
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("POST /ppl/trackstatus "));
    assert!(request.contains(r#"{"carrier_ids":["TRACK-1"]}"#));
}

#[test]
fn track_status_missing_resource_is_not_found() {
    let server = json_serve("404 Not Found", br#"{}"#.to_vec());
    let client = loopback_client(server.port);
    let error = client
        .track_status(&CarrierCode::PPL, "TRACK-1")
        .unwrap_err();
    assert!(matches!(error, Error::NotFound));
}

#[test]
fn track_status_carrier_mismatch_is_invalid_response() {
    let server = json_serve(
        "200 OK",
        br#"{"status":200,"packages":[{"carrier_id":"OTHER","status_id":1,"name":"Delivered"}]}"#
            .to_vec(),
    );
    let client = loopback_client(server.port);
    let error = client
        .track_status(&CarrierCode::PPL, "TRACK-1")
        .unwrap_err();
    assert!(matches!(error, Error::InvalidResponse));
}

#[test]
fn order_batch_returns_the_order_id() {
    let server = json_serve(
        "200 OK",
        br#"{"order_id":"order-ppl-2274514","status":200,"package_ids":["add-ppl-1"]}"#.to_vec(),
    );
    let client = loopback_client(server.port);
    let result = client
        .order_batch(&CarrierCode::PPL, "add-ppl-1")
        .expect("order");
    assert_eq!(result.order_id, "order-ppl-2274514");
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("POST /ppl/order "));
    assert!(request.contains(r#"{"package_ids":["add-ppl-1"]}"#));
}

#[test]
fn order_batch_body_rejection_is_rejected() {
    let server = json_serve("200 OK", br#"{"status":400}"#.to_vec());
    let client = loopback_client(server.port);
    let error = client
        .order_batch(&CarrierCode::PPL, "add-ppl-1")
        .unwrap_err();
    assert!(matches!(error, Error::Rejected));
}

#[test]
fn drop_package_treats_a_body_404_as_success() {
    let server = json_serve("200 OK", br#"{"status":404}"#.to_vec());
    let client = loopback_client(server.port);
    client
        .drop_package(&CarrierCode::PPL, "add-ppl-1")
        .expect("drop");
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("POST /ppl/drop "));
    assert!(request.contains(r#"{"package_ids":["add-ppl-1"]}"#));
}

#[test]
fn drop_package_rejects_a_body_405() {
    let server = json_serve("200 OK", br#"{"status":405}"#.to_vec());
    let client = loopback_client(server.port);
    let error = client
        .drop_package(&CarrierCode::PPL, "add-ppl-1")
        .unwrap_err();
    assert!(matches!(error, Error::Rejected));
}
