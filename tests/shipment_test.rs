use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use balikobot::{
    AddPackageRequest, CarrierCode, Client, Config, CountryCode, CurrencyCode, Error,
    resolve_branch_id,
};

struct Canned {
    port: u16,
    request: Receiver<String>,
}

fn serve<F>(status_line: &str, headers: &[(&str, &str)], body: F) -> Canned
where
    F: FnOnce(u16) -> Vec<u8> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local address").port();
    let mut response = format!("HTTP/1.1 {status_line}\r\n").into_bytes();
    for (name, value) in headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    let payload = body(port);
    response.extend_from_slice(format!("content-length: {}\r\n", payload.len()).as_bytes());
    response.extend_from_slice(b"connection: close\r\n\r\n");
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
        sender.send(String::from_utf8_lossy(&raw).into_owned()).ok();
        stream.write_all(&response).expect("write");
        stream.flush().expect("flush");
    });
    Canned { port, request }
}

fn json_serve<F>(status_line: &str, body: F) -> Canned
where
    F: FnOnce(u16) -> Vec<u8> + Send + 'static,
{
    serve(status_line, &[("content-type", "application/json")], body)
}

fn loopback_client(port: u16) -> Client {
    Client::new(Config::new("user", "key").with_base_url(format!("http://127.0.0.1:{port}")))
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

#[test]
fn add_package_returns_the_accepted_record() {
    let server = json_serve("200 OK", |port| {
        format!(
            r#"{{"status":200,"packages":[{{"eid":"018f00000000400080000000000000aa-S1","carrier_id":"DR1536622512M","package_id":"add-ppl-8728035","label_url":"http://127.0.0.1:{port}/label.pdf","status":200}}]}}"#
        )
        .into_bytes()
    });
    let client = loopback_client(server.port);
    let result = client
        .add_package(&CarrierCode::PPL, &valid_request())
        .expect("add");
    assert_eq!(result.package_id, "add-ppl-8728035");
    assert_eq!(result.carrier_id, "DR1536622512M");
    assert_eq!(
        result.label_url,
        format!("http://127.0.0.1:{}/label.pdf", server.port)
    );
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("POST /ppl/add "));
}

#[test]
fn add_package_duplicate_eid_returns_the_original_record() {
    let server = json_serve("200 OK", |port| {
        format!(
            r#"{{"status":200,"packages":[{{"eid":"018f00000000400080000000000000aa-S1","carrier_id":"ORIGINAL-CARRIER","package_id":"add-ppl-original","label_url":"http://127.0.0.1:{port}/label.pdf","status":208}}]}}"#
        )
        .into_bytes()
    });
    let client = loopback_client(server.port);
    let result = client
        .add_package(&CarrierCode::PPL, &valid_request())
        .expect("add");
    assert_eq!(result.package_id, "add-ppl-original");
    assert_eq!(result.carrier_id, "ORIGINAL-CARRIER");
}

#[test]
fn add_package_rejects_an_invalid_request() {
    let mut request = valid_request();
    request.eid = "short".to_owned();
    let error = loopback_client(9)
        .add_package(&CarrierCode::PPL, &request)
        .unwrap_err();
    assert!(matches!(error, Error::InvalidRequest));
}

#[test]
fn add_package_reports_a_rejected_request() {
    let server = json_serve("400 Bad Request", |_| br#"{"status":400}"#.to_vec());
    let client = loopback_client(server.port);
    let error = client
        .add_package(&CarrierCode::PPL, &valid_request())
        .unwrap_err();
    assert!(matches!(error, Error::Rejected));
}

#[test]
fn overview_returns_packages_and_accepts_integer_ids() {
    let server = json_serve("200 OK", |port| {
        format!(
            r#"{{"status":200,"packages":[
                {{"eid":"018f00000000400080000000000000aa-S1","carrier_id":"C1","package_id":"add-ppl-1","label_url":"http://127.0.0.1:{port}/a.pdf"}},
                {{"eid":"legacy-eid","carrier_id":"C2","package_id":42,"label_url":"http://127.0.0.1:{port}/b.pdf"}}
            ]}}"#
        )
        .into_bytes()
    });
    let client = loopback_client(server.port);
    let packages = client
        .overview(&CarrierCode::PPL, "018f00000000400080000000000000aa-S1")
        .expect("overview");
    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0].package_id, "add-ppl-1");
    assert_eq!(packages[1].package_id, "42");
    assert_eq!(packages[1].eid, "legacy-eid");
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("GET /ppl/overview "));
}

#[test]
fn overview_skips_invalid_unrelated_entries() {
    let server = json_serve("200 OK", |port| {
        format!(
            r#"{{"packages":[
                {{"eid":"legacy-eid","carrier_id":"C0","package_id":"p0","label_url":"https://evil.example/x.pdf"}},
                {{"eid":"018f00000000400080000000000000aa-S1","carrier_id":"C1","package_id":"p1","label_url":"http://127.0.0.1:{port}/a.pdf"}}
            ]}}"#
        )
        .into_bytes()
    });
    let client = loopback_client(server.port);
    let packages = client
        .overview(&CarrierCode::PPL, "018f00000000400080000000000000aa-S1")
        .expect("overview");
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].package_id, "p1");
}

#[test]
fn overview_fails_on_an_invalid_matching_entry() {
    let server = json_serve("200 OK", |_| {
        br#"{"status":200,"packages":[{"eid":"018f00000000400080000000000000aa-S1","carrier_id":"","label_url":""}]}"#
            .to_vec()
    });
    let client = loopback_client(server.port);
    let error = client
        .overview(&CarrierCode::PPL, "018f00000000400080000000000000aa-S1")
        .unwrap_err();
    assert!(matches!(error, Error::InvalidResponse));
}

#[test]
fn labels_returns_the_label_url() {
    let server = json_serve("200 OK", |port| {
        format!(r#"{{"status":200,"labels_url":"http://127.0.0.1:{port}/redownload.pdf"}}"#)
            .into_bytes()
    });
    let client = loopback_client(server.port);
    let label_url = client
        .labels(&CarrierCode::PPL, "add-ppl-1")
        .expect("labels");
    assert_eq!(
        label_url,
        format!("http://127.0.0.1:{}/redownload.pdf", server.port)
    );
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("POST /ppl/labels "));
}

#[test]
fn labels_rejects_a_foreign_label_url() {
    let server = json_serve("200 OK", |_| {
        br#"{"status":200,"labels_url":"https://evil.example/label.pdf"}"#.to_vec()
    });
    let client = loopback_client(server.port);
    let error = client.labels(&CarrierCode::PPL, "add-ppl-1").unwrap_err();
    assert!(matches!(error, Error::InvalidResponse));
}

#[test]
fn order_view_labels_returns_the_label_url() {
    let server = json_serve("200 OK", |port| {
        format!(
            r#"{{"status":200,"order_id":"order-ppl-1","package_ids":["add-ppl-other","add-ppl-1"],"labels_url":"http://127.0.0.1:{port}/ordered.pdf"}}"#
        )
        .into_bytes()
    });
    let client = loopback_client(server.port);
    let label_url = client
        .order_view_labels(&CarrierCode::PPL, "order-ppl-1", "add-ppl-1")
        .expect("order view");
    assert_eq!(
        label_url,
        format!("http://127.0.0.1:{}/ordered.pdf", server.port)
    );
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("GET /ppl/orderview/order-ppl-1 "));
}

#[test]
fn order_view_labels_requires_package_membership() {
    let server = json_serve("200 OK", |port| {
        format!(
            r#"{{"status":200,"order_id":"order-ppl-1","package_ids":["add-ppl-other"],"labels_url":"http://127.0.0.1:{port}/ordered.pdf"}}"#
        )
        .into_bytes()
    });
    let client = loopback_client(server.port);
    let error = client
        .order_view_labels(&CarrierCode::PPL, "order-ppl-1", "add-ppl-1")
        .unwrap_err();
    assert!(matches!(error, Error::InvalidResponse));
}

#[test]
fn download_label_returns_pdf_bytes() {
    let server = serve("200 OK", &[("content-type", "application/pdf")], |_| {
        b"%PDF-1.4 fake label contents".to_vec()
    });
    let client = loopback_client(server.port);
    let url = format!("http://127.0.0.1:{}/label.pdf", server.port);
    let (bytes, media_type) = client.download_label(&url).expect("download");
    assert_eq!(bytes, b"%PDF-1.4 fake label contents");
    assert_eq!(media_type, "application/pdf");
    let request = server.request.recv().expect("request");
    assert!(request.starts_with("GET /label.pdf "));
    assert!(!request.to_ascii_lowercase().contains("\r\nauthorization:"));
}

#[test]
fn download_label_rejects_a_wrong_media_type() {
    let server = serve("200 OK", &[("content-type", "text/plain")], |_| {
        b"%PDF-1.4".to_vec()
    });
    let client = loopback_client(server.port);
    let url = format!("http://127.0.0.1:{}/label.pdf", server.port);
    let error = client.download_label(&url).unwrap_err();
    assert!(matches!(error, Error::InvalidResponse));
}

#[test]
fn resolve_branch_id_follows_the_carrier_rules() {
    assert_eq!(
        resolve_branch_id(&CarrierCode::CP, "NP", "branch", "130 00"),
        "13000"
    );
    assert_eq!(
        resolve_branch_id(&CarrierCode::ULOZENKA, "CP_NP", "branch", "130 00"),
        "13000"
    );
    assert_eq!(
        resolve_branch_id(&CarrierCode::PPL, "1", "KM123", "0"),
        "123"
    );
}
