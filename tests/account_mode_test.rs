use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use balikobot::{
    AddPackageRequest, CarrierCode, Client, Config, CountryCode, CurrencyCode, Error, PickupRequest,
};

struct Scripted {
    port: u16,
    requests: Receiver<String>,
}

fn script<F>(build: F) -> Scripted
where
    F: FnOnce(u16) -> Vec<(&'static str, Vec<u8>)>,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local address").port();
    let responses = build(port);
    let (sender, requests) = mpsc::channel();
    thread::spawn(move || {
        for (status_line, payload) in responses {
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
            let mut response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                payload.len()
            )
            .into_bytes();
            response.extend_from_slice(&payload);
            stream.write_all(&response).expect("write");
            stream.flush().expect("flush");
        }
    });
    Scripted { port, requests }
}

fn live_client(port: u16) -> Client {
    Client::new(
        Config::new("user", "key")
            .with_base_url(format!("http://127.0.0.1:{port}"))
            .with_live_account(true),
    )
    .expect("client")
}

fn valid_request() -> AddPackageRequest {
    AddPackageRequest {
        eid: "018f00000000400080000000000000aa-S1".to_owned(),
        service_type: "1".to_owned(),
        rec_name: "Testovací Příjemce".to_owned(),
        rec_firm: String::new(),
        rec_street: "Psí 1".to_owned(),
        rec_city: "Praha".to_owned(),
        rec_zip: "11000".to_owned(),
        rec_country: CountryCode::CZ,
        rec_phone: "+420777000000".to_owned(),
        rec_email: "recipient@example.test".to_owned(),
        branch_id: String::new(),
        weight: 1.25,
        length: 30.0,
        width: 20.0,
        height: 10.0,
        price: 1990.0,
        cod_price: 0.0,
        cod_currency: CurrencyCode::CZK,
        vs: None,
    }
}

fn valid_pickup() -> PickupRequest {
    PickupRequest {
        date: "2026-09-14".to_owned(),
        weight_kg: 12.5,
        package_count: 3,
        note: String::new(),
    }
}

#[test]
fn a_false_flag_blocks_add_package_before_the_write() {
    let server = script(|_| {
        vec![(
            "200 OK",
            br#"{"status":200,"live_account":false,"carriers":[]}"#.to_vec(),
        )]
    });
    let client = live_client(server.port);
    let error = client
        .add_package(&CarrierCode::PPL, &valid_request())
        .unwrap_err();
    assert!(matches!(error, Error::Unavailable { .. }));
    let request = server.requests.recv().expect("request");
    assert!(request.starts_with("GET /info/whoami "));
    assert!(
        server
            .requests
            .recv_timeout(Duration::from_millis(250))
            .is_err()
    );
}

#[test]
fn a_false_flag_blocks_order_batch_before_the_write() {
    let server = script(|_| {
        vec![(
            "200 OK",
            br#"{"status":200,"live_account":false,"carriers":[]}"#.to_vec(),
        )]
    });
    let client = live_client(server.port);
    let error = client
        .order_batch(&CarrierCode::PPL, "add-ppl-1")
        .unwrap_err();
    assert!(matches!(error, Error::Unavailable { .. }));
    let request = server.requests.recv().expect("request");
    assert!(request.starts_with("GET /info/whoami "));
    assert!(
        server
            .requests
            .recv_timeout(Duration::from_millis(250))
            .is_err()
    );
}

#[test]
fn a_matching_flag_caches_the_verification() {
    let server = script(|port| {
        let accepted = format!(
            r#"{{"status":200,"packages":[{{"eid":"018f00000000400080000000000000aa-S1","carrier_id":"C1","package_id":"p1","label_url":"http://127.0.0.1:{port}/label.pdf","status":200}}]}}"#
        )
        .into_bytes();
        vec![
            (
                "200 OK",
                br#"{"status":200,"live_account":true,"carriers":[]}"#.to_vec(),
            ),
            ("200 OK", accepted.clone()),
            ("200 OK", accepted),
        ]
    });
    let client = live_client(server.port);
    client
        .add_package(&CarrierCode::PPL, &valid_request())
        .expect("first add");
    client
        .add_package(&CarrierCode::PPL, &valid_request())
        .expect("second add");
    let first = server.requests.recv().expect("request");
    let second = server.requests.recv().expect("request");
    let third = server.requests.recv().expect("request");
    assert!(first.starts_with("GET /info/whoami "));
    assert!(second.starts_with("POST /ppl/add "));
    assert!(third.starts_with("POST /ppl/add "));
}

#[test]
fn order_pickup_rejects_an_unverified_account() {
    let server = script(|_| {
        vec![(
            "200 OK",
            br#"{"status":200,"live_account":false,"carriers":[]}"#.to_vec(),
        )]
    });
    let client = live_client(server.port);
    let error = client
        .order_pickup(&CarrierCode::DPD, &valid_pickup())
        .unwrap_err();
    assert!(matches!(error, Error::Rejected));
    let request = server.requests.recv().expect("request");
    assert!(request.starts_with("GET /info/whoami "));
    assert!(
        server
            .requests
            .recv_timeout(Duration::from_millis(250))
            .is_err()
    );
}
