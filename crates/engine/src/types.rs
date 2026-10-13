use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::categories::default_categories;

pub type DownloadId = u64;

/// Persisted lifecycle state of a download.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadStatus {
    /// Not running and not finished (never started, paused or interrupted).
    #[default]
    Stopped,
    /// Resolving the address and waiting for the server to answer.
    Connecting,
    /// At least one connection is receiving data.
    Downloading,
    /// All bytes received; flushing and moving the file into place.
    Finalizing,
    Completed,
    Failed,
}

impl DownloadStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Connecting | Self::Downloading | Self::Finalizing)
    }
}

/// Byte range owned by one connection: `[start, end)`, with `pos` the next byte to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentState {
    pub start: u64,
    pub pos: u64,
    pub end: u64,
}

impl SegmentState {
    pub fn is_complete(&self) -> bool {
        self.pos >= self.end
    }

    pub fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }

    pub fn downloaded(&self) -> u64 {
        self.pos.saturating_sub(self.start)
    }
}

/// What to do after a download or a queue finishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostAction {
    #[default]
    Nothing,
    QuitApp,
    Sleep,
    ShutDown,
}

impl PostAction {
    pub const ALL: [PostAction; 4] = [Self::Nothing, Self::QuitApp, Self::Sleep, Self::ShutDown];

    pub fn label(self) -> &'static str {
        match self {
            Self::Nothing => "Do nothing",
            Self::QuitApp => "Quit Tachyon",
            Self::Sleep => "Put the Mac to sleep",
            Self::ShutDown => "Shut down the Mac",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompletionOptions {
    pub show_dialog: bool,
    pub open_file: bool,
    pub post_action: PostAction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Everything needed to (re)issue requests for a download.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestSpec {
    pub url: String,
    pub referer: Option<String>,
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    pub credentials: Option<Credentials>,
}

impl RequestSpec {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }
}

/// A request to add a new download.
#[derive(Clone, Debug, Default)]
pub struct AddRequest {
    pub request: RequestSpec,
    /// Overrides the file name derived from the server/URL.
    pub file_name: Option<String>,
    /// Overrides the category/default folder.
    pub directory: Option<PathBuf>,
    pub category: Option<String>,
    pub description: String,
    /// Start immediately; otherwise the download waits (in the queue if `queue` is set).
    pub start: bool,
    pub queue: bool,
    /// Size/resume info from an earlier probe, shown in the list before the download starts.
    pub probed: Option<ProbeInfo>,
}

/// Metadata learned from the server before downloading.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeInfo {
    pub final_url: String,
    pub total: Option<u64>,
    pub resumable: bool,
    pub file_name: String,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Per-connection state displayed in the progress window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Receiving,
    Retrying { attempt: u32, reason: String },
    Idle,
    Finished,
    /// Closed to reduce load after the server throttled us; its work was handed back.
    Released(String),
    Failed(String),
}

impl ConnectionState {
    pub fn label(&self) -> String {
        match self {
            Self::Connecting => "Connecting…".into(),
            Self::Receiving => "Receiving data…".into(),
            Self::Retrying { attempt, reason } => format!("Retry #{attempt}: {reason}"),
            Self::Idle => "Idle".into(),
            Self::Finished => "Finished".into(),
            Self::Released(reason) => format!("Closed ({reason}); using fewer connections"),
            Self::Failed(e) => format!("Error: {e}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionSnapshot {
    pub index: usize,
    pub downloaded: u64,
    pub state: ConnectionState,
}

/// A point-in-time view of a download for the UI.
#[derive(Clone, Debug)]
pub struct DownloadSnapshot {
    pub id: DownloadId,
    pub url: String,
    pub file_name: String,
    pub directory: PathBuf,
    pub category: String,
    pub description: String,
    pub size: Option<u64>,
    pub downloaded: u64,
    pub status: DownloadStatus,
    /// Bytes per second over the last few seconds.
    pub speed: u64,
    pub eta_secs: Option<u64>,
    /// `None` until the server has been contacted.
    pub resumable: Option<bool>,
    pub error: Option<String>,
    pub added_at: i64,
    pub last_try: Option<i64>,
    pub completed_at: Option<i64>,
    pub in_queue: bool,
    pub queue_order: u64,
    pub speed_limit: u64,
    pub active_connections: usize,
    pub completion: CompletionOptions,
}

impl DownloadSnapshot {
    pub fn path(&self) -> PathBuf {
        self.directory.join(&self.file_name)
    }

    pub fn progress(&self) -> Option<f64> {
        match self.size {
            Some(0) => Some(1.0),
            Some(total) => Some((self.downloaded as f64 / total as f64).clamp(0.0, 1.0)),
            None => None,
        }
    }
}

/// Segment map and per-connection state for the progress window.
#[derive(Clone, Debug, Default)]
pub struct DownloadDetails {
    pub segments: Vec<SegmentState>,
    pub connections: Vec<ConnectionSnapshot>,
}

/// Notifications for the UI, drained with [`crate::Engine::drain_events`].
#[derive(Clone, Debug)]
pub enum EngineEvent {
    Completed(DownloadId),
    Failed { id: DownloadId, error: String },
    /// The queue ran out of work after being started.
    QueueFinished { post_action: PostAction },
    /// A download was handed over by the browser extension and awaits confirmation.
    Captured(Box<AddRequest>),
    /// A download asked for a system action once it finished.
    PostAction(PostAction),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryDef {
    pub name: String,
    pub extensions: Vec<String>,
    /// Folder override; defaults to `<download_dir>/<name>`.
    pub folder: Option<PathBuf>,
}

/// Time of day as minutes after midnight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayTime {
    pub hour: u8,
    pub minute: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleSettings {
    pub start_enabled: bool,
    pub start_at: DayTime,
    pub stop_enabled: bool,
    pub stop_at: DayTime,
    /// Monday = bit 0 … Sunday = bit 6.
    pub days: u8,
}

impl Default for ScheduleSettings {
    fn default() -> Self {
        Self {
            start_enabled: false,
            start_at: DayTime { hour: 2, minute: 0 },
            stop_enabled: false,
            stop_at: DayTime { hour: 8, minute: 0 },
            days: 0b111_1111,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueSettings {
    pub max_concurrent: u32,
    /// How many times a failed file in the queue is retried before it is skipped.
    pub retries: u32,
    pub post_action: PostAction,
    pub schedule: ScheduleSettings,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            retries: 3,
            post_action: PostAction::Nothing,
            schedule: ScheduleSettings::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Maximum connections per download (IDM's "Max. connections number").
    pub max_connections: u32,
    /// A segment is only split if both halves would be at least this big.
    pub min_split_size: u64,
    pub max_retries: u32,
    pub retry_delay_secs: u64,
    pub connect_timeout_secs: u64,
    pub read_timeout_secs: u64,
    /// Bytes per second across all downloads; 0 means unlimited.
    pub global_speed_limit: u64,
    pub download_dir: PathBuf,
    pub use_category_folders: bool,
    pub user_agent: String,
    /// `http://`, `https://` or `socks5://` proxy URL; empty uses the system proxy.
    pub proxy: String,
    pub show_progress_window: bool,
    pub show_complete_dialog: bool,
    pub monitor_clipboard: bool,
    pub browser_integration: bool,
    pub integration_port: u16,
    /// Start browser-captured downloads without asking first.
    pub start_captured_immediately: bool,
    pub categories: Vec<CategoryDef>,
    pub queue: QueueSettings,
}

impl Default for Settings {
    fn default() -> Self {
        let download_dir = dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            max_connections: 8,
            min_split_size: 1024 * 1024,
            max_retries: 10,
            retry_delay_secs: 3,
            connect_timeout_secs: 20,
            read_timeout_secs: 30,
            global_speed_limit: 0,
            download_dir,
            use_category_folders: true,
            user_agent: format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Tachyon/{}",
                env!("CARGO_PKG_VERSION")
            ),
            proxy: String::new(),
            show_progress_window: true,
            show_complete_dialog: true,
            monitor_clipboard: false,
            browser_integration: true,
            integration_port: 7311,
            start_captured_immediately: false,
            categories: default_categories(),
            queue: QueueSettings::default(),
        }
    }
}

impl Settings {
    pub const MAX_CONNECTIONS_LIMIT: u32 = 32;

    /// Clamps values that would break the engine.
    pub fn sanitized(mut self) -> Self {
        self.max_connections = self.max_connections.clamp(1, Self::MAX_CONNECTIONS_LIMIT);
        self.min_split_size = self.min_split_size.max(256 * 1024);
        self.connect_timeout_secs = self.connect_timeout_secs.clamp(1, 600);
        self.read_timeout_secs = self.read_timeout_secs.clamp(1, 600);
        self.retry_delay_secs = self.retry_delay_secs.min(600);
        self.queue.max_concurrent = self.queue.max_concurrent.clamp(1, 32);
        self
    }
}
