use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use balikobot::{CarrierCode, Client, Config, CountryCode, Error};

struct Canned {
    port: u16,
    request: Receiver<String>,
}

fn serve(status_line: &str, headers: &[(&str, &str)], body: &[u8]) -> Canned {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local address").port();
    let mut response = format!("HTTP/1.1 {status_line}\r\n").into_bytes();
    for (name, value) in headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    response.extend_from_slice(format!("content-length: {}\r\n", body.len()).as_bytes());
    response.extend_from_slice(b"connection: close\r\n\r\n");
    response.extend_from_slice(body);
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
        sender.send(String::from_utf8_lossy(&raw).into_owned()).ok();
        stream.write_all(&response).expect("write");
        stream.flush().expect("flush");
    });
    Canned { port, request }
}

fn json_serve(status_line: &str, body: &[u8]) -> Canned {
    serve(status_line, &[("content-type", "application/json")], body)
}

fn loopback_client(port: u16) -> Client {
    Client::new(Config::new("user", "key").with_base_url(format!("http://127.0.0.1:{port}")))
        .expect("client")
}

#[test]
fn array_payload_returns_branches() {
    let body = br#"{
        "status": 200,
        "branches": [{
            "branch_id": "123",
            "type": "branch",
            "name": "PPL Pickup Praha",
            "street": "Psi 1",
            "city": "Praha",
            "zip": "11000",
            "country": "CZ",
            "lat": 50.08,
            "lng": 14.43
        }]
    }"#;
    let server = json_serve("200 OK", body);
    let client = loopback_client(server.port);
    let branches = client
        .branches(&CarrierCode::PPL, "1", &CountryCode::CZ)
        .expect("branches");
    assert_eq!(branches.len(), 1);
    let branch = &branches[0];
    assert_eq!(branch.id, "123");
    assert_eq!(branch.r#type, "branch");
    assert_eq!(branch.name, "PPL Pickup Praha");
    assert_eq!(branch.country, CountryCode::CZ);
    assert_eq!(branch.latitude, Some(50.08));
    assert_eq!(branch.longitude, Some(14.43));
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("GET /ppl/branches/service/1/country/CZ "));
}

#[test]
fn object_payload_returns_branches_in_numeric_key_order() {
    let body = br#"{
        "status": "200",
        "branches": {
            "1a": {"id": "non-numeric", "name": "Pobocka C", "zip": "13000"},
            "10": {"id": "ten", "name": "Pobocka B", "zip": "12000"},
            "2": {"id": "two", "name": "Pobocka A", "zip": "11000"}
        }
    }"#;
    let server = json_serve("200 OK", body);
    let client = loopback_client(server.port);
    let branches = client
        .branches(&CarrierCode::ZASILKOVNA, "VMCZ", &CountryCode::CZ)
        .expect("branches");
    let ids: Vec<_> = branches.iter().map(|branch| branch.id.as_str()).collect();
    assert_eq!(ids, ["two", "ten", "non-numeric"]);
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("GET /zasilkovna/branches/country/CZ "));
}

#[test]
fn nameless_branch_falls_back_to_zip_and_country_filter_applies() {
    let body = br#"{
        "status": 200,
        "branches": [
            {"id": "1", "zip": "11000", "country": "CZ"},
            {"id": "2", "name": "Balikovna Bratislava", "zip": "81101", "country": "SK"},
            {"id": "3", "name": "Balikovna bez zeme", "zip": "12000"}
        ]
    }"#;
    let server = json_serve("200 OK", body);
    let client = loopback_client(server.port);
    let branches = client
        .branches(&CarrierCode::CP, "NP", &CountryCode::CZ)
        .expect("branches");
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0].id, "1");
    assert_eq!(branches[0].name, "11000");
    assert_eq!(branches[1].id, "3");
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("GET /cp/branches/service/NP/country/CZ "));
}

#[test]
fn server_error_returns_unavailable() {
    let server = serve("500 Internal Server Error", &[], b"");
    let client = loopback_client(server.port);
    let error = client
        .branches(&CarrierCode::PPL, "1", &CountryCode::CZ)
        .unwrap_err();
    assert!(matches!(error, Error::Unavailable { retry_after: None }));
}

#[test]
fn malformed_status_returns_invalid_response() {
    let server = json_serve("200 OK", br#"{"status":"OK","branches":[]}"#);
    let client = loopback_client(server.port);
    let error = client
        .branches(&CarrierCode::PPL, "1", &CountryCode::CZ)
        .unwrap_err();
    assert!(matches!(error, Error::InvalidResponse));
}
