//! Native Windows loopback capture, conversion, rotation, and MP3 encoding.
//!
//! The backend opens the default render endpoint in shared loopback mode. It
//! converts the endpoint mix to stereo PCM16, performs stateful linear sample
//! rate conversion, and writes MP3 samples through Media Foundation. One audio
//! thread owns every COM audio object for its full lifetime.
//!
//! `OutputManager` splits encoded frames at exact integer boundaries. A session
//! writer also stores temporary stereo PCM for the active part. Automatic parts
//! discard that cache. Named clips lease their source until the TUI closes them.
//! Trimming writes a separate MP3 and uses `ReplaceFileW` only after finalization.
//!
//! Visualization uses `try_send` and reserves event capacity for state changes.
//! WASAPI buffers are always released before a packet error is returned.

use std::{
    ffi::c_void,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    mem,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Media::{
            Audio::{
                AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
                AUDCLNT_STREAMFLAGS_NOPERSIST, IAudioCaptureClient, IAudioClient,
                IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
                WAVEFORMATEXTENSIBLE, eConsole, eRender,
            },
            KernelStreaming::{
                KSDATAFORMAT_SUBTYPE_PCM, SPEAKER_BACK_CENTER, SPEAKER_BACK_LEFT,
                SPEAKER_BACK_RIGHT, SPEAKER_FRONT_CENTER, SPEAKER_FRONT_LEFT,
                SPEAKER_FRONT_LEFT_OF_CENTER, SPEAKER_FRONT_RIGHT, SPEAKER_FRONT_RIGHT_OF_CENTER,
                SPEAKER_LOW_FREQUENCY, SPEAKER_SIDE_LEFT, SPEAKER_SIDE_RIGHT,
                SPEAKER_TOP_BACK_CENTER, SPEAKER_TOP_BACK_LEFT, SPEAKER_TOP_BACK_RIGHT,
                SPEAKER_TOP_CENTER, SPEAKER_TOP_FRONT_CENTER, SPEAKER_TOP_FRONT_LEFT,
                SPEAKER_TOP_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE,
            },
            MediaFoundation::{
                IMFAttributes, IMFByteStream, IMFCollection, IMFMediaType, IMFSinkWriter,
                MF_ACCESSMODE_WRITE, MF_FILEFLAGS_NONE, MF_MT_ALL_SAMPLES_INDEPENDENT,
                MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
                MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
                MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_FIXED_SIZE_SAMPLES, MF_MT_MAJOR_TYPE,
                MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_OPENMODE_DELETE_IF_EXIST,
                MF_OPENMODE_FAIL_IF_EXIST, MF_VERSION, MFAudioFormat_MP3, MFAudioFormat_PCM,
                MFCreateFile, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
                MFCreateSinkWriterFromURL, MFMediaType_Audio, MFSTARTUP_FULL,
                MFSampleExtension_Discontinuity, MFShutdown, MFStartup, MFT_ENUM_FLAG_ALL,
                MFTranscodeGetAudioOutputAvailableTypes,
            },
            Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT},
        },
        Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW},
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize,
            },
            Threading::{
                AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
                WaitForSingleObject,
            },
        },
    },
    core::{Error as WindowsError, HRESULT, Interface, PCWSTR, w},
};

use crate::session::{OutputTarget, available_clip_path, part_path, segment_frame_limit};

use super::{
    AudioCommand, AudioEvent, ClipCacheLease, ClipEditSource, EventSender, RecordConfig,
    RecordingSummary, SavedFile, SavedFileKind,
};

/// Maximum frames in one visualization event.
const UI_CHUNK_FRAMES: usize = 1_024;
/// Channel slots that visualization events cannot use.
const STATE_EVENT_RESERVE: usize = 8;
/// Maximum device-position gap that becomes inserted silence.
const MAX_GAP_SECONDS: u64 = 5;
/// Frames read from a temporary PCM clip in one trim operation.
const FILE_ENCODE_CHUNK_FRAMES: usize = 8_192;

/// Checks the default playback endpoint and native MP3 encoder.
pub fn check_support() -> Result<()> {
    let _com = ComGuard::new()?;
    let _mf = MediaFoundationGuard::new()?;
    let (audio_client, mix_format) = default_audio_client()?;
    let source = unsafe { SourceFormat::from_wave(mix_format.0) }?;
    let (_, rate, bitrate) = unsafe { select_mp3_type(source.sample_rate, 320_000) }?;
    if rate == 0 || bitrate == 0 {
        bail!("the Windows MP3 encoder returned an invalid output format");
    }
    drop(audio_client);
    Ok(())
}

/// Encodes one PCM range and replaces the named clip after finalization.
pub(crate) fn trim_pcm_clip(
    pcm_path: &Path,
    mp3_path: &Path,
    sample_rate: u32,
    bitrate: u32,
    start_frame: u64,
    end_frame: u64,
) -> Result<u64> {
    if start_frame >= end_frame || sample_rate == 0 {
        bail!("the clip trim range is empty");
    }
    let mut source = File::open(pcm_path)
        .with_context(|| format!("could not open clip source {}", pcm_path.display()))?;
    let source_bytes = source.metadata()?.len();
    if !source_bytes.is_multiple_of(4) {
        bail!("the temporary clip source has an invalid byte length");
    }
    let source_frames = source_bytes / 4;
    if end_frame > source_frames {
        bail!("the clip source has {source_frames} frames, but the trim ends at {end_frame}");
    }

    let temporary = trim_temporary_path(mp3_path)?;
    let result = (|| -> Result<u64> {
        let _com = ComGuard::new()?;
        let _mf = MediaFoundationGuard::new()?;
        source.seek(SeekFrom::Start(start_frame.saturating_mul(4)))?;
        let mut writer = unsafe {
            Mp3Writer::open(&temporary, sample_rate, bitrate, false)
                .context("could not open the MP3 encoder for clip trimming")?
        };
        let mut remaining = end_frame - start_frame;
        let mut bytes = vec![0_u8; FILE_ENCODE_CHUNK_FRAMES * 4];
        while remaining > 0 {
            let frames = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(FILE_ENCODE_CHUNK_FRAMES);
            let byte_count = frames * 4;
            source
                .read_exact(&mut bytes[..byte_count])
                .context("the temporary clip source ended during trimming")?;
            let samples = bytes[..byte_count]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
                .collect::<Vec<_>>();
            writer.write(&samples)?;
            remaining -= frames as u64;
        }
        let saved = writer.close(SavedFileKind::Clip)?;
        replace_file(mp3_path, &saved.path)?;
        Ok(saved.frames)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Selects an unused hidden MP3 path beside the clip.
fn trim_temporary_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("clip");
    for suffix in 1..10_000 {
        let candidate = parent.join(format!(
            ".{stem}.record-trim-{}-{suffix}.mp3",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("could not choose a temporary clip file name")
}

/// Replaces a clip with a fully finalized temporary file in one Windows call.
fn replace_file(destination: &Path, replacement: &Path) -> Result<()> {
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replacement = replacement
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        ReplaceFileW(
            PCWSTR(destination.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR::null(),
            REPLACEFILE_WRITE_THROUGH,
            None,
            None,
        )
    }
    .context("Windows could not replace the clip with its trimmed file")
}

/// Captures system audio until a stop condition and finalizes every open MP3.
pub fn record(config: RecordConfig, events: &EventSender) -> Result<RecordingSummary> {
    record_inner(&config, events)
}

/// Owns COM, Media Foundation, WASAPI, and the packet-processing loop.
fn record_inner(config: &RecordConfig, events: &EventSender) -> Result<RecordingSummary> {
    let _com = ComGuard::new()?;
    let _mf = MediaFoundationGuard::new()?;
    let (audio_client, mix_format) = default_audio_client()?;
    let source = unsafe { SourceFormat::from_wave(mix_format.0) }?;
    let mut outputs = unsafe {
        OutputManager::open(&config.target, source.sample_rate, config.bitrate)
            .context("could not open the native Windows MP3 encoder")?
    };
    let mut converter = Converter::new(source, outputs.sample_rate());
    let event = OwnedHandle(unsafe { CreateEventW(None, false, false, None) }?);

    unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_NOPERSIST,
            0,
            0,
            mix_format.0,
            None,
        )?;
        audio_client.SetEventHandle(event.0)?;
    }
    let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService() }?;
    let mut task_index = 0;
    let mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index) }.ok();
    unsafe { audio_client.Start() }?;
    let _capture_guard = CaptureGuard {
        client: &audio_client,
        mmcss,
    };

    let _ = events.try_send(AudioEvent::Started {
        sample_rate: outputs.sample_rate(),
        bitrate: outputs.bitrate(),
        channels: 2,
    });

    let mut expected_position = None;
    let max_gap = u64::from(source.sample_rate) * MAX_GAP_SECONDS;
    let duration_frames = config
        .duration
        .map(|duration| segment_frame_limit(duration, outputs.sample_rate()));

    while !config.stop.load(Ordering::Relaxed) {
        process_commands(config, &mut outputs, events)?;
        if duration_frames.is_some_and(|limit| outputs.total_frames() >= limit) {
            break;
        }
        let wait = unsafe { WaitForSingleObject(event.0, 50) };
        if wait == WAIT_TIMEOUT {
            continue;
        }
        if wait == WAIT_FAILED {
            let error =
                WindowsError::from_hresult(HRESULT::from_win32(unsafe { GetLastError().0 }));
            return Err(error).context("waiting for system audio");
        }
        if wait != WAIT_OBJECT_0 {
            bail!(
                "Windows returned an unexpected audio wait result: {}",
                wait.0
            );
        }

        loop {
            let packet_frames = unsafe { capture_client.GetNextPacketSize() }?;
            if packet_frames == 0 {
                break;
            }
            let mut data = ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut device_position = 0;
            let mut qpc_position = 0;
            unsafe {
                capture_client.GetBuffer(
                    &raw mut data,
                    &raw mut frames,
                    &raw mut flags,
                    Some(&raw mut device_position),
                    Some(&raw mut qpc_position),
                )?;
            }

            // Save processing errors until ReleaseBuffer runs. WASAPI requires
            // exactly one release for each successful GetBuffer call.
            let packet_result = (|| -> Result<()> {
                if !config.paused.load(Ordering::Relaxed) {
                    if let Some(expected) = expected_position {
                        let gap = device_position.saturating_sub(expected);
                        if gap > 0 && gap <= max_gap {
                            process_source_frames(
                                &mut converter,
                                &mut outputs,
                                events,
                                None,
                                gap as usize,
                                true,
                            )?;
                        }
                    }
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    let bytes = if silent {
                        None
                    } else {
                        let byte_len = frames as usize * source.block_align;
                        Some(unsafe { std::slice::from_raw_parts(data, byte_len) })
                    };
                    process_source_frames(
                        &mut converter,
                        &mut outputs,
                        events,
                        bytes,
                        frames as usize,
                        silent,
                    )?;
                }
                Ok(())
            })();

            unsafe { capture_client.ReleaseBuffer(frames) }?;
            expected_position = Some(device_position + u64::from(frames));
            packet_result?;
            if config.stop.load(Ordering::Relaxed) {
                break;
            }
        }
    }

    let _ = events.try_send(AudioEvent::Finalizing);
    let flushed = converter.flush();
    if !flushed.pcm.is_empty() {
        outputs.write(&flushed.pcm, events)?;
        send_samples(events, flushed, outputs.total_frames());
    }
    process_commands(config, &mut outputs, events)?;
    outputs.finish(events)
}

/// Applies all pending low-frequency commands between capture waits.
fn process_commands(
    config: &RecordConfig,
    outputs: &mut OutputManager,
    events: &EventSender,
) -> Result<()> {
    for command in config.commands.try_iter() {
        match command {
            AudioCommand::SaveClip(stem) => outputs.save_clip(&stem, events)?,
        }
    }
    Ok(())
}

/// Converts one source block, writes it, and offers bounded display samples.
fn process_source_frames(
    converter: &mut Converter,
    outputs: &mut OutputManager,
    events: &EventSender,
    bytes: Option<&[u8]>,
    frames: usize,
    silent: bool,
) -> Result<()> {
    let converted = converter.process(bytes, frames, silent);
    if converted.pcm.is_empty() {
        return Ok(());
    }
    outputs.write(&converted.pcm, events)?;
    send_samples(events, converted, outputs.total_frames());
    Ok(())
}

/// Sends small display blocks while it preserves capacity for state events.
fn send_samples(events: &EventSender, converted: ConvertedBlock, frames: u64) {
    for (index, (left, right)) in converted
        .left
        .chunks(UI_CHUNK_FRAMES)
        .zip(converted.right.chunks(UI_CHUNK_FRAMES))
        .enumerate()
    {
        if !has_visual_event_capacity(events) {
            break;
        }
        let chunk_end = ((index + 1) * UI_CHUNK_FRAMES).min(converted.left.len());
        let chunk_frames = (converted.left.len() - chunk_end) as u64;
        let _ = events.try_send(AudioEvent::Samples {
            left: left.to_vec(),
            right: right.to_vec(),
            encoded_frames: frames.saturating_sub(chunk_frames),
        });
    }
}

/// Reports whether another display event can keep the state-event reserve free.
fn has_visual_event_capacity(events: &EventSender) -> bool {
    events
        .capacity()
        .is_none_or(|capacity| events.len() < capacity.saturating_sub(STATE_EVENT_RESERVE).max(1))
}

/// Activates the default console render endpoint and obtains its mix format.
pub(crate) fn default_audio_client() -> Result<(IAudioClient, MixFormat)> {
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .context("could not create the Windows audio-device enumerator")?
    };
    let endpoint = unsafe {
        enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .context("no default Windows playback device is available")?
    };
    let client: IAudioClient = unsafe {
        endpoint
            .Activate(CLSCTX_ALL, None)
            .context("could not activate WASAPI loopback on the default playback device")?
    };
    let format = unsafe { client.GetMixFormat() }?;
    if format.is_null() {
        bail!("Windows returned an empty playback mix format");
    }
    Ok((client, MixFormat(format)))
}

/// Initializes multithreaded COM and balances it during drop.
pub(crate) struct ComGuard;

impl ComGuard {
    /// Initializes COM for the current audio or trim thread.
    pub(crate) fn new() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
            .context("could not initialize COM for the audio thread")?;
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Starts Media Foundation and balances it during drop.
pub(crate) struct MediaFoundationGuard;

impl MediaFoundationGuard {
    /// Starts the full Media Foundation platform for the current process.
    pub(crate) fn new() -> Result<Self> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
            .context("could not start Windows Media Foundation")?;
        Ok(Self)
    }
}

impl Drop for MediaFoundationGuard {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

/// Owns a mix-format allocation returned by WASAPI.
pub(crate) struct MixFormat(pub(crate) *mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) };
    }
}

/// Owns one Windows kernel handle.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Stops WASAPI and leaves MMCSS when capture exits for any reason.
struct CaptureGuard<'a> {
    /// Borrows the active client until the guard stops it.
    client: &'a IAudioClient,
    /// Contains the MMCSS registration handle when registration succeeded.
    mmcss: Option<HANDLE>,
}

impl Drop for CaptureGuard<'_> {
    fn drop(&mut self) {
        let _ = unsafe { self.client.Stop() };
        if let Some(handle) = self.mmcss {
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

/// Owns exact session boundaries and all finalized file metadata.
struct OutputManager {
    /// Contains the immutable destination mode and root path.
    target: OutputTarget,
    /// Contains the current MP3 and optional edit cache writer.
    writer: Option<ActiveWriter>,
    /// Keeps the process-unique edit cache alive.
    clip_cache: Option<std::sync::Arc<ClipCacheLease>>,
    /// Contains the source rate used to open later parts.
    source_rate: u32,
    /// Contains the requested rate used to open later parts.
    requested_bitrate: u32,
    /// Contains the selected MP3 sample rate.
    sample_rate: u32,
    /// Contains the selected MP3 bit rate.
    bitrate: u32,
    /// Contains the exact frame limit for each session part.
    segment_limit: Option<u64>,
    /// Contains the number for the next session part.
    next_part_index: u32,
    /// Contains all encoded frames, including finalized parts.
    total_frames: u64,
    /// Contains finalized files in capture order.
    files: Vec<SavedFile>,
}

impl OutputManager {
    /// Opens the first writer and locks the selected MP3 profile for the session.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be active on the current thread.
    unsafe fn open(
        target: &OutputTarget,
        source_rate: u32,
        requested_bitrate: u32,
    ) -> Result<Self> {
        let (path, replace, next_part_index, segment_duration) = match target {
            OutputTarget::SingleFile { path, replace } => (path.clone(), *replace, 1, None),
            OutputTarget::Session {
                directory,
                segment_duration,
            } => {
                fs::create_dir_all(directory).with_context(|| {
                    format!("could not create session directory {}", directory.display())
                })?;
                (part_path(directory, 1), false, 2, Some(*segment_duration))
            }
        };
        let clip_cache = if target.is_session() {
            Some(create_clip_cache()?)
        } else {
            None
        };
        let pcm_path = clip_cache
            .as_ref()
            .map(|cache| clip_cache_part_path(cache, 1));
        let writer = unsafe {
            ActiveWriter::open(
                &path,
                pcm_path.as_deref(),
                source_rate,
                requested_bitrate,
                replace,
            )
        }?;
        let sample_rate = writer.sample_rate();
        let bitrate = writer.bitrate();
        Ok(Self {
            target: target.clone(),
            writer: Some(writer),
            clip_cache,
            source_rate,
            requested_bitrate,
            sample_rate,
            bitrate,
            segment_limit: segment_duration
                .map(|duration| segment_frame_limit(duration, sample_rate)),
            next_part_index,
            total_frames: 0,
            files: Vec::new(),
        })
    }

    /// Returns the selected output sample rate.
    const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the selected output bit rate.
    const fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Returns all encoded frames from this process.
    const fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// Writes an interleaved stereo block across exact part boundaries.
    fn write(&mut self, samples: &[i16], events: &EventSender) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        if !samples.len().is_multiple_of(2) {
            bail!("internal encoder error: stereo sample block has an odd length");
        }

        let input_frames = samples.len() / 2;
        let mut frame_offset = 0;
        while frame_offset < input_frames {
            // A source block can cross a part boundary. Limit this write to the
            // exact remaining frame count. Continue the same block in a new file.
            self.ensure_writer()?;
            let current_frames = self.writer.as_ref().map_or(0, ActiveWriter::frames);
            let remaining = self
                .segment_limit
                .map_or(u64::MAX, |limit| limit.saturating_sub(current_frames));
            if remaining == 0 {
                self.finalize_current(SavedFileKind::Part, None, events)?;
                continue;
            }

            let available_input = input_frames - frame_offset;
            let chunk_frames =
                available_input.min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let sample_start = frame_offset * 2;
            let sample_end = (frame_offset + chunk_frames) * 2;
            self.writer
                .as_mut()
                .ok_or_else(|| anyhow!("MP3 writer was not open"))?
                .write(&samples[sample_start..sample_end])?;
            self.total_frames = self
                .total_frames
                .checked_add(chunk_frames as u64)
                .ok_or_else(|| anyhow!("encoded audio timeline is too long"))?;
            frame_offset += chunk_frames;

            let reached_boundary = self.segment_limit.is_some_and(|limit| {
                self.writer
                    .as_ref()
                    .is_some_and(|writer| writer.frames() >= limit)
            });
            if reached_boundary {
                self.finalize_current(SavedFileKind::Part, None, events)?;
            }
        }
        Ok(())
    }

    /// Finalizes the current part as a named clip when sessions are active.
    fn save_clip(&mut self, stem: &str, events: &EventSender) -> Result<()> {
        if !self.target.is_session() {
            return Ok(());
        }
        if self
            .writer
            .as_ref()
            .is_none_or(|writer| writer.frames() == 0)
        {
            let _ = events.try_send(AudioEvent::Notice(
                "A clip needs captured audio. Try again after the waveform moves.".to_owned(),
            ));
            return Ok(());
        }
        self.finalize_current(SavedFileKind::Clip, Some(stem), events)?;
        Ok(())
    }

    /// Opens the next numbered session part when no writer is active.
    fn ensure_writer(&mut self) -> Result<()> {
        if self.writer.is_some() {
            return Ok(());
        }
        let OutputTarget::Session { directory, .. } = &self.target else {
            bail!("the single-file MP3 writer was already closed");
        };
        let path = part_path(directory, self.next_part_index);
        let pcm_path = self
            .clip_cache
            .as_ref()
            .map(|cache| clip_cache_part_path(cache, self.next_part_index));
        self.next_part_index = self
            .next_part_index
            .checked_add(1)
            .ok_or_else(|| anyhow!("the session has too many parts"))?;
        let writer = unsafe {
            ActiveWriter::open(
                &path,
                pcm_path.as_deref(),
                self.source_rate,
                self.requested_bitrate,
                false,
            )
            .with_context(|| format!("could not open session part {}", path.display()))?
        };
        if writer.sample_rate() != self.sample_rate || writer.bitrate() != self.bitrate {
            bail!("the Windows MP3 profile changed during the recording session");
        }
        self.writer = Some(writer);
        Ok(())
    }

    /// Closes one part, applies a clip name, and sends its final metadata.
    fn finalize_current(
        &mut self,
        kind: SavedFileKind,
        clip_stem: Option<&str>,
        events: &EventSender,
    ) -> Result<Option<SavedFile>> {
        let Some(writer) = self.writer.take() else {
            return Ok(None);
        };
        // Media Foundation cannot finalize an MP3 before it receives a sample.
        // Drop removes this empty output. This also makes an immediate stop safe.
        if writer.frames() == 0 {
            drop(writer);
            return Ok(None);
        }
        let mut file = writer.close(kind, self.clip_cache.as_ref())?;
        if let Some(stem) = clip_stem {
            let OutputTarget::Session { directory, .. } = &self.target else {
                bail!("named clips require a session output");
            };
            let destination = available_clip_path(directory, stem, &file.path)?;
            if destination != file.path {
                fs::rename(&file.path, &destination).with_context(|| {
                    format!(
                        "could not rename {} to {}",
                        file.path.display(),
                        destination.display()
                    )
                })?;
                file.path = destination;
            }
        }
        let _ = events.try_send(AudioEvent::Saved(file.clone()));
        self.files.push(file.clone());
        Ok(Some(file))
    }

    /// Finalizes the last writer and returns the process summary.
    fn finish(mut self, events: &EventSender) -> Result<RecordingSummary> {
        let kind = if self.target.is_session() {
            SavedFileKind::Part
        } else {
            SavedFileKind::Recording
        };
        self.finalize_current(kind, None, events)?;
        Ok(RecordingSummary {
            output: self.target.root().to_path_buf(),
            sample_rate: self.sample_rate,
            bitrate: self.bitrate,
            frames: self.total_frames,
            files: self.files,
        })
    }
}

/// Creates a process-unique temporary directory for editable PCM parts.
fn create_clip_cache() -> Result<std::sync::Arc<ClipCacheLease>> {
    let parent = std::env::temp_dir().join("record-clip-cache");
    fs::create_dir_all(&parent).context("could not create the clip cache root")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    for suffix in 1..1_000 {
        let candidate = parent.join(format!("{}-{nonce}-{suffix}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                return Ok(std::sync::Arc::new(ClipCacheLease { path: candidate }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("could not create the clip cache"),
        }
    }
    bail!("could not choose a unique clip cache directory")
}

/// Returns the temporary PCM path for one numbered part.
fn clip_cache_part_path(cache: &ClipCacheLease, index: u32) -> PathBuf {
    cache.path.join(format!("part-{index:06}.pcm"))
}

/// Writes interleaved stereo PCM16 for later clip review.
struct RawPcmWriter {
    /// Owns the open cache file.
    file: File,
    /// Keeps the path after the file handle closes.
    path: PathBuf,
}

impl RawPcmWriter {
    /// Creates a new cache file without replacing an existing path.
    fn open(path: &Path) -> Result<Self> {
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("could not create clip cache {}", path.display()))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Writes native little-endian `i16` samples on Windows.
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        // SAFETY: An i16 slice is contiguous. Its byte view has the same lifetime.
        let bytes = unsafe {
            std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), mem::size_of_val(samples))
        };
        self.file
            .write_all(bytes)
            .context("could not write the clip cache")
    }

    /// Flushes the cache file and returns its path.
    fn close(mut self) -> Result<PathBuf> {
        self.file
            .flush()
            .context("could not flush the clip cache")?;
        drop(self.file);
        Ok(self.path)
    }
}

/// Keeps the MP3 and optional PCM cache on the same frame timeline.
struct ActiveWriter {
    /// Owns the native MP3 sink.
    mp3: Mp3Writer,
    /// Owns the temporary edit source for a session part.
    pcm: Option<RawPcmWriter>,
}

impl ActiveWriter {
    /// Opens matching MP3 and PCM destinations.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be active on the current thread.
    unsafe fn open(
        mp3_path: &Path,
        pcm_path: Option<&Path>,
        source_rate: u32,
        requested_bitrate: u32,
        replace: bool,
    ) -> Result<Self> {
        let mp3 = unsafe { Mp3Writer::open(mp3_path, source_rate, requested_bitrate, replace)? };
        let pcm = pcm_path.map(RawPcmWriter::open).transpose()?;
        Ok(Self { mp3, pcm })
    }

    /// Returns the selected MP3 sample rate.
    const fn sample_rate(&self) -> u32 {
        self.mp3.sample_rate
    }

    /// Returns the selected MP3 bit rate.
    const fn bitrate(&self) -> u32 {
        self.mp3.bitrate
    }

    /// Returns frames written to both destinations.
    const fn frames(&self) -> u64 {
        self.mp3.frames
    }

    /// Writes the same interleaved samples to both active destinations.
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        self.mp3.write(samples)?;
        if let Some(pcm) = &mut self.pcm {
            pcm.write(samples)?;
        }
        Ok(())
    }

    /// Finalizes both outputs and keeps PCM only for a named clip.
    fn close(
        self,
        kind: SavedFileKind,
        cache: Option<&std::sync::Arc<ClipCacheLease>>,
    ) -> Result<SavedFile> {
        let pcm_path = self.pcm.map(RawPcmWriter::close).transpose()?;
        let mut file = self.mp3.close(kind)?;
        if kind == SavedFileKind::Clip {
            file.edit_source = pcm_path.zip(cache).map(|(pcm_path, cache)| ClipEditSource {
                pcm_path,
                start_frame: 0,
                end_frame: file.frames,
                cache: std::sync::Arc::clone(cache),
            });
        } else if let Some(path) = pcm_path {
            let _ = fs::remove_file(path);
        }
        Ok(file)
    }
}

/// Owns one Media Foundation sink writer and its output stream.
struct Mp3Writer {
    /// Contains the sink writer until finalization.
    writer: Option<IMFSinkWriter>,
    /// Contains the byte stream until finalization.
    byte_stream: Option<IMFByteStream>,
    /// Contains the path to remove after an incomplete encode.
    output: PathBuf,
    /// Identifies the fixed MP3 stream in the sink.
    stream_index: u32,
    /// Contains the selected output sample rate.
    sample_rate: u32,
    /// Contains the selected output bit rate.
    bitrate: u32,
    /// Contains frames already submitted to the sink.
    frames: u64,
    /// Reports whether Media Foundation finalized the file.
    finalized: bool,
}

impl Mp3Writer {
    /// Selects an MP3 profile and opens the built-in sink.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be active on the current thread.
    unsafe fn open(
        path: &Path,
        source_rate: u32,
        requested_bitrate: u32,
        replace: bool,
    ) -> Result<Self> {
        let (output_type, sample_rate, bitrate) =
            unsafe { select_mp3_type(source_rate, requested_bitrate) }?;
        let wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let open_mode = if replace {
            MF_OPENMODE_DELETE_IF_EXIST
        } else {
            MF_OPENMODE_FAIL_IF_EXIST
        };
        let byte_stream = unsafe {
            MFCreateFile(
                MF_ACCESSMODE_WRITE,
                open_mode,
                MF_FILEFLAGS_NONE,
                PCWSTR(wide_path.as_ptr()),
            )
        }
        .with_context(|| format!("could not create {}", path.display()))?;
        // Let the sink writer create and configure the built-in MP3 sink. The
        // MP3 sink has a fixed stream, so constructing it manually and then
        // calling AddStream fails with MF_E_STREAMSINKS_FIXED.
        let setup = (|| -> Result<(IMFSinkWriter, u32)> {
            let writer = unsafe {
                MFCreateSinkWriterFromURL(
                    PCWSTR(wide_path.as_ptr()),
                    &byte_stream,
                    None::<&IMFAttributes>,
                )
            }?;
            let stream_index = unsafe { writer.AddStream(&output_type) }?;
            let input_type = unsafe { pcm_input_type(sample_rate) }?;
            unsafe {
                writer.SetInputMediaType(stream_index, &input_type, None::<&IMFAttributes>)?;
                writer.BeginWriting()?;
            }
            Ok((writer, stream_index))
        })();
        let (writer, stream_index) = match setup {
            Ok(setup) => setup,
            Err(error) => {
                let _ = unsafe { byte_stream.Close() };
                drop(byte_stream);
                let _ = fs::remove_file(path);
                return Err(error);
            }
        };
        Ok(Self {
            writer: Some(writer),
            byte_stream: Some(byte_stream),
            output: path.to_path_buf(),
            stream_index,
            sample_rate,
            bitrate,
            frames: 0,
            finalized: false,
        })
    }

    /// Submits one timestamped interleaved PCM16 block.
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        if !samples.len().is_multiple_of(2) {
            bail!("internal encoder error: stereo sample block has an odd length");
        }
        let frames = (samples.len() / 2) as u64;
        let byte_count = u32::try_from(mem::size_of_val(samples))?;
        unsafe {
            let buffer = MFCreateMemoryBuffer(byte_count)?;
            let mut destination = ptr::null_mut();
            buffer.Lock(&raw mut destination, None, None)?;
            ptr::copy_nonoverlapping(
                samples.as_ptr().cast::<u8>(),
                destination,
                byte_count as usize,
            );
            buffer.Unlock()?;
            buffer.SetCurrentLength(byte_count)?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let start = media_time(self.frames, self.sample_rate);
            let end = media_time(self.frames + frames, self.sample_rate);
            sample.SetSampleTime(start)?;
            sample.SetSampleDuration(end - start)?;
            if self.frames == 0 {
                sample.SetUINT32(&MFSampleExtension_Discontinuity, 1)?;
            }
            self.writer
                .as_ref()
                .ok_or_else(|| anyhow!("MP3 writer was already closed"))?
                .WriteSample(self.stream_index, &sample)?;
        }
        self.frames += frames;
        Ok(())
    }

    /// Finalizes the sink and returns metadata for the closed file.
    fn close(mut self, kind: SavedFileKind) -> Result<SavedFile> {
        if !self.finalized {
            unsafe {
                self.writer
                    .as_ref()
                    .ok_or_else(|| anyhow!("MP3 writer was already closed"))?
                    .Finalize()
                    .context("Windows could not finalize the MP3")?;
            }
            self.finalized = true;
        }
        self.writer.take();
        if let Some(stream) = self.byte_stream.take() {
            // Finalize can close the sink-owned stream before this explicit close.
            let _ = unsafe { stream.Close() };
        }
        Ok(SavedFile {
            path: self.output.clone(),
            kind,
            sample_rate: self.sample_rate,
            bitrate: self.bitrate,
            frames: self.frames,
            edit_source: None,
        })
    }
}

impl Drop for Mp3Writer {
    fn drop(&mut self) {
        self.writer.take();
        if let Some(stream) = self.byte_stream.take() {
            let _ = unsafe { stream.Close() };
        }
        if !self.finalized {
            let _ = fs::remove_file(&self.output);
        }
    }
}

/// Converts frames to Media Foundation 100-nanosecond time units.
const fn media_time(frames: u64, sample_rate: u32) -> i64 {
    ((frames * 10_000_000) / sample_rate as u64) as i64
}

/// Selects the closest native stereo MP3 profile at or below the request.
///
/// # Safety
///
/// Media Foundation must be active on the current thread.
unsafe fn select_mp3_type(
    source_rate: u32,
    requested_bitrate: u32,
) -> Result<(IMFMediaType, u32, u32)> {
    let desired_rate = if source_rate.abs_diff(44_100) < source_rate.abs_diff(48_000) {
        44_100
    } else {
        48_000
    };
    let collection: IMFCollection = unsafe {
        MFTranscodeGetAudioOutputAvailableTypes(
            &MFAudioFormat_MP3,
            MFT_ENUM_FLAG_ALL.0 as u32,
            None::<&IMFAttributes>,
        )
    }
    .context("Windows has no available MP3 encoder")?;
    let count = unsafe { collection.GetElementCount() }?;
    let mut best: Option<(i64, IMFMediaType, u32, u32)> = None;

    for index in 0..count {
        let unknown = unsafe { collection.GetElement(index) }?;
        let Ok(media_type) = unknown.cast::<IMFMediaType>() else {
            continue;
        };
        let Ok(channels) = (unsafe { media_type.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }) else {
            continue;
        };
        let Ok(rate) = (unsafe { media_type.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }) else {
            continue;
        };
        let Ok(bytes_per_second) =
            (unsafe { media_type.GetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND) })
        else {
            continue;
        };
        let bitrate = bytes_per_second.saturating_mul(8);
        if channels != 2 || !(32_000..=48_000).contains(&rate) || bitrate > requested_bitrate {
            continue;
        }
        let mut score = i64::from(bitrate) * 100_000;
        if rate == desired_rate {
            score += 10_000;
        } else if rate == 48_000 {
            score += 5_000;
        }
        score -= i64::from(rate.abs_diff(desired_rate));
        if best
            .as_ref()
            .is_none_or(|(best_score, ..)| score > *best_score)
        {
            best = Some((score, media_type, rate, bitrate));
        }
    }

    best.map(|(_, media_type, rate, bitrate)| (media_type, rate, bitrate))
        .ok_or_else(|| {
            anyhow!("Windows has no stereo MP3 profile at or below {requested_bitrate} bps")
        })
}

/// Creates the stereo PCM16 input type for the sink writer.
///
/// # Safety
///
/// Media Foundation must be active on the current thread.
unsafe fn pcm_input_type(sample_rate: u32) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        media_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        media_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
        media_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        media_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
        media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, sample_rate * 4)?;
        media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        media_type.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        media_type.SetUINT32(&MF_MT_SAMPLE_SIZE, 4)?;
    }
    Ok(media_type)
}

#[derive(Clone, Copy, Debug)]
/// Describes the endpoint sample layout and its stereo mix matrix.
pub(crate) struct SourceFormat {
    /// Contains the number of interleaved source channels.
    pub(crate) channels: usize,
    /// Contains source frames per second.
    pub(crate) sample_rate: u32,
    /// Contains bytes between adjacent source frames.
    pub(crate) block_align: usize,
    /// Contains bits reserved for each source sample.
    container_bits: u16,
    /// Contains meaningful bits in each integer sample.
    valid_bits: u16,
    /// Reports whether samples use IEEE floating-point data.
    float: bool,
    /// Contains each source channel gain for the left output.
    mix_left: [f32; 32],
    /// Contains each source channel gain for the right output.
    mix_right: [f32; 32],
}

impl SourceFormat {
    /// Validates a WASAPI format and creates its stereo mix matrix.
    ///
    /// # Safety
    ///
    /// `pointer` must identify a valid `WAVEFORMATEX`. If its tag is extensible,
    /// it must identify a complete `WAVEFORMATEXTENSIBLE` value.
    pub(crate) unsafe fn from_wave(pointer: *const WAVEFORMATEX) -> Result<Self> {
        let wave = unsafe { pointer.read_unaligned() };
        let mut format = Self {
            channels: usize::from(wave.nChannels),
            sample_rate: wave.nSamplesPerSec,
            block_align: usize::from(wave.nBlockAlign),
            container_bits: wave.wBitsPerSample,
            valid_bits: wave.wBitsPerSample,
            float: false,
            mix_left: [0.0; 32],
            mix_right: [0.0; 32],
        };
        if format.channels == 0
            || format.channels > 32
            || format.sample_rate == 0
            || format.block_align == 0
            || format.container_bits == 0
        {
            bail!("the default playback device returned an invalid audio format");
        }

        let mut channel_mask = 0;
        match u32::from(wave.wFormatTag) {
            WAVE_FORMAT_EXTENSIBLE => {
                if wave.cbSize < 22 {
                    bail!("the playback device returned a truncated extensible audio format");
                }
                let extended = unsafe { pointer.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
                let valid_bits = unsafe { extended.Samples.wValidBitsPerSample };
                let sub_format = unsafe { ptr::addr_of!(extended.SubFormat).read_unaligned() };
                format.valid_bits = if valid_bits == 0 {
                    format.container_bits
                } else {
                    valid_bits
                };
                channel_mask = extended.dwChannelMask;
                if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                    format.float = true;
                } else if sub_format != KSDATAFORMAT_SUBTYPE_PCM {
                    bail!("the playback device uses an unsupported audio subtype");
                }
            }
            WAVE_FORMAT_IEEE_FLOAT => format.float = true,
            WAVE_FORMAT_PCM => {}
            tag => bail!("unsupported Windows playback format tag: 0x{tag:04X}"),
        }
        if format.valid_bits == 0 || format.valid_bits > format.container_bits {
            bail!("the playback device returned invalid sample bit depth");
        }
        if format.float && !matches!(format.container_bits, 32 | 64) {
            bail!(
                "unsupported floating-point sample width: {}",
                format.container_bits
            );
        }
        if !format.float && !matches!(format.container_bits, 8 | 16 | 24 | 32) {
            bail!("unsupported PCM sample width: {}", format.container_bits);
        }
        let packed_frame_bytes = format.channels * usize::from(format.container_bits.div_ceil(8));
        if format.block_align < packed_frame_bytes {
            bail!("the playback device returned an invalid frame alignment");
        }

        format.build_mix_matrix(channel_mask);
        Ok(format)
    }

    /// Builds normalized left and right gains from a Windows speaker mask.
    fn build_mix_matrix(&mut self, channel_mask: u32) {
        if self.channels == 1 {
            self.mix_left[0] = 1.0;
            self.mix_right[0] = 1.0;
        } else if channel_mask != 0 {
            let mut remaining = channel_mask;
            for channel in 0..self.channels {
                let speaker = remaining & remaining.wrapping_neg();
                let (left, right) = speaker_coefficients(speaker, channel);
                self.mix_left[channel] = left;
                self.mix_right[channel] = right;
                remaining &= !speaker;
            }
        } else {
            self.mix_left[0] = 1.0;
            self.mix_right[1] = 1.0;
            for channel in 2..self.channels {
                if channel.is_multiple_of(2) {
                    self.mix_left[channel] = 0.5;
                } else {
                    self.mix_right[channel] = 0.5;
                }
            }
        }
        let left_energy = self.mix_left[..self.channels]
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt()
            .max(1.0);
        let right_energy = self.mix_right[..self.channels]
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt()
            .max(1.0);
        for channel in 0..self.channels {
            self.mix_left[channel] /= left_energy;
            self.mix_right[channel] /= right_energy;
        }
    }

    /// Decodes and mixes one source frame, or returns silence for a silent packet.
    fn mix_frame(&self, bytes: Option<&[u8]>, frame: usize, silent: bool) -> (f32, f32) {
        if silent {
            return (0.0, 0.0);
        }
        let Some(bytes) = bytes else {
            return (0.0, 0.0);
        };
        let frame_start = frame * self.block_align;
        let bytes_per_sample = usize::from(self.container_bits / 8);
        let mut left = 0.0;
        let mut right = 0.0;
        for channel in 0..self.channels {
            let start = frame_start + channel * bytes_per_sample;
            let value = self.read_sample(&bytes[start..start + bytes_per_sample]);
            left += value * self.mix_left[channel];
            right += value * self.mix_right[channel];
        }
        (left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0))
    }

    /// Decodes one integer or floating-point source sample.
    fn read_sample(&self, bytes: &[u8]) -> f32 {
        if self.float {
            return match self.container_bits {
                32 => finite_f32(f32::from_le_bytes(bytes.try_into().unwrap())),
                64 => finite_f64(f64::from_le_bytes(bytes.try_into().unwrap())),
                _ => 0.0,
            };
        }
        match self.container_bits {
            8 => (f32::from(bytes[0]) - 128.0) / 128.0,
            16 => {
                let mut value = i16::from_le_bytes(bytes.try_into().unwrap()) as i32;
                value >>= 16 - self.valid_bits;
                value as f32 / (1_u64 << (self.valid_bits - 1)) as f32
            }
            24 => {
                let mut value =
                    i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
                if value & 0x0080_0000 != 0 {
                    value |= !0x00FF_FFFF;
                }
                value >>= 24 - self.valid_bits;
                value as f32 / (1_u64 << (self.valid_bits - 1)) as f32
            }
            32 => {
                let mut value = i32::from_le_bytes(bytes.try_into().unwrap());
                value >>= 32 - self.valid_bits;
                value as f64 as f32 / (1_u64 << (self.valid_bits - 1)) as f32
            }
            _ => 0.0,
        }
    }
}

/// Clamps one 32-bit float and converts invalid data to silence.
fn finite_f32(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Clamps one 64-bit float and converts invalid data to silence.
fn finite_f64(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0) as f32
    } else {
        0.0
    }
}

/// Returns left and right gains for one Windows speaker position.
fn speaker_coefficients(speaker: u32, channel: usize) -> (f32, f32) {
    const SURROUND_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match speaker {
        SPEAKER_FRONT_LEFT => (1.0, 0.0),
        SPEAKER_FRONT_RIGHT => (0.0, 1.0),
        SPEAKER_FRONT_CENTER => (SURROUND_GAIN, SURROUND_GAIN),
        SPEAKER_LOW_FREQUENCY => (0.30, 0.30),
        SPEAKER_BACK_LEFT
        | SPEAKER_SIDE_LEFT
        | SPEAKER_FRONT_LEFT_OF_CENTER
        | SPEAKER_TOP_FRONT_LEFT
        | SPEAKER_TOP_BACK_LEFT => (SURROUND_GAIN, 0.0),
        SPEAKER_BACK_RIGHT
        | SPEAKER_SIDE_RIGHT
        | SPEAKER_FRONT_RIGHT_OF_CENTER
        | SPEAKER_TOP_FRONT_RIGHT
        | SPEAKER_TOP_BACK_RIGHT => (0.0, SURROUND_GAIN),
        SPEAKER_BACK_CENTER
        | SPEAKER_TOP_CENTER
        | SPEAKER_TOP_FRONT_CENTER
        | SPEAKER_TOP_BACK_CENTER => (0.50, 0.50),
        0 if channel.is_multiple_of(2) => (0.5, 0.0),
        0 => (0.0, 0.5),
        _ => (0.35, 0.35),
    }
}

/// Converts source packets to stateful stereo PCM16 at the MP3 rate.
pub(crate) struct Converter {
    /// Contains the source decoder and stereo matrix.
    source: SourceFormat,
    /// Contains target frames per second.
    target_rate: u32,
    /// Contains the next fractional position on the source timeline.
    next_position: f64,
    /// Contains source frames processed before the current block.
    total_source_frames: u64,
    /// Contains the last frame from the previous block for interpolation.
    previous: Option<(f32, f32)>,
}

#[derive(Default)]
/// Contains one converted block for the writer and terminal.
pub(crate) struct ConvertedBlock {
    /// Contains interleaved stereo PCM16 for Media Foundation.
    pub(crate) pcm: Vec<i16>,
    /// Contains normalized left samples for visualization.
    left: Vec<f32>,
    /// Contains normalized right samples for visualization.
    right: Vec<f32>,
}

impl Converter {
    /// Creates an empty converter on a continuous source timeline.
    pub(crate) const fn new(source: SourceFormat, target_rate: u32) -> Self {
        Self {
            source,
            target_rate,
            next_position: 0.0,
            total_source_frames: 0,
            previous: None,
        }
    }

    /// Decodes and resamples one source block without resetting interpolation.
    pub(crate) fn process(
        &mut self,
        bytes: Option<&[u8]>,
        frames: usize,
        silent: bool,
    ) -> ConvertedBlock {
        let mut output = ConvertedBlock::default();
        if frames == 0 {
            return output;
        }
        let base = self.total_source_frames;
        let last = (base + frames as u64 - 1) as f64;
        let step = f64::from(self.source.sample_rate) / f64::from(self.target_rate);
        while self.next_position <= last + f64::EPSILON {
            let integer_position = self.next_position.floor() as u64;
            let fraction = (self.next_position - integer_position as f64) as f32;
            let (left_a, right_a, left_b, right_b) = if integer_position < base {
                let Some((left_a, right_a)) = self.previous else {
                    break;
                };
                let (left_b, right_b) = self.source.mix_frame(bytes, 0, silent);
                (left_a, right_a, left_b, right_b)
            } else {
                let local = (integer_position - base) as usize;
                let (left_a, right_a) = self.source.mix_frame(bytes, local, silent);
                if local + 1 < frames {
                    let (left_b, right_b) = self.source.mix_frame(bytes, local + 1, silent);
                    (left_a, right_a, left_b, right_b)
                } else {
                    if fraction > 1.0e-7 {
                        break;
                    }
                    (left_a, right_a, left_a, right_a)
                }
            };
            let left = (left_b - left_a).mul_add(fraction, left_a).clamp(-1.0, 1.0);
            let right = (right_b - right_a)
                .mul_add(fraction, right_a)
                .clamp(-1.0, 1.0);
            output.left.push(left);
            output.right.push(right);
            output.pcm.push(float_to_i16(left));
            output.pcm.push(float_to_i16(right));
            self.next_position += step;
        }
        self.previous = Some(self.source.mix_frame(bytes, frames - 1, silent));
        self.total_source_frames += frames as u64;
        output
    }

    /// Emits the final interpolation positions after capture stops.
    pub(crate) fn flush(&mut self) -> ConvertedBlock {
        let mut output = ConvertedBlock::default();
        let Some((left, right)) = self.previous else {
            return output;
        };
        let step = f64::from(self.source.sample_rate) / f64::from(self.target_rate);
        while self.next_position < self.total_source_frames as f64 && output.left.len() < 16 {
            output.left.push(left);
            output.right.push(right);
            output.pcm.push(float_to_i16(left));
            output.pcm.push(float_to_i16(right));
            self.next_position += step;
        }
        output
    }
}

/// Converts one normalized finite sample to signed PCM16.
fn float_to_i16(value: f32) -> i16 {
    let value = if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    if value <= -1.0 {
        i16::MIN
    } else {
        (value * f32::from(i16::MAX)).round() as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use tempfile::tempdir;

    fn stereo_format(rate: u32) -> SourceFormat {
        SourceFormat {
            channels: 2,
            sample_rate: rate,
            block_align: 4,
            container_bits: 16,
            valid_bits: 16,
            float: false,
            mix_left: {
                let mut mix = [0.0; 32];
                mix[0] = 1.0;
                mix
            },
            mix_right: {
                let mut mix = [0.0; 32];
                mix[1] = 1.0;
                mix
            },
        }
    }

    fn mono_format(container_bits: u16, valid_bits: u16, float: bool) -> SourceFormat {
        let mut format = SourceFormat {
            channels: 1,
            sample_rate: 48_000,
            block_align: usize::from(container_bits / 8),
            container_bits,
            valid_bits,
            float,
            mix_left: [0.0; 32],
            mix_right: [0.0; 32],
        };
        format.build_mix_matrix(0);
        format
    }

    #[test]
    fn converts_stereo_pcm() {
        let source = stereo_format(48_000);
        let bytes = [0xFF, 0x7F, 0x00, 0x80];
        assert_eq!(
            source.mix_frame(Some(&bytes), 0, false),
            (32767.0 / 32768.0, -1.0)
        );
    }

    #[test]
    fn converts_unsigned_eight_bit_pcm() {
        let source = mono_format(8, 8, false);
        assert_eq!(source.read_sample(&[0]), -1.0);
        assert_eq!(source.read_sample(&[128]), 0.0);
        assert_eq!(source.read_sample(&[255]), 127.0 / 128.0);
    }

    #[test]
    fn converts_signed_twenty_four_bit_pcm() {
        let source = mono_format(24, 24, false);
        assert_eq!(source.read_sample(&[0, 0, 0x80]), -1.0);
        assert_eq!(
            source.read_sample(&[0xFF, 0xFF, 0x7F]),
            8_388_607.0 / 8_388_608.0
        );
    }

    #[test]
    fn honors_valid_bits_in_a_wider_pcm_container() {
        let source = mono_format(32, 24, false);
        assert_eq!(source.read_sample(&[0, 0, 0, 0x80]), -1.0);
        assert!((source.read_sample(&[0, 0xFF, 0xFF, 0x7F]) - 1.0).abs() < 0.000_001);
    }

    #[test]
    fn clamps_float_samples_and_replaces_nonfinite_values() {
        let float32 = mono_format(32, 32, true);
        assert_eq!(float32.read_sample(&2.0_f32.to_le_bytes()), 1.0);
        assert_eq!(float32.read_sample(&f32::NAN.to_le_bytes()), 0.0);
        let float64 = mono_format(64, 64, true);
        assert_eq!(float64.read_sample(&(-2.0_f64).to_le_bytes()), -1.0);
        assert_eq!(float64.read_sample(&f64::INFINITY.to_le_bytes()), 0.0);
    }

    #[test]
    fn mono_audio_is_sent_to_both_channels() {
        let source = mono_format(16, 16, false);
        assert_eq!(
            source.mix_frame(Some(&0x4000_i16.to_le_bytes()), 0, false),
            (0.5, 0.5)
        );
    }

    #[test]
    fn silent_and_missing_packets_produce_stereo_silence() {
        let source = stereo_format(48_000);
        assert_eq!(source.mix_frame(None, 0, true), (0.0, 0.0));
        assert_eq!(source.mix_frame(None, 0, false), (0.0, 0.0));
    }

    #[test]
    fn surround_center_has_equal_stereo_coefficients() {
        let (left, right) = speaker_coefficients(SPEAKER_FRONT_CENTER, 2);
        assert_eq!(left, right);
        assert_eq!(left, std::f32::consts::FRAC_1_SQRT_2);
    }

    #[test]
    fn resamples_without_losing_channel_order() {
        let mut converter = Converter::new(stereo_format(48_000), 48_000);
        let bytes = [0x00, 0x40, 0x00, 0xC0, 0x00, 0x20, 0x00, 0xE0];
        let block = converter.process(Some(&bytes), 2, false);
        assert_eq!(block.left, vec![0.5, 0.25]);
        assert_eq!(block.right, vec![-0.5, -0.25]);
    }

    #[test]
    fn resampling_is_contiguous_across_input_blocks() {
        let mut converter = Converter::new(stereo_format(48_000), 48_000);
        let first = converter.process(Some(&[0, 0x10, 0, 0x20]), 1, false);
        let second = converter.process(Some(&[0, 0x30, 0, 0x40]), 1, false);
        assert_eq!(first.left, vec![0.125]);
        assert_eq!(second.left, vec![0.375]);
        assert_eq!(first.right, vec![0.25]);
        assert_eq!(second.right, vec![0.5]);
    }

    #[test]
    fn upsampling_flushes_the_last_interpolated_frame() {
        let mut converter = Converter::new(stereo_format(24_000), 48_000);
        let block = converter.process(Some(&[0, 0, 0, 0, 0, 0x40, 0, 0x40]), 2, false);
        let flushed = converter.flush();
        assert_eq!(block.left.len() + flushed.left.len(), 4);
        assert_eq!(flushed.left.last(), Some(&0.5));
    }

    #[test]
    fn an_empty_source_block_does_not_advance_the_converter() {
        let mut converter = Converter::new(stereo_format(48_000), 48_000);
        let block = converter.process(None, 0, true);
        assert!(block.pcm.is_empty());
        assert_eq!(converter.total_source_frames, 0);
    }

    #[test]
    fn pcm_conversion_covers_full_scale_and_nonfinite_values() {
        assert_eq!(float_to_i16(-1.0), i16::MIN);
        assert_eq!(float_to_i16(1.0), i16::MAX);
        assert_eq!(float_to_i16(0.0), 0);
        assert_eq!(float_to_i16(f32::NAN), 0);
        assert_eq!(float_to_i16(f32::INFINITY), 0);
    }

    #[test]
    fn media_times_are_contiguous() {
        assert_eq!(media_time(48_000, 48_000), 10_000_000);
        assert_eq!(media_time(96_000, 48_000), 20_000_000);
        assert_eq!(media_time(44_100, 44_100), 10_000_000);
    }

    #[test]
    fn duration_limit_uses_encoded_timeline() {
        let limit = std::time::Duration::from_millis(250);
        assert_eq!(limit.as_secs_f64() * 48_000.0, 12_000.0);
    }

    #[test]
    fn visualization_events_leave_room_for_state_events() {
        let (events, _receiver) = bounded(10);
        assert!(has_visual_event_capacity(&events));
        events.send(AudioEvent::Finalizing).unwrap();
        assert!(has_visual_event_capacity(&events));
        events.send(AudioEvent::Finalizing).unwrap();
        assert!(!has_visual_event_capacity(&events));
    }

    #[test]
    fn trim_temporary_names_do_not_replace_an_existing_candidate() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("clip.mp3");
        let first = trim_temporary_path(&output).unwrap();
        fs::write(&first, []).unwrap();
        let second = trim_temporary_path(&output).unwrap();
        assert_ne!(first, second);
        assert_eq!(second.parent(), Some(directory.path()));
    }

    #[test]
    fn raw_pcm_writer_preserves_interleaved_little_endian_samples() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("part.pcm");
        let mut writer = RawPcmWriter::open(&path).unwrap();
        writer.write(&[i16::MIN, -1, 0, i16::MAX]).unwrap();
        assert_eq!(writer.close().unwrap(), path);
        assert_eq!(
            fs::read(path).unwrap(),
            [0, 0x80, 0xFF, 0xFF, 0, 0, 0xFF, 0x7F]
        );
    }
}
