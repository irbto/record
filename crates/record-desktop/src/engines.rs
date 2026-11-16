//! The engines of `record-core` behind the dashboard boundary.
//!
//! Each recording runs on its own thread. A second thread translates engine
//! events into the plain updates that the dashboard reads, so capture never
//! waits for drawing.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    thread,
};

use crossbeam_channel::{Sender, bounded, unbounded};
use record_core::{
    audio::{self, AudioEvent, RecordConfig},
    screenshot::copy_file_to_clipboard,
    session::OutputTarget,
    video::{
        self, CropRect, FitMode, VideoConfig, VideoEvent, VideoSource, enumerate_monitors,
        preview::PreviewFrame, video_timestamp,
    },
};

use crate::backend::{Engines, Mode, Monitor, Session, SoundUpdate, VideoUpdate};

/// H.264 bit rate of dashboard recordings in bits per second.
const VIDEO_BITRATE: u32 = 20_000_000;
/// AAC bit rate of dashboard recordings in bits per second.
const AUDIO_BITRATE: u32 = 192_000;

/// Runs the real capture engines.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreEngines;

/// Returns the largest absolute sample of a block.
fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

/// Creates the folder of an output file and formats a failure.
fn prepare(output: &Path) -> Result<(), String> {
    match output.parent() {
        Some(parent) => fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display())),
        None => Ok(()),
    }
}

/// Starts a named thread. A failure to start is reported as an update.
fn spawn<U: Send + 'static>(
    name: &str,
    updates: &Sender<U>,
    failed: fn(String) -> U,
    work: impl FnOnce() + Send + 'static,
) {
    if let Err(error) = thread::Builder::new().name(name.to_owned()).spawn(work) {
        let _ = updates.send(failed(format!(
            "could not start the {name} thread: {error}"
        )));
    }
}

impl Engines for CoreEngines {
    fn start_sound(&self, output: PathBuf, bitrate: u32) -> Session<SoundUpdate> {
        let (updates, receiver) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let session = Session {
            updates: receiver,
            stop: Arc::clone(&stop),
            paused: Arc::clone(&paused),
        };
        let worker_updates = updates.clone();
        spawn(
            "sound",
            &updates,
            |error| SoundUpdate::Stopped(Err(error)),
            move || {
                let updates = worker_updates;
                let (events, event_receiver) = unbounded::<AudioEvent>();
                // The engine reads clip commands from this channel. The
                // dashboard sends none, and the sender must outlive the run.
                let (_commands, command_receiver) = bounded(1);
                let translated = updates.clone();
                let translator = thread::spawn(move || {
                    let mut rate = 48_000_u32;
                    for event in event_receiver {
                        let update = match event {
                            AudioEvent::Started {
                                sample_rate,
                                bitrate,
                                ..
                            } => {
                                rate = sample_rate.max(1);
                                SoundUpdate::Started {
                                    sample_rate,
                                    bitrate,
                                }
                            }
                            AudioEvent::Samples {
                                left,
                                right,
                                encoded_frames,
                            } => SoundUpdate::Levels {
                                left: peak(&left),
                                right: peak(&right),
                                seconds: encoded_frames as f64 / f64::from(rate),
                            },
                            AudioEvent::Finalizing => SoundUpdate::Finalizing,
                            AudioEvent::Notice(notice) => SoundUpdate::Notice(notice),
                            AudioEvent::Saved(_) => continue,
                        };
                        if translated.send(update).is_err() {
                            break;
                        }
                    }
                });
                let result = prepare(&output).and_then(|()| {
                    let config = RecordConfig {
                        target: OutputTarget::SingleFile {
                            path: output,
                            replace: false,
                        },
                        bitrate,
                        duration: None,
                        commands: command_receiver,
                        stop,
                        paused,
                    };
                    audio::record(config, &events)
                        .map(|summary| summary.output)
                        .map_err(|error| format!("{error:#}"))
                });
                drop(events);
                let _ = translator.join();
                let _ = updates.send(SoundUpdate::Stopped(result));
            },
        );
        session
    }

    fn start_video(&self, output: PathBuf, monitor: u32, fps: u32) -> Session<VideoUpdate> {
        let (updates, receiver) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let session = Session {
            updates: receiver,
            stop: Arc::clone(&stop),
            paused: Arc::clone(&paused),
        };
        let worker_updates = updates.clone();
        spawn(
            "video",
            &updates,
            |error| VideoUpdate::Stopped(Err(error)),
            move || {
                let updates = worker_updates;
                let (events, event_receiver) = unbounded::<VideoEvent>();
                // A full preview channel makes the engine skip a picture.
                let (previews, preview_receiver) = bounded::<PreviewFrame>(2);
                let translated = updates.clone();
                let translator = thread::spawn(move || {
                    for event in event_receiver {
                        let update = match event {
                            VideoEvent::Started {
                                width, height, fps, ..
                            } => VideoUpdate::Started { width, height, fps },
                            VideoEvent::Finalizing => VideoUpdate::Finalizing,
                            VideoEvent::Notice(notice) => VideoUpdate::Notice(notice),
                            VideoEvent::Saved(_) => continue,
                        };
                        if translated.send(update).is_err() {
                            break;
                        }
                    }
                });
                let pictures = updates.clone();
                let painter = thread::spawn(move || {
                    for frame in preview_receiver {
                        let mut rgba = frame.bgra;
                        for pixel in rgba.chunks_exact_mut(4) {
                            pixel.swap(0, 2);
                            pixel[3] = 255;
                        }
                        let update = VideoUpdate::Preview {
                            width: frame.width,
                            height: frame.height,
                            rgba,
                        };
                        if pictures.send(update).is_err() {
                            break;
                        }
                    }
                });
                let result = prepare(&output).and_then(|()| {
                    let config = VideoConfig {
                        source: VideoSource::Index(monitor),
                        crop: CropRect::default(),
                        fit: FitMode::default(),
                        canvas_width: 0,
                        canvas_height: 0,
                        video_bitrate: VIDEO_BITRATE,
                        audio_bitrate: AUDIO_BITRATE,
                        fps,
                        duration: None,
                        output,
                        replace: false,
                        stop,
                        paused,
                    };
                    video::record_video(config, &events, Some(&previews))
                        .map(|summary| summary.output)
                        .map_err(|error| format!("{error:#}"))
                });
                drop(events);
                drop(previews);
                let _ = translator.join();
                let _ = painter.join();
                let _ = updates.send(VideoUpdate::Stopped(result));
            },
        );
        session
    }

    fn monitors(&self) -> Vec<Monitor> {
        enumerate_monitors()
            .unwrap_or_default()
            .into_iter()
            .map(|monitor| Monitor {
                index: monitor.index,
                name: monitor.name,
                width: monitor.width,
                height: monitor.height,
                primary: monitor.primary,
            })
            .collect()
    }

    fn timestamp(&self) -> String {
        video_timestamp()
    }

    fn folder(&self, mode: Mode) -> PathBuf {
        let home = env::var_os("USERPROFILE").map_or_else(|| PathBuf::from("."), PathBuf::from);
        match mode {
            Mode::Sound => home.join("Music").join("record"),
            Mode::Video => home.join("Videos").join("record"),
            Mode::Screenshot => record_core::screenshot::default_directory(),
        }
    }

    fn reveal(&self, path: &Path) {
        let mut command = Command::new("explorer");
        if path.is_file() {
            command.arg("/select,").arg(path);
        } else {
            let _ = fs::create_dir_all(path);
            command.arg(path);
        }
        let _ = command.spawn();
    }

    fn copy_image(&self, path: &Path) -> Result<(), String> {
        copy_file_to_clipboard(path).map_err(|error| format!("{error:#}"))
    }
}
