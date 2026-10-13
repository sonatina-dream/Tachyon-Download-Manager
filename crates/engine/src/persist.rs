use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsutil::write_atomic;
use crate::types::{
    CompletionOptions, DownloadId, DownloadStatus, RequestSpec, SegmentState, Settings,
};

/// A download as stored on disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DownloadRecord {
    pub id: DownloadId,
    pub spec: RequestSpec,
    pub final_url: Option<String>,
    pub file_name: String,
    /// The name was chosen by the user or by a probe, so the server must not rename it.
    pub name_locked: bool,
    pub directory: PathBuf,
    pub category: String,
    pub description: String,
    pub size: Option<u64>,
    pub resumable: Option<bool>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub status: DownloadStatus,
    pub error: Option<String>,
    pub segments: Vec<SegmentState>,
    pub downloaded: u64,
    pub added_at: i64,
    pub last_try: Option<i64>,
    pub completed_at: Option<i64>,
    pub in_queue: bool,
    pub queue_order: u64,
    pub queue_failures: u32,
    /// Bytes per second; 0 means unlimited.
    pub speed_limit: u64,
    pub completion: CompletionOptions,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct StateFile {
    pub version: u32,
    pub next_id: DownloadId,
    pub next_queue_order: u64,
    pub downloads: Vec<DownloadRecord>,
}

const STATE_FILE: &str = "downloads.json";
const SETTINGS_FILE: &str = "settings.json";

pub(crate) fn load_state(dir: &Path) -> StateFile {
    let path = dir.join(STATE_FILE);
    let mut state: StateFile = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            log::error!("{} is unreadable ({e}); starting with an empty list", path.display());
            let backup = path.with_extension("json.corrupt");
            let _ = std::fs::rename(&path, backup);
            StateFile::default()
        }),
        Err(_) => StateFile::default(),
    };
    for record in &mut state.downloads {
        // Anything that was running when the app last quit is now stopped.
        if record.status.is_active() {
            record.status = DownloadStatus::Stopped;
        }
    }
    state.next_id = state
        .next_id
        .max(state.downloads.iter().map(|d| d.id + 1).max().unwrap_or(1));
    state
}

pub(crate) fn save_state(dir: &Path, state: &StateFile) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    write_atomic(&dir.join(STATE_FILE), &bytes)
}

pub(crate) fn load_settings(dir: &Path) -> Settings {
    std::fs::read(dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .unwrap_or_default()
        .sanitized()
}

pub(crate) fn save_settings(dir: &Path, settings: &Settings) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    write_atomic(&dir.join(SETTINGS_FILE), &bytes)
}
