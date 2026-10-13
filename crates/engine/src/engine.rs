use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use parking_lot::{Mutex, RwLock};
use reqwest::Client;
use tokio_util::sync::CancellationToken;

use crate::categories::{classify, folder_for};
use crate::error::EngineError;
use crate::fsutil::{part_path, unique_name};
use crate::http;
use crate::integration;
use crate::limiter::RateLimiter;
use crate::persist::{self, DownloadRecord, StateFile};
use crate::task::{self, TaskEnv, TaskInput, TaskOutcome, TaskShared};
use crate::types::{
    AddRequest, CompletionOptions, DayTime, DownloadDetails, DownloadId, DownloadSnapshot,
    DownloadStatus, EngineEvent, PostAction, ProbeInfo, RequestSpec, Settings,
};

/// The download manager core. Cheap to share behind an `Arc`; every method is
/// synchronous and safe to call from the UI thread.
pub struct Engine {
    inner: Arc<Inner>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
}

struct Inner {
    handle: tokio::runtime::Handle,
    data_dir: PathBuf,
    settings: RwLock<Settings>,
    client: RwLock<Client>,
    global_limiter: Arc<RateLimiter>,
    state: Mutex<State>,
    events: Mutex<Vec<EngineEvent>>,
    dirty: AtomicBool,
    integration: Mutex<Option<(u16, CancellationToken)>>,
}

#[derive(Default)]
struct State {
    records: BTreeMap<DownloadId, DownloadRecord>,
    tasks: HashMap<DownloadId, Arc<TaskShared>>,
    next_id: DownloadId,
    next_queue_order: u64,
    queue_running: bool,
    /// Queue members the user stopped by hand during this queue run.
    queue_skipped: HashSet<DownloadId>,
    /// Files to delete once the task writing them has stopped.
    pending_deletes: HashMap<DownloadId, Vec<PathBuf>>,
    last_schedule_fire: Option<(i32, u32, u32, bool)>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Engine {
    /// Opens the engine with state in `~/Library/Application Support/Tachyon`.
    pub fn open_default() -> anyhow::Result<Self> {
        let dir = dirs::data_dir()
            .ok_or_else(|| anyhow::anyhow!("no Application Support directory"))?
            .join("Tachyon");
        Self::open(dir)
    }

    pub fn open(data_dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&data_dir)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("tachyon-net")
            .enable_all()
            .build()?;
        let settings = persist::load_settings(&data_dir);
        let client = http::build_client(&settings)?;
        let saved = persist::load_state(&data_dir);
        let state = State {
            records: saved.downloads.into_iter().map(|r| (r.id, r)).collect(),
            next_id: saved.next_id.max(1),
            next_queue_order: saved.next_queue_order,
            ..Default::default()
        };
        let inner = Arc::new(Inner {
            handle: runtime.handle().clone(),
            data_dir,
            global_limiter: Arc::new(RateLimiter::new(settings.global_speed_limit)),
            settings: RwLock::new(settings),
            client: RwLock::new(client),
            state: Mutex::new(state),
            events: Mutex::new(Vec::new()),
            dirty: AtomicBool::new(false),
            integration: Mutex::new(None),
        });
        inner.sync_integration();
        let weak = Arc::downgrade(&inner);
        runtime.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            let mut tick = 0u64;
            loop {
                ticker.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                tick += 1;
                inner.maintenance(tick);
            }
        });
        Ok(Self {
            inner,
            runtime: Mutex::new(Some(runtime)),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.read().clone()
    }

    pub fn update_settings(&self, settings: Settings) -> Result<(), EngineError> {
        let settings = settings.sanitized();
        let client = http::build_client(&settings)?;
        *self.inner.client.write() = client;
        self.inner.global_limiter.set_rate(settings.global_speed_limit);
        persist::save_settings(&self.inner.data_dir, &settings)?;
        *self.inner.settings.write() = settings;
        self.inner.sync_integration();
        Ok(())
    }

    /// Asks the server about `spec` without downloading anything.
    pub fn probe(&self, spec: RequestSpec) -> impl Future<Output = Result<ProbeInfo, EngineError>> + Send + 'static {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let client = self.inner.client.read().clone();
        let timeout = Duration::from_secs(self.inner.settings.read().connect_timeout_secs + 10);
        self.inner.handle.spawn(async move {
            let result = match tokio::time::timeout(timeout, http::probe(&client, &spec, 0, None)).await {
                Ok(result) => result.map(|probe| probe.info),
                Err(_) => Err(EngineError::Timeout),
            };
            let _ = tx.send(result);
        });
        async move { rx.await.unwrap_or(Err(EngineError::Cancelled)) }
    }

    /// The category and folder a file would be saved to by default.
    pub fn default_target(&self, file_name: &str) -> (String, PathBuf) {
        let settings = self.inner.settings.read();
        let category = classify(file_name, &settings.categories);
        let folder = folder_for(&category, &settings);
        (category, folder)
    }

    pub fn folder_for_category(&self, category: &str) -> PathBuf {
        folder_for(category, &self.inner.settings.read())
    }

    pub fn add(&self, request: AddRequest) -> Result<DownloadId, EngineError> {
        self.inner.add(request)
    }

    /// Starts or resumes a download.
    pub fn start(&self, id: DownloadId) -> Result<(), EngineError> {
        let mut state = self.inner.state.lock();
        state.queue_skipped.remove(&id);
        self.inner.start_locked(&mut state, id)
    }

    /// Stops a download, keeping what was received so far.
    pub fn pause(&self, id: DownloadId) {
        let mut state = self.inner.state.lock();
        if state.records.get(&id).is_some_and(|r| r.in_queue) {
            state.queue_skipped.insert(id);
        }
        if let Some(task) = state.tasks.get(&id) {
            task.cancel.cancel();
        }
    }

    pub fn pause_all(&self) {
        let mut state = self.inner.state.lock();
        state.queue_running = false;
        for task in state.tasks.values() {
            task.cancel.cancel();
        }
    }

    /// Deletes downloads from the list, and optionally their files from disk.
    pub fn remove(&self, ids: &[DownloadId], delete_files: bool) {
        let mut state = self.inner.state.lock();
        for id in ids {
            let Some(record) = state.records.remove(id) else {
                continue;
            };
            let mut files = vec![part_path(&record.directory, &record.file_name)];
            if delete_files && record.status == DownloadStatus::Completed {
                files.push(record.directory.join(&record.file_name));
            }
            if let Some(task) = state.tasks.get(id) {
                task.cancel.cancel();
                state.pending_deletes.insert(*id, files);
            } else {
                delete_paths(&files);
            }
        }
        drop(state);
        self.inner.mark_dirty();
    }

    /// Removes finished downloads from the list (files stay on disk).
    pub fn remove_completed(&self) -> usize {
        let mut state = self.inner.state.lock();
        let before = state.records.len();
        state.records.retain(|_, r| r.status != DownloadStatus::Completed);
        let removed = before - state.records.len();
        drop(state);
        if removed > 0 {
            self.inner.mark_dirty();
        }
        removed
    }

    /// Discards received data and downloads the file again from the start.
    pub fn redownload(&self, id: DownloadId) -> Result<(), EngineError> {
        let mut state = self.inner.state.lock();
        if state.tasks.contains_key(&id) {
            return Err(EngineError::Config("stop the download first".into()));
        }
        let record = state.records.get_mut(&id).ok_or(EngineError::NotFound)?;
        let _ = std::fs::remove_file(part_path(&record.directory, &record.file_name));
        record.segments.clear();
        record.downloaded = 0;
        record.status = DownloadStatus::Stopped;
        record.completed_at = None;
        record.error = None;
        self.inner.start_locked(&mut state, id)
    }

    pub fn snapshot(&self) -> Vec<DownloadSnapshot> {
        let state = self.inner.state.lock();
        state
            .records
            .values()
            .map(|r| snapshot_of(r, state.tasks.get(&r.id)))
            .collect()
    }

    pub fn get(&self, id: DownloadId) -> Option<DownloadSnapshot> {
        let state = self.inner.state.lock();
        state.records.get(&id).map(|r| snapshot_of(r, state.tasks.get(&id)))
    }

    pub fn details(&self, id: DownloadId) -> DownloadDetails {
        let state = self.inner.state.lock();
        match (state.tasks.get(&id), state.records.get(&id)) {
            (Some(task), record) => DownloadDetails {
                segments: task
                    .segments()
                    .or_else(|| record.map(|r| r.segments.clone()))
                    .unwrap_or_default(),
                connections: task.connections(),
            },
            (None, Some(record)) => DownloadDetails {
                segments: record.segments.clone(),
                connections: Vec::new(),
            },
            (None, None) => DownloadDetails::default(),
        }
    }

    /// Sets a per-download speed limit in bytes per second (0 = unlimited).
    pub fn set_speed_limit(&self, id: DownloadId, limit: u64) {
        let mut state = self.inner.state.lock();
        if let Some(record) = state.records.get_mut(&id) {
            record.speed_limit = limit;
        }
        if let Some(task) = state.tasks.get(&id) {
            task.limiter.set_rate(limit);
        }
        drop(state);
        self.inner.mark_dirty();
    }

    pub fn set_completion(&self, id: DownloadId, completion: CompletionOptions) {
        if let Some(record) = self.inner.state.lock().records.get_mut(&id) {
            record.completion = completion;
        }
        self.inner.mark_dirty();
    }

    /// Edits the address and description (IDM's "Properties").
    pub fn update_properties(&self, id: DownloadId, url: Option<String>, description: Option<String>) -> Result<(), EngineError> {
        let mut state = self.inner.state.lock();
        let running = state.tasks.contains_key(&id);
        let record = state.records.get_mut(&id).ok_or(EngineError::NotFound)?;
        if let Some(url) = url {
            let parsed = url::Url::parse(url.trim()).map_err(|e| EngineError::Config(format!("invalid address: {e}")))?;
            if running {
                return Err(EngineError::Config("stop the download before changing its address".into()));
            }
            record.spec.url = parsed.to_string();
            record.final_url = None;
        }
        if let Some(description) = description {
            record.description = description;
        }
        drop(state);
        self.inner.mark_dirty();
        Ok(())
    }

    pub fn set_queued(&self, ids: &[DownloadId], queued: bool) {
        let mut state = self.inner.state.lock();
        for id in ids {
            let order = state.next_queue_order;
            if let Some(record) = state.records.get_mut(id) {
                if queued && !record.in_queue && record.status != DownloadStatus::Completed {
                    record.in_queue = true;
                    record.queue_order = order;
                    record.queue_failures = 0;
                    state.next_queue_order += 1;
                } else if !queued {
                    record.in_queue = false;
                }
            }
        }
        drop(state);
        self.inner.mark_dirty();
    }

    /// Moves a queued download one place earlier (`up`) or later in the queue.
    pub fn move_in_queue(&self, id: DownloadId, up: bool) {
        let mut state = self.inner.state.lock();
        let mut queue: Vec<(u64, DownloadId)> = state
            .records
            .values()
            .filter(|r| r.in_queue)
            .map(|r| (r.queue_order, r.id))
            .collect();
        queue.sort();
        let Some(pos) = queue.iter().position(|&(_, i)| i == id) else {
            return;
        };
        let other = if up { pos.checked_sub(1) } else { (pos + 1 < queue.len()).then_some(pos + 1) };
        if let Some(other) = other {
            let (a, b) = (queue[pos], queue[other]);
            if let Some(r) = state.records.get_mut(&a.1) {
                r.queue_order = b.0;
            }
            if let Some(r) = state.records.get_mut(&b.1) {
                r.queue_order = a.0;
            }
        }
        drop(state);
        self.inner.mark_dirty();
    }

    pub fn start_queue(&self) {
        self.inner.start_queue();
    }

    pub fn stop_queue(&self) {
        self.inner.stop_queue();
    }

    pub fn is_queue_running(&self) -> bool {
        self.inner.state.lock().queue_running
    }

    pub fn total_speed(&self) -> u64 {
        self.inner.state.lock().tasks.values().map(|t| t.speed()).sum()
    }

    pub fn drain_events(&self) -> Vec<EngineEvent> {
        std::mem::take(&mut *self.inner.events.lock())
    }

    /// Stops all downloads, saves state and shuts the network runtime down.
    pub fn shutdown(&self) {
        {
            let state = self.inner.state.lock();
            for task in state.tasks.values() {
                task.cancel.cancel();
            }
        }
        if let Some((_, cancel)) = self.inner.integration.lock().take() {
            cancel.cancel();
        }
        // Give tasks a moment to record their final positions.
        for _ in 0..40 {
            if self.inner.state.lock().tasks.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.inner.checkpoint();
        self.inner.save();
        if let Some(runtime) = self.runtime.lock().take() {
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.runtime.lock().is_some() {
            self.shutdown();
        }
    }
}

fn delete_paths(paths: &[PathBuf]) {
    for path in paths {
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("could not delete {}: {e}", path.display());
        }
    }
}

fn snapshot_of(record: &DownloadRecord, task: Option<&Arc<TaskShared>>) -> DownloadSnapshot {
    let mut snapshot = DownloadSnapshot {
        id: record.id,
        url: record.spec.url.clone(),
        file_name: record.file_name.clone(),
        directory: record.directory.clone(),
        category: record.category.clone(),
        description: record.description.clone(),
        size: record.size,
        downloaded: record.downloaded,
        status: record.status,
        speed: 0,
        eta_secs: None,
        resumable: record.resumable,
        error: record.error.clone(),
        added_at: record.added_at,
        last_try: record.last_try,
        completed_at: record.completed_at,
        in_queue: record.in_queue,
        queue_order: record.queue_order,
        speed_limit: record.speed_limit,
        active_connections: 0,
        completion: record.completion.clone(),
    };
    if let Some(task) = task {
        snapshot.status = task.phase();
        snapshot.speed = task.speed();
        snapshot.active_connections = task.active_connections();
        if let Some(downloaded) = task.downloaded() {
            snapshot.downloaded = downloaded;
        }
        if let Some(meta) = task.meta() {
            snapshot.size = meta.info.total;
            snapshot.resumable = Some(meta.info.resumable);
            snapshot.file_name = meta.file_name;
        }
        if let Some(total) = snapshot.size
            && snapshot.speed > 0
        {
            snapshot.eta_secs = Some(total.saturating_sub(snapshot.downloaded) / snapshot.speed);
        }
    }
    snapshot
}

/// Copies a running task's progress into its record.
fn absorb(record: &mut DownloadRecord, task: &TaskShared) {
    if let Some(meta) = task.meta() {
        record.final_url = Some(meta.info.final_url);
        record.size = meta.info.total;
        record.resumable = Some(meta.info.resumable);
        record.content_type = meta.info.content_type;
        record.etag = meta.info.etag;
        record.last_modified = meta.info.last_modified;
        record.file_name = meta.file_name;
        record.name_locked = true;
    }
    if let Some(segments) = task.segments() {
        record.downloaded = segments.iter().map(|s| s.downloaded()).sum();
        record.segments = segments;
    }
    record.status = task.phase();
}

fn matches_time(t: DayTime, hour: u32, minute: u32) -> bool {
    u32::from(t.hour) == hour && u32::from(t.minute) == minute
}

impl Inner {
    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    fn push_event(&self, event: EngineEvent) {
        self.events.lock().push(event);
    }

    fn add(self: &Arc<Self>, request: AddRequest) -> Result<DownloadId, EngineError> {
        let parsed = url::Url::parse(request.request.url.trim())
            .map_err(|e| EngineError::Config(format!("invalid address: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(EngineError::Config(format!(
                "unsupported protocol \"{}\" (use http or https)",
                parsed.scheme()
            )));
        }
        let mut spec = request.request.clone();
        spec.url = parsed.to_string();

        let settings = self.settings.read().clone();
        let probed = request.probed.clone();
        let user_name = request
            .file_name
            .as_deref()
            .map(http::sanitize_file_name)
            .filter(|n| !n.is_empty());
        let name_locked = user_name.is_some() || probed.is_some();
        let base_name = user_name
            .or_else(|| probed.as_ref().map(|p| p.file_name.clone()))
            .unwrap_or_else(|| http::derive_file_name(None, &spec.url, None));
        let category = request
            .category
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| classify(&base_name, &settings.categories));
        let directory = request
            .directory
            .clone()
            .unwrap_or_else(|| folder_for(&category, &settings));

        let mut state = self.state.lock();
        let taken: HashSet<String> = state
            .records
            .values()
            .filter(|r| r.directory == directory && r.status != DownloadStatus::Completed)
            .map(|r| r.file_name.clone())
            .collect();
        let file_name = unique_name(&directory, &base_name, |n| taken.contains(n));

        let id = state.next_id;
        state.next_id += 1;
        let mut record = DownloadRecord {
            id,
            spec,
            file_name,
            name_locked,
            directory,
            category,
            description: request.description.clone(),
            added_at: now(),
            completion: CompletionOptions {
                show_dialog: settings.show_complete_dialog,
                ..Default::default()
            },
            ..Default::default()
        };
        if let Some(info) = probed {
            record.final_url = Some(info.final_url);
            record.size = info.total;
            record.resumable = Some(info.resumable);
            record.content_type = info.content_type;
            record.etag = info.etag;
            record.last_modified = info.last_modified;
        }
        if request.queue {
            record.in_queue = true;
            record.queue_order = state.next_queue_order;
            state.next_queue_order += 1;
        }
        state.records.insert(id, record);
        if request.start {
            self.start_locked(&mut state, id)?;
        }
        drop(state);
        self.mark_dirty();
        Ok(id)
    }

    fn start_locked(self: &Arc<Self>, state: &mut State, id: DownloadId) -> Result<(), EngineError> {
        if state.tasks.contains_key(&id) {
            return Ok(());
        }
        let record = state.records.get_mut(&id).ok_or(EngineError::NotFound)?;
        if record.status == DownloadStatus::Completed {
            return Ok(());
        }
        record.status = DownloadStatus::Connecting;
        record.error = None;
        record.last_try = Some(now());
        let input = TaskInput {
            id,
            spec: record.spec.clone(),
            file_name: record.file_name.clone(),
            name_locked: record.name_locked,
            directory: record.directory.clone(),
            size: record.size,
            resumable: record.resumable.unwrap_or(false),
            etag: record.etag.clone(),
            last_modified: record.last_modified.clone(),
            segments: record.segments.clone(),
        };
        let shared = Arc::new(TaskShared::new(record.speed_limit));
        state.tasks.insert(id, shared.clone());
        let env = TaskEnv {
            client: self.client.read().clone(),
            settings: self.settings.read().clone(),
            global_limiter: self.global_limiter.clone(),
        };
        let inner = self.clone();
        self.handle.spawn(async move {
            let outcome = task::run(input, shared.clone(), env).await;
            inner.finish(id, &shared, outcome);
        });
        self.mark_dirty();
        Ok(())
    }

    fn finish(self: &Arc<Self>, id: DownloadId, shared: &TaskShared, outcome: TaskOutcome) {
        let mut state = self.state.lock();
        state.tasks.remove(&id);
        if let Some(files) = state.pending_deletes.remove(&id) {
            delete_paths(&files);
        }
        let Some(record) = state.records.get_mut(&id) else {
            return;
        };
        absorb(record, shared);
        let mut events = Vec::new();
        match outcome {
            TaskOutcome::Completed { file_name, size } => {
                record.file_name = file_name;
                record.size = Some(size);
                record.downloaded = size;
                record.segments.clear();
                record.status = DownloadStatus::Completed;
                record.completed_at = Some(now());
                record.in_queue = false;
                events.push(EngineEvent::Completed(id));
                if record.completion.post_action != PostAction::Nothing {
                    events.push(EngineEvent::PostAction(record.completion.post_action));
                }
            }
            TaskOutcome::Paused => record.status = DownloadStatus::Stopped,
            TaskOutcome::Failed(error) => {
                log::warn!("download {id} failed: {error}");
                record.status = DownloadStatus::Failed;
                record.error = Some(error.to_string());
                if record.in_queue {
                    record.queue_failures += 1;
                }
                events.push(EngineEvent::Failed {
                    id,
                    error: error.to_string(),
                });
            }
        }
        drop(state);
        self.events.lock().extend(events);
        self.mark_dirty();
        self.pump_queue();
    }

    fn start_queue(self: &Arc<Self>) {
        {
            let mut state = self.state.lock();
            state.queue_running = true;
            state.queue_skipped.clear();
            for record in state.records.values_mut().filter(|r| r.in_queue) {
                record.queue_failures = 0;
            }
        }
        self.pump_queue();
    }

    fn stop_queue(&self) {
        let mut state = self.state.lock();
        state.queue_running = false;
        let queued: Vec<DownloadId> = state.records.values().filter(|r| r.in_queue).map(|r| r.id).collect();
        for id in queued {
            if let Some(task) = state.tasks.get(&id) {
                task.cancel.cancel();
            }
        }
    }

    /// Starts queued downloads until the queue's concurrency limit is reached.
    fn pump_queue(self: &Arc<Self>) {
        let queue = self.settings.read().queue.clone();
        let mut state = self.state.lock();
        if !state.queue_running {
            return;
        }
        let active = state
            .records
            .values()
            .filter(|r| r.in_queue && state.tasks.contains_key(&r.id))
            .count();
        let mut pending: Vec<(u64, DownloadId)> = state
            .records
            .values()
            .filter(|r| {
                r.in_queue
                    && !state.tasks.contains_key(&r.id)
                    && !state.queue_skipped.contains(&r.id)
                    && r.status != DownloadStatus::Completed
                    && !(r.status == DownloadStatus::Failed && r.queue_failures > queue.retries)
            })
            .map(|r| (r.queue_order, r.id))
            .collect();
        pending.sort();
        if active == 0 && pending.is_empty() {
            state.queue_running = false;
            drop(state);
            self.push_event(EngineEvent::QueueFinished {
                post_action: queue.post_action,
            });
            return;
        }
        let free = (queue.max_concurrent as usize).saturating_sub(active);
        for (_, id) in pending.into_iter().take(free) {
            if let Err(e) = self.start_locked(&mut state, id) {
                log::warn!("queue could not start download {id}: {e}");
            }
        }
    }

    fn checkpoint(&self) {
        let mut state = self.state.lock();
        let State { records, tasks, .. } = &mut *state;
        for (id, task) in tasks.iter() {
            if let Some(record) = records.get_mut(id) {
                absorb(record, task);
                self.dirty.store(true, Ordering::Release);
            }
        }
    }

    fn save(&self) {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let file = {
            let state = self.state.lock();
            StateFile {
                version: 1,
                next_id: state.next_id,
                next_queue_order: state.next_queue_order,
                downloads: state.records.values().cloned().collect(),
            }
        };
        if let Err(e) = persist::save_state(&self.data_dir, &file) {
            log::error!("could not save downloads: {e}");
            self.mark_dirty();
        }
    }

    fn run_scheduler(self: &Arc<Self>) {
        let schedule = self.settings.read().queue.schedule.clone();
        if !schedule.start_enabled && !schedule.stop_enabled {
            return;
        }
        let now = Local::now();
        if schedule.days & (1 << now.weekday().num_days_from_monday()) == 0 {
            return;
        }
        let (hour, minute) = (now.hour(), now.minute());
        let fire_start = schedule.start_enabled && matches_time(schedule.start_at, hour, minute);
        let fire_stop = schedule.stop_enabled && matches_time(schedule.stop_at, hour, minute);
        if !fire_start && !fire_stop {
            return;
        }
        let key = (now.year(), now.ordinal(), hour * 60 + minute, fire_start);
        {
            let mut state = self.state.lock();
            if state.last_schedule_fire == Some(key) {
                return;
            }
            state.last_schedule_fire = Some(key);
        }
        if fire_start {
            log::info!("scheduler: starting the queue");
            self.start_queue();
        } else {
            log::info!("scheduler: stopping the queue");
            self.stop_queue();
        }
    }

    fn maintenance(self: &Arc<Self>, tick: u64) {
        self.checkpoint();
        self.pump_queue();
        self.run_scheduler();
        if tick.is_multiple_of(2) {
            self.save();
        }
    }

    /// Starts, restarts or stops the browser endpoint to match the settings.
    fn sync_integration(self: &Arc<Self>) {
        let (enabled, port) = {
            let settings = self.settings.read();
            (settings.browser_integration, settings.integration_port)
        };
        let mut current = self.integration.lock();
        if let Some((running_port, cancel)) = current.as_ref() {
            if enabled && *running_port == port {
                return;
            }
            cancel.cancel();
            *current = None;
        }
        if !enabled {
            return;
        }
        let cancel = CancellationToken::new();
        let weak = Arc::downgrade(self);
        let sink: integration::CaptureSink = Arc::new(move |request: AddRequest| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.settings.read().start_captured_immediately {
                if let Err(e) = inner.add(request) {
                    log::warn!("captured download rejected: {e}");
                }
            } else {
                inner.push_event(EngineEvent::Captured(Box::new(request)));
            }
        });
        self.handle.spawn(integration::serve(port, cancel.clone(), sink));
        *current = Some((port, cancel));
    }
}
