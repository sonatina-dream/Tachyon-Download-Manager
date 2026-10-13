use std::time::Duration;

use percent_encoding::percent_decode_str;
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Client, Response, StatusCode};

use crate::error::EngineError;
use crate::types::{ProbeInfo, RequestSpec, Settings};

/// Builds the shared HTTP client.
///
/// HTTP/1.1 only: download acceleration depends on each segment having its own TCP
/// connection, which HTTP/2 would multiplex onto one.
pub fn build_client(settings: &Settings) -> Result<Client, EngineError> {
    let mut builder = Client::builder()
        .http1_only()
        .user_agent(settings.user_agent.clone())
        .connect_timeout(Duration::from_secs(settings.connect_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::limited(10))
        .tcp_nodelay(true);
    let proxy = settings.proxy.trim();
    if !proxy.is_empty() {
        let proxy = reqwest::Proxy::all(proxy)
            .map_err(|e| EngineError::Config(format!("invalid proxy: {e}")))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| EngineError::Config(format!("cannot create HTTP client: {e}")))
}

/// Issues a GET for `bytes=start-` (or `bytes=start-end_inclusive`).
pub async fn get_range(
    client: &Client,
    spec: &RequestSpec,
    url: &str,
    start: u64,
    end_exclusive: Option<u64>,
) -> Result<Response, EngineError> {
    let range = match end_exclusive {
        Some(end) if end > start => format!("bytes={start}-{}", end - 1),
        _ => format!("bytes={start}-"),
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_str(&range).expect("ascii"));
    // Compressed transfer encodings would make byte ranges meaningless.
    headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
    if let Some(referer) = spec.referer.as_deref().and_then(|r| HeaderValue::from_str(r).ok()) {
        headers.insert(header::REFERER, referer);
    }
    if let Some(cookies) = spec.cookies.as_deref().and_then(|c| HeaderValue::from_str(c).ok()) {
        headers.insert(header::COOKIE, cookies);
    }
    if let Some(ua) = spec.user_agent.as_deref().and_then(|u| HeaderValue::from_str(u).ok()) {
        headers.insert(header::USER_AGENT, ua);
    }
    let mut request = client.get(url).headers(headers);
    if let Some(creds) = &spec.credentials {
        request = request.basic_auth(&creds.username, Some(&creds.password));
    }
    request.send().await.map_err(EngineError::from_reqwest)
}

/// Parses `Content-Range: bytes a-b/total` into `(a, b_inclusive, total)`.
pub fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (a, b) = range.trim().split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, total))
}

fn header_str(resp: &Response, name: header::HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Checks the status of a ranged response and returns the error to report, if any.
pub fn check_status(resp: &Response) -> Result<(), EngineError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    // Only the delta-seconds form; HTTP-dates are rare for 429/503.
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    Err(EngineError::Http {
        status: status.as_u16(),
        reason: status.canonical_reason().unwrap_or("error").to_string(),
        retry_after,
    })
}

/// What a probe learned plus the open response, whose body starts at `start`.
pub struct Probe {
    pub info: ProbeInfo,
    pub response: Response,
    /// First byte offset of `response`'s body.
    pub body_start: u64,
}

/// Contacts the server, asking for `[start, end)` (or everything from `start`), and
/// inspects the reply.
///
/// The response is returned open so the first connection can keep receiving from it,
/// the way IDM reuses the probing connection as the first segment.
pub async fn probe(
    client: &Client,
    spec: &RequestSpec,
    start: u64,
    end: Option<u64>,
) -> Result<Probe, EngineError> {
    let mut response = get_range(client, spec, &spec.url, start, end).await?;
    if response.status() == StatusCode::RANGE_NOT_SATISFIABLE && start == 0 {
        // `bytes=0-` is unsatisfiable only for an empty file; ask again without a range.
        let mut request = client.get(&spec.url).header(header::ACCEPT_ENCODING, "identity");
        if let Some(creds) = &spec.credentials {
            request = request.basic_auth(&creds.username, Some(&creds.password));
        }
        response = request.send().await.map_err(EngineError::from_reqwest)?;
    }
    check_status(&response)?;
    let final_url = response.url().to_string();
    let content_type = header_str(&response, header::CONTENT_TYPE);
    let etag = header_str(&response, header::ETAG);
    let last_modified = header_str(&response, header::LAST_MODIFIED);
    let accept_ranges = header_str(&response, header::ACCEPT_RANGES)
        .map(|v| v.eq_ignore_ascii_case("bytes"))
        .unwrap_or(false);

    let (total, resumable, body_start) = if response.status() == StatusCode::PARTIAL_CONTENT {
        let range = header_str(&response, header::CONTENT_RANGE)
            .and_then(|v| parse_content_range(&v))
            .ok_or_else(|| EngineError::Protocol("206 response without a valid Content-Range".into()))?;
        if range.0 != start {
            return Err(EngineError::Protocol(format!(
                "server returned bytes from {} instead of {start}",
                range.0
            )));
        }
        (range.2, range.2.is_some(), start)
    } else {
        // 200: the server sent the whole entity. It may still honour ranges later
        // if it advertises Accept-Ranges (some servers answer `bytes=0-` with 200).
        let total = response.content_length();
        (total, accept_ranges && total.is_some() && start == 0, 0)
    };

    let disposition = header_str(&response, header::CONTENT_DISPOSITION);
    let file_name = derive_file_name(disposition.as_deref(), &final_url, content_type.as_deref());

    Ok(Probe {
        info: ProbeInfo {
            final_url,
            total,
            resumable,
            file_name,
            content_type,
            etag,
            last_modified,
        },
        response,
        body_start,
    })
}

/// Picks a file name from Content-Disposition, then the URL path, then the MIME type.
pub fn derive_file_name(disposition: Option<&str>, url: &str, content_type: Option<&str>) -> String {
    let from_header = disposition.and_then(parse_content_disposition);
    let mut name = from_header
        .or_else(|| file_name_from_url(url))
        .map(|n| sanitize_file_name(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "download".to_string());
    if std::path::Path::new(&name).extension().is_none()
        && let Some(ext) = content_type.and_then(extension_for_mime)
    {
        name.push('.');
        name.push_str(ext);
    }
    name
}

/// Last non-empty path segment of a URL, percent-decoded.
pub fn file_name_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segment = parsed.path_segments()?.rev().find(|s| !s.is_empty())?;
    Some(percent_decode_str(segment).decode_utf8_lossy().into_owned())
}

/// Extracts the file name from a Content-Disposition header (RFC 6266 / RFC 5987).
pub fn parse_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    for part in split_params(value) {
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        if key == "filename*" {
            // charset'lang'percent-encoded
            let mut pieces = val.splitn(3, '\'');
            let charset = pieces.next().unwrap_or_default();
            let _lang = pieces.next();
            if let Some(encoded) = pieces.next() {
                let decoded = percent_decode_str(encoded.trim_matches('"'));
                let name = if charset.eq_ignore_ascii_case("utf-8") {
                    decoded.decode_utf8_lossy().into_owned()
                } else {
                    decoded.map(char::from).collect()
                };
                if !name.is_empty() {
                    return Some(name);
                }
            }
        } else if key == "filename" && plain.is_none() {
            let unquoted = if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
                val[1..val.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\")
            } else {
                val.to_string()
            };
            if !unquoted.is_empty() {
                plain = Some(unquoted);
            }
        }
    }
    plain
}

/// Splits on `;` outside quotes.
fn split_params(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in value.chars() {
        match ch {
            '\\' if quoted && !escaped => {
                escaped = true;
                current.push(ch);
                continue;
            }
            '"' if !escaped => quoted = !quoted,
            ';' if !quoted => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        escaped = false;
        current.push(ch);
    }
    parts.push(current);
    parts
}

/// Makes a server-supplied name safe to use as a single path component on macOS.
pub fn sanitize_file_name(name: &str) -> String {
    // Servers sometimes send full paths; keep only the last component.
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            ':' => '-',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let mut out: String = trimmed.to_string();
    // APFS allows 255 UTF-8 bytes per component; leave room for a " (n)" suffix and ".part".
    while out.len() > 200 {
        out.pop();
    }
    out
}

fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let essence = mime.split(';').next()?.trim().to_ascii_lowercase();
    Some(match essence.as_str() {
        "application/zip" => "zip",
        "application/pdf" => "pdf",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-7z-compressed" => "7z",
        "application/vnd.rar" | "application/x-rar-compressed" => "rar",
        "application/x-apple-diskimage" => "dmg",
        "application/x-tar" => "tar",
        "application/json" => "json",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/flac" => "flac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "text/plain" => "txt",
        "text/html" => "html",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(parse_content_range("bytes 0-99/1000"), Some((0, 99, Some(1000))));
        assert_eq!(parse_content_range("bytes 100-199/*"), Some((100, 199, None)));
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn parses_content_disposition() {
        assert_eq!(
            parse_content_disposition("attachment; filename=\"a b.zip\"").as_deref(),
            Some("a b.zip")
        );
        assert_eq!(
            parse_content_disposition("attachment; filename=plain.txt").as_deref(),
            Some("plain.txt")
        );
        assert_eq!(
            parse_content_disposition(
                "attachment; filename=\"fallback.txt\"; filename*=UTF-8''%E2%82%AC%20rates.txt"
            )
            .as_deref(),
            Some("€ rates.txt")
        );
        assert_eq!(
            parse_content_disposition("attachment; filename=\"semi;colon.zip\"").as_deref(),
            Some("semi;colon.zip")
        );
    }

    #[test]
    fn derives_names() {
        assert_eq!(
            derive_file_name(None, "https://x.org/dir/My%20File.tar.gz?y=1", None),
            "My File.tar.gz"
        );
        assert_eq!(derive_file_name(None, "https://x.org/", Some("application/pdf")), "download.pdf");
        assert_eq!(
            derive_file_name(Some("attachment; filename=\"../../etc/passwd\""), "https://x.org/a", None),
            "passwd"
        );
        assert_eq!(derive_file_name(None, "https://x.org/get", Some("video/mp4")), "get.mp4");
    }
}
