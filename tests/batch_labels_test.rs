use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use balikobot::{CarrierCode, Client, Config, Error};

struct Scripted {
    port: u16,
    requests: Receiver<String>,
}

struct Response {
    status_line: &'static str,
    content_type: &'static str,
    retry_after: Option<&'static str>,
    payload: Vec<u8>,
}

impl Response {
    fn json(status_line: &'static str, payload: Vec<u8>) -> Self {
        Self {
            status_line,
            content_type: "application/json",
            retry_after: None,
            payload,
        }
    }

    fn label(status_line: &'static str, content_type: &'static str, payload: &[u8]) -> Self {
        Self {
            status_line,
            content_type,
            retry_after: None,
            payload: payload.to_vec(),
        }
    }
}

fn script<F>(build: F) -> Scripted
where
    F: FnOnce(u16) -> Vec<Response>,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local address").port();
    let responses = build(port);
    let (sender, requests) = mpsc::channel();
    thread::spawn(move || {
        for entry in responses {
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
            let mut headers = format!("content-type: {}\r\n", entry.content_type);
            if let Some(retry_after) = entry.retry_after {
                headers.push_str(&format!("retry-after: {retry_after}\r\n"));
            }
            let mut response = format!(
                "HTTP/1.1 {}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n",
                entry.status_line,
                entry.payload.len()
            )
            .into_bytes();
            response.extend_from_slice(&entry.payload);
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

fn lookup(port: u16) -> Response {
    Response::json(
        "200 OK",
        format!(r#"{{"status":200,"labels_url":"http://127.0.0.1:{port}/labels.bin"}}"#)
            .into_bytes(),
    )
}

fn one_package() -> Vec<String> {
    vec!["add-ppl-1".to_owned()]
}

fn two_packages() -> Vec<String> {
    vec!["add-ppl-1".to_owned(), "add-ppl-2".to_owned()]
}

#[test]
fn batch_labels_single_package_returns_pdf() {
    let pdf = b"%PDF-1.4 single label";
    let server = script(|port| {
        vec![
            lookup(port),
            Response::label("200 OK", "application/pdf", pdf),
        ]
    });
    let client = loopback_client(server.port);
    let (body, media_type) = client
        .download_batch_labels(&CarrierCode::PPL, &one_package())
        .expect("batch labels");
    assert_eq!(body, pdf);
    assert_eq!(media_type, "application/pdf");
    let request = server.requests.recv().expect("lookup request");
    assert!(request.starts_with("POST /ppl/labels "));
    assert!(request.contains(r#"{"package_ids":["add-ppl-1"]}"#));
    let download = server.requests.recv().expect("download request");
    assert!(download.starts_with("GET /labels.bin "));
    assert!(!download.to_ascii_lowercase().contains("\r\nauthorization:"));
}

#[test]
fn batch_labels_multiple_packages_return_zip() {
    let archive = b"PK\x03\x04zip archive bytes";
    let server = script(|port| {
        vec![
            lookup(port),
            Response::label("200 OK", "application/zip", archive),
        ]
    });
    let client = loopback_client(server.port);
    let (body, media_type) = client
        .download_batch_labels(&CarrierCode::PPL, &two_packages())
        .expect("batch labels");
    assert_eq!(body, archive);
    assert_eq!(media_type, "application/zip");
    let lookup = server.requests.recv().expect("lookup request");
    assert!(lookup.contains(r#"{"package_ids":["add-ppl-1","add-ppl-2"]}"#));
}

#[test]
fn batch_labels_single_package_accepts_zpl() {
    let zpl = b"^XA^FO50,50^FDLABEL^FS^XZ";
    let server = script(|port| {
        vec![
            lookup(port),
            Response::label("200 OK", "application/zpl", zpl),
        ]
    });
    let client = loopback_client(server.port);
    let (body, media_type) = client
        .download_batch_labels(&CarrierCode::PPL, &one_package())
        .expect("batch labels");
    assert_eq!(body, zpl);
    assert_eq!(media_type, "application/zpl");
}

#[test]
fn batch_labels_validate_before_any_request() {
    let client = loopback_client(9);
    let too_many: Vec<String> = (0..=1_000)
        .map(|index| format!("add-ppl-{index}"))
        .collect();
    let cases = [
        ("missing ids", Vec::new()),
        ("empty id", vec![String::new()]),
        ("invalid id", vec!["bad\nid".to_owned()]),
        ("too many ids", too_many),
    ];
    for (name, package_ids) in cases {
        let error = client
            .download_batch_labels(&CarrierCode::PPL, &package_ids)
            .unwrap_err();
        assert!(matches!(error, Error::InvalidRequest), "{name}");
    }
}

#[test]
fn batch_label_lookup_classifies_responses() {
    let cases = vec![
        (
            "http rate limit is transient with the Retry-After hint",
            Response {
                status_line: "429 Too Many Requests",
                content_type: "application/json",
                retry_after: Some("30"),
                payload: Vec::new(),
            },
            Error::Unavailable {
                retry_after: Some(Duration::from_secs(30)),
            },
        ),
        (
            "http server error is transient",
            Response::json("500 Internal Server Error", Vec::new()),
            Error::Unavailable { retry_after: None },
        ),
        (
            "http validation is rejected",
            Response::json("400 Bad Request", Vec::new()),
            Error::Rejected,
        ),
        (
            "body 503 is transient",
            Response::json("200 OK", br#"{"status":503}"#.to_vec()),
            Error::Unavailable { retry_after: None },
        ),
        (
            "body 404 is rejected",
            Response::json("200 OK", br#"{"status":404}"#.to_vec()),
            Error::Rejected,
        ),
        (
            "missing status is a protocol error",
            Response::json(
                "200 OK",
                br#"{"labels_url":"http://127.0.0.1:1/labels.pdf"}"#.to_vec(),
            ),
            Error::InvalidResponse,
        ),
        (
            "non JSON body is a protocol error",
            Response::label("200 OK", "text/html", b"<html></html>"),
            Error::InvalidResponse,
        ),
        (
            "foreign label host is a protocol error",
            Response::json(
                "200 OK",
                br#"{"status":200,"labels_url":"https://evil.example/labels.pdf"}"#.to_vec(),
            ),
            Error::InvalidResponse,
        ),
    ];
    for (name, response, want) in cases {
        let server = script(move |_| vec![response]);
        let client = loopback_client(server.port);
        let error = client
            .download_batch_labels(&CarrierCode::PPL, &one_package())
            .unwrap_err();
        assert_eq!(error, want, "{name}");
    }
}

#[test]
fn batch_label_download_validates_content() {
    struct Case {
        name: &'static str,
        package_ids: Vec<String>,
        status_line: &'static str,
        content_type: &'static str,
        body: &'static [u8],
        want: Option<Error>,
    }
    let cases = vec![
        Case {
            name: "single PDF",
            package_ids: one_package(),
            status_line: "200 OK",
            content_type: "application/pdf",
            body: b"%PDF-1.4 label",
            want: None,
        },
        Case {
            name: "single ZPL",
            package_ids: one_package(),
            status_line: "200 OK",
            content_type: "application/zpl",
            body: b"^XA^XZ",
            want: None,
        },
        Case {
            name: "several packages return ZIP",
            package_ids: two_packages(),
            status_line: "200 OK",
            content_type: "application/zip",
            body: b"PK\x03\x04archive",
            want: None,
        },
        Case {
            name: "single package rejects ZIP",
            package_ids: one_package(),
            status_line: "200 OK",
            content_type: "application/zip",
            body: b"PK\x03\x04archive",
            want: Some(Error::InvalidResponse),
        },
        Case {
            name: "several packages reject PDF",
            package_ids: two_packages(),
            status_line: "200 OK",
            content_type: "application/pdf",
            body: b"%PDF-1.4 label",
            want: Some(Error::InvalidResponse),
        },
        Case {
            name: "several packages reject ZPL",
            package_ids: two_packages(),
            status_line: "200 OK",
            content_type: "application/zpl",
            body: b"^XA^XZ",
            want: Some(Error::InvalidResponse),
        },
        Case {
            name: "broken PDF",
            package_ids: one_package(),
            status_line: "200 OK",
            content_type: "application/pdf",
            body: b"not a pdf",
            want: Some(Error::InvalidResponse),
        },
        Case {
            name: "broken ZIP",
            package_ids: two_packages(),
            status_line: "200 OK",
            content_type: "application/zip",
            body: b"%PDF-1.4 label",
            want: Some(Error::InvalidResponse),
        },
        Case {
            name: "empty body",
            package_ids: one_package(),
            status_line: "200 OK",
            content_type: "application/pdf",
            body: b"",
            want: Some(Error::InvalidResponse),
        },
        Case {
            name: "unexpected content type",
            package_ids: one_package(),
            status_line: "200 OK",
            content_type: "text/html",
            body: b"<html></html>",
            want: Some(Error::InvalidResponse),
        },
        Case {
            name: "http 404 is rejected",
            package_ids: one_package(),
            status_line: "404 Not Found",
            content_type: "application/json",
            body: b"{}",
            want: Some(Error::Rejected),
        },
        Case {
            name: "http 500 is transient",
            package_ids: one_package(),
            status_line: "500 Internal Server Error",
            content_type: "application/json",
            body: b"{}",
            want: Some(Error::Unavailable { retry_after: None }),
        },
    ];
    for case in cases {
        let server = script(|port| {
            vec![
                lookup(port),
                Response::label(case.status_line, case.content_type, case.body),
            ]
        });
        let client = loopback_client(server.port);
        let result = client.download_batch_labels(&CarrierCode::PPL, &case.package_ids);
        match case.want {
            Some(want) => {
                let error = result.unwrap_err();
                assert_eq!(error, want, "{}", case.name);
            }
            None => {
                let (body, media_type) = result.expect(case.name);
                assert_eq!(media_type, case.content_type, "{}", case.name);
                assert_eq!(body, case.body, "{}", case.name);
            }
        }
    }
}

#[test]
fn batch_label_download_rejects_a_body_past_the_limit() {
    let mut oversized = b"%PDF-".to_vec();
    oversized.resize((4 << 20) + 1, b'a');
    let server = script(|port| {
        vec![
            lookup(port),
            Response::label("200 OK", "application/pdf", &oversized),
        ]
    });
    let client = loopback_client(server.port);
    let error = client
        .download_batch_labels(&CarrierCode::PPL, &one_package())
        .unwrap_err();
    assert_eq!(error, Error::InvalidResponse);
}
