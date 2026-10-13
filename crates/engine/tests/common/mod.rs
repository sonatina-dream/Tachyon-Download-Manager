//! A small HTTP/1.1 file server for exercising the engine.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tachyon_engine::{DownloadId, DownloadSnapshot, DownloadStatus, Engine, Settings};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone)]
pub struct ServerConfig {
    pub size: usize,
    pub ranges: bool,
    pub content_length: bool,
    /// Per-connection throughput in bytes per second (0 = unthrottled).
    pub throttle: usize,
    /// Drop this many responses partway through the body.
    pub fail_first: usize,
    /// Answer every request with this status instead of the file.
    pub status_override: Option<u16>,
    pub etag: &'static str,
    /// Answer 429 (Retry-After: 1) while more than this many requests are in flight.
    pub max_concurrent: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            size: 4 << 20,
            ranges: true,
            content_length: true,
            throttle: 0,
            fail_first: 0,
            status_override: None,
            etag: "\"v1\"",
            max_concurrent: usize::MAX,
        }
    }
}

pub struct TestServer {
    pub url: String,
    pub body: Arc<Vec<u8>>,
    pub stats: Arc<Stats>,
    config: Arc<Mutex<ServerConfig>>,
}

#[derive(Default)]
pub struct Stats {
    pub requests: AtomicUsize,
    pub active: AtomicUsize,
    pub max_active: AtomicUsize,
    pub bytes_sent: AtomicUsize,
    pub range_starts: Mutex<Vec<u64>>,
    pub throttled: AtomicUsize,
    in_flight: AtomicUsize,
    failures_done: AtomicUsize,
}

impl TestServer {
    pub async fn start(config: ServerConfig) -> Self {
        let body: Arc<Vec<u8>> = Arc::new(
            (0..config.size)
                .map(|i| (i as u32).wrapping_mul(2_654_435_761).rotate_left(7) as u8)
                .collect(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/files/test%20file.bin", listener.local_addr().unwrap());
        let stats = Arc::new(Stats::default());
        let config = Arc::new(Mutex::new(config));
        let (b, s, c) = (body.clone(), stats.clone(), config.clone());
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                let (b, s, c) = (b.clone(), s.clone(), c.clone());
                tokio::spawn(async move {
                    s.active.fetch_add(1, Ordering::SeqCst);
                    let now = s.active.load(Ordering::SeqCst);
                    s.max_active.fetch_max(now, Ordering::SeqCst);
                    let _ = serve_connection(stream, &b, &s, &c).await;
                    s.active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Self { url, body, stats, config }
    }

    pub fn set_throttle(&self, throttle: usize) {
        self.config.lock().throttle = throttle;
    }

    pub fn set_etag(&self, etag: &'static str) {
        self.config.lock().etag = etag;
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    body: &[u8],
    stats: &Stats,
    config: &Mutex<ServerConfig>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        // Read one request head (keep-alive: several requests per connection).
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        buf.drain(..head_end + 4);
        stats.requests.fetch_add(1, Ordering::SeqCst);
        let cfg = config.lock().clone();

        if stats.in_flight.load(Ordering::SeqCst) >= cfg.max_concurrent {
            stats.throttled.fetch_add(1, Ordering::SeqCst);
            let msg = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\n\r\n";
            stream.write_all(msg.as_bytes()).await?;
            continue;
        }
        struct InFlight<'a>(&'a AtomicUsize);
        impl Drop for InFlight<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        stats.in_flight.fetch_add(1, Ordering::SeqCst);
        let _in_flight = InFlight(&stats.in_flight);

        if let Some(status) = cfg.status_override {
            let msg = format!("HTTP/1.1 {status} Nope\r\nContent-Length: 0\r\n\r\n");
            stream.write_all(msg.as_bytes()).await?;
            continue;
        }

        let range = head
            .lines()
            .find_map(|l| l.strip_prefix("Range: ").or_else(|| l.strip_prefix("range: ")))
            .and_then(|r| r.trim().strip_prefix("bytes="))
            .and_then(|r| {
                let (a, b) = r.split_once('-')?;
                let a: usize = a.parse().ok()?;
                let b: usize = if b.is_empty() { body.len() - 1 } else { b.parse().ok()? };
                Some((a, b.min(body.len() - 1)))
            });

        let (status, start, end) = match range {
            Some((a, b)) if cfg.ranges && a < body.len() => {
                stats.range_starts.lock().push(a as u64);
                ("206 Partial Content", a, b + 1)
            }
            Some((a, _)) if cfg.ranges => {
                let msg = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\n\r\n",
                    body.len()
                );
                let _ = a;
                stream.write_all(msg.as_bytes()).await?;
                continue;
            }
            _ => ("200 OK", 0, body.len()),
        };

        let mut headers = format!("HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nETag: {}\r\n", cfg.etag);
        if cfg.ranges {
            headers.push_str("Accept-Ranges: bytes\r\n");
        }
        if status.starts_with("206") {
            headers.push_str(&format!("Content-Range: bytes {start}-{}/{}\r\n", end - 1, body.len()));
        }
        let keep_alive = cfg.content_length;
        if cfg.content_length {
            headers.push_str(&format!("Content-Length: {}\r\n", end - start));
        } else {
            headers.push_str("Connection: close\r\n");
        }
        headers.push_str("\r\n");
        stream.write_all(headers.as_bytes()).await?;

        let fail_this = stats.failures_done.load(Ordering::SeqCst) < cfg.fail_first;
        if fail_this {
            stats.failures_done.fetch_add(1, Ordering::SeqCst);
        }
        let started = Instant::now();
        let mut sent = 0usize;
        let mut pos = start;
        while pos < end {
            let chunk_end = (pos + 16 * 1024).min(end);
            stream.write_all(&body[pos..chunk_end]).await?;
            stats.bytes_sent.fetch_add(chunk_end - pos, Ordering::SeqCst);
            sent += chunk_end - pos;
            pos = chunk_end;
            if fail_this && sent >= (end - start) / 3 {
                return Ok(()); // drop the connection mid-body
            }
            let throttle = config.lock().throttle;
            if throttle > 0 {
                let due = Duration::from_secs_f64(sent as f64 / throttle as f64);
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    tokio::time::sleep(wait).await;
                }
            }
        }
        if !keep_alive {
            stream.shutdown().await?;
            return Ok(());
        }
    }
}

/// Opens an engine in `dir` with test-friendly settings.
pub fn open_engine(dir: &Path, tweak: impl FnOnce(&mut Settings)) -> Engine {
    let mut settings = Settings {
        download_dir: dir.join("downloads"),
        use_category_folders: false,
        browser_integration: false,
        min_split_size: 256 * 1024,
        retry_delay_secs: 0,
        max_retries: 5,
        read_timeout_secs: 5,
        connect_timeout_secs: 5,
        show_complete_dialog: false,
        ..Settings::default()
    };
    tweak(&mut settings);
    std::fs::create_dir_all(dir.join("state")).unwrap();
    std::fs::write(
        dir.join("state/settings.json"),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    Engine::open(dir.join("state")).unwrap()
}

pub fn wait_for(
    engine: &Engine,
    id: DownloadId,
    timeout: Duration,
    mut done: impl FnMut(&DownloadSnapshot) -> bool,
) -> DownloadSnapshot {
    let started = Instant::now();
    loop {
        let snap = engine.get(id).expect("download exists");
        if done(&snap) {
            return snap;
        }
        if started.elapsed() > timeout {
            panic!("timed out; last state: {snap:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn wait_finished(engine: &Engine, id: DownloadId) -> DownloadSnapshot {
    wait_for(engine, id, Duration::from_secs(30), |s| {
        matches!(s.status, DownloadStatus::Completed | DownloadStatus::Failed)
    })
}

/// Runtime that hosts test servers; separate from the engine's own runtime.
pub fn server_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

pub fn read(path: PathBuf) -> Vec<u8> {
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}
