//! Audio-only MP4 playback for the integrated video editor.
//!
//! A helper thread owns the Media Foundation Media Engine and every COM
//! object associated with it. The terminal sends low-frequency controls over
//! a bounded channel and receives coalesced position updates, so decoding and
//! audio rendering never block terminal input or drawing.

use std::{
    collections::VecDeque,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, bounded, select, tick, unbounded};
use windows::{
    Win32::{
        Media::{
            Audio::AudioCategory_Media,
            MediaFoundation::{
                CLSID_MFMediaEngineClassFactory, IMFByteStream, IMFMediaEngine,
                IMFMediaEngineClassFactory, IMFMediaEngineEx, IMFMediaEngineNotify,
                IMFMediaEngineNotify_Impl, MF_ACCESSMODE_READ, MF_FILEFLAGS_NONE,
                MF_MEDIA_ENGINE_AUDIO_CATEGORY, MF_MEDIA_ENGINE_AUDIOONLY,
                MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR,
                MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA, MF_MEDIA_ENGINE_EVENT_TIMELINE_MARKER,
                MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC, MF_MEDIA_ENGINE_READY_HAVE_METADATA,
                MF_OPENMODE_FAIL_IF_NOT_EXIST, MFCreateAttributes, MFCreateFile,
            },
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::{BSTR, Interface, PCWSTR, implement},
};

use crate::audio::windows::{ComGuard, MediaFoundationGuard};

/// Frequency of playback-position updates while a range is playing.
const POSITION_INTERVAL: Duration = Duration::from_millis(40);

/// Public events retain room for state changes even when one position is stale.
const EVENT_CAPACITY: usize = 8;

/// Controls one audio-only Media Engine running on its worker thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioPreviewCommand {
    /// Seeks to `start` and plays until the exclusive `end` boundary.
    PlayRange {
        /// First presentation time included in playback.
        start: Duration,
        /// Presentation time at which playback stops.
        end: Duration,
    },
    /// Pauses at the current presentation time.
    Pause,
    /// Pauses for a scrub or edit without reporting a stale pre-seek position.
    PauseForEdit,
    /// Continues the active range, restarting it when already at its end.
    Resume,
    /// Seeks within the active range while preserving its play/pause state.
    Seek(Duration),
    /// Sets the preview gain. Finite values are clamped to `0.0..=1.0`.
    SetVolume(f64),
    /// Stops playback and releases the MP4.
    Close,
}

/// State changes emitted by the audio-preview worker.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioPreviewEvent {
    /// The MP4 opened and its audio duration is available.
    Ready {
        /// Total presentation duration of the finalized MP4.
        duration: Duration,
    },
    /// A coalesced playback-position update.
    Position {
        /// Current presentation time, clamped to the active range.
        position: Duration,
        /// Whether the active range is currently advancing.
        playing: bool,
    },
    /// Playback paused at this presentation time.
    Paused {
        /// Presentation time retained for a later [`AudioPreviewCommand::Resume`].
        position: Duration,
    },
    /// Playback reached the active range boundary.
    End {
        /// The exact requested range end, after clamping to media duration.
        position: Duration,
    },
    /// Playback stopped because the worker encountered a hard error.
    Failed(String),
}

/// Spawns an audio-only Media Foundation worker for one finalized MP4.
///
/// The worker does not require a window handle. Dropping the command sender is
/// equivalent to sending [`AudioPreviewCommand::Close`].
pub fn spawn_audio_preview_worker(
    path: PathBuf,
) -> Result<(
    Sender<AudioPreviewCommand>,
    Receiver<AudioPreviewEvent>,
    thread::JoinHandle<()>,
)> {
    let (commands, command_receiver) = bounded(8);
    let (event_sender, events) = bounded(EVENT_CAPACITY);
    let worker = thread::Builder::new()
        .name("record-video-audio-preview".to_owned())
        .spawn(move || run_worker(&path, &command_receiver, &event_sender))
        .context("could not start the video audio-preview thread")?;
    Ok((commands, events, worker))
}

/// Owns initialization, event translation, and shutdown for one worker.
fn run_worker(
    path: &Path,
    commands: &Receiver<AudioPreviewCommand>,
    events: &Sender<AudioPreviewEvent>,
) {
    let outcome = run_worker_inner(path, commands, events);
    if let Err(error) = outcome {
        let _ = events.try_send(AudioPreviewEvent::Failed(format!("{error:#}")));
    }
}

/// Serves commands while keeping every Media Engine call on this MTA thread.
fn run_worker_inner(
    path: &Path,
    commands: &Receiver<AudioPreviewCommand>,
    events: &Sender<AudioPreviewEvent>,
) -> Result<()> {
    let _com = ComGuard::new()?;
    let _media_foundation = MediaFoundationGuard::new()?;
    let (native_sender, native_events) = unbounded();
    let player = unsafe { MediaEnginePlayer::open(path, native_sender)? };
    let timer = tick(POSITION_INTERVAL);
    let mut state = PlaybackState::default();
    let mut pending = VecDeque::new();

    loop {
        select! {
            recv(commands) -> command => {
                let Ok(command) = command else { break };
                if command == AudioPreviewCommand::Close {
                    break;
                }
                if !state.ready && !matches!(command, AudioPreviewCommand::SetVolume(_)) {
                    pending.push_back(command);
                    continue;
                }
                unsafe { handle_command(&player, &mut state, events, command)? };
            }
            recv(native_events) -> event => {
                let Ok(event) = event else { continue };
                if unsafe { handle_native_event(&player, &mut state, events, event)? } {
                    while let Some(command) = pending.pop_front() {
                        unsafe { handle_command(&player, &mut state, events, command)? };
                    }
                }
            }
            recv(timer) -> _ => {
                if !state.ready && unsafe { player.has_metadata() } {
                    unsafe { announce_ready(&player, &mut state, events)? };
                    while let Some(command) = pending.pop_front() {
                        unsafe { handle_command(&player, &mut state, events, command)? };
                    }
                }
                unsafe { poll_position(&player, &mut state, events)? };
            }
        }
    }
    Ok(())
}

/// Bounded range currently controlled by the interface.
#[derive(Clone, Copy, Debug)]
struct PreviewRange {
    start: Duration,
    end: Duration,
}

/// Worker-owned state that does not rely on asynchronous HTML media flags.
#[derive(Default)]
struct PlaybackState {
    ready: bool,
    duration: Duration,
    range: Option<PreviewRange>,
    playing: bool,
    at_range_end: bool,
}

/// Applies one public command after metadata has loaded.
///
/// # Safety
///
/// The player must belong to this initialized COM worker thread.
unsafe fn handle_command(
    player: &MediaEnginePlayer,
    state: &mut PlaybackState,
    events: &Sender<AudioPreviewEvent>,
    command: AudioPreviewCommand,
) -> Result<()> {
    match command {
        AudioPreviewCommand::PlayRange { start, end } => {
            let range = checked_range(start, end, state.duration)?;
            unsafe { player.pause()? };
            unsafe { player.seek(range.start)? };
            unsafe { player.arm_end_marker(range.end)? };
            unsafe { player.play()? };
            state.range = Some(range);
            state.playing = true;
            state.at_range_end = false;
            emit_position(events, range.start, true);
        }
        AudioPreviewCommand::Pause | AudioPreviewCommand::PauseForEdit => {
            let position = state.position(player);
            unsafe { player.pause()? };
            state.playing = false;
            if command == AudioPreviewCommand::Pause {
                let _ = events.try_send(AudioPreviewEvent::Paused { position });
            }
        }
        AudioPreviewCommand::Resume => {
            let range = state
                .range
                .ok_or_else(|| anyhow!("no audio preview range is active"))?;
            let position = state.position(player);
            if state.at_range_end || position >= range.end {
                unsafe { player.seek(range.start)? };
                emit_position(events, range.start, true);
            }
            unsafe { player.arm_end_marker(range.end)? };
            unsafe { player.play()? };
            state.playing = true;
            state.at_range_end = false;
        }
        AudioPreviewCommand::Seek(target) => {
            let range = state.range.unwrap_or(PreviewRange {
                start: Duration::ZERO,
                end: state.duration,
            });
            let target = target.clamp(range.start, range.end);
            unsafe { player.seek(target)? };
            state.at_range_end = target >= range.end;
            if state.playing && state.at_range_end {
                finish_range(player, state, events)?;
            } else {
                if state.playing {
                    unsafe { player.arm_end_marker(range.end)? };
                }
                emit_position(events, target, state.playing);
            }
        }
        AudioPreviewCommand::SetVolume(volume) => {
            if !volume.is_finite() {
                bail!("audio preview volume must be finite");
            }
            unsafe { player.set_volume(volume.clamp(0.0, 1.0))? };
        }
        AudioPreviewCommand::Close => {}
    }
    Ok(())
}

/// Handles the small subset of callbacks that affect worker state.
///
/// Returns true when metadata became ready during this call.
///
/// # Safety
///
/// The player must belong to this initialized COM worker thread.
unsafe fn handle_native_event(
    player: &MediaEnginePlayer,
    state: &mut PlaybackState,
    events: &Sender<AudioPreviewEvent>,
    event: NativeEvent,
) -> Result<bool> {
    match event {
        NativeEvent::Metadata => {
            if !state.ready && unsafe { player.has_metadata() } {
                unsafe { announce_ready(player, state, events)? };
                return Ok(true);
            }
        }
        NativeEvent::TimelineMarker | NativeEvent::SourceEnded => {
            if state.playing && state.range.is_some() {
                finish_range(player, state, events)?;
            }
        }
        NativeEvent::Error { code, hresult } => {
            bail!("Windows Media Engine error {code} (HRESULT 0x{hresult:08X})");
        }
    }
    Ok(false)
}

/// Announces source metadata exactly once.
///
/// # Safety
///
/// The player must belong to this initialized COM worker thread.
unsafe fn announce_ready(
    player: &MediaEnginePlayer,
    state: &mut PlaybackState,
    events: &Sender<AudioPreviewEvent>,
) -> Result<()> {
    if !unsafe { player.has_audio() } {
        bail!("the finalized MP4 does not contain a playable audio stream");
    }
    let duration = unsafe { player.duration()? };
    state.ready = true;
    state.duration = duration;
    let _ = events.try_send(AudioPreviewEvent::Ready { duration });
    Ok(())
}

/// Polls for UI position updates and provides a fallback for marker delivery.
///
/// # Safety
///
/// The player must belong to this initialized COM worker thread.
unsafe fn poll_position(
    player: &MediaEnginePlayer,
    state: &mut PlaybackState,
    events: &Sender<AudioPreviewEvent>,
) -> Result<()> {
    let Some(range) = state.range else {
        return Ok(());
    };
    if !state.playing {
        return Ok(());
    }
    let position = state.position(player);
    if position >= range.end || unsafe { player.ended() } {
        finish_range(player, state, events)?;
    } else {
        emit_position(events, position, true);
    }
    Ok(())
}

/// Stops exactly at the active range end and emits one terminal event.
fn finish_range(
    player: &MediaEnginePlayer,
    state: &mut PlaybackState,
    events: &Sender<AudioPreviewEvent>,
) -> Result<()> {
    let Some(range) = state.range else {
        return Ok(());
    };
    if !state.playing {
        return Ok(());
    }
    unsafe { player.pause()? };
    unsafe { player.cancel_end_marker() };
    unsafe { player.seek(range.end)? };
    state.playing = false;
    state.at_range_end = true;
    let _ = events.try_send(AudioPreviewEvent::End {
        position: range.end,
    });
    Ok(())
}

/// Keeps at most one low-priority position event waiting in the public queue.
fn emit_position(events: &Sender<AudioPreviewEvent>, position: Duration, playing: bool) {
    if events.is_empty() {
        let _ = events.try_send(AudioPreviewEvent::Position { position, playing });
    }
}

/// Validates and clamps one requested range to the finalized media duration.
fn checked_range(start: Duration, end: Duration, duration: Duration) -> Result<PreviewRange> {
    let end = end.min(duration);
    if start >= end {
        bail!("the audio preview range is empty");
    }
    Ok(PreviewRange { start, end })
}

/// Reads the player's current time and clamps it to the current editor range.
impl PlaybackState {
    fn position(&self, player: &MediaEnginePlayer) -> Duration {
        let position = unsafe { player.position() };
        self.range.map_or_else(
            || position.min(self.duration),
            |range| position.clamp(range.start, range.end),
        )
    }
}

/// Callback messages retained from the asynchronous Media Engine thread.
#[derive(Clone, Copy, Debug)]
enum NativeEvent {
    Metadata,
    TimelineMarker,
    SourceEnded,
    Error { code: usize, hresult: u32 },
}

/// Minimal COM callback that only wakes the owning worker thread.
#[implement(IMFMediaEngineNotify)]
struct MediaEngineCallback {
    events: Sender<NativeEvent>,
}

#[allow(non_snake_case)]
impl IMFMediaEngineNotify_Impl for MediaEngineCallback_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        let event = if event == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 as u32 {
            Some(NativeEvent::Metadata)
        } else if event == MF_MEDIA_ENGINE_EVENT_TIMELINE_MARKER.0 as u32 {
            Some(NativeEvent::TimelineMarker)
        } else if event == MF_MEDIA_ENGINE_EVENT_ENDED.0 as u32 {
            Some(NativeEvent::SourceEnded)
        } else if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 {
            Some(NativeEvent::Error {
                code: param1,
                hresult: param2,
            })
        } else {
            None
        };
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        Ok(())
    }
}

/// Native audio-only engine and the byte stream retained for its source.
struct MediaEnginePlayer {
    engine: IMFMediaEngine,
    extended: IMFMediaEngineEx,
    _source: IMFByteStream,
}

impl MediaEnginePlayer {
    /// Opens a local MP4 without converting its path to a URL.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the calling thread.
    unsafe fn open(path: &Path, native_events: Sender<NativeEvent>) -> Result<Self> {
        let absolute = std::path::absolute(path)
            .with_context(|| format!("could not resolve video path {}", path.display()))?;
        let wide = absolute
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let source = unsafe {
            MFCreateFile(
                MF_ACCESSMODE_READ,
                MF_OPENMODE_FAIL_IF_NOT_EXIST,
                MF_FILEFLAGS_NONE,
                PCWSTR(wide.as_ptr()),
            )
        }
        .with_context(|| format!("could not open video audio from {}", absolute.display()))?;

        let callback: IMFMediaEngineNotify = MediaEngineCallback {
            events: native_events,
        }
        .into();
        let mut attributes_slot = None;
        unsafe { MFCreateAttributes(&mut attributes_slot, 2)? };
        let attributes = attributes_slot.context("Media Foundation returned no attributes")?;
        unsafe {
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &callback)?;
            attributes.SetUINT32(
                &MF_MEDIA_ENGINE_AUDIO_CATEGORY,
                AudioCategory_Media.0 as u32,
            )?;
        }
        let factory: IMFMediaEngineClassFactory = unsafe {
            CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
        }
        .context("could not create the Windows Media Engine")?;
        let engine =
            unsafe { factory.CreateInstance(MF_MEDIA_ENGINE_AUDIOONLY.0 as u32, &attributes) }
                .context("could not create an audio-only Windows Media Engine")?;
        let extended = engine
            .cast::<IMFMediaEngineEx>()
            .context("the Windows Media Engine does not support local byte streams")?;
        let source_name = BSTR::from(absolute.to_string_lossy().as_ref());
        unsafe {
            engine.SetAutoPlay(false)?;
            engine.SetLoop(false)?;
            engine.SetPreload(MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC)?;
            extended.SetSourceFromByteStream(&source, &source_name)?;
            engine.Load()?;
        }
        Ok(Self {
            engine,
            extended,
            _source: source,
        })
    }

    /// Reports whether duration and stream metadata can be queried.
    unsafe fn has_metadata(&self) -> bool {
        unsafe { self.engine.GetReadyState() >= MF_MEDIA_ENGINE_READY_HAVE_METADATA.0 as u16 }
    }

    /// Reports whether the selected source exposes decoded audio.
    unsafe fn has_audio(&self) -> bool {
        unsafe { self.engine.HasAudio().as_bool() }
    }

    /// Reads and validates the finalized source duration.
    unsafe fn duration(&self) -> Result<Duration> {
        duration_from_seconds(unsafe { self.engine.GetDuration() })
            .context("Windows returned an invalid video audio duration")
    }

    /// Reads the current presentation time, falling back to zero while loading.
    unsafe fn position(&self) -> Duration {
        duration_from_seconds(unsafe { self.engine.GetCurrentTime() }).unwrap_or_default()
    }

    /// Seeks to an absolute source presentation time.
    unsafe fn seek(&self, position: Duration) -> Result<()> {
        unsafe { self.engine.SetCurrentTime(position.as_secs_f64()) }
            .context("could not seek the video audio preview")
    }

    /// Begins or continues audio rendering.
    unsafe fn play(&self) -> Result<()> {
        unsafe { self.engine.Play() }.context("could not play the video audio preview")
    }

    /// Pauses audio rendering without changing the current time.
    unsafe fn pause(&self) -> Result<()> {
        unsafe { self.engine.Pause() }.context("could not pause the video audio preview")
    }

    /// Changes native Media Engine output gain.
    unsafe fn set_volume(&self, volume: f64) -> Result<()> {
        unsafe { self.engine.SetVolume(volume) }
            .context("could not change the video audio-preview volume")
    }

    /// Arms a presentation-time callback at the active range end.
    unsafe fn arm_end_marker(&self, end: Duration) -> Result<()> {
        unsafe { self.extended.SetTimelineMarkerTimer(end.as_secs_f64()) }
            .context("could not set the video audio-preview end marker")
    }

    /// Cancels a marker that might remain after manual pause or seek.
    unsafe fn cancel_end_marker(&self) {
        let _ = unsafe { self.extended.CancelTimelineMarkerTimer() };
    }

    /// Reports whether the underlying source itself reached EOS.
    unsafe fn ended(&self) -> bool {
        unsafe { self.engine.IsEnded().as_bool() }
    }
}

impl Drop for MediaEnginePlayer {
    fn drop(&mut self) {
        let _ = unsafe { self.engine.Pause() };
        let _ = unsafe { self.extended.CancelTimelineMarkerTimer() };
        let _ = unsafe { self.engine.Shutdown() };
    }
}

/// Converts finite, nonnegative Media Engine seconds into a Rust duration.
fn duration_from_seconds(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Mp4Writer, Nv12Frame, check_video_support};
    use crossbeam_channel::RecvTimeoutError;

    #[test]
    fn ranges_are_clamped_to_media_duration() {
        assert_eq!(
            checked_range(
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(3),
            )
            .unwrap()
            .end,
            Duration::from_secs(3)
        );
        assert!(
            checked_range(
                Duration::from_secs(3),
                Duration::from_secs(4),
                Duration::from_secs(3),
            )
            .is_err()
        );
    }

    #[test]
    fn seconds_conversion_rejects_invalid_media_times() {
        assert_eq!(
            duration_from_seconds(1.25),
            Some(Duration::from_millis(1_250))
        );
        assert_eq!(duration_from_seconds(f64::NAN), None);
        assert_eq!(duration_from_seconds(-1.0), None);
    }

    #[test]
    fn native_worker_plays_pauses_seeks_and_finishes_a_bounded_range() {
        if check_video_support().is_err() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio-preview-source.mp4");
        write_test_mp4(&path);

        let (commands, events, worker) = spawn_audio_preview_worker(path).unwrap();
        let duration = wait_ready(&events);
        assert!(duration >= Duration::from_millis(900));

        commands.send(AudioPreviewCommand::SetVolume(0.0)).unwrap();
        commands
            .send(AudioPreviewCommand::PlayRange {
                start: Duration::from_millis(100),
                end: Duration::from_millis(650),
            })
            .unwrap();
        let advancing = wait_for(&events, |event| match event {
            AudioPreviewEvent::Position {
                position,
                playing: true,
            } if *position >= Duration::from_millis(100) => Some(*position),
            _ => None,
        });
        commands.send(AudioPreviewCommand::Pause).unwrap();
        let paused = wait_for(&events, |event| match event {
            AudioPreviewEvent::Paused { position } => Some(*position),
            _ => None,
        });
        assert!(paused >= advancing);

        commands
            .send(AudioPreviewCommand::Seek(Duration::from_millis(400)))
            .unwrap();
        commands.send(AudioPreviewCommand::Resume).unwrap();
        let end = wait_for(&events, |event| match event {
            AudioPreviewEvent::End { position } => Some(*position),
            _ => None,
        });
        assert_eq!(end, Duration::from_millis(650));

        commands.send(AudioPreviewCommand::Close).unwrap();
        worker.join().unwrap();
    }

    /// Receives source metadata or turns worker failure into a useful panic.
    fn wait_ready(events: &Receiver<AudioPreviewEvent>) -> Duration {
        wait_for(events, |event| match event {
            AudioPreviewEvent::Ready { duration } => Some(*duration),
            _ => None,
        })
    }

    /// Waits for a matching event while surfacing asynchronous failures.
    fn wait_for<T>(
        events: &Receiver<AudioPreviewEvent>,
        mut found: impl FnMut(&AudioPreviewEvent) -> Option<T>,
    ) -> T {
        loop {
            match events.recv_timeout(Duration::from_secs(10)) {
                Ok(AudioPreviewEvent::Failed(message)) => {
                    panic!("audio preview failed: {message}")
                }
                Ok(event) => {
                    if let Some(value) = found(&event) {
                        return value;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("audio preview event timed out"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("audio preview worker disconnected")
                }
            }
        }
    }

    /// Writes one second of silent stereo audio and video through Mp4Writer.
    fn write_test_mp4(path: &Path) {
        let _com = ComGuard::new().unwrap();
        let _media_foundation = MediaFoundationGuard::new().unwrap();
        let mut writer = unsafe {
            Mp4Writer::open(
                path, 64, 48, 10, 2_000_000, 48_000, 192_000, true, None, 0.0,
            )
        }
        .unwrap();
        for frame in 0..10_u64 {
            unsafe {
                writer
                    .write_video_frame(
                        &Nv12Frame::black(64, 48),
                        Duration::from_millis(frame * 100),
                    )
                    .unwrap();
            }
        }
        unsafe { writer.write_audio(&vec![0_i16; 48_000 * 2]).unwrap() };
        unsafe { writer.close().unwrap() };
    }
}
