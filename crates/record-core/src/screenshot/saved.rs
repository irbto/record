//! The choices that the hub stores between runs.
//!
//! One small text file holds `name=value` lines. Unknown lines are ignored,
//! so an older version can read a file that a newer version wrote.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use super::{
    format::ImageFormat,
    hotkeys::{Bindings, Chord, HotkeyId, default_bindings},
    upload::{settings_directory, write_setting},
};

/// Name of the settings file.
const FILE: &str = "screenshot-settings.txt";
/// Name of the registry value that starts the tray process at sign-in.
const AUTOSTART_NAME: &str = "record-screenshot";
/// Registry key that Windows reads at sign-in for the current user.
const AUTOSTART_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Every stored default of `record screenshot`.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedSettings {
    /// Places each image on the clipboard.
    pub copy: bool,
    /// Writes each image to a PNG file.
    pub save: bool,
    /// Uploads each image to the default destination.
    pub upload: bool,
    /// Folder for screenshots. `None` selects `Pictures\Screenshots`.
    pub folder: Option<PathBuf>,
    /// File format of saved screenshots.
    pub format: ImageFormat,
    /// Seconds that the delayed captures wait.
    pub delay_seconds: f32,
    /// Seconds between auto captures.
    pub interval_seconds: f32,
    /// Captures the primary monitor in place of the monitor under the pointer.
    pub primary_monitor: bool,
    /// SDR white level for HDR monitors in nits. `None` asks Windows.
    pub sdr_white_nits: Option<f32>,
    /// The global hotkeys.
    pub hotkeys: Bindings,
    /// Seconds that the desktop program waits before a recording starts.
    pub countdown_seconds: u32,
}

/// Longest countdown before a recording, in seconds.
pub const COUNTDOWN_LIMIT: u32 = 60;

impl Default for SavedSettings {
    fn default() -> Self {
        Self {
            copy: true,
            save: true,
            upload: false,
            folder: None,
            format: ImageFormat::Png,
            delay_seconds: 3.0,
            interval_seconds: 5.0,
            primary_monitor: false,
            sdr_white_nits: None,
            hotkeys: default_bindings(),
            countdown_seconds: 0,
        }
    }
}

/// Parses the countdown before a recording. An empty text turns it off.
pub fn parse_countdown(text: &str) -> Result<u32, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    match text.parse::<f64>() {
        Ok(seconds) if (0.0..=f64::from(COUNTDOWN_LIMIT)).contains(&seconds) => {
            Ok(seconds.round() as u32)
        }
        _ => Err(format!(
            "enter a countdown from 0 to {COUNTDOWN_LIMIT} seconds"
        )),
    }
}

/// Parses a number of seconds inside the range that the hub accepts.
pub fn parse_seconds(text: &str) -> Result<f32, String> {
    match text.trim().parse::<f32>() {
        Ok(seconds) if (0.25..=3_600.0).contains(&seconds) => Ok(seconds),
        _ => Err("enter a number of seconds from 0.25 to 3600".to_owned()),
    }
}

/// Parses an SDR white level. An empty text selects the Windows value.
pub fn parse_sdr_white(text: &str) -> Result<Option<f32>, String> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    match text.parse::<f32>() {
        Ok(nits) if (80.0..=10_000.0).contains(&nits) => Ok(Some(nits)),
        _ => Err("enter 80 to 10000 nits, or leave the field empty".to_owned()),
    }
}

impl SavedSettings {
    /// Reads `name=value` lines. Missing and invalid lines keep the default.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let flag = value.parse::<bool>().ok();
            match (name.trim(), flag) {
                ("copy", Some(flag)) => settings.copy = flag,
                ("save", Some(flag)) => settings.save = flag,
                ("upload", Some(flag)) => settings.upload = flag,
                ("primary_monitor", Some(flag)) => settings.primary_monitor = flag,
                ("folder", _) if !value.is_empty() => settings.folder = Some(value.into()),
                ("format", _) => {
                    settings.format = ImageFormat::parse(value).unwrap_or(settings.format);
                }
                ("delay", _) => {
                    settings.delay_seconds = parse_seconds(value).unwrap_or(settings.delay_seconds);
                }
                ("interval", _) => {
                    settings.interval_seconds =
                        parse_seconds(value).unwrap_or(settings.interval_seconds);
                }
                ("sdr_white", _) => {
                    settings.sdr_white_nits = parse_sdr_white(value).ok().flatten();
                }
                ("countdown", _) => {
                    settings.countdown_seconds =
                        parse_countdown(value).unwrap_or(settings.countdown_seconds);
                }
                (name, flag) => {
                    // A hotkey has two lines: its keys, and its switch with
                    // the ending `_on`.
                    for id in HotkeyId::ALL {
                        let binding = &mut settings.hotkeys[id.index()];
                        if name == id.setting() {
                            binding.chord = Chord::parse(value).unwrap_or(binding.chord);
                        } else if name.strip_suffix("_on") == Some(id.setting()) {
                            binding.enabled = flag.unwrap_or(binding.enabled);
                        }
                    }
                }
            }
        }
        if !settings.copy && !settings.save {
            settings.copy = true;
        }
        settings
    }

    /// Formats the settings in the form that [`Self::parse`] reads.
    #[must_use]
    pub fn to_text(&self) -> String {
        let folder = self
            .folder
            .as_ref()
            .map(|folder| folder.display().to_string())
            .unwrap_or_default();
        let sdr_white = self
            .sdr_white_nits
            .map(|nits| nits.to_string())
            .unwrap_or_default();
        let mut text = format!(
            "copy={}\nsave={}\nupload={}\nfolder={folder}\nformat={}\ndelay={}\ninterval={}\n\
             primary_monitor={}\nsdr_white={sdr_white}\ncountdown={}\n",
            self.copy,
            self.save,
            self.upload,
            self.format.extension(),
            self.delay_seconds,
            self.interval_seconds,
            self.primary_monitor,
            self.countdown_seconds,
        );
        for id in HotkeyId::ALL {
            let binding = self.hotkeys[id.index()];
            let name = id.setting();
            text.push_str(&format!(
                "{name}={}\n{name}_on={}\n",
                binding.chord.to_text(),
                binding.enabled
            ));
        }
        text
    }

    /// Loads the stored settings, or the defaults when nothing is stored.
    #[must_use]
    pub fn load() -> Self {
        settings_directory()
            .and_then(|directory| fs::read_to_string(directory.join(FILE)).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    /// Stores the settings for later runs.
    pub fn store(&self) -> Result<(), String> {
        write_setting(FILE, &self.to_text())
    }
}

/// Reports whether the tray process starts when the user signs in.
#[must_use]
pub fn autostart_enabled() -> bool {
    cfg!(windows)
        && registry_tool()
            .args(["query", AUTOSTART_KEY, "/v", AUTOSTART_NAME])
            .output()
            .is_ok_and(|output| output.status.success())
}

/// Builds a `reg` command that opens no console window.
///
/// The desktop program has no console, so a plain child process would flash
/// one on the screen.
fn registry_tool() -> Command {
    let mut command = Command::new("reg");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// The `CREATE_NO_WINDOW` process creation flag.
        const NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(NO_WINDOW);
    }
    command
}

/// Picks the program that the sign-in entry starts.
///
/// `record` names itself. Any other program, such as the desktop program,
/// names the `record.exe` in its own folder.
fn tray_program(current: &Path) -> Result<PathBuf, String> {
    let is_record = current
        .file_stem()
        .is_some_and(|stem| stem.eq_ignore_ascii_case("record"));
    if is_record {
        return Ok(current.to_path_buf());
    }
    let sibling = current.with_file_name("record.exe");
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(format!(
            "the tray needs record.exe in {}",
            current.parent().unwrap_or(current).display()
        ))
    }
}

/// Adds or removes the sign-in entry that starts the tray process.
///
/// The entry belongs to the current user and needs no administrator rights.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("starting at sign-in is available on Windows only".to_owned());
    }
    let line = if enabled {
        let current = std::env::current_exe()
            .map_err(|error| format!("could not find the record executable: {error}"))?;
        let program = tray_program(&current)?;
        Some(format!("\"{}\" screenshot --tray", program.display()))
    } else {
        None
    };
    set_autostart_line(line.as_deref())
}

/// Sets or removes the command line that Windows runs at sign-in.
///
/// The desktop program uses this to start itself in the tray. Both programs
/// share one entry, because only one of them can own the hotkeys.
pub fn set_autostart_line(line: Option<&str>) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("starting at sign-in is available on Windows only".to_owned());
    }
    let mut command = registry_tool();
    if let Some(line) = line {
        command.args([
            "add",
            AUTOSTART_KEY,
            "/v",
            AUTOSTART_NAME,
            "/t",
            "REG_SZ",
            "/d",
        ]);
        command.arg(line).arg("/f");
    } else {
        command.args(["delete", AUTOSTART_KEY, "/v", AUTOSTART_NAME, "/f"]);
    }
    match command.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "the registry refused the change: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(error) => Err(format!("could not run reg.exe: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let settings = SavedSettings {
            copy: false,
            save: true,
            upload: true,
            folder: Some(PathBuf::from(r"D:\Shots = here")),
            format: ImageFormat::Webp,
            delay_seconds: 1.5,
            interval_seconds: 30.0,
            primary_monitor: true,
            sdr_white_nits: Some(203.0),
            hotkeys: {
                let mut hotkeys = default_bindings();
                hotkeys[0].chord = Chord::parse("Ctrl+Alt+R").unwrap();
                hotkeys[3].enabled = false;
                hotkeys[4].chord = Chord::parse("Win+Shift+F12").unwrap();
                hotkeys
            },
            countdown_seconds: 5,
        };
        let text = settings.to_text();
        assert!(text.contains("hotkey_region=Ctrl+Alt+R\n"), "{text}");
        assert!(text.contains("hotkey_video_region_on=false\n"), "{text}");
        assert!(text.contains("countdown=5\n"), "{text}");
        assert_eq!(SavedSettings::parse(&settings.to_text()), settings);
        assert_eq!(
            SavedSettings::parse(&SavedSettings::default().to_text()),
            SavedSettings::default()
        );
    }

    #[test]
    fn bad_lines_keep_the_defaults_and_one_output_stays_on() {
        let settings = SavedSettings::parse(
            "copy=false\nsave=false\ndelay=never\ninterval=0\nsdr_white=5\nnoise\nformat=heic\nupload = true",
        );
        assert_eq!(
            settings,
            SavedSettings {
                save: false,
                upload: true,
                ..SavedSettings::default()
            }
        );
    }

    #[test]
    fn a_file_of_an_older_version_loads_with_the_default_hotkeys() {
        let old = "copy=true\nsave=false\nupload=false\nfolder=D:\\Shots\nformat=jpg\n\
                   delay=2\ninterval=9\nprimary_monitor=true\nsdr_white=\n";
        let settings = SavedSettings::parse(old);
        assert_eq!(settings.hotkeys, default_bindings());
        assert_eq!(settings.countdown_seconds, 0);
        assert_eq!(settings.format, ImageFormat::Jpeg);
        assert!(settings.primary_monitor);
        // A damaged hotkey line keeps the default of that hotkey.
        let damaged = SavedSettings::parse(
            "hotkey_window=Ctrl+Nothing\nhotkey_window_on=maybe\ncountdown=99\n\
             hotkey_fullscreen_on=false\nhotkey_video_fullscreen = alt+f10",
        );
        let mut expected = default_bindings();
        expected[1].enabled = false;
        expected[4].chord = Chord::parse("Alt+F10").unwrap();
        assert_eq!(damaged.hotkeys, expected);
        assert_eq!(damaged.countdown_seconds, 0);
    }

    #[test]
    fn field_parsers_enforce_their_ranges() {
        assert_eq!(parse_countdown(""), Ok(0));
        assert_eq!(parse_countdown(" 3 "), Ok(3));
        assert_eq!(parse_countdown("2.6"), Ok(3));
        assert_eq!(parse_countdown("60"), Ok(60));
        for text in ["-1", "61", "soon", "NaN"] {
            assert!(parse_countdown(text).is_err(), "{text} should fail");
        }
        assert_eq!(parse_seconds(" 2.5 "), Ok(2.5));
        for text in ["0", "-1", "4000", "soon", ""] {
            assert!(parse_seconds(text).is_err(), "{text} should fail");
        }
        assert_eq!(parse_sdr_white(""), Ok(None));
        assert_eq!(parse_sdr_white("Auto"), Ok(None));
        assert_eq!(parse_sdr_white("240"), Ok(Some(240.0)));
        assert!(parse_sdr_white("50").is_err());
    }

    #[test]
    fn the_sign_in_entry_starts_record_and_not_the_desktop_program() {
        let folder = tempfile::tempdir().unwrap();
        let record = folder.path().join("record.exe");
        assert_eq!(tray_program(&record), Ok(record.clone()));
        let desktop = folder.path().join("record-desktop.exe");
        assert!(tray_program(&desktop).unwrap_err().contains("record.exe"));
        fs::write(&record, b"").unwrap();
        assert_eq!(tray_program(&desktop), Ok(record));
    }
}
