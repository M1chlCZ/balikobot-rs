use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use balikobot::{CarrierCode, Client, Config, CountryCode};

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

fn loopback_client(port: u16) -> Client {
    Client::new(Config::new("user", "key").with_base_url(format!("http://127.0.0.1:{port}")))
        .expect("client")
}

#[test]
fn who_am_i_returns_the_account_carriers() {
    let server = script(|_| {
        vec![(
            "200 OK",
            br#"{"status":200,"live_account":true,"carriers":[{"slug":"ppl","name":"PPL"}]}"#
                .to_vec(),
        )]
    });
    let client = loopback_client(server.port);
    let whoami = client.who_am_i().expect("whoami");
    assert_eq!(whoami.status, 200);
    assert_eq!(whoami.live_account, Some(true));
    assert_eq!(whoami.carriers.len(), 1);
    assert_eq!(whoami.carriers[0].slug, CarrierCode::PPL);
    assert_eq!(whoami.carriers[0].name, "PPL");
    let request = server.requests.recv().expect("request");
    assert!(request.starts_with("GET /info/whoami "));
}

#[test]
fn activated_services_with_inactive_parcel_yields_no_services() {
    let server = script(|_| {
        vec![(
            "200 OK",
            br#"{"status":200,"active_parcel":false,"service_types":[{"service_type":"1","name":"PPL"}]}"#
                .to_vec(),
        )]
    });
    let client = loopback_client(server.port);
    let activated = client
        .activated_services(&CarrierCode::PPL)
        .expect("activated");
    assert_eq!(activated.active_parcel, Some(false));
    assert!(activated.services.is_empty());
    let request = server.requests.recv().expect("request");
    assert!(request.starts_with("GET /ppl/activatedservices "));
}

#[test]
fn countries_accepts_the_array_shape() {
    let server = script(|_| {
        vec![(
            "200 OK",
            br#"{"status":200,"service_types":[{"service_type":"1","countries":[" de ","cz"]}]}"#
                .to_vec(),
        )]
    });
    let client = loopback_client(server.port);
    let countries = client.countries(&CarrierCode::PPL).expect("countries");
    assert_eq!(countries.len(), 1);
    assert_eq!(countries[0].service_type, "1");
    assert_eq!(
        countries[0].countries,
        vec![CountryCode::DE, CountryCode::CZ]
    );
}

#[test]
fn countries_accepts_the_sparse_object_shape_in_lexical_order() {
    let server = script(|_| {
        vec![(
            "200 OK",
            br#"{"status":200,"service_types":{"10":{"service_type":"B","countries":["DE"]},"2":{"service_type":"A","countries":["SK"]}}}"#
                .to_vec(),
        )]
    });
    let client = loopback_client(server.port);
    let countries = client.countries(&CarrierCode::PPL).expect("countries");
    assert_eq!(countries.len(), 2);
    assert_eq!(countries[0].service_type, "B");
    assert_eq!(countries[0].countries, vec![CountryCode::DE]);
    assert_eq!(countries[1].service_type, "A");
    assert_eq!(countries[1].countries, vec![CountryCode::SK]);
}

#[test]
fn cod_treats_http_501_as_unsupported() {
    let server = script(|_| vec![("501 Not Implemented", br#"{}"#.to_vec())]);
    let client = loopback_client(server.port);
    let result = client.cod(&CarrierCode::PPL).expect("cod");
    assert!(result.is_empty());
    let request = server.requests.recv().expect("request");
    assert!(request.starts_with("GET /ppl/cod4services "));
}

#[test]
fn carrier_capabilities_aggregates_eu_countries() {
    let server = script(|_| {
        vec![
            (
                "200 OK",
                br#"{"status":200,"live_account":true,"carriers":[{"slug":"ppl","name":"PPL"}]}"#
                    .to_vec(),
            ),
            (
                "200 OK",
                br#"{"status":200,"active_parcel":true,"service_types":[{"service_type":"1","name":"PPL Parcel","home_delivery":true}]}"#
                    .to_vec(),
            ),
            (
                "200 OK",
                br#"{"status":200,"service_types":[{"service_type":"1","countries":["DE","US","CZ"]}]}"#
                    .to_vec(),
            ),
        ]
    });
    let client = loopback_client(server.port);
    let carriers = client.carrier_capabilities(None).expect("capabilities");
    assert_eq!(carriers.len(), 1);
    assert_eq!(carriers[0].carrier_code, CarrierCode::PPL);
    assert_eq!(carriers[0].services.len(), 1);
    let service = &carriers[0].services[0];
    assert_eq!(service.code, "1");
    assert_eq!(service.name, "PPL Parcel");
    assert_eq!(service.home_delivery, Some(true));
    assert_eq!(service.countries.get(&CountryCode::DE), Some(&true));
    assert_eq!(service.countries.get(&CountryCode::CZ), Some(&true));
    assert_eq!(service.countries.get(&CountryCode::US), None);
}
