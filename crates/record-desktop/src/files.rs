//! Lists of saved files and the check that keeps file commands in bounds.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::payload::FileEntry;

/// Largest number of files that one list returns.
pub const FILE_LIMIT: usize = 200;

/// Lists the files of one folder with the given extension, newest first.
///
/// A missing or unreadable folder gives an empty list.
#[must_use]
pub fn recent_files(folder: &Path, extension: &str, now: SystemTime) -> Vec<FileEntry> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let matches = path
                .extension()
                .is_some_and(|found| found.eq_ignore_ascii_case(extension));
            let metadata = entry.metadata().ok()?;
            (matches && metadata.is_file())
                .then(|| (metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path))
        })
        .collect();
    files.sort_by(|first, second| second.cmp(first));
    files.truncate(FILE_LIMIT);
    files
        .into_iter()
        .map(|(modified, path)| FileEntry {
            name: crate::payload::file_name(&path),
            age_seconds: now.duration_since(modified).map_or(0, |age| age.as_secs()),
            path: path.display().to_string(),
        })
        .collect()
}

/// Accepts a path only when the file lies directly in one of the folders.
///
/// The interface sends paths back to the engine. This check stops a path
/// that the program did not list from reaching the file commands.
pub fn within(folders: &[PathBuf], path: &Path) -> Result<PathBuf, String> {
    let refusal = || format!("{} is not a file of this program", path.display());
    let file = path.canonicalize().map_err(|_| refusal())?;
    let parent = file.parent().ok_or_else(refusal)?;
    let listed = folders
        .iter()
        .filter_map(|folder| folder.canonicalize().ok())
        .any(|folder| folder == parent);
    if listed && file.is_file() {
        Ok(file)
    } else {
        Err(refusal())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn lists_only_the_asked_extension_newest_first() {
        let folder = tempfile::tempdir().unwrap();
        for (name, age) in [("old.mp4", 90), ("new.MP4", 5), ("note.txt", 1)] {
            let path = folder.path().join(name);
            fs::write(&path, b"x").unwrap();
            let file = fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
        fs::create_dir(folder.path().join("folder.mp4")).unwrap();
        let files = recent_files(folder.path(), "mp4", SystemTime::now());
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["new.MP4", "old.mp4"]);
        assert!((5..8).contains(&files[0].age_seconds), "{files:?}");
        assert!(recent_files(&folder.path().join("missing"), "mp4", SystemTime::now()).is_empty());
    }

    #[test]
    fn file_commands_accept_only_listed_files() {
        let folder = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let inside = folder.path().join("shot.png");
        let outside = other.path().join("secret.png");
        fs::write(&inside, b"x").unwrap();
        fs::write(&outside, b"x").unwrap();
        fs::create_dir(folder.path().join("nested")).unwrap();
        let nested = folder.path().join("nested").join("deep.png");
        fs::write(&nested, b"x").unwrap();
        let folders = [folder.path().to_path_buf()];
        assert!(within(&folders, &inside).is_ok());
        assert!(
            within(
                &folders,
                &folder.path().join("nested").join("..").join("shot.png")
            )
            .is_ok()
        );
        for refused in [
            outside,
            nested,
            folder.path().join("missing.png"),
            folder.path().join("..").join("escape.png"),
            folder.path().to_path_buf(),
        ] {
            assert!(within(&folders, &refused).is_err(), "{}", refused.display());
        }
    }
}
