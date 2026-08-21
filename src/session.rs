//! Output naming and session-boundary rules.
//!
//! A default recording uses one directory. The recorder writes numbered MP3
//! parts into that directory. This module keeps path selection independent
//! from WASAPI and Media Foundation so that tests can cover every naming rule.

use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

/// The default length of one MP3 part.
pub const DEFAULT_SEGMENT_DURATION: Duration = Duration::from_secs(10 * 60);

/// The maximum number of Unicode characters in a clip file stem.
pub const MAX_CLIP_STEM_CHARS: usize = 80;

/// Selects the files that one recorder process can create.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputTarget {
    /// Write one MP3 and keep the legacy `-o` behavior.
    SingleFile {
        /// Destination of the MP3 file.
        path: PathBuf,
        /// Replace the file when it already exists.
        replace: bool,
    },
    /// Write numbered MP3 parts into one directory.
    Session {
        /// Directory that contains every file from this process.
        directory: PathBuf,
        /// Maximum encoded duration of each automatic part.
        segment_duration: Duration,
    },
}

impl OutputTarget {
    /// Returns the file or directory shown to the user.
    #[must_use]
    pub fn root(&self) -> &Path {
        match self {
            Self::SingleFile { path, .. } => path,
            Self::Session { directory, .. } => directory,
        }
    }

    /// Reports whether the target supports rotation and named clips.
    #[must_use]
    pub const fn is_session(&self) -> bool {
        matches!(self, Self::Session { .. })
    }
}

/// Describes an invalid output or clip name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameError {
    message: &'static str,
}

impl NameError {
    const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for NameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for NameError {}

/// Adds an MP3 extension or rejects a different extension.
pub fn mp3_output_path(requested: &Path) -> Result<PathBuf, NameError> {
    let mut output = requested.to_path_buf();
    if output.extension().is_none() {
        output.set_extension("mp3");
    } else if !output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
    {
        return Err(NameError::new(
            "the output file must use the .mp3 extension",
        ));
    }
    Ok(output)
}

/// Creates a unique `recording-TIMESTAMP` directory below `parent`.
///
/// This function uses `create_dir` for atomic selection. A concurrent process
/// cannot claim the same session directory between a check and its creation.
pub fn create_session_directory(parent: &Path, timestamp: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(parent)?;
    for suffix in 1..10_000 {
        let name = if suffix == 1 {
            format!("recording-{timestamp}")
        } else {
            format!("recording-{timestamp}-{suffix}")
        };
        let candidate = parent.join(name);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not choose a unique recording directory",
    ))
}

/// Returns the path for a one-based numbered session part.
#[must_use]
pub fn part_path(directory: &Path, index: u32) -> PathBuf {
    directory.join(format!("part-{index:03}.mp3"))
}

/// Validates a user-provided clip name and returns its MP3 file stem.
///
/// The result never contains a directory separator. The function also rejects
/// Windows device names because those names cannot identify ordinary files.
pub fn clip_stem(requested: &str) -> Result<String, NameError> {
    let trimmed = requested.trim();
    let stem = match trimmed.rsplit_once('.') {
        Some((stem, extension)) if extension.eq_ignore_ascii_case("mp3") => stem,
        _ => trimmed,
    };

    if stem.is_empty() {
        return Err(NameError::new("enter a file name"));
    }
    if stem == "." || stem == ".." {
        return Err(NameError::new("the file name cannot be a dot path"));
    }
    if stem.ends_with(' ') || stem.ends_with('.') {
        return Err(NameError::new(
            "the file name cannot end with a space or dot",
        ));
    }
    if stem.chars().count() > MAX_CLIP_STEM_CHARS {
        return Err(NameError::new("the file name is too long"));
    }
    if stem
        .chars()
        .any(|character| character.is_control() || r#"\/:*?"<>|"#.contains(character))
    {
        return Err(NameError::new(
            "the file name contains a character that Windows does not allow",
        ));
    }

    let device = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    let numbered_device = device
        .strip_prefix("COM")
        .or_else(|| device.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    if matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || numbered_device
    {
        return Err(NameError::new("the file name is reserved by Windows"));
    }

    Ok(stem.to_owned())
}

/// Chooses a non-existing path for a validated clip name.
///
/// `current_part` can equal the first candidate. This case occurs when a user
/// names a clip `part-001`. The current file can keep that name safely.
pub fn available_clip_path(
    directory: &Path,
    requested: &str,
    current_part: &Path,
) -> Result<PathBuf, NameError> {
    let stem = clip_stem(requested)?;
    for suffix in 1..10_000 {
        let name = if suffix == 1 {
            format!("{stem}.mp3")
        } else {
            format!("{stem}-{suffix}.mp3")
        };
        let candidate = directory.join(name);
        if candidate == current_part || !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(NameError::new("could not choose a unique clip file name"))
}

/// Converts a duration to a whole number of encoded audio frames.
///
/// The result is at least one frame. Integer arithmetic makes exact durations,
/// such as ten minutes at 48 kHz, independent from floating-point rounding.
#[must_use]
pub fn segment_frame_limit(duration: Duration, sample_rate: u32) -> u64 {
    let nanoseconds = duration.as_nanos();
    let frames = nanoseconds.saturating_mul(u128::from(sample_rate)) / 1_000_000_000;
    u64::try_from(frames).unwrap_or(u64::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn adds_an_mp3_extension() {
        assert_eq!(
            mp3_output_path(Path::new("take-one")).unwrap(),
            PathBuf::from("take-one.mp3")
        );
    }

    #[test]
    fn preserves_an_mp3_extension_without_changing_case() {
        assert_eq!(
            mp3_output_path(Path::new("take-one.MP3")).unwrap(),
            PathBuf::from("take-one.MP3")
        );
    }

    #[test]
    fn rejects_a_different_output_extension() {
        assert!(mp3_output_path(Path::new("take-one.wav")).is_err());
    }

    #[test]
    fn creates_unique_session_directories() {
        let parent = tempdir().unwrap();
        let first = create_session_directory(parent.path(), "20260820-190800").unwrap();
        let second = create_session_directory(parent.path(), "20260820-190800").unwrap();
        assert_eq!(first.file_name().unwrap(), "recording-20260820-190800");
        assert_eq!(second.file_name().unwrap(), "recording-20260820-190800-2");
    }

    #[test]
    fn formats_part_numbers_with_a_minimum_width() {
        let directory = Path::new("session");
        assert_eq!(part_path(directory, 1), directory.join("part-001.mp3"));
        assert_eq!(part_path(directory, 1_024), directory.join("part-1024.mp3"));
    }

    #[test]
    fn accepts_a_clip_name_with_or_without_the_extension() {
        assert_eq!(clip_stem("  intro  ").unwrap(), "intro");
        assert_eq!(clip_stem("intro.MP3").unwrap(), "intro");
    }

    #[test]
    fn rejects_unsafe_and_reserved_clip_names() {
        for name in [
            "",
            ".",
            "..",
            "../take",
            r"folder\take",
            "bad:name",
            "take.",
            "NUL",
            "CONIN$",
            "CONOUT$.txt",
            "CLOCK$",
            "com1.mp3",
            "LPT9.notes",
        ] {
            assert!(clip_stem(name).is_err(), "{name:?} should be invalid");
        }
    }

    #[test]
    fn rejects_a_clip_name_over_the_character_limit() {
        let name = "é".repeat(MAX_CLIP_STEM_CHARS + 1);
        assert!(clip_stem(&name).is_err());
    }

    #[test]
    fn adds_a_suffix_when_a_clip_name_exists() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("intro.mp3"), []).unwrap();
        let current = directory.path().join("part-001.mp3");
        assert_eq!(
            available_clip_path(directory.path(), "intro", &current).unwrap(),
            directory.path().join("intro-2.mp3")
        );
    }

    #[test]
    fn permits_the_current_part_path_as_a_clip_path() {
        let directory = tempdir().unwrap();
        let current = directory.path().join("part-001.mp3");
        fs::write(&current, []).unwrap();
        assert_eq!(
            available_clip_path(directory.path(), "part-001", &current).unwrap(),
            current
        );
    }

    #[test]
    fn converts_ten_minutes_to_exact_frame_counts() {
        assert_eq!(
            segment_frame_limit(DEFAULT_SEGMENT_DURATION, 48_000),
            28_800_000
        );
        assert_eq!(
            segment_frame_limit(DEFAULT_SEGMENT_DURATION, 44_100),
            26_460_000
        );
    }

    #[test]
    fn gives_a_positive_limit_for_a_sub_frame_duration() {
        assert_eq!(segment_frame_limit(Duration::from_nanos(1), 48_000), 1);
    }

    #[test]
    fn output_targets_report_their_root_and_capabilities() {
        let single = OutputTarget::SingleFile {
            path: PathBuf::from("take.mp3"),
            replace: false,
        };
        let session = OutputTarget::Session {
            directory: PathBuf::from("session"),
            segment_duration: DEFAULT_SEGMENT_DURATION,
        };
        assert_eq!(single.root(), Path::new("take.mp3"));
        assert!(!single.is_session());
        assert_eq!(session.root(), Path::new("session"));
        assert!(session.is_session());
    }

    #[test]
    fn clip_collision_suffixes_continue_until_a_free_path() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("intro.mp3"), []).unwrap();
        fs::write(directory.path().join("intro-2.mp3"), []).unwrap();
        assert_eq!(
            available_clip_path(directory.path(), "intro", Path::new("part-001.mp3")).unwrap(),
            directory.path().join("intro-3.mp3")
        );
    }

    #[test]
    fn maximum_length_uses_characters_instead_of_utf8_bytes() {
        let name = "é".repeat(MAX_CLIP_STEM_CHARS);
        assert_eq!(clip_stem(&name).unwrap(), name);
    }

    #[test]
    fn unicode_names_without_an_extension_do_not_require_a_byte_boundary() {
        assert_eq!(clip_stem("éaé").unwrap(), "éaé");
    }

    #[test]
    fn zero_rate_and_zero_duration_still_return_a_safe_frame_limit() {
        assert_eq!(segment_frame_limit(Duration::ZERO, 48_000), 1);
        assert_eq!(segment_frame_limit(Duration::from_secs(10), 0), 1);
    }
}
