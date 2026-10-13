use std::path::{Path, PathBuf};

pub const PART_SUFFIX: &str = ".tachyon-part";

/// Where a download's bytes live until it completes.
pub fn part_path(dir: &Path, file_name: &str) -> PathBuf {
    dir.join(format!("{file_name}{PART_SUFFIX}"))
}

/// Splits `name` into stem and extension, keeping `.tar.*` together.
fn split_name(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for compound in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(compound) && name.len() > compound.len() {
            let at = name.len() - compound.len();
            return (&name[..at], &name[at..]);
        }
    }
    match name.rfind('.') {
        Some(at) if at > 0 => (&name[..at], &name[at..]),
        _ => (name, ""),
    }
}

/// Returns `name`, or `name (n)` with the smallest `n` such that neither the file
/// nor its partial download exists in `dir` and `taken` doesn't claim it.
pub fn unique_name(dir: &Path, name: &str, taken: impl Fn(&str) -> bool) -> String {
    first_free(name, |candidate| {
        !dir.join(candidate).exists() && !part_path(dir, candidate).exists() && !taken(candidate)
    })
}

/// Name to move a finished download to: `name`, numbered only if a file with that
/// name already exists (the download's own partial file doesn't count).
pub fn final_name(dir: &Path, name: &str) -> String {
    first_free(name, |candidate| !dir.join(candidate).exists())
}

fn first_free(name: &str, free: impl Fn(&str) -> bool) -> String {
    if free(name) {
        return name.to_string();
    }
    let (stem, ext) = split_name(name);
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| free(candidate))
        .expect("unbounded search")
}

/// Writes `contents` to `path` atomically via a temporary sibling file.
/// The file is private to the user since it can hold cookies and passwords.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.zip"), b"").unwrap();
        std::fs::write(dir.path().join("a (1).zip"), b"").unwrap();
        assert_eq!(unique_name(dir.path(), "a.zip", |_| false), "a (2).zip");
        assert_eq!(unique_name(dir.path(), "b.zip", |n| n == "b.zip"), "b (1).zip");
        std::fs::write(dir.path().join("x.tar.gz"), b"").unwrap();
        assert_eq!(unique_name(dir.path(), "x.tar.gz", |_| false), "x (1).tar.gz");
        std::fs::write(part_path(dir.path(), "p.bin"), b"").unwrap();
        assert_eq!(unique_name(dir.path(), "p.bin", |_| false), "p (1).bin");
        assert_eq!(unique_name(dir.path(), "README", |_| false), "README");
        assert_eq!(final_name(dir.path(), "p.bin"), "p.bin");
        assert_eq!(final_name(dir.path(), "a.zip"), "a (2).zip");
    }
}
