use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(
            (concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg"))),
        )*]
    };
}

static ICONS: &[(&str, &[u8])] = icons![
    "add", "alert", "app", "archive", "arrow-down", "bolt", "calendar", "check-circle", "check",
    "chevron-down", "chevron-right", "chevron-up", "clock", "copy", "document", "external", "file",
    "folder", "globe", "inbox", "info", "list", "music", "pause", "play",
    "queue-play", "queue-stop", "refresh", "sliders", "stop-all", "trash-check", "trash", "video",
    "x",
];

/// Serves the icons compiled into the binary.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

/// Icon names, so typos fail to compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Add,
    Alert,
    App,
    Archive,
    ArrowDown,
    Bolt,
    Calendar,
    Check,
    CheckCircle,
    ChevronDown,
    ChevronRight,
    ChevronUp,
    Clock,
    Copy,
    Document,
    External,
    File,
    Folder,
    Globe,
    Inbox,
    Info,
    List,
    Music,
    Pause,
    Play,
    QueuePlay,
    QueueStop,
    Refresh,
    Sliders,
    StopAll,
    Trash,
    TrashCheck,
    Video,
    X,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Self::Add => "icons/add.svg",
            Self::Alert => "icons/alert.svg",
            Self::App => "icons/app.svg",
            Self::Archive => "icons/archive.svg",
            Self::ArrowDown => "icons/arrow-down.svg",
            Self::Bolt => "icons/bolt.svg",
            Self::Calendar => "icons/calendar.svg",
            Self::Check => "icons/check.svg",
            Self::CheckCircle => "icons/check-circle.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronUp => "icons/chevron-up.svg",
            Self::Clock => "icons/clock.svg",
            Self::Copy => "icons/copy.svg",
            Self::Document => "icons/document.svg",
            Self::External => "icons/external.svg",
            Self::File => "icons/file.svg",
            Self::Folder => "icons/folder.svg",
            Self::Globe => "icons/globe.svg",
            Self::Inbox => "icons/inbox.svg",
            Self::Info => "icons/info.svg",
            Self::List => "icons/list.svg",
            Self::Music => "icons/music.svg",
            Self::Pause => "icons/pause.svg",
            Self::Play => "icons/play.svg",
            Self::QueuePlay => "icons/queue-play.svg",
            Self::QueueStop => "icons/queue-stop.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::Sliders => "icons/sliders.svg",
            Self::StopAll => "icons/stop-all.svg",
            Self::Trash => "icons/trash.svg",
            Self::TrashCheck => "icons/trash-check.svg",
            Self::Video => "icons/video.svg",
            Self::X => "icons/x.svg",
        }
    }

    /// Icon for a download category.
    pub fn for_category(category: &str) -> Self {
        match category {
            "Compressed" => Self::Archive,
            "Documents" => Self::Document,
            "Music" => Self::Music,
            "Programs" => Self::App,
            "Video" => Self::Video,
            _ => Self::File,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_embedded() {
        let all = [
            Icon::Add, Icon::Alert, Icon::App, Icon::Archive, Icon::ArrowDown, Icon::Bolt,
            Icon::Calendar, Icon::Check, Icon::CheckCircle, Icon::ChevronDown, Icon::ChevronRight,
            Icon::ChevronUp, Icon::Clock, Icon::Copy, Icon::Document, Icon::External, Icon::File,
            Icon::Folder, Icon::Globe, Icon::Inbox, Icon::Info, Icon::List,
            Icon::Music, Icon::Pause, Icon::Play, Icon::QueuePlay, Icon::QueueStop, Icon::Refresh,
            Icon::Sliders, Icon::StopAll, Icon::Trash, Icon::TrashCheck, Icon::Video, Icon::X,
        ];
        for icon in all {
            assert!(Assets.load(icon.path()).unwrap().is_some(), "{icon:?}");
        }
    }
}
