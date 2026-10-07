use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use balikobot::wire;
use balikobot::{Client, Config, Error};
use ureq::http;

struct Canned {
    port: u16,
    request: Receiver<String>,
}

fn serve(status_line: &str, headers: &[(&str, &str)], body: &str) -> Canned {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local address").port();
    let mut response = format!("HTTP/1.1 {status_line}\r\n");
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str(&format!("content-length: {}\r\n", body.len()));
    response.push_str("connection: close\r\n\r\n");
    response.push_str(body);
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
        stream.write_all(response.as_bytes()).expect("write");
        stream.flush().expect("flush");
    });
    Canned { port, request }
}

fn loopback_client(port: u16, config: Config) -> Client {
    Client::new(config.with_base_url(format!("http://127.0.0.1:{port}"))).expect("client")
}

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

#[test]
fn loopback_http_base_url_is_accepted() {
    let server = serve("200 OK", &[], "{}");
    let config =
        Config::new("user", "key").with_base_url(format!("http://127.0.0.1:{}", server.port));
    assert!(Client::new(config).is_ok());
}

#[test]
fn non_loopback_http_base_url_is_rejected() {
    let config = Config::new("user", "key").with_base_url("http://example.com");
    assert!(matches!(Client::new(config), Err(Error::Config(_))));
}

#[test]
fn invalid_configurations_are_rejected() {
    let cases = [
        Config::new("", "key"),
        Config::new("user", ""),
        Config::new("x".repeat(101), "key"),
        Config::new("user", "x".repeat(4097)),
        Config::new("user", "key").with_base_url("example.com"),
        Config::new("user", "key").with_base_url("https://example.com/api"),
        Config::new("user", "key").with_base_url("https://example.com?query=1"),
        Config::new("user", "key").with_base_url("https://example.com#fragment"),
        Config::new("user", "key").with_base_url("ftp://example.com"),
        Config::new("user", "key").with_max_response_bytes((1 << 30) + 1),
        Config::new("user", "key").with_label_hosts(["bad/host"]),
        Config::new("user", "key").with_label_hosts(["bad@host"]),
    ];
    for config in cases {
        assert!(matches!(Client::new(config), Err(Error::Config(_))));
    }
}

#[test]
fn redirects_are_not_followed() {
    let server = serve("302 Found", &[("location", "http://example.com/next")], "");
    let client = loopback_client(server.port, Config::new("user", "key"));
    let response = wire::request(&client, http::Method::GET, "/redirect", None).expect("response");
    assert_eq!(response.status, 302);
}

#[test]
fn request_carries_basic_auth_and_accept_headers() {
    let server = serve("200 OK", &[], "{}");
    let client = loopback_client(server.port, Config::new("user", "secret"));
    wire::request(&client, http::Method::GET, "/info/whoami", None).expect("response");
    let request = server.request.recv().expect("captured request");
    assert_eq!(
        header(&request, "authorization"),
        Some("Basic dXNlcjpzZWNyZXQ=")
    );
    assert_eq!(header(&request, "accept"), Some("application/json"));
}

#[test]
fn oversized_response_body_is_rejected() {
    let server = serve(
        "200 OK",
        &[("content-type", "application/json")],
        "0123456789",
    );
    let config = Config::new("user", "key").with_max_response_bytes(8);
    let client = loopback_client(server.port, config);
    let error = wire::request(&client, http::Method::GET, "/big", None).unwrap_err();
    assert!(matches!(error, Error::InvalidResponse));
}
