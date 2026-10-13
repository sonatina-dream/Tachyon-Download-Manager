//! One running download: probing, the segment workers, and finalisation.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::{Client, Response, StatusCode};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::EngineError;
use crate::fsutil::{final_name, part_path, unique_name};
use crate::http::{self, Probe};
use crate::limiter::RateLimiter;
use crate::segment::{Segment, SegmentMap};
use crate::types::{
    ConnectionSnapshot, ConnectionState, DownloadId, DownloadStatus, ProbeInfo, RequestSpec,
    SegmentState, Settings,
};

/// Largest slice written (and throttled) at once. Must stay well below the minimum
/// split size; see [`SegmentMap::split_largest`].
const WRITE_CHUNK: usize = 64 * 1024;

/// How often a new connection may be opened while ramping up.
const RAMP_INTERVAL: Duration = Duration::from_millis(250);

/// Quiet time after throttling before the connection cap is raised again.
const THROTTLE_RECOVERY: Duration = Duration::from_secs(30);

/// Longest wait between retries, whatever the server asks for.
const MAX_BACKOFF: Duration = Duration::from_secs(120);

/// Snapshot of the persisted record that a task needs.
#[derive(Clone)]
pub(crate) struct TaskInput {
    pub id: DownloadId,
    pub spec: RequestSpec,
    pub file_name: String,
    /// Keep `file_name` instead of using the server's name.
    pub name_locked: bool,
    pub directory: PathBuf,
    pub size: Option<u64>,
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub segments: Vec<SegmentState>,
}

pub(crate) struct TaskEnv {
    pub client: Client,
    pub settings: Settings,
    pub global_limiter: Arc<RateLimiter>,
}

/// What the server told us, plus the name chosen for the file.
#[derive(Clone)]
pub(crate) struct TaskMeta {
    pub info: ProbeInfo,
    pub file_name: String,
}

pub(crate) enum TaskOutcome {
    Completed { file_name: String, size: u64 },
    Paused,
    Failed(EngineError),
}

/// State shared between a running task and the engine/UI.
pub(crate) struct TaskShared {
    pub cancel: CancellationToken,
    pub limiter: RateLimiter,
    phase: Mutex<DownloadStatus>,
    receiving: AtomicBool,
    meta: Mutex<Option<TaskMeta>>,
    map: Mutex<Option<Arc<SegmentMap>>>,
    speed: AtomicU64,
    connections: Mutex<Vec<ConnectionSnapshot>>,
}

impl TaskShared {
    pub fn new(speed_limit: u64) -> Self {
        Self {
            cancel: CancellationToken::new(),
            limiter: RateLimiter::new(speed_limit),
            phase: Mutex::new(DownloadStatus::Connecting),
            receiving: AtomicBool::new(false),
            meta: Mutex::new(None),
            map: Mutex::new(None),
            speed: AtomicU64::new(0),
            connections: Mutex::new(Vec::new()),
        }
    }

    pub fn phase(&self) -> DownloadStatus {
        *self.phase.lock()
    }

    fn set_phase(&self, phase: DownloadStatus) {
        *self.phase.lock() = phase;
    }

    pub fn meta(&self) -> Option<TaskMeta> {
        self.meta.lock().clone()
    }

    pub fn segments(&self) -> Option<Vec<SegmentState>> {
        self.map.lock().as_ref().map(|m| m.snapshot())
    }

    pub fn downloaded(&self) -> Option<u64> {
        self.map.lock().as_ref().map(|m| m.downloaded())
    }

    pub fn speed(&self) -> u64 {
        self.speed.load(Ordering::Relaxed)
    }

    pub fn connections(&self) -> Vec<ConnectionSnapshot> {
        self.connections.lock().clone()
    }

    pub fn active_connections(&self) -> usize {
        self.connections
            .lock()
            .iter()
            .filter(|c| matches!(c.state, ConnectionState::Receiving | ConnectionState::Connecting))
            .count()
    }

    fn receiving_count(&self) -> usize {
        self.connections
            .lock()
            .iter()
            .filter(|c| c.state == ConnectionState::Receiving)
            .count()
    }

    fn add_connection(&self) -> usize {
        let mut conns = self.connections.lock();
        let index = conns.len();
        conns.push(ConnectionSnapshot {
            index,
            downloaded: 0,
            state: ConnectionState::Connecting,
        });
        conns.len() - 1
    }

    fn set_connection(&self, index: usize, state: ConnectionState) {
        if state == ConnectionState::Receiving && !self.receiving.swap(true, Ordering::Relaxed) {
            self.set_phase(DownloadStatus::Downloading);
        }
        if let Some(c) = self.connections.lock().get_mut(index) {
            c.state = state;
        }
    }

    fn add_connection_bytes(&self, index: usize, bytes: usize) {
        if let Some(c) = self.connections.lock().get_mut(index) {
            c.downloaded += bytes as u64;
        }
    }
}

pub(crate) async fn run(input: TaskInput, shared: Arc<TaskShared>, env: TaskEnv) -> TaskOutcome {
    let result = run_inner(&input, &shared, &env).await;
    shared.speed.store(0, Ordering::Relaxed);
    match result {
        Ok(outcome) => outcome,
        Err(EngineError::Cancelled) => TaskOutcome::Paused,
        Err(e) => TaskOutcome::Failed(e),
    }
}

fn validators_match(input: &TaskInput, info: &ProbeInfo) -> bool {
    if let (Some(a), Some(b)) = (&input.etag, &info.etag) {
        return a == b;
    }
    match (&input.last_modified, &info.last_modified) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

async fn sleep_or_cancel(cancel: &CancellationToken, duration: Duration) -> Result<(), EngineError> {
    tokio::select! {
        _ = cancel.cancelled() => Err(EngineError::Cancelled),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

async fn probe_with_retries(
    input: &TaskInput,
    env: &TaskEnv,
    cancel: &CancellationToken,
    start: u64,
    end: Option<u64>,
) -> Result<Probe, EngineError> {
    let mut attempt = 0;
    loop {
        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(EngineError::Cancelled),
            r = http::probe(&env.client, &input.spec, start, end) => r,
        };
        match result {
            Ok(probe) => return Ok(probe),
            Err(e) if e.is_transient() && attempt < env.settings.max_retries => {
                attempt += 1;
                log::warn!("download {}: probe failed ({e}), retry {attempt}", input.id);
                let base = Duration::from_secs(env.settings.retry_delay_secs);
                sleep_or_cancel(cancel, retry_delay(&e, base, attempt)).await?;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn run_inner(
    input: &TaskInput,
    shared: &Arc<TaskShared>,
    env: &TaskEnv,
) -> Result<TaskOutcome, EngineError> {
    let settings = &env.settings;
    let cancel = shared.cancel.clone();

    let has_progress = input.resumable
        && input.size.is_some()
        && input.segments.iter().any(|s| s.pos > s.start);
    // When resuming, the probe fetches exactly the first unfinished segment so its
    // connection can carry on with it.
    let (probe_from, probe_to) = if has_progress {
        input
            .segments
            .iter()
            .filter(|s| !s.is_complete())
            .min_by_key(|s| s.pos)
            .map(|s| (s.pos, Some(s.end)))
            .unwrap_or((0, None))
    } else {
        (0, None)
    };

    let probe = probe_with_retries(input, env, &cancel, probe_from, probe_to).await?;
    let info = probe.info.clone();
    let continuing = has_progress
        && info.resumable
        && probe.body_start == probe_from
        && info.total == input.size
        && validators_match(input, &info);
    if has_progress && !continuing {
        log::warn!(
            "download {}: the file changed on the server or lost resume support; restarting",
            input.id
        );
    }

    let file_name = if input.name_locked {
        input.file_name.clone()
    } else {
        unique_name(&input.directory, &info.file_name, |_| false)
    };
    *shared.meta.lock() = Some(TaskMeta {
        info: info.clone(),
        file_name: file_name.clone(),
    });

    let segments = if continuing {
        input.segments.clone()
    } else {
        vec![SegmentState {
            start: 0,
            pos: 0,
            end: info.total.unwrap_or(u64::MAX),
        }]
    };
    // The probe's body is usable by the connection whose segment starts where it does.
    let first_response = (continuing || probe.body_start == 0).then_some(probe.response);
    let first_at = if continuing { probe.body_start } else { 0 };

    std::fs::create_dir_all(&input.directory)?;
    let part = part_path(&input.directory, &file_name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(!continuing)
        .open(&part)?;
    if let Some(total) = info.total {
        // Sparse on APFS, so this reserves nothing up front but lets segments write anywhere.
        file.set_len(total)?;
    }
    let file = Arc::new(file);

    let splittable = info.resumable && info.total.is_some();
    let map = Arc::new(SegmentMap::new(segments, splittable, settings.min_split_size));
    *shared.map.lock() = Some(map.clone());

    let workers_cancel = cancel.child_token();
    let ctx = Arc::new(WorkerCtx {
        client: env.client.clone(),
        spec: input.spec.clone(),
        url: info.final_url.clone(),
        file: file.clone(),
        map: map.clone(),
        shared: shared.clone(),
        global: env.global_limiter.clone(),
        cancel: workers_cancel.clone(),
        max_retries: settings.max_retries,
        retry_delay: Duration::from_secs(settings.retry_delay_secs),
        read_timeout: Duration::from_secs(settings.read_timeout_secs),
        size_known: info.total.is_some(),
        resumable: info.resumable,
        cap: AtomicUsize::new(if splittable { settings.max_connections.max(1) as usize } else { 1 }),
        alive: AtomicUsize::new(0),
        last_throttle: Mutex::new(None),
    });

    let max_connections = ctx.cap.load(Ordering::Relaxed);
    let mut workers = JoinSet::new();
    let spawn = |workers: &mut JoinSet<Result<(), EngineError>>,
                 segment: Arc<Segment>,
                 response: Option<Response>| {
        let index = ctx.shared.add_connection();
        ctx.alive.fetch_add(1, Ordering::AcqRel);
        workers.spawn(run_worker(ctx.clone(), index, segment, response));
    };

    if let Some(response) = first_response
        && let Some(segment) = map.take_at(first_at)
    {
        spawn(&mut workers, segment, Some(response));
    }
    if workers.is_empty()
        && let Some(segment) = map.next_work()
    {
        spawn(&mut workers, segment, None);
    }

    let mut sampler = SpeedSampler::default();
    let mut ticker = tokio::time::interval(RAMP_INTERVAL);
    let mut failure: Option<EngineError> = None;
    let mut failures = 0usize;
    loop {
        tokio::select! {
            joined = workers.join_next() => {
                let result = match joined {
                    None => break,
                    Some(Ok(result)) => result,
                    Some(Err(e)) => Err(EngineError::Protocol(format!("connection task crashed: {e}"))),
                };
                match result {
                    Ok(()) | Err(EngineError::Cancelled) => {}
                    Err(e) => {
                        log::warn!("download {}: connection gave up: {e}", input.id);
                        failures += 1;
                        if !e.is_transient() {
                            // Every connection would hit this too (disk full, 404, …).
                            workers_cancel.cancel();
                        }
                        failure.get_or_insert(e);
                    }
                }
            }
            _ = ticker.tick() => {
                shared.speed.store(sampler.sample(map.downloaded()), Ordering::Relaxed);
                // Let the cap creep back up once the server has been quiet for a while.
                if ctx.cap.load(Ordering::Relaxed) < max_connections && !ctx.throttled_within(THROTTLE_RECOVERY) {
                    ctx.cap.fetch_add(1, Ordering::AcqRel);
                    *ctx.last_throttle.lock() = Some(Instant::now());
                }
                // Ramp up one connection per tick (leftovers first, then splits)
                // instead of opening them all at once, which servers treat as abuse.
                let fatal = failure.as_ref().is_some_and(|e| !e.is_transient());
                if !fatal
                    && !workers_cancel.is_cancelled()
                    && failures <= max_connections
                    && workers.len() < ctx.cap.load(Ordering::Relaxed)
                    && !ctx.throttled_within(Duration::from_secs(5))
                    && let Some(segment) = map.next_work()
                {
                    spawn(&mut workers, segment, None);
                }
            }
        }
    }

    file.sync_data()?;
    if cancel.is_cancelled() {
        return Ok(TaskOutcome::Paused);
    }
    if !map.is_complete() {
        return Err(failure.unwrap_or_else(|| EngineError::Network("all connections closed".into())));
    }

    shared.set_phase(DownloadStatus::Finalizing);
    let total = match info.total {
        Some(total) => total,
        None => map.snapshot().iter().map(|s| s.end).max().unwrap_or(0),
    };
    file.set_len(total)?;
    file.sync_all()?;
    drop(file);
    drop(ctx);

    let file_name = final_name(&input.directory, &file_name);
    std::fs::rename(&part, input.directory.join(&file_name))?;
    Ok(TaskOutcome::Completed {
        file_name,
        size: total,
    })
}

/// Average speed over a sliding window of recent samples.
#[derive(Default)]
struct SpeedSampler {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedSampler {
    const WINDOW: Duration = Duration::from_secs(4);

    fn sample(&mut self, downloaded: u64) -> u64 {
        let now = Instant::now();
        self.samples.push_back((now, downloaded));
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > Self::WINDOW && self.samples.len() > 2 {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        let (t0, b0) = *self.samples.front().expect("just pushed");
        let elapsed = now.duration_since(t0).as_secs_f64();
        if elapsed < 0.4 {
            return 0;
        }
        (downloaded.saturating_sub(b0) as f64 / elapsed) as u64
    }
}

struct WorkerCtx {
    client: Client,
    spec: RequestSpec,
    /// Address after redirects, reused for every segment request in this session.
    url: String,
    file: Arc<File>,
    map: Arc<SegmentMap>,
    shared: Arc<TaskShared>,
    global: Arc<RateLimiter>,
    cancel: CancellationToken,
    max_retries: u32,
    retry_delay: Duration,
    read_timeout: Duration,
    size_known: bool,
    resumable: bool,
    /// Connections allowed right now; lowered when the server throttles us.
    cap: AtomicUsize,
    /// Running connection tasks.
    alive: AtomicUsize,
    last_throttle: Mutex<Option<Instant>>,
}

impl WorkerCtx {
    fn throttled_within(&self, window: Duration) -> bool {
        self.last_throttle.lock().is_some_and(|t| t.elapsed() < window)
    }

    /// Records a throttling response and lowers the connection cap. Returns true if
    /// this connection should close because more are open than the cap allows.
    fn on_throttle(&self) -> bool {
        *self.last_throttle.lock() = Some(Instant::now());
        let previous = self
            .cap
            .try_update(Ordering::AcqRel, Ordering::Acquire, |c| Some(c.saturating_sub(1).max(1)))
            .unwrap_or(1);
        let cap = previous.saturating_sub(1).max(1);
        self.alive.load(Ordering::Acquire) > cap
    }
}

enum StreamEnd {
    /// The segment is full (possibly because a split moved its end).
    ReachedEnd,
    /// The server closed the body.
    Eof,
}

/// One connection: downloads its segment, then keeps taking new work until none is left.
async fn run_worker(
    ctx: Arc<WorkerCtx>,
    index: usize,
    first: Arc<Segment>,
    response: Option<Response>,
) -> Result<(), EngineError> {
    let result = worker_loop(&ctx, index, first, response).await;
    ctx.alive.fetch_sub(1, Ordering::AcqRel);
    result
}

async fn worker_loop(
    ctx: &Arc<WorkerCtx>,
    index: usize,
    first: Arc<Segment>,
    mut response: Option<Response>,
) -> Result<(), EngineError> {
    let mut segment = first;
    loop {
        let result = download_segment(ctx, index, &segment, response.take()).await;
        segment.release();
        if let Ok(SegmentEnd::Released(reason)) = result {
            // Another connection picks the segment up when it finishes its own.
            ctx.shared.set_connection(index, ConnectionState::Released(reason));
            return Ok(());
        }
        if let Err(e) = result {
            let state = match &e {
                EngineError::Cancelled => ConnectionState::Idle,
                e => ConnectionState::Failed(e.to_string()),
            };
            ctx.shared.set_connection(index, state);
            return Err(e);
        }
        if ctx.cancel.is_cancelled() {
            ctx.shared.set_connection(index, ConnectionState::Idle);
            return Err(EngineError::Cancelled);
        }
        // Reuse this connection slot for the largest remaining piece of work.
        match ctx.map.next_work() {
            Some(next) => segment = next,
            None => {
                ctx.shared.set_connection(index, ConnectionState::Finished);
                return Ok(());
            }
        }
    }
}

/// Checks that a response body starts at `pos`.
fn validate_response(response: &Response, pos: u64) -> Result<(), EngineError> {
    http::check_status(response)?;
    match response.status() {
        StatusCode::PARTIAL_CONTENT => {
            let start = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(http::parse_content_range)
                .map(|r| r.0);
            match start {
                Some(start) if start == pos => Ok(()),
                Some(start) => Err(EngineError::Protocol(format!(
                    "asked for byte {pos}, got byte {start}"
                ))),
                None => Err(EngineError::Protocol("206 without Content-Range".into())),
            }
        }
        _ if pos == 0 => Ok(()),
        _ => Err(EngineError::NotResumable),
    }
}

enum SegmentEnd {
    Done,
    /// Gave the segment back to reduce the number of connections.
    Released(String),
}

async fn download_segment(
    ctx: &WorkerCtx,
    index: usize,
    segment: &Segment,
    mut response: Option<Response>,
) -> Result<SegmentEnd, EngineError> {
    let mut attempt = 0u32;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        if segment.is_complete() {
            return Ok(SegmentEnd::Done);
        }
        if !ctx.resumable && segment.pos() > segment.start {
            // Without range support a retry has to start over.
            segment.reset();
        }
        let state = segment.state();

        let response = match response.take() {
            Some(r) => r,
            None => {
                ctx.shared.set_connection(index, ConnectionState::Connecting);
                let end = ctx.size_known.then_some(state.end);
                let request = http::get_range(&ctx.client, &ctx.spec, &ctx.url, state.pos, end);
                let result = tokio::select! {
                    _ = ctx.cancel.cancelled() => return Err(EngineError::Cancelled),
                    // Servers that cap connections per client often just never answer.
                    r = tokio::time::timeout(ctx.read_timeout, request) => r.unwrap_or(Err(EngineError::Timeout)),
                };
                match result.and_then(|r| validate_response(&r, state.pos).map(|()| r)) {
                    Ok(r) => r,
                    Err(e) => {
                        let unanswered = matches!(e, EngineError::Timeout) && ctx.shared.receiving_count() > 0;
                        if (e.is_throttle() || unanswered) && ctx.on_throttle() {
                            return Ok(SegmentEnd::Released(e.to_string()));
                        }
                        attempt = backoff(ctx, index, e, attempt).await?;
                        continue;
                    }
                }
            }
        };

        ctx.shared.set_connection(index, ConnectionState::Receiving);
        let before = segment.pos();
        let result = stream_body(ctx, index, segment, response).await;
        if segment.pos() > before {
            // Progress was made, so the retry budget starts fresh.
            attempt = 0;
        }
        match result {
            Ok(StreamEnd::ReachedEnd) => return Ok(SegmentEnd::Done),
            Ok(StreamEnd::Eof) if !ctx.size_known => {
                segment.close_at_current();
                return Ok(SegmentEnd::Done);
            }
            Ok(StreamEnd::Eof) if segment.is_complete() => return Ok(SegmentEnd::Done),
            Ok(StreamEnd::Eof) => {
                let e = EngineError::Network("server closed the connection early".into());
                attempt = backoff(ctx, index, e, attempt).await?;
            }
            Err(e) => attempt = backoff(ctx, index, e, attempt).await?,
        }
    }
}

/// Waits before a retry, or returns the error if retrying is pointless.
async fn backoff(ctx: &WorkerCtx, index: usize, error: EngineError, attempt: u32) -> Result<u32, EngineError> {
    if matches!(error, EngineError::Cancelled) || !error.is_transient() || attempt >= ctx.max_retries {
        return Err(error);
    }
    let attempt = attempt + 1;
    ctx.shared.set_connection(
        index,
        ConnectionState::Retrying {
            attempt,
            reason: error.to_string(),
        },
    );
    sleep_or_cancel(&ctx.cancel, retry_delay(&error, ctx.retry_delay, attempt)).await?;
    Ok(attempt)
}

/// Delay before retry number `attempt` (1-based): what the server asked for, or
/// exponential backoff that starts higher when the server says it's overloaded.
fn retry_delay(error: &EngineError, base: Duration, attempt: u32) -> Duration {
    if let EngineError::Http { retry_after: Some(secs), .. } = error {
        return Duration::from_secs(*secs).min(MAX_BACKOFF);
    }
    let base = if error.is_throttle() { base.max(Duration::from_secs(5)) } else { base };
    let exponential = base.saturating_mul(1 << (attempt - 1).min(5)).min(MAX_BACKOFF);
    // Jitter so connections that failed together don't retry together.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    exponential.max(Duration::from_millis(250)) + Duration::from_millis(u64::from(nanos % 500))
}

async fn stream_body(
    ctx: &WorkerCtx,
    index: usize,
    segment: &Segment,
    response: Response,
) -> Result<StreamEnd, EngineError> {
    let mut body = response.bytes_stream();
    loop {
        let next = tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(EngineError::Cancelled),
            next = tokio::time::timeout(ctx.read_timeout, body.next()) => next,
        };
        let chunk = match next {
            Err(_) => return Err(EngineError::Timeout),
            Ok(None) => return Ok(StreamEnd::Eof),
            Ok(Some(Err(e))) => return Err(EngineError::from_reqwest(e)),
            Ok(Some(Ok(chunk))) => chunk,
        };
        let mut data: &[u8] = &chunk;
        while !data.is_empty() {
            let piece = &data[..data.len().min(WRITE_CHUNK)];
            if ctx.shared.limiter.rate() > 0 || ctx.global.rate() > 0 {
                tokio::select! {
                    _ = ctx.cancel.cancelled() => return Err(EngineError::Cancelled),
                    _ = async {
                        ctx.shared.limiter.acquire(piece.len()).await;
                        ctx.global.acquire(piece.len()).await;
                    } => {}
                }
            }
            let written = segment.write(&ctx.file, piece)?;
            ctx.shared.add_connection_bytes(index, written);
            if written < piece.len() {
                return Ok(StreamEnd::ReachedEnd);
            }
            data = &data[written..];
        }
        if segment.is_complete() {
            return Ok(StreamEnd::ReachedEnd);
        }
    }
}
