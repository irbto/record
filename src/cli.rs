//! Command-line surface and fast-path defaults.
//!
//! A bare `record` command does not invoke Clap. The executable constructs
//! [`crate::cli::Cli::default`] and starts audio capture immediately. Commands with options
//! use the same values through Clap, which keeps both paths behaviorally equal.
//! Duration parsers accept decimal values and reject zero, negative, infinite,
//! and unrepresentable values before the audio thread starts.

use std::{path::PathBuf, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};

use crate::session::DEFAULT_SEGMENT_DURATION;

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
/// Utility commands that do not record audio.
pub enum Command {
    /// Verify that WASAPI loopback and the native MP3 encoder are available.
    Doctor,
}

/// Parses a duration in seconds.
fn parse_duration(value: &str) -> Result<Duration, String> {
    parse_scaled_duration(value, 1.0, "duration")
}

/// Parses a session part length in minutes.
fn parse_minutes(value: &str) -> Result<Duration, String> {
    parse_scaled_duration(value, 60.0, "part length")
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
    fn parser_rejects_nonfinite_and_too_large_values() {
        for value in ["NaN", "inf", "-inf", "1e999"] {
            assert!(parse_duration(value).is_err(), "{value} should fail");
            assert!(parse_minutes(value).is_err(), "{value} should fail");
        }
    }
}
