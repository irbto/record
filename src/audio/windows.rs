use std::{
    ffi::c_void,
    fs, mem,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::atomic::Ordering,
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

use super::{AudioEvent, EventSender, RecordConfig, RecordingSummary};

const UI_CHUNK_FRAMES: usize = 1_024;
const MAX_GAP_SECONDS: u64 = 5;

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

pub fn record(config: RecordConfig, events: &EventSender) -> Result<RecordingSummary> {
    record_inner(&config, events)
}

fn record_inner(config: &RecordConfig, events: &EventSender) -> Result<RecordingSummary> {
    let _com = ComGuard::new()?;
    let _mf = MediaFoundationGuard::new()?;
    let (audio_client, mix_format) = default_audio_client()?;
    let source = unsafe { SourceFormat::from_wave(mix_format.0) }?;
    let mut writer = unsafe {
        Mp3Writer::open(
            &config.output,
            source.sample_rate,
            config.bitrate,
            config.force,
        )
        .context("could not open the native Windows MP3 encoder")?
    };
    let mut converter = Converter::new(source, writer.sample_rate);
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
        sample_rate: writer.sample_rate,
        bitrate: writer.bitrate,
        channels: 2,
    });

    let mut expected_position = None;
    let max_gap = u64::from(source.sample_rate) * MAX_GAP_SECONDS;
    let duration_frames = config
        .duration
        .map(|duration| duration.as_secs_f64() * f64::from(writer.sample_rate));

    while !config.stop.load(Ordering::Relaxed) {
        if duration_frames.is_some_and(|limit| writer.frames as f64 >= limit) {
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

            let packet_result = (|| -> Result<()> {
                if !config.paused.load(Ordering::Relaxed) {
                    if let Some(expected) = expected_position {
                        let gap = device_position.saturating_sub(expected);
                        if gap > 0 && gap <= max_gap {
                            process_source_frames(
                                &mut converter,
                                &mut writer,
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
                        &mut writer,
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
        writer.write(&flushed.pcm)?;
        send_samples(events, flushed, writer.frames);
    }
    writer.finish()?;

    Ok(RecordingSummary {
        output: config.output.clone(),
        sample_rate: writer.sample_rate,
        bitrate: writer.bitrate,
        frames: writer.frames,
    })
}

fn process_source_frames(
    converter: &mut Converter,
    writer: &mut Mp3Writer,
    events: &EventSender,
    bytes: Option<&[u8]>,
    frames: usize,
    silent: bool,
) -> Result<()> {
    let converted = converter.process(bytes, frames, silent);
    if converted.pcm.is_empty() {
        return Ok(());
    }
    writer.write(&converted.pcm)?;
    send_samples(events, converted, writer.frames);
    Ok(())
}

fn send_samples(events: &EventSender, converted: ConvertedBlock, frames: u64) {
    for (index, (left, right)) in converted
        .left
        .chunks(UI_CHUNK_FRAMES)
        .zip(converted.right.chunks(UI_CHUNK_FRAMES))
        .enumerate()
    {
        if events.is_full() {
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

fn default_audio_client() -> Result<(IAudioClient, MixFormat)> {
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

struct ComGuard;

impl ComGuard {
    fn new() -> Result<Self> {
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

struct MediaFoundationGuard;

impl MediaFoundationGuard {
    fn new() -> Result<Self> {
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

struct MixFormat(*mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) };
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct CaptureGuard<'a> {
    client: &'a IAudioClient,
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

struct Mp3Writer {
    writer: Option<IMFSinkWriter>,
    byte_stream: Option<IMFByteStream>,
    output: PathBuf,
    stream_index: u32,
    sample_rate: u32,
    bitrate: u32,
    frames: u64,
    finalized: bool,
}

impl Mp3Writer {
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

    fn finish(&mut self) -> Result<()> {
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
        Ok(())
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

const fn media_time(frames: u64, sample_rate: u32) -> i64 {
    ((frames * 10_000_000) / sample_rate as u64) as i64
}

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
struct SourceFormat {
    channels: usize,
    sample_rate: u32,
    block_align: usize,
    container_bits: u16,
    valid_bits: u16,
    float: bool,
    mix_left: [f32; 32],
    mix_right: [f32; 32],
}

impl SourceFormat {
    unsafe fn from_wave(pointer: *const WAVEFORMATEX) -> Result<Self> {
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

    fn read_sample(&self, bytes: &[u8]) -> f32 {
        if self.float {
            return match self.container_bits {
                32 => f32::from_le_bytes(bytes.try_into().unwrap()).clamp(-1.0, 1.0),
                64 => f64::from_le_bytes(bytes.try_into().unwrap()).clamp(-1.0, 1.0) as f32,
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

struct Converter {
    source: SourceFormat,
    target_rate: u32,
    next_position: f64,
    total_source_frames: u64,
    previous: Option<(f32, f32)>,
}

#[derive(Default)]
struct ConvertedBlock {
    pcm: Vec<i16>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Converter {
    const fn new(source: SourceFormat, target_rate: u32) -> Self {
        Self {
            source,
            target_rate,
            next_position: 0.0,
            total_source_frames: 0,
            previous: None,
        }
    }

    fn process(&mut self, bytes: Option<&[u8]>, frames: usize, silent: bool) -> ConvertedBlock {
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

    fn flush(&mut self) -> ConvertedBlock {
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
    fn resamples_without_losing_channel_order() {
        let mut converter = Converter::new(stereo_format(48_000), 48_000);
        let bytes = [0x00, 0x40, 0x00, 0xC0, 0x00, 0x20, 0x00, 0xE0];
        let block = converter.process(Some(&bytes), 2, false);
        assert_eq!(block.left, vec![0.5, 0.25]);
        assert_eq!(block.right, vec![-0.5, -0.25]);
    }

    #[test]
    fn media_times_are_contiguous() {
        assert_eq!(media_time(48_000, 48_000), 10_000_000);
        assert_eq!(media_time(96_000, 48_000), 20_000_000);
    }

    #[test]
    fn duration_limit_uses_encoded_timeline() {
        let limit = std::time::Duration::from_millis(250);
        assert_eq!(limit.as_secs_f64() * 48_000.0, 12_000.0);
    }
}
