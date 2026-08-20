use std::{path::PathBuf, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Bitrate {
    K128,
    K192,
    K256,
    K320,
}

impl Bitrate {
    #[must_use]
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
pub struct Cli {
    /// MP3 destination. Defaults to ./recording-YYYYMMDD-HHMMSS.mp3.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// MP3 bitrate.
    #[arg(short, long, value_enum, default_value = "k320")]
    pub bitrate: Bitrate,

    /// Stop automatically after this many seconds.
    #[arg(short, long, value_parser = parse_duration, value_name = "SECONDS")]
    pub duration: Option<Duration>,

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
    pub command: Option<Command>,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            output: None,
            bitrate: Bitrate::K320,
            duration: None,
            no_tui: false,
            force: false,
            startup_probe: false,
            command: None,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Verify that WASAPI loopback and the native MP3 encoder are available.
    Doctor,
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| "duration must be a number of seconds, such as 30 or 2.5".to_owned())?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("duration must be greater than zero".to_owned());
    }
    Ok(Duration::from_secs_f64(seconds))
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
}
