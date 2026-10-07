use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use balikobot::{CarrierCode, Client, Config, Error, PickupRequest, PickupResult};

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

fn valid_request() -> PickupRequest {
    PickupRequest {
        date: "2026-09-14".to_owned(),
        weight_kg: 12.5,
        package_count: 3,
        note: "Zazvoňte u skladu.".to_owned(),
    }
}

#[test]
fn order_pickup_dpd_returns_a_confirmed_booking() {
    let server = json_serve("200 OK", br#"{"status":"200"}"#.to_vec());
    let client = loopback_client(server.port);
    let result = client
        .order_pickup(&CarrierCode::DPD, &valid_request())
        .expect("pickup");
    assert_eq!(
        result,
        PickupResult {
            provider_id: String::new(),
            confirmed: true,
        }
    );
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("POST /dpd/orderpickup "));
    assert!(request.contains(r#""date":"2026-09-14""#));
    assert!(request.contains(r#""weight":12.5"#));
    assert!(request.contains(r#""package_count":3"#));
    assert!(request.contains(r#""message":"Zazvoňte u skladu.""#));
}

#[test]
fn order_pickup_ppl_preserves_the_provider_confirmation() {
    let server = json_serve(
        "200 OK",
        br#"{"status":200,"pickup_order_id":"BB12345600152024001","confirmed":true}"#.to_vec(),
    );
    let client = loopback_client(server.port);
    let result = client
        .order_pickup(&CarrierCode::PPL, &valid_request())
        .expect("pickup");
    assert_eq!(
        result,
        PickupResult {
            provider_id: "BB12345600152024001".to_owned(),
            confirmed: true,
        }
    );
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("POST /ppl/orderpickup "));
    assert!(request.contains(r#""note":"Zazvoňte u skladu.""#));
    assert!(!request.contains("\"weight\""));
}

#[test]
fn order_pickup_rejects_an_unsupported_carrier_before_any_request() {
    let client = loopback_client(9);
    let error = client
        .order_pickup(&CarrierCode::CP, &valid_request())
        .unwrap_err();
    assert!(matches!(error, Error::Rejected));
}

#[test]
fn order_pickup_ppl_requires_the_confirmation() {
    let server = json_serve(
        "200 OK",
        br#"{"status":200,"pickup_order_id":"BB12345600152024001"}"#.to_vec(),
    );
    let client = loopback_client(server.port);
    let error = client
        .order_pickup(&CarrierCode::PPL, &valid_request())
        .unwrap_err();
    assert!(matches!(error, Error::Ambiguous));
}
