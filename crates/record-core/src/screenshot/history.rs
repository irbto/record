//! The list of saved screenshots shown by the hub.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Largest number of files that the hub lists.
const HISTORY_LIMIT: usize = 500;

/// One saved screenshot file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    /// Full path of the image file.
    pub path: PathBuf,
    /// File name without its directory.
    pub name: String,
    /// Last modification time, used for the newest-first order.
    pub modified: SystemTime,
}

/// Lists the image files of a directory, newest first.
///
/// A missing or unreadable directory gives an empty list.
#[must_use]
pub fn list_history(directory: &Path) -> Vec<HistoryEntry> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut history: Vec<HistoryEntry> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let is_png = super::format::is_image_file(&path);
            let metadata = entry.metadata().ok()?;
            (is_png && metadata.is_file()).then(|| HistoryEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                path,
            })
        })
        .collect();
    history.sort_by(|first, second| {
        second
            .modified
            .cmp(&first.modified)
            .then_with(|| second.name.cmp(&first.name))
    });
    history.truncate(HISTORY_LIMIT);
    history
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_png_files_with_the_newest_first() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["a.png", "b.PNG", "notes.txt"] {
            fs::write(directory.path().join(name), b"x").unwrap();
        }
        fs::create_dir(directory.path().join("folder.png")).unwrap();
        let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(directory.path().join("a.png"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let names: Vec<String> = list_history(directory.path())
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, ["b.PNG", "a.png"]);
    }

    #[test]
    fn a_missing_directory_is_an_empty_history() {
        assert!(list_history(Path::new("this/folder/does/not/exist")).is_empty());
    }
}
