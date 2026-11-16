//! Command-line surface and fast-path defaults.
//!
//! A bare `record` command does not invoke Clap. The executable constructs
//! [`crate::cli::Cli::default`] and starts audio capture immediately. Commands with options
//! use the same values through Clap, which keeps both paths behaviorally equal.
//! Duration parsers accept decimal values and reject zero, negative, infinite,
//! and unrepresentable values before the audio thread starts.

use std::{path::PathBuf, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};

use crate::{
    screenshot::{DesktopRect, format::ImageFormat},
    session::DEFAULT_SEGMENT_DURATION,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
/// Selects what `record screenshot` captures without the hub.
pub enum ScreenshotMode {
    /// Select and annotate a region on a frozen monitor.
    Region,
    /// Capture every monitor as one image.
    Full,
    /// Capture the monitor under the mouse pointer.
    Monitor,
    /// Capture the active window.
    Window,
    /// Capture the last region again.
    Last,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
/// Selects one of the MP3 bit rates offered by the command line.
pub enum Bitrate {
    /// Request 128 kilobits per second.
    K128,
    /// Request 192 kilobits per second.
    K192,
    /// Request 256 kilobits per second.
    K256,
    /// Request 320 kilobits per second.
    K320,
}

impl Bitrate {
    #[must_use]
    /// Returns the selected rate in bits per second.
    pub const fn bits_per_second(self) -> u32 {
        match self {
            Self::K128 => 128_000,
            Self::K192 => 192_000,
            Self::K256 => 256_000,
            Self::K320 => 320_000,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "record",
    version,
    about = "Record Windows system audio straight to MP3",
    long_about = "Run `record` and capture starts immediately. Press Ctrl+C to stop and save a high-quality MP3."
)]
/// Contains all parsed command-line options.
pub struct Cli {
    /// MP3 destination. Omit this option to create a session directory.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// MP3 bitrate.
    #[arg(short, long, value_enum, default_value = "k320")]
    pub bitrate: Bitrate,

    /// Stop automatically after this many seconds.
    #[arg(short, long, value_parser = parse_duration, value_name = "SECONDS")]
    pub duration: Option<Duration>,

    /// Length of each automatic session part in minutes.
    #[arg(
        long = "part-minutes",
        value_parser = parse_minutes,
        default_value = "10",
        value_name = "MINUTES"
    )]
    pub segment_duration: Duration,

    /// Disable the full-screen TUI (useful in scripts and redirected shells).
    #[arg(long)]
    pub no_tui: bool,

    /// Replace an existing output file.
    #[arg(short, long)]
    pub force: bool,

    /// Internal probe used by the startup benchmark.
    #[arg(long, hide = true)]
    pub startup_probe: bool,

    #[command(subcommand)]
    /// Optional utility command that runs instead of capture.
    pub command: Option<Command>,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            output: None,
            bitrate: Bitrate::K320,
            duration: None,
            segment_duration: DEFAULT_SEGMENT_DURATION,
            no_tui: false,
            force: false,
            startup_probe: false,
            command: None,
        }
    }
}

#[derive(Debug, Subcommand)]
/// Commands that run instead of audio capture.
pub enum Command {
    /// Verify that WASAPI loopback and the native MP3 encoder are available.
    Doctor,
    /// Capture the primary monitor and system audio to one MP4 file.
    Video {
        /// MP4 destination. Omit this option to create a timestamped file.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// H.264 bit rate in megabits per second.
        #[arg(long, default_value_t = 20, value_name = "MBPS")]
        video_bitrate: u32,

        /// AAC bit rate in kilobits per second.
        #[arg(long, default_value_t = 192, value_name = "KBPS")]
        audio_bitrate: u32,

        /// Monitor selection: primary (default), an index number, or "list".
        #[arg(long, default_value = "primary", value_name = "MONITOR")]
        monitor: String,

        /// Fit mode: contain, cover, stretch, or native.
        #[arg(long, default_value = "contain", value_name = "MODE")]
        fit: String,

        /// Canvas size as WxH (e.g. 1920x1080) or a preset name.
        #[arg(long, value_name = "CANVAS")]
        canvas: Option<String>,

        /// Crop as LEFT,TOP,WIDTH,HEIGHT in source pixels.
        #[arg(long, value_name = "CROP")]
        crop: Option<String>,

        /// Capture frame rate in frames per second.
        #[arg(long, default_value_t = 60, value_name = "FPS")]
        fps: u32,

        /// Stop automatically after this many seconds.
        #[arg(short, long, value_parser = parse_duration, value_name = "SECONDS")]
        duration: Option<Duration>,

        /// Replace an existing output file.
        #[arg(short, long)]
        force: bool,

        /// Show the selected source and transform, then wait for Enter.
        #[arg(long)]
        setup: bool,

        /// Disable the full-screen TUI.
        #[arg(long)]
        no_tui: bool,
    },
    /// Open the screenshot hub, or capture at once when a mode is given.
    Screenshot {
        /// Capture this at once and exit. Omit it to open the hub.
        #[arg(value_enum)]
        mode: Option<ScreenshotMode>,

        /// Image destination. Its extension selects the format. Omit this
        /// option to create a timestamped file.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// File format of timestamped files: png, jpg, webp, bmp, gif, or
        /// tiff. The default is the stored format.
        #[arg(long, value_name = "FORMAT", value_parser = parse_format)]
        format: Option<ImageFormat>,

        /// Folder for timestamped files. The default is the stored folder or
        /// Pictures\Screenshots.
        #[arg(long, value_name = "FOLDER")]
        dir: Option<PathBuf>,

        /// Monitor selection: pointer, primary, or an index number. The
        /// default is the monitor under the pointer or the stored setting.
        #[arg(long, value_name = "MONITOR")]
        monitor: Option<String>,

        /// Capture LEFT,TOP,WIDTH,HEIGHT in desktop pixels at once.
        #[arg(long, value_parser = DesktopRect::parse, value_name = "REGION", allow_hyphen_values = true)]
        region: Option<DesktopRect>,

        /// Wait this many seconds before the capture.
        #[arg(short, long, value_parser = parse_duration, value_name = "SECONDS")]
        delay: Option<Duration>,

        /// Auto capture: repeat the capture at this interval.
        #[arg(long, value_parser = parse_duration, value_name = "SECONDS")]
        every: Option<Duration>,

        /// Auto capture: stop after this many captures.
        #[arg(long, value_name = "COUNT")]
        count: Option<u32>,

        /// Do not place the image on the clipboard.
        #[arg(long)]
        no_copy: bool,

        /// Override the SDR white level of an HDR monitor in nits.
        #[arg(long, value_parser = parse_sdr_white, value_name = "NITS")]
        sdr_white: Option<f32>,

        /// Upload each image and copy its link. DESTINATION is imgur, the
        /// name of a .sxcu file in %APPDATA%\record\uploaders, or a
        /// .sxcu path. Without a value, uploader.txt names the destination.
        #[arg(long, value_name = "DESTINATION", num_args = 0..=1, default_missing_value = "")]
        upload: Option<String>,

        /// Replace an existing output file.
        #[arg(short, long)]
        force: bool,

        /// Run in the notification area with global hotkeys and no terminal.
        #[arg(long)]
        tray: bool,
    },
}

/// Parses a screenshot file format.
fn parse_format(value: &str) -> Result<ImageFormat, String> {
    ImageFormat::parse(value).ok_or_else(|| "use png, jpg, webp, bmp, gif, or tiff".to_owned())
}

/// Parses a duration in seconds.
fn parse_duration(value: &str) -> Result<Duration, String> {
    parse_scaled_duration(value, 1.0, "duration")
}

/// Parses a session part length in minutes.
fn parse_minutes(value: &str) -> Result<Duration, String> {
    parse_scaled_duration(value, 60.0, "part length")
}

/// Parses an SDR white level in nits.
fn parse_sdr_white(value: &str) -> Result<f32, String> {
    let nits = value
        .parse::<f32>()
        .map_err(|_| "SDR white level must be a number".to_owned())?;
    if !(80.0..=10_000.0).contains(&nits) {
        return Err("SDR white level must be from 80 to 10000 nits".to_owned());
    }
    Ok(nits)
}

/// Parses one positive decimal value and converts it to seconds.
fn parse_scaled_duration(value: &str, scale: f64, label: &str) -> Result<Duration, String> {
    let seconds = value
        .parse::<f64>()
        .map(|value| value * scale)
        .map_err(|_| format!("{label} must be a number"))?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(format!("{label} must be greater than zero"));
    }
    Duration::try_from_secs_f64(seconds).map_err(|_| format!("{label} is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fractional_duration() {
        assert_eq!(parse_duration("2.5").unwrap(), Duration::from_millis(2_500));
    }

    #[test]
    fn rejects_non_positive_duration() {
        assert!(parse_duration("0").is_err());
        assert!(parse_duration("-1").is_err());
    }

    #[test]
    fn parses_fractional_part_minutes() {
        assert_eq!(parse_minutes("0.5").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn default_cli_uses_ten_minute_parts() {
        assert_eq!(Cli::default().segment_duration, Duration::from_secs(600));
    }

    #[test]
    fn clap_defaults_match_the_bare_command_fast_path() {
        let parsed = Cli::try_parse_from(["record"]).unwrap();
        let fast = Cli::default();
        assert_eq!(parsed.output, fast.output);
        assert_eq!(
            parsed.bitrate.bits_per_second(),
            fast.bitrate.bits_per_second()
        );
        assert_eq!(parsed.duration, fast.duration);
        assert_eq!(parsed.segment_duration, fast.segment_duration);
        assert_eq!(parsed.no_tui, fast.no_tui);
    }

    #[test]
    fn clap_parses_output_rate_duration_and_part_length() {
        let parsed = Cli::try_parse_from([
            "record",
            "-o",
            "take.mp3",
            "-b",
            "k192",
            "-d",
            "2.5",
            "--part-minutes",
            "0.25",
            "--no-tui",
        ])
        .unwrap();
        assert_eq!(parsed.output, Some(PathBuf::from("take.mp3")));
        assert_eq!(parsed.bitrate.bits_per_second(), 192_000);
        assert_eq!(parsed.duration, Some(Duration::from_millis(2_500)));
        assert_eq!(parsed.segment_duration, Duration::from_secs(15));
        assert!(parsed.no_tui);
    }

    #[test]
    fn clap_parses_screenshot_options() {
        let parsed = Cli::try_parse_from([
            "record",
            "screenshot",
            "window",
            "--dir",
            "shots",
            "--monitor",
            "1",
            "--region",
            "-1920,0,800,600",
            "--every",
            "2.5",
            "--count",
            "4",
            "--no-copy",
            "--sdr-white",
            "200",
            "--upload",
            "imgur",
        ])
        .unwrap();
        let Some(Command::Screenshot {
            upload,
            mode,
            dir,
            monitor,
            region,
            every,
            count,
            no_copy,
            sdr_white,
            tray,
            ..
        }) = parsed.command
        else {
            panic!("expected the screenshot command");
        };
        assert_eq!(mode, Some(ScreenshotMode::Window));
        assert_eq!(dir, Some(PathBuf::from("shots")));
        assert_eq!(monitor.as_deref(), Some("1"));
        assert_eq!(region.map(|region| region.left), Some(-1920));
        assert_eq!(every, Some(Duration::from_millis(2_500)));
        assert_eq!(count, Some(4));
        assert!(no_copy && !tray);
        assert_eq!(sdr_white, Some(200.0));
        assert_eq!(upload.as_deref(), Some("imgur"));
    }

    #[test]
    fn upload_accepts_a_missing_destination() {
        let parsed = Cli::try_parse_from(["record", "screenshot", "full", "--upload"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Screenshot { upload: Some(destination), .. }) if destination.is_empty()
        ));
        let parsed = Cli::try_parse_from(["record", "screenshot", "full"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Screenshot { upload: None, .. })
        ));
    }

    #[test]
    fn a_bare_screenshot_command_selects_the_hub() {
        let parsed = Cli::try_parse_from(["record", "screenshot"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Screenshot {
                mode: None,
                region: None,
                tray: false,
                ..
            })
        ));
    }

    #[test]
    fn sdr_white_rejects_values_outside_the_display_range() {
        for value in ["79", "10001", "NaN", "bright"] {
            assert!(parse_sdr_white(value).is_err(), "{value} should fail");
        }
        assert_eq!(parse_sdr_white("240").unwrap(), 240.0);
    }

    #[test]
    fn parser_rejects_nonfinite_and_too_large_values() {
        for value in ["NaN", "inf", "-inf", "1e999"] {
            assert!(parse_duration(value).is_err(), "{value} should fail");
            assert!(parse_minutes(value).is_err(), "{value} should fail");
        }
    }
}
