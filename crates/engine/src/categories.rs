use std::path::{Path, PathBuf};

use crate::types::{CategoryDef, Settings};

/// Category for files that match no extension list.
pub const GENERAL: &str = "General";

fn def(name: &str, exts: &str) -> CategoryDef {
    CategoryDef {
        name: name.to_string(),
        extensions: exts.split_whitespace().map(str::to_string).collect(),
        folder: None,
    }
}

/// IDM's predefined categories, with macOS formats added.
pub fn default_categories() -> Vec<CategoryDef> {
    vec![
        def(
            "Compressed",
            "zip rar 7z gz tgz bz2 tbz xz txz zst tar arj ace sit sitx sea lz lzma cab z",
        ),
        def(
            "Documents",
            "pdf doc docx xls xlsx ppt pptx pps odt ods odp rtf txt csv md epub mobi pages numbers key",
        ),
        def(
            "Music",
            "mp3 wav wma mpa aac aif aiff m4a flac ogg oga opus alac mid midi ra ram",
        ),
        def(
            "Programs",
            "dmg pkg mpkg app iso img exe msi apk deb rpm appimage jar",
        ),
        def(
            "Video",
            "mp4 m4v mkv mov avi wmv flv webm mpg mpeg mpe asf qt rm rmvb 3gp ogv ts m2ts vob",
        ),
    ]
}

/// Extension of a file name, lowercase, handling `.tar.gz`-style names by the last part.
pub fn extension_of(file_name: &str) -> Option<String> {
    Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Picks the category for a file name by its extension.
pub fn classify(file_name: &str, categories: &[CategoryDef]) -> String {
    if let Some(ext) = extension_of(file_name) {
        for category in categories {
            if category.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
                return category.name.clone();
            }
        }
    }
    GENERAL.to_string()
}

/// The folder new downloads in `category` are saved to.
pub fn folder_for(category: &str, settings: &Settings) -> PathBuf {
    if let Some(folder) = settings
        .categories
        .iter()
        .find(|c| c.name == category)
        .and_then(|c| c.folder.clone())
    {
        return folder;
    }
    if settings.use_category_folders && category != GENERAL {
        settings.download_dir.join(category)
    } else {
        settings.download_dir.clone()
    }
}

/// Whether a URL looks like a direct link to a downloadable file, for clipboard monitoring.
pub fn looks_downloadable(url: &str, categories: &[CategoryDef]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let last = parsed
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or_default();
    classify(last, categories) != GENERAL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension() {
        let cats = default_categories();
        assert_eq!(classify("movie.MKV", &cats), "Video");
        assert_eq!(classify("archive.tar.gz", &cats), "Compressed");
        assert_eq!(classify("Installer.dmg", &cats), "Programs");
        assert_eq!(classify("README", &cats), GENERAL);
    }

    #[test]
    fn detects_downloadable_links() {
        let cats = default_categories();
        assert!(looks_downloadable("https://example.com/a/b/file.zip?x=1", &cats));
        assert!(!looks_downloadable("https://example.com/page.html", &cats));
        assert!(!looks_downloadable("ftp://example.com/file.zip", &cats));
    }
}
