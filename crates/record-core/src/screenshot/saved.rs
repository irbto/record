//! The choices that the hub stores between runs.
//!
//! One small text file holds `name=value` lines. Unknown lines are ignored,
//! so an older version can read a file that a newer version wrote.

use std::{fs, path::PathBuf, process::Command};

use super::upload::{settings_directory, write_setting};

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
    /// Seconds that the delayed captures wait.
    pub delay_seconds: f32,
    /// Seconds between auto captures.
    pub interval_seconds: f32,
    /// Captures the primary monitor in place of the monitor under the pointer.
    pub primary_monitor: bool,
    /// SDR white level for HDR monitors in nits. `None` asks Windows.
    pub sdr_white_nits: Option<f32>,
}

impl Default for SavedSettings {
    fn default() -> Self {
        Self {
            copy: true,
            save: true,
            upload: false,
            folder: None,
            delay_seconds: 3.0,
            interval_seconds: 5.0,
            primary_monitor: false,
            sdr_white_nits: None,
        }
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
                _ => {}
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
        format!(
            "copy={}\nsave={}\nupload={}\nfolder={folder}\ndelay={}\ninterval={}\n\
             primary_monitor={}\nsdr_white={sdr_white}\n",
            self.copy,
            self.save,
            self.upload,
            self.delay_seconds,
            self.interval_seconds,
            self.primary_monitor,
        )
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
        && Command::new("reg")
            .args(["query", AUTOSTART_KEY, "/v", AUTOSTART_NAME])
            .output()
            .is_ok_and(|output| output.status.success())
}

/// Adds or removes the sign-in entry that starts the tray process.
///
/// The entry belongs to the current user and needs no administrator rights.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("starting at sign-in is available on Windows only".to_owned());
    }
    let mut command = Command::new("reg");
    if enabled {
        let program = std::env::current_exe()
            .map_err(|error| format!("could not find the record executable: {error}"))?;
        let line = format!("\"{}\" screenshot --tray", program.display());
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
            delay_seconds: 1.5,
            interval_seconds: 30.0,
            primary_monitor: true,
            sdr_white_nits: Some(203.0),
        };
        assert_eq!(SavedSettings::parse(&settings.to_text()), settings);
        assert_eq!(
            SavedSettings::parse(&SavedSettings::default().to_text()),
            SavedSettings::default()
        );
    }

    #[test]
    fn bad_lines_keep_the_defaults_and_one_output_stays_on() {
        let settings = SavedSettings::parse(
            "copy=false\nsave=false\ndelay=never\ninterval=0\nsdr_white=5\nnoise\nupload = true",
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
    fn field_parsers_enforce_their_ranges() {
        assert_eq!(parse_seconds(" 2.5 "), Ok(2.5));
        for text in ["0", "-1", "4000", "soon", ""] {
            assert!(parse_seconds(text).is_err(), "{text} should fail");
        }
        assert_eq!(parse_sdr_white(""), Ok(None));
        assert_eq!(parse_sdr_white("Auto"), Ok(None));
        assert_eq!(parse_sdr_white("240"), Ok(Some(240.0)));
        assert!(parse_sdr_white("50").is_err());
    }
}
