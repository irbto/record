//! Lists of saved files and the check that keeps file commands in bounds.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::payload::FileEntry;

/// Largest number of files that one list returns.
pub const FILE_LIMIT: usize = 200;

/// Collects the files of one folder with the given extension.
///
/// With `nested`, the folders directly inside it are read too.
fn collect(folder: &Path, extension: &str, nested: bool, files: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if nested {
                collect(&path, extension, false, files);
            }
        } else if path
            .extension()
            .is_some_and(|found| found.eq_ignore_ascii_case(extension))
        {
            files.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path));
        }
    }
}

/// Lists the files of one folder with the given extension, newest first.
///
/// With `nested`, the list also holds the files of the folders directly
/// inside it, named with their folder. An audio session is such a folder. A
/// missing or unreadable folder gives an empty list.
#[must_use]
pub fn recent_files(
    folder: &Path,
    extension: &str,
    nested: bool,
    now: SystemTime,
) -> Vec<FileEntry> {
    let mut files = Vec::new();
    collect(folder, extension, nested, &mut files);
    files.sort_by(|first, second| second.cmp(first));
    files.truncate(FILE_LIMIT);
    files
        .into_iter()
        .map(|(modified, path)| FileEntry {
            name: path.strip_prefix(folder).map_or_else(
                |_| crate::payload::file_name(&path),
                |inner| inner.display().to_string(),
            ),
            age_seconds: now.duration_since(modified).map_or(0, |age| age.as_secs()),
            path: path.display().to_string(),
        })
        .collect()
}

/// Accepts a path only when the file lies in one of the folders.
///
/// Each folder comes with a flag. Without the flag the file must lie directly
/// in the folder. With it, the file may also lie one folder deeper, as the
/// files of an audio session do. The interface sends paths back to the
/// engine. This check stops a path that the program did not list from
/// reaching the file commands.
pub fn within(folders: &[(PathBuf, bool)], path: &Path) -> Result<PathBuf, String> {
    let refusal = || format!("{} is not a file of this program", path.display());
    let file = path.canonicalize().map_err(|_| refusal())?;
    let parent = file.parent().ok_or_else(refusal)?;
    let listed = folders.iter().any(|(folder, nested)| {
        folder.canonicalize().is_ok_and(|folder| {
            folder == parent || (*nested && parent.parent() == Some(folder.as_path()))
        })
    });
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
        let session = folder.path().join("session");
        fs::create_dir(&session).unwrap();
        fs::write(session.join("part.mp4"), b"x").unwrap();
        fs::create_dir(session.join("deeper")).unwrap();
        fs::write(session.join("deeper").join("lost.mp4"), b"x").unwrap();
        let now = SystemTime::now();
        let files = recent_files(folder.path(), "mp4", false, now);
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["new.MP4", "old.mp4"]);
        assert!((5..8).contains(&files[0].age_seconds), "{files:?}");
        let nested = recent_files(folder.path(), "mp4", true, now);
        let inner = Path::new("session").join("part.mp4").display().to_string();
        let names: Vec<&str> = nested.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, [inner.as_str(), "new.MP4", "old.mp4"]);
        assert!(recent_files(&folder.path().join("missing"), "mp4", true, now).is_empty());
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
        let folders = [(folder.path().to_path_buf(), false)];
        assert!(within(&folders, &inside).is_ok());
        let open = [(folder.path().to_path_buf(), true)];
        assert!(within(&open, &nested).is_ok());
        assert!(within(&open, &outside).is_err());
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
