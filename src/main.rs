use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
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
    audio::{self, AudioEvent, RecordConfig, RecordingSummary},
    cli::{Cli, Command},
    tui,
};

fn main() -> Result<()> {
    let launched_at = Instant::now();
    // The no-argument path is the product: skip Clap's parser and begin capture.
    let cli = if env::args_os().nth(1).is_none() {
        Cli::default()
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

    let output = output_path(cli.output.as_deref())?;
    if output.exists() && !cli.force {
        bail!(
            "refusing to overwrite {}; choose another path or pass --force",
            output.display()
        );
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create output directory {}", parent.display()))?;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))
            .context("could not install the Ctrl+C handler")?;
    }

    let (event_sender, event_receiver) = bounded(64);
    let config = RecordConfig {
        output: output.clone(),
        bitrate: cli.bitrate.bits_per_second(),
        duration: cli.duration,
        force: cli.force,
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
            output.clone(),
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

fn print_summary(summary: &RecordingSummary) {
    let seconds = summary.duration().as_secs_f64();
    println!("✓ Saved {}", summary.output.display());
    println!(
        "  {:.1}s · {} kbps MP3 · {} kHz stereo",
        seconds,
        summary.bitrate / 1_000,
        summary.sample_rate / 1_000
    );
}

fn output_path(requested: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = requested {
        let mut output = path.to_path_buf();
        if output.extension().is_none() {
            output.set_extension("mp3");
        } else if !output
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
        {
            bail!("output must use the .mp3 extension: {}", output.display());
        }
        return Ok(output);
    }
    let timestamp = local_timestamp();
    let base = PathBuf::from(format!("recording-{timestamp}.mp3"));
    if !base.exists() {
        return Ok(base);
    }
    for suffix in 2..10_000 {
        let candidate = PathBuf::from(format!("recording-{timestamp}-{suffix}.mp3"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("could not choose a unique recording filename")
}

#[cfg(windows)]
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
    fn adds_mp3_extension() {
        let path = output_path(Some(Path::new("take-one"))).unwrap();
        assert_eq!(path, PathBuf::from("take-one.mp3"));
    }

    #[test]
    fn accepts_explicit_mp3_extension() {
        let path = output_path(Some(Path::new("take-one.MP3"))).unwrap();
        assert_eq!(path, PathBuf::from("take-one.MP3"));
    }

    #[test]
    fn rejects_a_non_mp3_extension() {
        assert!(output_path(Some(Path::new("take-one.wav"))).is_err());
    }

    #[test]
    fn explicit_path_can_live_in_a_temp_directory() {
        let directory = tempdir().unwrap();
        let requested = directory.path().join("take");
        assert_eq!(
            output_path(Some(&requested)).unwrap(),
            requested.with_extension("mp3")
        );
    }
}
