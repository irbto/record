//! The engines of `record-core` behind plain updates.
//!
//! Each recording runs on its own thread. A second thread translates engine
//! events into the updates that the interface reads, so capture never waits
//! for drawing.

use std::{
    collections::VecDeque,
    env, fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use crossbeam_channel::{Receiver, unbounded};
use record_core::{
    audio::{self, AudioCommand, AudioEvent, RecordConfig, SavedFile},
    session::{DEFAULT_SEGMENT_DURATION, OutputTarget},
    spectrum::Spectrum,
    video::{self, CropRect, FitMode, VideoConfig, VideoEvent, VideoSource, VideoSummary},
};

use crate::payload::{Mode, SoundUpdate, VideoUpdate, extent, file_name, spectrum_bands};

/// H.264 bit rate of recordings in bits per second.
pub const VIDEO_BITRATE: u32 = 20_000_000;
/// AAC bit rate of video recordings in bits per second.
pub const VIDEO_AUDIO_BITRATE: u32 = 192_000;
/// MP3 bit rate of audio recordings in bits per second.
pub const SOUND_BITRATE: u32 = 320_000;
/// Number of samples in one spectrum transform.
const SPECTRUM_SIZE: usize = 2_048;
/// Number of bands that the interface draws.
const SPECTRUM_BANDS: usize = 24;

/// The switches that stop and pause one recording.
#[derive(Clone, Debug, Default)]
pub struct Control {
    /// Ends the recording and closes the file.
    pub stop: Arc<AtomicBool>,
    /// Holds the recording without closing the file.
    pub paused: Arc<AtomicBool>,
}

/// Receives the updates of one recording.
pub type Report<U> = Arc<dyn Fn(U) + Send + Sync>;

/// Returns the folder that receives the recordings of one mode.
#[must_use]
pub fn default_folder(mode: Mode) -> PathBuf {
    let home = env::var_os("USERPROFILE").map_or_else(|| PathBuf::from("."), PathBuf::from);
    match mode {
        Mode::Audio => home.join("Music").join("record"),
        Mode::Video => home.join("Videos").join("record"),
        Mode::Screenshot => record_core::screenshot::default_directory(),
    }
}

/// Creates the folder of an output file and formats a failure.
fn prepare(output: &Path) -> Result<(), String> {
    match output.parent() {
        Some(parent) => fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display())),
        None => Ok(()),
    }
}

/// Splits an engine result into the file name and the failure text.
fn outcome(result: Result<PathBuf, String>) -> (Option<String>, Option<String>) {
    match result {
        Ok(path) => (Some(file_name(&path)), None),
        Err(error) => (None, Some(error)),
    }
}

/// Starts an audio recording on a new thread.
///
/// The recording writes numbered parts into `directory`. A clip request on
/// `commands` closes the current part under a name, and `saved` receives
/// each finished file. The last update is always `Stopped`.
pub fn start_sound(
    directory: PathBuf,
    control: &Control,
    commands: Receiver<AudioCommand>,
    saved: Arc<dyn Fn(SavedFile) + Send + Sync>,
    report: Report<SoundUpdate>,
) {
    let control = control.clone();
    let worker = Arc::clone(&report);
    let started = thread::Builder::new()
        .name("sound".to_owned())
        .spawn(move || {
            let report = worker;
            let (events, event_receiver) = unbounded::<AudioEvent>();
            let translated = Arc::clone(&report);
            let translator = thread::spawn(move || {
                let mut rate = 48_000_u32;
                let mut spectrum = None;
                let mut window = VecDeque::with_capacity(SPECTRUM_SIZE);
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
                        } => {
                            for (left, right) in left.iter().zip(&right) {
                                if window.len() == SPECTRUM_SIZE {
                                    window.pop_front();
                                }
                                window.push_back((left + right) / 2.0);
                            }
                            let spectrum =
                                spectrum.get_or_insert_with(|| Spectrum::new(SPECTRUM_SIZE));
                            spectrum.update(window.iter().copied());
                            SoundUpdate::Block {
                                left: extent(&left),
                                right: extent(&right),
                                seconds: encoded_frames as f64 / f64::from(rate),
                                bands: spectrum_bands(spectrum.bins(), rate, SPECTRUM_BANDS),
                            }
                        }
                        AudioEvent::Finalizing => SoundUpdate::Finalizing,
                        AudioEvent::Notice(text) => SoundUpdate::Notice { text },
                        AudioEvent::Saved(file) => {
                            saved(file);
                            continue;
                        }
                    };
                    translated(update);
                }
            });
            let config = RecordConfig {
                target: OutputTarget::Session {
                    directory,
                    segment_duration: DEFAULT_SEGMENT_DURATION,
                },
                bitrate: SOUND_BITRATE,
                duration: None,
                commands,
                stop: control.stop,
                paused: control.paused,
            };
            let result = audio::record(config, &events)
                .map(|summary| summary.output)
                .map_err(|error| format!("{error:#}"));
            drop(events);
            let _ = translator.join();
            let (file, error) = outcome(result);
            report(SoundUpdate::Stopped { file, error });
        });
    if let Err(error) = started {
        report(SoundUpdate::Stopped {
            file: None,
            error: Some(format!("could not start the audio thread: {error}")),
        });
    }
}

/// Starts a video recording of one monitor on a new thread.
///
/// `saved` receives the encoding of the finished file. The last update is
/// always `Stopped`.
pub fn start_video(
    output: PathBuf,
    monitor: u32,
    fps: u32,
    control: &Control,
    saved: Arc<dyn Fn(&VideoSummary) + Send + Sync>,
    report: Report<VideoUpdate>,
) {
    let control = control.clone();
    let worker = Arc::clone(&report);
    let started = thread::Builder::new()
        .name("video".to_owned())
        .spawn(move || {
            let report = worker;
            let (events, event_receiver) = unbounded::<VideoEvent>();
            let translated = Arc::clone(&report);
            let translator = thread::spawn(move || {
                for event in event_receiver {
                    let update = match event {
                        VideoEvent::Started {
                            width, height, fps, ..
                        } => VideoUpdate::Started { width, height, fps },
                        VideoEvent::Finalizing => VideoUpdate::Finalizing,
                        VideoEvent::Notice(text) => VideoUpdate::Notice { text },
                        VideoEvent::Saved(_) => continue,
                    };
                    translated(update);
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
                    audio_bitrate: VIDEO_AUDIO_BITRATE,
                    fps,
                    duration: None,
                    output,
                    replace: false,
                    stop: control.stop,
                    paused: control.paused,
                };
                // The interface draws its own preview of the monitor, so the
                // recorder sends none.
                video::record_video(config, &events, None)
                    .map(|summary| {
                        saved(&summary);
                        summary.output
                    })
                    .map_err(|error| format!("{error:#}"))
            });
            drop(events);
            let _ = translator.join();
            let (file, error) = outcome(result);
            report(VideoUpdate::Stopped { file, error });
        });
    if let Err(error) = started {
        report(VideoUpdate::Stopped {
            file: None,
            error: Some(format!("could not start the video thread: {error}")),
        });
    }
}

/// Reports whether a control asks its recording to stop.
#[must_use]
pub fn stopping(control: &Control) -> bool {
    control.stop.load(Ordering::Relaxed)
}
