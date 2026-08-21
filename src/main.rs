//! Process entry point and startup coordinator.
//!
//! This file owns process-level work: argument selection, output creation,
//! signal handling, thread startup, headless output, and the final clickable
//! path. Audio devices and encoders stay in the library backend. The terminal
//! interface receives only bounded events and sends only low-frequency control
//! commands.
//!
//! The no-argument path constructs defaults without Clap. This path starts the
//! audio worker before it mounts Ratatui. The startup benchmark measures the
//! backend event that follows both MP3 writer creation and WASAPI start.

use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use crossbeam_channel::{Receiver, bounded};
use record_tui::{
    audio::{self, AudioEvent, RecordConfig, RecordingSummary, SavedFileKind},
    cli::{Cli, Command},
    session::{OutputTarget, create_session_directory, mp3_output_path},
    tui,
};

fn main() -> Result<()> {
    let launched_at = Instant::now();
    // The no-argument path is the product: skip Clap's parser and begin capture.
    let no_arguments = env::args_os().nth(1).is_none();
    let cli = if no_arguments {
        let mut defaults = Cli::default();
        if env::var_os("RECORD_INTERNAL_STARTUP_PROBE").is_some() {
            defaults.startup_probe = true;
            defaults.no_tui = true;
        }
        defaults
    } else {
        Cli::parse()
    };
    if matches!(cli.command, Some(Command::Doctor)) {
        audio::check_support()?;
        println!("✓ default Windows output found");
        println!("✓ WASAPI loopback available");
        println!("✓ native Media Foundation MP3 encoder available");
        return Ok(());
    }

    let target = output_target(
        cli.output.as_deref(),
        cli.force,
        cli.segment_duration,
        Path::new("."),
    )?;
    let output = target.root().to_path_buf();

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))
            .context("could not install the Ctrl+C handler")?;
    }

    let (event_sender, event_receiver) = bounded(64);
    let (command_sender, command_receiver) = bounded(8);
    let config = RecordConfig {
        target: target.clone(),
        bitrate: cli.bitrate.bits_per_second(),
        duration: cli.duration,
        commands: command_receiver,
        stop: Arc::clone(&stop),
        paused: Arc::clone(&paused),
    };
    let worker = thread::Builder::new()
        .name("record-audio".to_owned())
        .spawn(move || audio::record(config, &event_sender))
        .context("could not start the audio thread")?;

    let use_tui = !cli.no_tui
        && !cli.startup_probe
        && io::stdout().is_terminal()
        && io::stdin().is_terminal();
    if use_tui {
        tui::run(
            &event_receiver,
            &worker,
            Arc::clone(&stop),
            Arc::clone(&paused),
            command_sender,
            target,
        )?;
    } else {
        run_headless(
            &event_receiver,
            &worker,
            &stop,
            &output,
            cli.startup_probe,
            launched_at,
        )?;
    }

    let summary = worker.join().map_err(|panic| {
        if let Some(message) = panic.downcast_ref::<&str>() {
            anyhow!("audio thread panicked: {message}")
        } else if let Some(message) = panic.downcast_ref::<String>() {
            anyhow!("audio thread panicked: {message}")
        } else {
            anyhow!("audio thread panicked")
        }
    })??;
    print_summary(&summary);
    Ok(())
}

/// Reports capture progress when full-screen terminal control is not available.
fn run_headless(
    events: &Receiver<AudioEvent>,
    worker: &thread::JoinHandle<Result<RecordingSummary>>,
    stop: &AtomicBool,
    output: &Path,
    startup_probe: bool,
    launched_at: Instant,
) -> Result<()> {
    eprintln!("● Recording system audio → {}", output.display());
    eprintln!("  Press Ctrl+C to stop and save.");
    let mut sample_rate = 48_000;
    while !worker.is_finished() {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
            match event {
                AudioEvent::Started {
                    sample_rate: rate,
                    bitrate,
                    ..
                } => {
                    sample_rate = rate;
                    eprintln!("  {} kbps · {} kHz · stereo", bitrate / 1_000, rate / 1_000);
                    if startup_probe {
                        eprintln!(
                            "RECORD_READY_MS={:.3}",
                            launched_at.elapsed().as_secs_f64() * 1_000.0
                        );
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                AudioEvent::Samples { encoded_frames, .. } => {
                    let seconds = encoded_frames / u64::from(sample_rate);
                    eprint!(
                        "\r  {:02}:{:02}:{:02}  Ctrl+C to stop",
                        seconds / 3_600,
                        seconds / 60 % 60,
                        seconds % 60
                    );
                    io::stderr().flush()?;
                }
                AudioEvent::Saved(file) => {
                    eprintln!(
                        "\r✓ Saved {} ({:.1}s)                         ",
                        file.path.display(),
                        file.duration().as_secs_f64()
                    );
                }
                AudioEvent::Notice(message) => eprintln!("\r  {message}"),
                AudioEvent::Finalizing => eprint!("\r  Finalizing MP3...                     "),
            }
        }
        if stop.load(Ordering::Relaxed) {
            eprint!("\r  Finalizing MP3...                     ");
        }
    }
    eprintln!();
    Ok(())
}

/// Prints a clickable file or session path and its final audio properties.
fn print_summary(summary: &RecordingSummary) {
    if summary.files.is_empty() {
        println!("✓ No audio was captured");
        println!("  {}", summary_detail(summary, false));
        return;
    }
    let single_file = summary.files.len() == 1 && summary.files[0].kind == SavedFileKind::Recording;
    if single_file {
        println!("✓ Saved {}", output_link(&summary.output));
    } else {
        println!("✓ Saved session {}", output_link(&summary.output));
    }
    println!("  {}", summary_detail(summary, !single_file));
}

/// Formats the stable second line of the final process summary.
fn summary_detail(summary: &RecordingSummary, include_file_count: bool) -> String {
    let seconds = summary.duration().as_secs_f64();
    let format = format!(
        "{} kbps MP3 · {} kHz stereo",
        summary.bitrate / 1_000,
        summary.sample_rate / 1_000
    );
    if include_file_count {
        format!(
            "{seconds:.1}s · {} file{} · {format}",
            summary.files.len(),
            if summary.files.len() == 1 { "" } else { "s" }
        )
    } else {
        format!("{seconds:.1}s · {format}")
    }
}

/// Adds an OSC 8 file link when standard output is a terminal.
fn output_link(path: &Path) -> String {
    let label = path.display().to_string();
    if !io::stdout().is_terminal() {
        return label;
    }

    let Some(uri) = file_uri(path) else {
        return label;
    };
    format!("\x1b]8;;{uri}\x1b\\{label}\x1b]8;;\x1b\\")
}

/// Converts an absolute local path to a percent-encoded file URI.
fn file_uri(path: &Path) -> Option<String> {
    let absolute = std::path::absolute(path).ok()?;
    let normalized = absolute.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let normalized = normalized.strip_prefix("//?/UNC/").map_or_else(
        || {
            normalized
                .strip_prefix("//?/")
                .unwrap_or(&normalized)
                .to_owned()
        },
        |path| format!("//{path}"),
    );
    let encoded = percent_encode_uri_path(&normalized);

    if normalized.starts_with("//") {
        Some(format!("file:{encoded}"))
    } else if normalized.starts_with('/') {
        Some(format!("file://{encoded}"))
    } else {
        Some(format!("file:///{encoded}"))
    }
}

/// Encodes bytes that are not safe in a file URI path.
fn percent_encode_uri_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

/// Validates one explicit file or creates one unique session directory.
fn output_target(
    requested: Option<&Path>,
    force: bool,
    segment_duration: Duration,
    session_parent: &Path,
) -> Result<OutputTarget> {
    if let Some(path) = requested {
        let output = mp3_output_path(path)
            .with_context(|| format!("invalid output path {}", path.display()))?;
        if output.exists() && !force {
            bail!(
                "refusing to overwrite {}; choose another path or pass --force",
                output.display()
            );
        }
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("could not create output directory {}", parent.display())
            })?;
        }
        return Ok(OutputTarget::SingleFile {
            path: output,
            replace: force,
        });
    }
    let timestamp = local_timestamp();
    let directory = create_session_directory(session_parent, &timestamp).with_context(|| {
        format!(
            "could not create a recording session in {}",
            session_parent.display()
        )
    })?;
    Ok(OutputTarget::Session {
        directory,
        segment_duration,
    })
}

#[cfg(windows)]
/// Returns a local wall-clock timestamp for default session names.
fn local_timestamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;

    // SAFETY: GetLocalTime has no preconditions and returns the value by copy.
    let time = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

#[cfg(not(windows))]
/// Returns a portable timestamp for unsupported-platform diagnostics and tests.
fn local_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    seconds.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn explicit_target_can_live_in_a_temp_directory() {
        let directory = tempdir().unwrap();
        let requested = directory.path().join("take");
        let target = output_target(
            Some(&requested),
            false,
            Duration::from_secs(600),
            directory.path(),
        )
        .unwrap();
        assert_eq!(target.root(), requested.with_extension("mp3"));
        assert!(!target.is_session());
    }

    #[test]
    fn default_target_creates_a_session_directory() {
        let directory = tempdir().unwrap();
        let target =
            output_target(None, false, Duration::from_secs(600), directory.path()).unwrap();
        assert!(target.is_session());
        assert!(target.root().is_dir());
    }

    #[test]
    fn explicit_target_protects_an_existing_file() {
        let directory = tempdir().unwrap();
        let requested = directory.path().join("take.mp3");
        fs::write(&requested, []).unwrap();
        assert!(
            output_target(
                Some(&requested),
                false,
                Duration::from_secs(600),
                directory.path()
            )
            .is_err()
        );
        let target = output_target(
            Some(&requested),
            true,
            Duration::from_secs(600),
            directory.path(),
        )
        .unwrap();
        assert_eq!(
            target,
            OutputTarget::SingleFile {
                path: requested,
                replace: true
            }
        );
    }

    #[test]
    fn explicit_target_rejects_a_non_mp3_extension() {
        let directory = tempdir().unwrap();
        assert!(
            output_target(
                Some(Path::new("take.wav")),
                false,
                Duration::from_secs(600),
                directory.path()
            )
            .is_err()
        );
    }

    #[test]
    fn single_file_summary_keeps_the_compact_original_format() {
        let summary = RecordingSummary {
            output: Path::new("take.mp3").to_path_buf(),
            sample_rate: 48_000,
            bitrate: 320_000,
            frames: 273_600,
            files: vec![record_tui::audio::SavedFile {
                path: Path::new("take.mp3").to_path_buf(),
                kind: SavedFileKind::Recording,
                sample_rate: 48_000,
                bitrate: 320_000,
                frames: 273_600,
                edit_source: None,
            }],
        };
        assert_eq!(
            summary_detail(&summary, false),
            "5.7s · 320 kbps MP3 · 48 kHz stereo"
        );
    }

    #[test]
    fn session_summary_reports_its_file_count() {
        let summary = RecordingSummary {
            output: Path::new("session").to_path_buf(),
            sample_rate: 48_000,
            bitrate: 320_000,
            frames: 48_000,
            files: vec![],
        };
        assert_eq!(
            summary_detail(&summary, true),
            "1.0s · 0 files · 320 kbps MP3 · 48 kHz stereo"
        );
    }

    #[test]
    fn empty_summary_has_a_valid_zero_duration_detail() {
        let summary = RecordingSummary {
            output: Path::new("session").to_path_buf(),
            sample_rate: 48_000,
            bitrate: 320_000,
            frames: 0,
            files: vec![],
        };
        assert_eq!(
            summary_detail(&summary, false),
            "0.0s · 320 kbps MP3 · 48 kHz stereo"
        );
    }

    #[test]
    fn hyperlink_uri_escapes_spaces_symbols_and_unicode() {
        assert_eq!(
            percent_encode_uri_path("C:/My Music/café #1.mp3"),
            "C:/My%20Music/caf%C3%A9%20%231.mp3"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_uri_targets_an_absolute_path() {
        assert_eq!(
            file_uri(Path::new(r"C:\Users\Ada Lovelace\take #1.mp3")).as_deref(),
            Some("file:///C:/Users/Ada%20Lovelace/take%20%231.mp3")
        );
    }
}
