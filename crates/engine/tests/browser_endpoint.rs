mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use common::open_engine;
use tachyon_engine::EngineEvent;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn send(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn post(port: u16, origin: Option<&str>, client_header: bool, host: &str, body: &str) -> String {
    let mut request = format!("POST /v1/download HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n");
    if let Some(origin) = origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
    }
    if client_header {
        request.push_str("X-Tachyon-Client: test\r\n");
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    send(port, &request)
}

#[test]
fn browser_endpoint_accepts_extensions_and_rejects_pages() {
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let engine = open_engine(dir.path(), |s| {
        s.browser_integration = true;
        s.integration_port = port;
    });
    // Wait for the listener.
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let host = format!("127.0.0.1:{port}");
    let body = r#"{"url":"https://example.com/file.zip","referrer":"https://example.com/","filename":"file.zip"}"#;

    let ping = send(port, &format!("GET /v1/ping HTTP/1.1\r\nHost: {host}\r\nX-Tachyon-Client: t\r\n\r\n"));
    assert!(ping.starts_with("HTTP/1.1 200"), "{ping}");

    // A web page (even one that rebinds DNS to 127.0.0.1) is refused.
    assert!(post(port, Some("https://evil.example"), true, &host, body).starts_with("HTTP/1.1 403"));
    assert!(post(port, Some("chrome-extension://abc"), true, "evil.example:80", body).starts_with("HTTP/1.1 403"));
    // The client header is mandatory.
    assert!(post(port, Some("chrome-extension://abc"), false, &host, body).starts_with("HTTP/1.1 403"));
    // Only http(s) links are taken.
    let bad = r#"{"url":"file:///etc/passwd"}"#;
    assert!(post(port, Some("chrome-extension://abc"), true, &host, bad).starts_with("HTTP/1.1 422"));
    assert!(engine.drain_events().is_empty());

    let ok = post(port, Some("chrome-extension://abc"), true, &host, body);
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    let events = engine.drain_events();
    let [EngineEvent::Captured(request)] = events.as_slice() else {
        panic!("expected one capture, got {events:?}");
    };
    assert_eq!(request.request.url, "https://example.com/file.zip");
    assert_eq!(request.request.referer.as_deref(), Some("https://example.com/"));
    assert_eq!(request.file_name.as_deref(), Some("file.zip"));
    // Nothing is downloaded until the user confirms.
    assert!(engine.snapshot().is_empty());
}
