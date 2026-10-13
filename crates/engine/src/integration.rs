//! Local endpoint the browser extension hands downloads to.
//!
//! Listens on 127.0.0.1 only. A request is accepted only if:
//! - its Host header names the loopback address, which defeats DNS rebinding;
//! - it has no Origin, or an extension Origin, so web pages are refused;
//! - it carries the `X-Tachyon-Client` header. Pages can't send that without a CORS
//!   preflight, which this server never approves.
//!
//! Captured downloads are not started silently: by default the app asks the user first.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::types::{AddRequest, RequestSpec};

const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 256 * 1024;
const EXTENSION_ORIGINS: [&str; 3] = ["chrome-extension://", "moz-extension://", "safari-web-extension://"];

pub(crate) type CaptureSink = Arc<dyn Fn(AddRequest) + Send + Sync>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureBody {
    url: String,
    #[serde(default)]
    referrer: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    cookies: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

pub(crate) async fn serve(port: u16, cancel: CancellationToken, sink: CaptureSink) {
    let listener = match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => listener,
        Err(e) => {
            log::error!("browser integration: cannot listen on 127.0.0.1:{port}: {e}");
            return;
        }
    };
    log::info!("browser integration listening on 127.0.0.1:{port}");
    loop {
        let stream = tokio::select! {
            _ = cancel.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            },
        };
        let sink = sink.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(10), handle(stream, port, sink)).await;
        });
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
    };
    let head = std::str::from_utf8(&buf[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return None;
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(length);
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn is_allowed(request: &Request, port: u16) -> bool {
    let host_ok = request.header("host").is_some_and(|h| {
        h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
    });
    let origin_ok = match request.header("origin") {
        None => true,
        Some(origin) => EXTENSION_ORIGINS.iter().any(|p| origin.starts_with(p)),
    };
    host_ok && origin_ok && request.header("x-tachyon-client").is_some()
}

async fn handle(mut stream: TcpStream, port: u16, sink: CaptureSink) {
    let Some(request) = read_request(&mut stream).await else {
        respond(&mut stream, "400 Bad Request", r#"{"error":"bad request"}"#).await;
        return;
    };
    if !is_allowed(&request, port) {
        respond(&mut stream, "403 Forbidden", r#"{"error":"forbidden"}"#).await;
        return;
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1/ping") => {
            let body = format!(r#"{{"app":"Tachyon","version":"{}"}}"#, env!("CARGO_PKG_VERSION"));
            respond(&mut stream, "200 OK", &body).await;
        }
        ("POST", "/v1/download") => {
            let Ok(capture) = serde_json::from_slice::<CaptureBody>(&request.body) else {
                respond(&mut stream, "400 Bad Request", r#"{"error":"invalid json"}"#).await;
                return;
            };
            let valid = url::Url::parse(&capture.url)
                .map(|u| matches!(u.scheme(), "http" | "https"))
                .unwrap_or(false);
            if !valid {
                respond(&mut stream, "422 Unprocessable Entity", r#"{"error":"unsupported url"}"#).await;
                return;
            }
            sink(AddRequest {
                request: RequestSpec {
                    url: capture.url,
                    referer: capture.referrer.filter(|r| !r.is_empty()),
                    cookies: capture.cookies.filter(|c| !c.is_empty()),
                    user_agent: capture.user_agent.filter(|u| !u.is_empty()),
                    credentials: None,
                },
                file_name: capture.filename.filter(|f| !f.is_empty()),
                description: capture.description.unwrap_or_default(),
                start: true,
                ..Default::default()
            });
            respond(&mut stream, "200 OK", r#"{"ok":true}"#).await;
        }
        _ => respond(&mut stream, "404 Not Found", r#"{"error":"not found"}"#).await,
    }
}
