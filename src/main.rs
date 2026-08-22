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
    path::PathBuf,
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
    video::{
        CropRect, FitMode, VideoConfig, VideoEvent, VideoSource, VideoSummary, canvas_presets,
        check_video_support, enumerate_monitors, record_video, video_timestamp,
    },
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
        match check_video_support() {
            Ok(()) => {
                println!("✓ D3D11 device with desktop duplication available");
                println!("✓ native Media Foundation H.264 and AAC encoders available");
            }
            Err(error) => println!("✗ screen capture unavailable: {error:#}"),
        }
        return Ok(());
    }

    if matches!(cli.command, Some(Command::Video { .. })) {
        return run_video(cli, launched_at);
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

/// Runs the `record video` command with its own pipeline and headless output.
fn run_video(cli: Cli, _launched_at: Instant) -> Result<()> {
    let Some(Command::Video {
        output,
        monitor,
        fit,
        canvas,
        crop,
        video_bitrate,
        audio_bitrate,
        fps,
        duration,
        force,
        setup,
        no_tui,
    }) = cli.command
    else {
        unreachable!("run_video is only called for the video command");
    };
    check_video_support()?;
    let output = match output {
        Some(path) => {
            let mut path = path;
            if path.extension().is_none() {
                path.set_extension("mp4");
            }
            if path
                .extension()
                .is_some_and(|extension| !extension.eq_ignore_ascii_case("mp4"))
            {
                bail!("the video output file must use the .mp4 extension");
            }
            if path.exists() && !force {
                bail!(
                    "refusing to overwrite {}; choose another path or pass --force",
                    path.display()
                );
            }
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent).with_context(|| {
                    format!("could not create output directory {}", parent.display())
                })?;
            }
            path
        }
        None => PathBuf::from(format!("recording-{}.mp4", video_timestamp())),
    };
    let fit_mode = match fit.to_lowercase().as_str() {
        "contain" => FitMode::Contain,
        "cover" => FitMode::Cover,
        "stretch" => FitMode::Stretch,
        "native" => FitMode::Native,
        _ => bail!("invalid fit {fit}; use contain, cover, stretch, or native"),
    };
    let (canvas_width, canvas_height) = match canvas.as_deref() {
        None => (0, 0),
        Some(value) => {
            let preset = canvas_presets()
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(value));
            if let Some((_, size)) = preset {
                (size.width, size.height)
            } else if let Some((width, height)) = value.split_once('x') {
                let width: u32 = width
                    .parse()
                    .map_err(|_| anyhow!("invalid canvas width in {value}"))?;
                let height: u32 = height
                    .parse()
                    .map_err(|_| anyhow!("invalid canvas height in {value}"))?;
                if width == 0 || height == 0 {
                    bail!("canvas dimensions must be greater than zero");
                }
                (width, height)
            } else {
                bail!("invalid canvas {value}; use WxH or a preset name");
            }
        }
    };
    let crop_rect = match crop.as_deref() {
        None => CropRect::default(),
        Some(value) => {
            let parts: Vec<&str> = value.split(',').collect();
            if parts.len() != 4 {
                bail!("crop must be LEFT,TOP,WIDTH,HEIGHT");
            }
            let left: u32 = parts[0]
                .trim()
                .parse()
                .map_err(|_| anyhow!("invalid crop left in {value}"))?;
            let top: u32 = parts[1]
                .trim()
                .parse()
                .map_err(|_| anyhow!("invalid crop top in {value}"))?;
            let width: u32 = parts[2]
                .trim()
                .parse()
                .map_err(|_| anyhow!("invalid crop width in {value}"))?;
            let height: u32 = parts[3]
                .trim()
                .parse()
                .map_err(|_| anyhow!("invalid crop height in {value}"))?;
            if width == 0 || height == 0 {
                bail!("crop dimensions must be greater than zero");
            }
            CropRect {
                left,
                top,
                width,
                height,
            }
        }
    };
    let monitors = enumerate_monitors().context("could not enumerate monitors")?;
    let monitor_index = if monitor.eq_ignore_ascii_case("primary") {
        monitors
            .iter()
            .find(|monitor| monitor.primary)
            .map(|monitor| monitor.index)
            .context("no primary monitor found")?
    } else if monitor.eq_ignore_ascii_case("list") {
        for monitor in &monitors {
            let primary = if monitor.primary { " (primary)" } else { "" };
            println!(
                "  {} {} {}x{}{}",
                monitor.index, monitor.name, monitor.width, monitor.height, primary
            );
        }
        return Ok(());
    } else {
        monitor.parse::<u32>().context(format!(
            "invalid monitor {monitor}; use a monitor index, primary, or list"
        ))?
    };
    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))
            .context("could not install the Ctrl+C handler")?;
    }
    if setup {
        println!("record video setup");
        println!("  monitor: {monitor}");
        println!("  fit: {}", fit_mode.label());
        if canvas_width > 0 {
            println!("  canvas: {canvas_width}x{canvas_height}");
        } else {
            println!("  canvas: native source size");
        }
        if crop_rect.left > 0 || crop_rect.top > 0 {
            println!(
                "  crop: {},{},{},{}",
                crop_rect.left, crop_rect.top, crop_rect.width, crop_rect.height
            );
        } else {
            println!("  crop: full frame");
        }
        println!("  fps: {fps}");
        println!();
        println!("Press Enter to start capture.");
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("could not read Enter")?;
    }
    let (event_sender, event_receiver) = bounded(64);
    let config = VideoConfig {
        source: if monitor_index == 0 {
            VideoSource::Primary
        } else {
            VideoSource::Index(monitor_index)
        },
        crop: crop_rect,
        fit: fit_mode,
        canvas_width,
        canvas_height,
        video_bitrate: video_bitrate * 1_000_000,
        audio_bitrate: audio_bitrate * 1_000,
        fps: fps.clamp(1, 60),
        duration,
        output: output.clone(),
        replace: force,
        stop: Arc::clone(&stop),
        paused: Arc::clone(&paused),
    };
    let worker = thread::Builder::new()
        .name("record-video".to_owned())
        .spawn(move || record_video(config, &event_sender))
        .context("could not start the video thread")?;
    let use_tui = !no_tui && io::stdout().is_terminal() && io::stdin().is_terminal();
    if use_tui {
        tui::run_video(
            &event_receiver,
            &worker,
            Arc::clone(&stop),
            Arc::clone(&paused),
            output.clone(),
        )?;
    } else {
        run_video_headless(&event_receiver, &worker, &stop)?;
    }
    let summary = worker.join().map_err(|panic| {
        if let Some(message) = panic.downcast_ref::<&str>() {
            anyhow!("video thread panicked: {message}")
        } else if let Some(message) = panic.downcast_ref::<String>() {
            anyhow!("video thread panicked: {message}")
        } else {
            anyhow!("video thread panicked")
        }
    })??;
    println!(
        "  {:.1}s · {}x{} @ {} fps · H.264 {} Mbps + AAC {} kbps",
        summary.duration().as_secs_f64(),
        summary.width,
        summary.height,
        summary.fps,
        summary.video_bitrate / 1_000_000,
        summary.audio_bitrate / 1_000
    );
    if let Some(ready) = summary.recording_ready_ms {
        println!(
            "  capture ready {ready:.1} ms · finalized in {:.1} ms",
            summary.finalize_ms
        );
    }
    Ok(())
}

/// Reports capture progress when the full-screen terminal is unavailable.
fn run_video_headless(
    events: &Receiver<VideoEvent>,
    worker: &thread::JoinHandle<Result<VideoSummary>>,
    stop: &AtomicBool,
) -> Result<()> {
    eprintln!("● Recording screen");
    eprintln!("  Press Ctrl+C to stop and save.");
    while !worker.is_finished() {
        if let Ok(event) = events.try_recv() {
            match event {
                VideoEvent::Started {
                    width,
                    height,
                    fps,
                    capture_ready_ms,
                    ..
                } => {
                    eprintln!("  {width}x{height} @ {fps} fps · H.264 + AAC");
                    eprintln!("CAPTURE_READY_MS={capture_ready_ms:.3}");
                }
                VideoEvent::Notice(message) => eprintln!("  {message}"),
                VideoEvent::Finalizing => eprint!("  Finalizing MP4..."),
                VideoEvent::Saved(_) => {}
            }
        }
        if stop.load(Ordering::Relaxed) {
            eprint!("  Finalizing MP4...");
        }
        std::thread::sleep(Duration::from_millis(33));
    }
    eprintln!();
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
