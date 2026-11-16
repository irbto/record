//! Clips and edits of finished recordings.
//!
//! An audio recording can cut named clips while it runs, and a clip can be
//! trimmed afterward. A finished video opens in an editor that trims, fades,
//! and crops it. The engines of `record-core` do the work. This module keeps
//! what they need between two calls from the interface.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender};
use record_core::{
    audio::{AudioCommand, SavedFile, SavedFileKind},
    clip::{PreviewPlayer, load_pcm_waveform, trim_clip},
    glance::rgb_from_bgra,
    screenshot::{
        format::jpeg,
        upload::{read_setting, write_setting},
    },
    session::clip_stem,
    video::{
        VideoSummary,
        clip::{TrimProgress, trim_video_with_curves_and_progress},
        edit::{VideoEditCurves, VideoEditSettings},
        preview::{PreviewCommand, PreviewEvent, spawn_preview_worker},
        waveform::{WaveformLoad, load_mp4_waveform},
    },
};
use tauri::{AppHandle, Emitter, Manager, State, ipc::Response};

use crate::{
    commands::AppState,
    engines::{VIDEO_AUDIO_BITRATE, VIDEO_BITRATE},
    payload::{ClipShape, EditUpdate, SoundUpdate, VideoEdit, VideoInfo, file_name},
};

/// Event that carries the progress of a video export.
const EDIT_EVENT: &str = "edit";
/// Settings file that remembers the encoding of each recorded video.
const VIDEO_INDEX: &str = "videos.txt";
/// Largest number of videos that the index remembers.
const VIDEO_INDEX_LIMIT: usize = 200;
/// Number of columns in a waveform for the interface.
const WAVEFORM_BINS: usize = 1_200;
/// Longest wait for one decoded picture.
const FRAME_WAIT: Duration = Duration::from_secs(3);
/// JPEG quality of an editor picture.
const FRAME_QUALITY: u8 = 82;
/// Audio rate of a video that this program did not record itself.
const ASSUMED_AUDIO_RATE: u32 = 48_000;

/// One video that is open in the editor.
struct OpenVideo {
    /// Encoding of the file. The export needs it.
    summary: VideoSummary,
    /// Requests to the decoder thread.
    commands: Sender<PreviewCommand>,
    /// Pictures from the decoder thread.
    events: Receiver<PreviewEvent>,
    /// The decoder thread. It holds the file open.
    worker: JoinHandle<()>,
}

impl OpenVideo {
    /// Stops the decoder and waits until it released the file.
    fn close(self) {
        let _ = self.commands.send(PreviewCommand::Close);
        drop(self.commands);
        let _ = self.worker.join();
    }
}

/// Everything that clips and edits keep between two calls.
#[derive(Default)]
pub struct EditState {
    /// Clip requests to the running audio recording.
    clips: Mutex<Option<Sender<AudioCommand>>>,
    /// Every file of the newest audio session, in capture order.
    saves: Mutex<Vec<SavedFile>>,
    /// Encoding of each video recorded since the program started.
    videos: Mutex<HashMap<PathBuf, VideoSummary>>,
    /// The video in the editor.
    editor: Mutex<Option<OpenVideo>>,
    /// Stops the clip that plays now.
    playing: Mutex<Option<Arc<AtomicBool>>>,
}

/// Locks a mutex and keeps its value after a panic on another thread.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Formats one line of the video index.
#[must_use]
pub fn index_line(summary: &VideoSummary) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        summary.output.display(),
        summary.width,
        summary.height,
        summary.fps,
        summary.frames,
        summary.sample_rate,
        summary.video_bitrate,
        summary.audio_bitrate,
    )
}

/// Reads one line of the video index. A damaged line gives `None`.
#[must_use]
pub fn parse_index_line(line: &str) -> Option<VideoSummary> {
    let mut fields = line.split('\t');
    let output = PathBuf::from(fields.next().filter(|path| !path.is_empty())?);
    let mut number = || fields.next()?.trim().parse::<u64>().ok();
    let (width, height, fps, frames) = (number()?, number()?, number()?, number()?);
    let (sample_rate, video_bitrate, audio_bitrate) = (number()?, number()?, number()?);
    Some(VideoSummary {
        output,
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        fps: u32::try_from(fps).ok().filter(|fps| *fps > 0)?,
        frames,
        sample_rate: u32::try_from(sample_rate).ok()?,
        video_bitrate: u32::try_from(video_bitrate).ok()?,
        audio_bitrate: u32::try_from(audio_bitrate).ok()?,
        recording_ready_ms: None,
        finalize_ms: 0.0,
    })
}

/// Replaces or adds one video in the text of the index, newest last.
#[must_use]
pub fn updated_index(text: &str, summary: &VideoSummary) -> String {
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|line| parse_index_line(line).is_some_and(|other| other.output != summary.output))
        .collect();
    let line = index_line(summary);
    lines.push(&line);
    let skip = lines.len().saturating_sub(VIDEO_INDEX_LIMIT);
    let mut text = lines[skip..].join("\n");
    text.push('\n');
    text
}

/// Removes one video from the text of the index.
#[must_use]
pub fn index_without(text: &str, path: &Path) -> String {
    text.lines()
        .filter(|line| parse_index_line(line).is_some_and(|other| other.output != path))
        .map(|line| format!("{line}\n"))
        .collect()
}

impl EditState {
    /// Prepares for a new audio recording that accepts clip requests.
    pub fn begin_audio(&self, clips: Sender<AudioCommand>) {
        lock(&self.saves).clear();
        *lock(&self.clips) = Some(clips);
    }

    /// Forgets the clip channel of a recording that ended.
    pub fn end_audio(&self) {
        *lock(&self.clips) = None;
    }

    /// Forgets every file of the audio session, so that nothing holds the
    /// files of a discarded recording open.
    pub fn forget_audio(&self) {
        if let Some(stop) = lock(&self.playing).take() {
            stop.store(true, Ordering::Relaxed);
        }
        lock(&self.saves).clear();
    }

    /// Forgets the encoding of a video that no longer exists.
    pub fn forget_video(&self, path: &Path) {
        lock(&self.videos).remove(path);
        if let Some(text) = read_setting(VIDEO_INDEX) {
            let _ = write_setting(VIDEO_INDEX, &index_without(&text, path));
        }
    }

    /// Remembers the encoding of a video, also for later runs.
    pub fn remember_video(&self, summary: &VideoSummary) {
        lock(&self.videos).insert(summary.output.clone(), summary.clone());
        let text = read_setting(VIDEO_INDEX).unwrap_or_default();
        // The index only saves a probe of the file later. A failed write
        // costs nothing now.
        let _ = write_setting(VIDEO_INDEX, &updated_index(&text, summary));
    }

    /// Finds the stored encoding of a video.
    fn known_video(&self, path: &Path) -> Option<VideoSummary> {
        lock(&self.videos).get(path).cloned().or_else(|| {
            read_setting(VIDEO_INDEX)?
                .lines()
                .filter_map(parse_index_line)
                .find(|summary| summary.output == path)
        })
    }
}

/// Reports one finished file of the audio session to the interface.
pub fn audio_saved(app: &AppHandle, file: SavedFile) {
    let update = SoundUpdate::Saved {
        name: file_name(&file.path),
        path: file.path.display().to_string(),
        clip: file.kind == SavedFileKind::Clip,
        seconds: file.duration().as_secs_f64(),
        editable: file.edit_source.is_some(),
    };
    lock(&app.state::<EditState>().saves).push(file);
    let _ = app.emit("sound", update);
}

/// Cuts the audio recorded since the last file into a named clip.
///
/// The recording continues in a new part.
#[tauri::command(async)]
pub fn save_clip(edits: State<'_, EditState>, name: &str) -> Result<(), String> {
    let stem = clip_stem(name).map_err(|error| error.to_string())?;
    let clips = lock(&edits.clips);
    let sender = clips.as_ref().ok_or("No audio recording runs")?;
    sender
        .try_send(AudioCommand::SaveClip(stem))
        .map_err(|_| "The recorder is busy. Try again.".to_owned())
}

/// Finds one clip of the audio session that can still be trimmed.
fn editable_clip(edits: &EditState, path: &str) -> Result<SavedFile, String> {
    lock(&edits.saves)
        .iter()
        .find(|file| file.path == Path::new(path) && file.edit_source.is_some())
        .cloned()
        .ok_or_else(|| "This clip can no longer be edited".to_owned())
}

/// Returns the shape of one clip for the trim view.
#[tauri::command(async)]
pub fn clip_waveform(edits: State<'_, EditState>, path: &str) -> Result<ClipShape, String> {
    let file = editable_clip(&edits, path)?;
    let source = file.edit_source.as_ref().ok_or("The clip has no source")?;
    let waveform = load_pcm_waveform(
        &source.pcm_path,
        source.start_frame,
        source.end_frame,
        WAVEFORM_BINS,
    )
    .map_err(|error| format!("{error:#}"))?;
    let pairs = |bins: &[record_core::clip::WaveformBin]| {
        bins.iter().map(|bin| [bin.minimum, bin.maximum]).collect()
    };
    Ok(ClipShape {
        left: pairs(&waveform.left),
        right: pairs(&waveform.right),
        seconds: (source.end_frame - source.start_frame) as f64
            / f64::from(file.sample_rate.max(1)),
    })
}

/// Stops the clip that plays now.
#[tauri::command(async)]
pub fn stop_clip(edits: State<'_, EditState>) {
    if let Some(stop) = lock(&edits.playing).take() {
        stop.store(true, Ordering::Relaxed);
    }
}

/// Plays one part of a clip through the speakers.
///
/// A recording that runs pauses for that time, because it would record the
/// clip again.
#[tauri::command(async)]
pub fn play_clip(
    app: AppHandle,
    edits: State<'_, EditState>,
    path: &str,
    start: f64,
    end: f64,
) -> Result<(), String> {
    let file = editable_clip(&edits, path)?;
    if !(start >= 0.0 && end > start) {
        return Err("The range is empty".to_owned());
    }
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(previous) = lock(&edits.playing).replace(Arc::clone(&stop)) {
        previous.store(true, Ordering::Relaxed);
    }
    let range = (Duration::from_secs_f64(start), Duration::from_secs_f64(end));
    // The player belongs to the thread that opened it.
    thread::Builder::new()
        .name("clip-player".to_owned())
        .spawn(move || {
            let pause = app.state::<AppState>().pause_switch();
            let was_paused = pause
                .as_ref()
                .map(|pause| pause.swap(true, Ordering::Relaxed));
            if let Ok(mut player) = PreviewPlayer::start(&file.path, range.0, range.1) {
                while !stop.load(Ordering::Relaxed) && player.is_playing().unwrap_or(false) {
                    thread::sleep(Duration::from_millis(40));
                }
                player.stop();
            }
            if let (Some(pause), Some(was_paused)) = (pause, was_paused) {
                pause.store(was_paused, Ordering::Relaxed);
            }
            let _ = app.emit("played", ());
        })
        .map(drop)
        .map_err(|error| format!("could not start the player: {error}"))
}

/// Rewrites a clip so that it holds only one part of itself.
///
/// The times count from the start of the clip. Returns the new length in
/// seconds.
#[tauri::command(async)]
pub fn trim_audio_clip(
    edits: State<'_, EditState>,
    path: &str,
    start: f64,
    end: f64,
) -> Result<f64, String> {
    stop_clip(edits.clone());
    let file = editable_clip(&edits, path)?;
    let source = file.edit_source.as_ref().ok_or("The clip has no source")?;
    let rate = f64::from(file.sample_rate.max(1));
    let length = source.end_frame - source.start_frame;
    let first = ((start.max(0.0) * rate).round() as u64).min(length);
    let last = ((end.max(0.0) * rate).round() as u64).min(length);
    if first >= last {
        return Err("The range is empty".to_owned());
    }
    let (start_frame, end_frame) = (source.start_frame + first, source.start_frame + last);
    let frames = trim_clip(
        &source.pcm_path,
        &file.path,
        file.sample_rate,
        file.bitrate,
        start_frame,
        end_frame,
    )
    .map_err(|error| format!("{error:#}"))?;
    if let Some(stored) = lock(&edits.saves)
        .iter_mut()
        .find(|stored| stored.path == file.path)
    {
        stored.frames = frames;
        if let Some(source) = &mut stored.edit_source {
            source.start_frame = start_frame;
            source.end_frame = end_frame;
        }
    }
    Ok(frames as f64 / rate)
}

/// Starts the decoder of one video and waits for its format.
fn open_video(edits: &EditState, path: &Path) -> Result<OpenVideo, String> {
    let (commands, events, worker) =
        spawn_preview_worker(path.to_path_buf()).map_err(|error| format!("{error:#}"))?;
    let format = loop {
        match events.recv_timeout(FRAME_WAIT) {
            Ok(PreviewEvent::Ready(format)) => break format,
            Ok(PreviewEvent::Failed(error)) => return Err(error),
            Ok(_) => {}
            Err(_) => return Err("The video did not open".to_owned()),
        }
    };
    let summary = edits.known_video(path).unwrap_or_else(|| {
        // Another program made this file. Its picture format comes from the
        // decoder, and the rest is what this program would use.
        let fps = (format.fps.round() as u32).max(1);
        VideoSummary {
            output: path.to_path_buf(),
            width: format.width,
            height: format.height,
            fps,
            frames: (format.duration.as_secs_f64() * f64::from(fps)).round() as u64,
            sample_rate: ASSUMED_AUDIO_RATE,
            video_bitrate: VIDEO_BITRATE,
            audio_bitrate: VIDEO_AUDIO_BITRATE,
            recording_ready_ms: None,
            finalize_ms: 0.0,
        }
    });
    Ok(OpenVideo {
        summary,
        commands,
        events,
        worker,
    })
}

/// Describes an open video for the interface.
fn video_info(summary: &VideoSummary) -> VideoInfo {
    VideoInfo {
        name: file_name(&summary.output),
        path: summary.output.display().to_string(),
        width: summary.width,
        height: summary.height,
        fps: summary.fps,
        frames: summary.frames,
    }
}

/// Opens one saved video in the editor.
#[tauri::command(async)]
pub fn open_video_editor(
    state: State<'_, AppState>,
    edits: State<'_, EditState>,
    path: &str,
) -> Result<VideoInfo, String> {
    let file = state.listed(path)?;
    let mut editor = lock(&edits.editor);
    if let Some(previous) = editor.take() {
        previous.close();
    }
    // The index stores the path as the recorder wrote it.
    let known = edits
        .known_video(Path::new(path))
        .map(|summary| summary.output);
    let video = open_video(&edits, &known.unwrap_or(file))?;
    let info = video_info(&video.summary);
    *editor = Some(video);
    Ok(info)
}

/// Closes the editor and releases the file.
#[tauri::command(async)]
pub fn close_video_editor(edits: State<'_, EditState>) {
    if let Some(video) = lock(&edits.editor).take() {
        video.close();
    }
}

/// Returns the picture of the open video at one time, as a JPEG.
#[tauri::command(async)]
pub fn editor_frame(edits: State<'_, EditState>, seconds: f64) -> Result<Response, String> {
    let editor = lock(&edits.editor);
    let video = editor.as_ref().ok_or("No video is open")?;
    // A picture from an earlier request must not answer this one.
    while video.events.try_recv().is_ok() {}
    video
        .commands
        .send(PreviewCommand::Seek(Duration::from_secs_f64(
            seconds.max(0.0),
        )))
        .map_err(|_| "The video closed".to_owned())?;
    loop {
        match video.events.recv_timeout(FRAME_WAIT) {
            Ok(PreviewEvent::Frame(frame)) => {
                let rgb = rgb_from_bgra(&frame.bgra);
                return jpeg(&rgb, frame.width, frame.height, FRAME_QUALITY)
                    .map(Response::new)
                    .map_err(|error| format!("{error:#}"));
            }
            Ok(PreviewEvent::End) => return Err("The video ends before this time".to_owned()),
            Ok(PreviewEvent::Failed(error)) => return Err(error),
            Ok(PreviewEvent::Ready(_)) => {}
            Err(_) => return Err("The picture took too long".to_owned()),
        }
    }
}

/// Returns the loudness of the open video over its whole length.
///
/// Each entry holds the lowest and the highest sample of one column. A video
/// without audio gives an empty list.
#[tauri::command(async)]
pub fn editor_waveform(edits: State<'_, EditState>) -> Result<Vec<[f32; 2]>, String> {
    let path = lock(&edits.editor)
        .as_ref()
        .map(|video| video.summary.output.clone())
        .ok_or("No video is open")?;
    let never = AtomicBool::new(false);
    match load_mp4_waveform(&path, WAVEFORM_BINS, &never).map_err(|error| format!("{error:#}"))? {
        WaveformLoad::Ready(bins) => {
            Ok(bins.iter().map(|bin| [bin.minimum, bin.maximum]).collect())
        }
        WaveformLoad::Unavailable | WaveformLoad::Cancelled => Ok(Vec::new()),
    }
}

/// Writes the edited video over the original file.
///
/// The work runs on its own thread and reports through the `edit` event. The
/// engine writes a new file first and replaces the original only when the new
/// file is complete.
#[tauri::command(async)]
pub fn export_video(
    app: AppHandle,
    edits: State<'_, EditState>,
    edit: VideoEdit,
) -> Result<(), String> {
    let video = lock(&edits.editor).take().ok_or("No video is open")?;
    let summary = video.summary.clone();
    // The decoder holds the file open, and the export replaces the file.
    video.close();
    let settings = VideoEditSettings {
        audio_fade_in: Duration::from_secs_f64(edit.audio_fade_in.max(0.0)),
        audio_fade_out: Duration::from_secs_f64(edit.audio_fade_out.max(0.0)),
        video_fade_in: Duration::from_secs_f64(edit.video_fade_in.max(0.0)),
        video_fade_out: Duration::from_secs_f64(edit.video_fade_out.max(0.0)),
        crop_left: edit.crop_left,
        crop_right: edit.crop_right,
        crop_top: edit.crop_top,
        crop_bottom: edit.crop_bottom,
    };
    let keyframes = |points: &[[f64; 2]]| {
        points
            .iter()
            .filter(|point| point[0].is_finite() && point[1].is_finite())
            .map(|point| (Duration::from_secs_f64(point[0].max(0.0)), point[1]))
            .collect()
    };
    let curves = VideoEditCurves {
        video_gain: keyframes(&edit.opacity),
        audio_gain: keyframes(&edit.volume),
    };
    thread::Builder::new()
        .name("video-export".to_owned())
        .spawn(move || {
            let edits = app.state::<EditState>();
            let progress = |stage: TrimProgress| {
                let update = match stage {
                    TrimProgress::Preparing => EditUpdate::Stage { name: "Preparing" },
                    TrimProgress::Processing {
                        completed_frames,
                        total_frames,
                    } => EditUpdate::Progress {
                        done: completed_frames,
                        total: total_frames,
                    },
                    TrimProgress::Finishing => EditUpdate::Stage { name: "Finishing" },
                    TrimProgress::Saving => EditUpdate::Stage { name: "Saving" },
                };
                let _ = app.emit(EDIT_EVENT, update);
            };
            let result = trim_video_with_curves_and_progress(
                &summary.output,
                &summary,
                edit.start_frame,
                edit.end_frame,
                &settings,
                &curves,
                progress,
            );
            let (saved, error) = match result {
                Ok(saved) => (saved, None),
                Err(error) => (summary, Some(format!("{error:#}"))),
            };
            if error.is_none() {
                edits.remember_video(&saved);
            }
            // The editor shows the file as it is now.
            let reopened = open_video(&edits, &saved.output);
            let info = reopened
                .as_ref()
                .ok()
                .map(|video| video_info(&video.summary));
            *lock(&edits.editor) = reopened.ok();
            let _ = app.emit(EDIT_EVENT, EditUpdate::Done { info, error });
        })
        .map(drop)
        .map_err(|error| format!("could not start the export: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(path: &str, frames: u64) -> VideoSummary {
        VideoSummary {
            output: PathBuf::from(path),
            width: 2_560,
            height: 1_440,
            fps: 60,
            frames,
            sample_rate: 48_000,
            video_bitrate: 20_000_000,
            audio_bitrate: 192_000,
            recording_ready_ms: None,
            finalize_ms: 0.0,
        }
    }

    #[test]
    fn a_deleted_video_leaves_the_index() {
        let first = summary(r"C:\Videos\record\a.mp4", 10);
        let second = summary(r"C:\Videos\record\b.mp4", 20);
        let text = updated_index(&updated_index("", &first), &second);
        let left = index_without(&text, &first.output);
        assert_eq!(left, format!("{}\n", index_line(&second)));
        assert_eq!(index_without(&left, &second.output), "");
        assert_eq!(index_without(&text, Path::new("other.mp4")), text);
    }

    #[test]
    fn an_index_line_reads_back() {
        let line = index_line(&summary(r"C:\Videos\record\a b.mp4", 900));
        let read = parse_index_line(&line).unwrap();
        assert_eq!(read.output, PathBuf::from(r"C:\Videos\record\a b.mp4"));
        assert_eq!(
            (read.width, read.height, read.fps, read.frames),
            (2_560, 1_440, 60, 900)
        );
        assert_eq!((read.sample_rate, read.audio_bitrate), (48_000, 192_000));
        for damaged in [
            "",
            "a.mp4",
            "a.mp4\t1\t2",
            "a.mp4\t1\t2\t0\t4\t5\t6\t7",
            "\t1\t2\t3\t4\t5\t6\t7",
        ] {
            assert!(parse_index_line(damaged).is_none(), "{damaged}");
        }
    }

    #[test]
    fn the_index_keeps_one_line_for_each_video() {
        let first = updated_index("noise\n", &summary("a.mp4", 100));
        let second = updated_index(&first, &summary("b.mp4", 200));
        let third = updated_index(&second, &summary("a.mp4", 50));
        let videos: Vec<VideoSummary> = third.lines().filter_map(parse_index_line).collect();
        let listed: Vec<(String, u64)> = videos
            .iter()
            .map(|video| (video.output.display().to_string(), video.frames))
            .collect();
        assert_eq!(
            listed,
            [("b.mp4".to_owned(), 200), ("a.mp4".to_owned(), 50)]
        );
        assert_eq!(third.lines().count(), 2);
    }
}
