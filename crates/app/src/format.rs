use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use tachyon_engine::{DownloadSnapshot, DownloadStatus};

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

pub fn speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        return String::new();
    }
    format!("{}/s", bytes(bytes_per_sec))
}

pub fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

pub fn date(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|t| Local.timestamp_opt(t, 0).single())
        .map(|t| t.format("%b %-d, %Y %H:%M").to_string())
        .unwrap_or_default()
}

pub fn size_cell(d: &DownloadSnapshot) -> String {
    match d.size {
        Some(size) => bytes(size),
        None if d.downloaded > 0 => bytes(d.downloaded),
        None => String::new(),
    }
}

/// The "Status" column, worded like IDM.
pub fn status_cell(d: &DownloadSnapshot) -> String {
    match d.status {
        DownloadStatus::Completed => "Complete".into(),
        DownloadStatus::Failed => "Error".into(),
        DownloadStatus::Connecting => "Connecting…".into(),
        DownloadStatus::Finalizing => "Finishing…".into(),
        DownloadStatus::Downloading | DownloadStatus::Stopped => match d.progress() {
            Some(p) if d.downloaded > 0 => format!("{:.2}%", p * 100.0),
            None if d.downloaded > 0 => bytes(d.downloaded),
            _ if d.status == DownloadStatus::Downloading => "Receiving…".into(),
            _ if d.in_queue => "Queued".into(),
            _ => String::new(),
        },
    }
}

pub fn eta_cell(d: &DownloadSnapshot) -> String {
    match (d.status, d.eta_secs) {
        (DownloadStatus::Downloading, Some(eta)) => duration(eta),
        _ => String::new(),
    }
}

pub fn status_line(d: &DownloadSnapshot) -> String {
    match d.status {
        DownloadStatus::Stopped if d.downloaded > 0 => "Paused".into(),
        DownloadStatus::Stopped => "Waiting to start".into(),
        DownloadStatus::Connecting => "Connecting…".into(),
        DownloadStatus::Downloading => "Receiving data…".into(),
        DownloadStatus::Finalizing => "Writing file…".into(),
        DownloadStatus::Completed => "Complete".into(),
        DownloadStatus::Failed => format!("Error: {}", d.error.as_deref().unwrap_or("unknown")),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Shows paths under the home folder as `~/…`.
pub fn display_path(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// Inverse of [`display_path`]: expands a leading `~`.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

/// Parses "2.5", "512k", "3 MB" etc. as KB/s and returns bytes per second.
pub fn parse_rate_kb(input: &str) -> Option<u64> {
    let s = input.trim().to_ascii_lowercase().replace(' ', "");
    if s.is_empty() {
        return Some(0);
    }
    let (num, mult) = if let Some(n) = s.strip_suffix("mb").or_else(|| s.strip_suffix('m')) {
        (n, 1024.0 * 1024.0)
    } else if let Some(n) = s.strip_suffix("kb").or_else(|| s.strip_suffix('k')) {
        (n, 1024.0)
    } else {
        (s.as_str(), 1024.0)
    };
    let value: f64 = num.parse().ok()?;
    (value >= 0.0).then_some((value * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.50 KB");
        assert_eq!(bytes(250 * 1024 * 1024), "250 MB");
        assert_eq!(duration(3725), "1h 02m");
        assert_eq!(duration(59), "59s");
    }

    #[test]
    fn parses_rates() {
        assert_eq!(parse_rate_kb("100"), Some(100 * 1024));
        assert_eq!(parse_rate_kb("2 MB"), Some(2 * 1024 * 1024));
        assert_eq!(parse_rate_kb(""), Some(0));
        assert_eq!(parse_rate_kb("abc"), None);
    }
}
