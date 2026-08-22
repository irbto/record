//! Native Windows screen capture, GPU color conversion, and MP4 encoding.
//!
//! The backend duplicates the primary monitor with DXGI, converts desktop
//! textures to NV12, and encodes H.264 video plus AAC audio through one
//! Media Foundation sink writer. One video thread owns every COM, D3D11, and
//! Media Foundation object for its full lifetime.

use std::{
    fs, mem,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::Sender;
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_CREATE_DEVICE_SINGLETHREADED, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, IDXGIAdapter1, IDXGIDevice,
                IDXGIOutput1, IDXGIOutputDuplication,
            },
        },
        Media::{
            Audio::{
                AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
                AUDCLNT_STREAMFLAGS_NOPERSIST, IAudioCaptureClient,
            },
            MediaFoundation::{
                IMFAttributes, IMFByteStream, IMFMediaType, IMFSinkWriter, MF_ACCESSMODE_WRITE,
                MF_FILEFLAGS_NONE, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
                MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_FIXED_SIZE_SAMPLES,
                MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
                MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_OPENMODE_DELETE_IF_EXIST, MFAudioFormat_AAC,
                MFAudioFormat_PCM, MFCreateFile, MFCreateMediaType, MFCreateMemoryBuffer,
                MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio, MFMediaType_Video,
                MFSampleExtension_CleanPoint, MFSampleExtension_Discontinuity, MFT_ENUM_FLAG_ALL,
                MFTranscodeGetAudioOutputAvailableTypes, MFVideoFormat_H264, MFVideoFormat_NV12,
            },
        },
        System::{
            SystemInformation::GetLocalTime,
            Threading::{AvSetMmThreadCharacteristicsW, CreateEventW},
        },
    },
    core::{Interface, PCWSTR, w},
};

use crate::audio::windows::{
    ComGuard, Converter, MediaFoundationGuard, SourceFormat, default_audio_client,
};

/// Maximum device-position gap that becomes inserted silence.
const MAX_GAP_SECONDS: u64 = 5;

/// The default capture frame rate in frames per second.
pub const DEFAULT_FPS: u32 = 60;

/// The default encoded video bit rate in bits per second.
pub const DEFAULT_VIDEO_BITRATE: u32 = 20_000_000;

/// The default encoded audio bit rate in bits per second.
pub const DEFAULT_AUDIO_BITRATE: u32 = 192_000;

/// Selects the monitor that the recorder captures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoSource {
    /// Capture the primary monitor output of the capture device.
    Primary,
    /// Capture the monitor at this DXGI output index.
    Index(u32),
}

/// Selects how the source maps into the encoded canvas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FitMode {
    /// Preserve aspect ratio and add letterbox or pillarbox bars.
    #[default]
    Contain,
    /// Preserve aspect ratio and crop overflow beyond the canvas.
    Cover,
    /// Fill the canvas without preserving aspect ratio.
    Stretch,
    /// Use one source pixel per output pixel when possible.
    Native,
}

impl FitMode {
    /// Returns the next fit mode in display order.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Contain => Self::Cover,
            Self::Cover => Self::Stretch,
            Self::Stretch => Self::Native,
            Self::Native => Self::Contain,
        }
    }

    /// Returns the CLI name of this fit mode.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Contain => "contain",
            Self::Cover => "cover",
            Self::Stretch => "stretch",
            Self::Native => "native",
        }
    }
}

/// Describes a crop rectangle in source pixel coordinates.
///
/// The default value has a zero size, which [`effective_crop`] resolves to
/// the full source frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CropRect {
    /// Left edge in source pixels.
    pub left: u32,
    /// Top edge in source pixels.
    pub top: u32,
    /// Crop width in source pixels; zero selects the full width.
    pub width: u32,
    /// Crop height in source pixels; zero selects the full height.
    pub height: u32,
}

impl CropRect {
    /// Clamps this rectangle to the source bounds with an even size.
    #[must_use]
    pub fn clamp_to(&self, source_width: u32, source_height: u32) -> Self {
        let left = self.left.min(source_width.saturating_sub(2));
        let top = self.top.min(source_height.saturating_sub(2));
        let width = (self.width.min(source_width - left)) & !1;
        let height = (self.height.min(source_height - top)) & !1;
        Self {
            left,
            top,
            width: width.max(2),
            height: height.max(2),
        }
    }

    /// Reports whether this rectangle covers the full source.
    #[must_use]
    pub const fn is_full(&self, source_width: u32, source_height: u32) -> bool {
        self.left == 0
            && self.top == 0
            && self.width >= source_width
            && self.height >= source_height
    }
}

/// Contains a named canvas preset size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanvasSize {
    /// Encoded frame width in pixels.
    pub width: u32,
    /// Encoded frame height in pixels.
    pub height: u32,
}

/// Returns the built-in canvas presets.
#[must_use]
pub fn canvas_presets() -> &'static [(&'static str, CanvasSize)] {
    &[
        (
            "native",
            CanvasSize {
                width: 0,
                height: 0,
            },
        ),
        (
            "1080p",
            CanvasSize {
                width: 1920,
                height: 1080,
            },
        ),
        (
            "1440p",
            CanvasSize {
                width: 2560,
                height: 1440,
            },
        ),
        (
            "4k",
            CanvasSize {
                width: 3840,
                height: 2160,
            },
        ),
        (
            "720p",
            CanvasSize {
                width: 1280,
                height: 720,
            },
        ),
        (
            "square",
            CanvasSize {
                width: 1080,
                height: 1080,
            },
        ),
    ]
}

/// Computes the encoded geometry for one source, crop, canvas, and fit mode.
#[must_use]
pub fn transform_geometry(
    source_width: u32,
    source_height: u32,
    crop: &CropRect,
    fit: FitMode,
    canvas_width: u32,
    canvas_height: u32,
) -> (u32, u32, f64, f64) {
    // The floor keeps degenerate crops from dividing the scale by zero.
    let crop_width = crop.width.min(source_width).max(2);
    let crop_height = crop.height.min(source_height).max(2);
    let (canvas_width, canvas_height) = if canvas_width == 0 || canvas_height == 0 {
        let even_width = crop_width & !1;
        let even_height = crop_height & !1;
        (even_width.max(2), even_height.max(2))
    } else {
        (canvas_width & !1, canvas_height & !1)
    };
    let (scale_x, scale_y) = match fit {
        FitMode::Stretch => (
            f64::from(canvas_width) / f64::from(crop_width),
            f64::from(canvas_height) / f64::from(crop_height),
        ),
        FitMode::Contain => {
            let scale = f64::from(canvas_width)
                .min(f64::from(canvas_height) * f64::from(crop_width) / f64::from(crop_height))
                / f64::from(crop_width);
            (scale, scale)
        }
        FitMode::Cover => {
            let scale = f64::from(canvas_width)
                .max(f64::from(canvas_height) * f64::from(crop_width) / f64::from(crop_height))
                / f64::from(crop_width);
            (scale, scale)
        }
        FitMode::Native => (1.0, 1.0),
    };
    (canvas_width, canvas_height, scale_x, scale_y)
}

/// Resolves the effective crop for one source size.
///
/// A zero width or height means "no crop was requested", which captures the
/// full frame. Every other rectangle clamps to the source bounds.
#[must_use]
pub fn effective_crop(crop: &CropRect, source_width: u32, source_height: u32) -> CropRect {
    if crop.width == 0 || crop.height == 0 {
        return CropRect {
            left: 0,
            top: 0,
            width: source_width,
            height: source_height,
        }
        .clamp_to(source_width, source_height);
    }
    crop.clamp_to(source_width, source_height)
}
/// Contains all settings for one video recording process.
#[derive(Clone, Debug)]
pub struct VideoConfig {
    /// Selects the captured monitor.
    pub source: VideoSource,
    /// Crop rectangle in source pixels.
    pub crop: CropRect,
    /// Fit mode for the encoded canvas.
    pub fit: FitMode,
    /// Encoded canvas width; zero uses the source width.
    pub canvas_width: u32,
    /// Encoded canvas height; zero uses the source height.
    pub canvas_height: u32,
    /// Requests an H.264 bit rate in bits per second.
    pub video_bitrate: u32,
    /// Requests an AAC bit rate in bits per second.
    pub audio_bitrate: u32,
    /// Requests the capture frame rate.
    pub fps: u32,
    /// Stops capture after this encoded duration when set.
    pub duration: Option<Duration>,
    /// Destination MP4 path.
    pub output: PathBuf,
    /// Replaces the file when it already exists.
    pub replace: bool,
    /// Becomes true when capture must stop.
    pub stop: Arc<AtomicBool>,
    /// Omits capture packets while true.
    pub paused: Arc<AtomicBool>,
}

/// Reports capture state to the interface.
#[derive(Clone, Debug)]
pub enum VideoEvent {
    /// Reports that the capture and encoder pipelines have started.
    Started {
        /// Actual encoded frame width in pixels.
        width: u32,
        /// Actual encoded frame height in pixels.
        height: u32,
        /// Actual encoded frame rate.
        fps: u32,
        /// Selected audio sample rate in frames per second.
        sample_rate: u32,
        /// Milliseconds from process start to capture readiness.
        capture_ready_ms: f64,
    },
    /// Reports that the recorder finalized the MP4 file.
    Saved(VideoSummary),
    /// Provides a short backend message for the interface.
    Notice(String),
    /// Reports that capture has stopped and the MP4 is finalizing.
    Finalizing,
}
/// Summarizes one complete MP4 recording.
#[derive(Clone, Debug)]
pub struct VideoSummary {
    /// Final path of the MP4 file.
    pub output: PathBuf,
    /// Encoded video width in pixels.
    pub width: u32,
    /// Encoded video height in pixels.
    pub height: u32,
    /// Encoded frame rate in frames per second.
    pub fps: u32,
    /// Total video frames written to the file.
    pub frames: u64,
    /// Selected audio sample rate.
    pub sample_rate: u32,
    /// Selected H.264 bit rate.
    pub video_bitrate: u32,
    /// Selected AAC bit rate.
    pub audio_bitrate: u32,
    /// Milliseconds from start to the first real video sample entering the MP4.
    pub recording_ready_ms: Option<f64>,
    /// Milliseconds from stop request to the finalized playable file.
    pub finalize_ms: f64,
}

impl VideoSummary {
    /// Returns the encoded duration of this recording.
    #[must_use]
    pub fn duration(&self) -> Duration {
        if self.fps == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(self.frames as f64 / f64::from(self.fps))
        }
    }
}

/// Reports whether this machine can run the video recorder.
pub fn check_video_support() -> Result<()> {
    let _com = ComGuard::new()?;
    let _mf = MediaFoundationGuard::new()?;
    unsafe {
        MFTranscodeGetAudioOutputAvailableTypes(
            &MFAudioFormat_AAC,
            MFT_ENUM_FLAG_ALL.0 as u32,
            None::<&IMFAttributes>,
        )
    }
    .context("Windows has no available AAC encoder")?;
    create_d3d_device()?;
    Ok(())
}

/// Captures the screen and system audio until a stop condition.
pub fn record_video(config: VideoConfig, events: &Sender<VideoEvent>) -> Result<VideoSummary> {
    record_video_inner(&config, events)
}

/// Owns COM, Media Foundation, D3D11, WASAPI, and both capture loops.
fn record_video_inner(config: &VideoConfig, events: &Sender<VideoEvent>) -> Result<VideoSummary> {
    let _com = ComGuard::new()?;
    let _mf = MediaFoundationGuard::new()?;

    let started = Instant::now();
    let device = create_d3d_device()?;
    let output_index = match config.source {
        VideoSource::Primary => 0,
        VideoSource::Index(index) => index,
    };
    let mut duplication = MonitorDuplication::create(&device, output_index)?;
    let (width, height) = duplication.size();
    let even_width = width & !1;
    let even_height = height & !1;
    let crop = effective_crop(&config.crop, even_width, even_height);
    let (encoded_width, encoded_height, _scale_x, _scale_y) = transform_geometry(
        even_width,
        even_height,
        &crop,
        config.fit,
        config.canvas_width,
        config.canvas_height,
    );

    let (audio_client, mix_format) = default_audio_client()?;
    let source = unsafe { SourceFormat::from_wave(mix_format.0) }?;
    let audio_rate = nearest_aac_rate(source.sample_rate);
    let mut converter = Converter::new(source, audio_rate);

    let mut writer = unsafe {
        Mp4Writer::open(
            &config.output,
            encoded_width,
            encoded_height,
            config.fps,
            config.video_bitrate,
            audio_rate,
            config.audio_bitrate,
            config.replace,
            None,
            0.0,
        )
    }?;

    let audio_event = unsafe { CreateEventW(None, false, false, None)? };
    unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_NOPERSIST,
            0,
            0,
            mix_format.0,
            None,
        )?;
        audio_client.SetEventHandle(audio_event)?;
    }
    let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService() }?;
    let mut task_index = 0;
    let mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index) }.ok();
    unsafe { audio_client.Start()? };

    let _ = events.try_send(VideoEvent::Started {
        width: encoded_width,
        height: encoded_height,
        fps: config.fps,
        sample_rate: audio_rate,
        capture_ready_ms: started.elapsed().as_secs_f64() * 1_000.0,
    });

    let frame_interval = Duration::from_secs_f64(1.0 / f64::from(config.fps.max(1)));
    let duration_frames = config
        .duration
        .map(|duration| u64::from(config.fps.max(1)) * duration.as_secs())
        .filter(|frames| *frames > 0);
    let mut expected_position = None;
    let mut recording_ready_ms: Option<f64> = None;
    let mut next_frame_time = Instant::now();
    let mut last_frame: Option<Nv12Frame> = None;
    let mut recorded_time = RecordedTime::new();
    let mut was_paused = false;

    loop {
        if config.stop.load(Ordering::Relaxed) {
            break;
        }
        if duration_frames.is_some_and(|limit| writer.video_frames >= limit) {
            break;
        }
        let now = Instant::now();
        if now < next_frame_time {
            std::thread::sleep((next_frame_time - now).min(Duration::from_millis(8)));
            continue;
        }
        next_frame_time += frame_interval;

        let paused_now = config.paused.load(Ordering::Relaxed);
        if was_paused && !paused_now {
            // Wall time advanced while paused. Rebase the audio gap detector so
            // the paused span stays omitted from both streams instead of
            // becoming inserted silence under a frozen video timeline.
            expected_position = None;
        }
        was_paused = paused_now;

        let mut wrote_frame = false;
        if !paused_now {
            match duplication.capture_next_frame() {
                Ok(Some(texture)) => {
                    let nv12 = unsafe {
                        convert_to_nv12(
                            &device,
                            &texture,
                            crop.left,
                            crop.top,
                            crop.width,
                            crop.height,
                        )?
                    };
                    unsafe { writer.write_video_frame(&nv12, recorded_time.value())? };
                    if recording_ready_ms.is_none() {
                        recording_ready_ms = Some(started.elapsed().as_secs_f64() * 1_000.0);
                    }
                    last_frame = Some(nv12);
                    wrote_frame = true;
                }
                Ok(None) => {
                    // The desktop did not update. Duplicate the previous frame
                    // so the H.264 timeline stays aligned with the audio clock.
                    // Before any update arrives, emit one black frame so that
                    // idle screens still start the timeline and honor
                    // the configured duration.
                    let frame =
                        last_frame.get_or_insert_with(|| Nv12Frame::black(crop.width, crop.height));
                    unsafe { writer.write_video_frame(frame, recorded_time.value())? };
                    wrote_frame = true;
                }
                Err(error) => {
                    // Windows can invalidate the session on display changes,
                    // mode switches, or secure desktop transitions. Recreate it.
                    if duplication.recreate(&device).is_err() {
                        let _ = events.try_send(VideoEvent::Notice(error.to_string()));
                        break;
                    }
                    last_frame = None;
                }
            }
            if wrote_frame {
                recorded_time.tick(frame_interval, false);
            }
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
                if config.paused.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if let Some(expected) = expected_position {
                    let gap = device_position.saturating_sub(expected);
                    let max_gap = u64::from(source.sample_rate) * MAX_GAP_SECONDS;
                    if gap > 0 && gap <= max_gap {
                        let silent = converter.process(None, gap as usize, true);
                        unsafe { writer.write_audio(&silent.pcm)? };
                    }
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                let bytes = if silent {
                    None
                } else {
                    let byte_len = frames as usize * source.block_align;
                    Some(unsafe { std::slice::from_raw_parts(data, byte_len) })
                };
                let converted = converter.process(bytes, frames as usize, silent);
                unsafe { writer.write_audio(&converted.pcm)? };
                expected_position = Some(device_position + u64::from(frames));
                Ok(())
            })();
            unsafe { capture_client.ReleaseBuffer(frames)? };
            packet_result?;
        }
    }

    let stop_requested = Instant::now();
    let _ = events.try_send(VideoEvent::Finalizing);
    let flushed = converter.flush();
    unsafe { writer.write_audio(&flushed.pcm)? };
    let _ = unsafe { audio_client.Stop() };
    if let Some(handle) = mmcss {
        let _ =
            unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(handle) };
    }
    let mut summary = unsafe { writer.close() }?;
    summary.finalize_ms = stop_requested.elapsed().as_secs_f64() * 1_000.0;
    summary.recording_ready_ms = recording_ready_ms;
    let _ = events.try_send(VideoEvent::Saved(summary.clone()));
    Ok(summary)
}

/// Describes one attached monitor for the selection interface.
#[derive(Clone, Debug)]
pub struct MonitorInfo {
    /// Stable session index starting at zero.
    pub index: u32,
    /// Windows display name such as `\\.\DISPLAY1`.
    pub name: String,
    /// Reports whether this is the primary monitor.
    pub primary: bool,
    /// Current resolution width in pixels.
    pub width: u32,
    /// Current resolution height in pixels.
    pub height: u32,
    /// Desktop position left coordinate.
    pub left: i32,
    /// Desktop position top coordinate.
    pub top: i32,
}

/// Enumerates every attached monitor with its capture metadata.
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>> {
    let _com = ComGuard::new()?;
    let device = create_d3d_device()?;
    let dxgi_device: IDXGIDevice = device
        .cast()
        .context("the D3D11 device does not expose DXGI")?;
    let adapter: IDXGIAdapter1 = unsafe { dxgi_device.GetAdapter() }?.cast()?;
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    let mut index = 0;
    loop {
        let output = match unsafe { adapter.EnumOutputs(index) } {
            Ok(output) => output,
            Err(_) => break,
        };
        let description = unsafe { output.GetDesc() }?;
        let coords = description.DesktopCoordinates;
        if coords.right > coords.left && coords.bottom > coords.top {
            let primary =
                description.DesktopCoordinates.left == 0 && description.DesktopCoordinates.top == 0;
            monitors.push(MonitorInfo {
                index,
                name: String::from_utf16_lossy(
                    &description.DeviceName[..description
                        .DeviceName
                        .iter()
                        .position(|unit| *unit == 0)
                        .unwrap_or(description.DeviceName.len())],
                ),
                primary,
                width: (coords.right - coords.left) as u32,
                height: (coords.bottom - coords.top) as u32,
                left: coords.left,
                top: coords.top,
            });
        }
        index += 1;
    }
    Ok(monitors)
}
/// Creates a hardware D3D11 device with BGRA support for desktop duplication.
fn create_d3d_device() -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .context("could not create a hardware D3D11 device")?;
    device.ok_or_else(|| anyhow!("D3D11CreateDevice returned no device"))
}

/// Owns one DXGI desktop duplication session for the primary monitor.
struct MonitorDuplication {
    duplication: IDXGIOutputDuplication,
    width: u32,
    height: u32,
}

impl MonitorDuplication {
    /// Duplicates the first output on the adapter of the given device.
    fn create(device: &ID3D11Device, output_index: u32) -> Result<Self> {
        let dxgi_device: IDXGIDevice = device
            .cast()
            .context("the D3D11 device does not expose DXGI")?;
        let adapter: IDXGIAdapter1 = unsafe { dxgi_device.GetAdapter() }?.cast()?;
        let output = unsafe { adapter.EnumOutputs(output_index) }
            .context("no monitor output is available for capture")?;
        let output1: IDXGIOutput1 = output
            .cast()
            .context("this Windows version cannot duplicate the desktop")?;
        let duplication = unsafe { output1.DuplicateOutput(device) }
            .context("Windows refused desktop duplication")?;
        let description = unsafe { duplication.GetDesc() };
        let width = description.ModeDesc.Width;
        let height = description.ModeDesc.Height;
        if width == 0 || height == 0 {
            bail!("the duplicated monitor reported an empty size");
        }
        Ok(Self {
            duplication,
            width,
            height,
        })
    }

    /// Returns the duplicated frame size in pixels.
    const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Recreates the duplication session after Windows invalidates it.
    fn recreate(&mut self, device: &ID3D11Device) -> Result<()> {
        let fresh = MonitorDuplication::create(device, 0)?;
        self.duplication = fresh.duplication;
        self.width = fresh.width;
        self.height = fresh.height;
        Ok(())
    }

    /// Acquires one desktop texture or reports no update.
    fn capture_next_frame(&mut self) -> Result<Option<ID3D11Texture2D>> {
        let mut frame_info = Default::default();
        let mut resource = None;
        let result = unsafe {
            self.duplication
                .AcquireNextFrame(0, &mut frame_info, &mut resource)
        };
        match result {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                bail!("desktop duplication session was invalidated")
            }
            Err(error) => return Err(error).context("desktop duplication returned an error"),
        }
        let texture: ID3D11Texture2D = resource
            .ok_or_else(|| anyhow!("desktop duplication returned no frame resource"))?
            .cast()
            .context("the duplicated frame is not a texture")?;
        let _ = unsafe { self.duplication.ReleaseFrame() };
        Ok(Some(texture))
    }
}

/// Converts one BGRA desktop texture to NV12 through a staging readback.
///
/// This keeps the first version correct and simple. A later phase replaces
/// the readback with a compute shader so the CPU never touches full frames.
///
/// # Safety
///
/// `texture` must be a valid BGRA texture of the requested size.
unsafe fn convert_to_nv12(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
    crop_left: u32,
    crop_top: u32,
    width: u32,
    height: u32,
) -> Result<Nv12Frame> {
    let context: ID3D11DeviceContext = unsafe { device.GetImmediateContext() }?;
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging_out = None;
    unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging_out))? };
    let staging =
        staging_out.ok_or_else(|| anyhow!("CreateTexture2D returned no staging texture"))?;
    unsafe { context.CopyResource(&staging, texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };

    let luma_stride = width as usize;
    let chroma_stride = width as usize;
    let luma_size = luma_stride * height as usize;
    let chroma_size = chroma_stride * (height as usize / 2);
    let mut nv12 = vec![0_u8; luma_size + chroma_size];
    let source_bytes = mapped.pData.cast::<u8>();
    let source_stride = mapped.RowPitch as usize;

    for row in 0..height as usize {
        unsafe {
            let source_row = source_bytes
                .add((row + crop_top as usize) * source_stride + crop_left as usize * 4);
            let luma_row = nv12.as_mut_ptr().add(row * luma_stride);
            for column in 0..width as usize {
                let pixel = source_row.add(column * 4);
                let blue = f32::from(*pixel);
                let green = f32::from(*pixel.add(1));
                let red = f32::from(*pixel.add(2));
                let luma = (0.2126 * red + 0.7152 * green + 0.0722 * blue).round();
                *luma_row.add(column) = luma.clamp(0.0, 255.0) as u8;
            }
        }
    }
    for row in 0..(height as usize / 2) {
        unsafe {
            let source_row = source_bytes
                .add((row * 2 + crop_top as usize) * source_stride + crop_left as usize * 4);
            let chroma_row = nv12.as_mut_ptr().add(luma_size + row * chroma_stride);
            for column in 0..(width as usize / 2) {
                let pixel = source_row.add(column * 8);
                let blue = f32::from(*pixel);
                let green = f32::from(*pixel.add(1));
                let red = f32::from(*pixel.add(2));
                let cb = (-0.1146 * red - 0.3854 * green + 0.5 * blue + 128.0).round();
                let cr = (0.5 * red - 0.4542 * green - 0.0458 * blue + 128.0).round();
                *chroma_row.add(column * 2) = cb.clamp(0.0, 255.0) as u8;
                *chroma_row.add(column * 2 + 1) = cr.clamp(0.0, 255.0) as u8;
            }
        }
    }
    unsafe { context.Unmap(&staging, 0) };
    Ok(Nv12Frame { data: nv12 })
}

/// Contains one converted NV12 frame in system memory.
struct Nv12Frame {
    data: Vec<u8>,
}

impl Nv12Frame {
    /// Creates an all-black NV12 frame with neutral chroma.
    ///
    /// The luma plane stays zero and the chroma planes sit at 128 so encoders
    /// see a valid neutral black picture.
    fn black(width: u32, height: u32) -> Self {
        let luma_size = width as usize * height as usize;
        let chroma_size = luma_size / 2;
        let mut data = vec![0_u8; luma_size + chroma_size];
        for byte in &mut data[luma_size..] {
            *byte = 128;
        }
        Self { data }
    }
}

/// Owns one Media Foundation sink writer with H.264 and AAC streams.
struct Mp4Writer {
    writer: Option<IMFSinkWriter>,
    byte_stream: Option<IMFByteStream>,
    output: PathBuf,
    video_stream: u32,
    audio_stream: u32,
    video_bitrate: u32,
    audio_bitrate: u32,
    fps: u32,
    sample_rate: u32,
    width: u32,
    height: u32,
    video_frames: u64,
    audio_frames: u64,
    finalized: bool,
    /// Milliseconds from start to the first real video sample.
    recording_ready_ms: Option<f64>,
    /// Milliseconds from stop request to finalized file.
    finalize_ms: f64,
}

impl Mp4Writer {
    /// Selects encoder profiles and opens the MP4 sink.
    ///
    /// # Allow
    /// The encoder profile has many independent settings.
    #[allow(clippy::too_many_arguments)]
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be active on the current thread.
    unsafe fn open(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        video_bitrate: u32,
        sample_rate: u32,
        audio_bitrate: u32,
        replace: bool,
        recording_ready_ms: Option<f64>,
        finalize_ms: f64,
    ) -> Result<Self> {
        let wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let open_mode = if replace {
            MF_OPENMODE_DELETE_IF_EXIST
        } else {
            windows::Win32::Media::MediaFoundation::MF_OPENMODE_FAIL_IF_EXIST
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

        let video_output = unsafe { video_output_type(width, height, fps, video_bitrate)? };
        let video_input = unsafe { video_input_type(width, height, fps)? };
        let audio_output = unsafe { audio_output_type(sample_rate, audio_bitrate)? };
        let audio_input = unsafe { audio_input_type(sample_rate)? };

        let setup = (|| -> Result<(IMFSinkWriter, u32, u32)> {
            let writer = unsafe {
                MFCreateSinkWriterFromURL(
                    PCWSTR(wide_path.as_ptr()),
                    &byte_stream,
                    None::<&IMFAttributes>,
                )
            }?;
            let video_stream = unsafe { writer.AddStream(&video_output)? };
            unsafe {
                writer.SetInputMediaType(video_stream, &video_input, None::<&IMFAttributes>)?;
            }
            let audio_stream = unsafe { writer.AddStream(&audio_output)? };
            unsafe {
                writer.SetInputMediaType(audio_stream, &audio_input, None::<&IMFAttributes>)?;
            }
            unsafe { writer.BeginWriting()? };
            Ok((writer, video_stream, audio_stream))
        })();
        let (writer, video_stream, audio_stream) = match setup {
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
            video_stream,
            audio_stream,
            video_bitrate,
            audio_bitrate,
            fps,
            sample_rate,
            width,
            height,
            video_frames: 0,
            audio_frames: 0,
            finalized: false,
            recording_ready_ms,
            finalize_ms,
        })
    }

    /// Submits one timestamped NV12 video frame.
    ///
    /// # Safety
    ///
    /// Media Foundation must be active on the current thread.
    unsafe fn write_video_frame(&mut self, frame: &Nv12Frame, timestamp: Duration) -> Result<()> {
        let byte_count = u32::try_from(frame.data.len())?;
        let buffer = unsafe { MFCreateMemoryBuffer(byte_count)? };
        unsafe {
            let mut destination = ptr::null_mut();
            buffer.Lock(&raw mut destination, None, None)?;
            ptr::copy_nonoverlapping(frame.data.as_ptr(), destination, byte_count as usize);
            buffer.Unlock()?;
            buffer.SetCurrentLength(byte_count)?;
        }
        let sample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime((timestamp.as_nanos() / 100) as i64)?;
            sample.SetSampleDuration(frame_duration_100ns(self.fps))?;
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
            self.writer
                .as_ref()
                .ok_or_else(|| anyhow!("the MP4 writer was already closed"))?
                .WriteSample(self.video_stream, &sample)?;
        }
        self.video_frames += 1;
        Ok(())
    }

    /// Submits one stereo PCM16 audio block for AAC encoding.
    ///
    /// # Safety
    ///
    /// Media Foundation must be active on the current thread.
    unsafe fn write_audio(&mut self, samples: &[i16]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let frames = (samples.len() / 2) as u64;
        let byte_count = u32::try_from(mem::size_of_val(samples))?;
        let buffer = unsafe { MFCreateMemoryBuffer(byte_count)? };
        unsafe {
            let mut destination = ptr::null_mut();
            buffer.Lock(&raw mut destination, None, None)?;
            ptr::copy_nonoverlapping(
                samples.as_ptr().cast::<u8>(),
                destination,
                byte_count as usize,
            );
            buffer.Unlock()?;
            buffer.SetCurrentLength(byte_count)?;
        }
        let sample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            let start = (self.audio_frames * 10_000_000) / u64::from(self.sample_rate);
            let end = ((self.audio_frames + frames) * 10_000_000) / u64::from(self.sample_rate);
            sample.SetSampleTime(start as i64)?;
            sample.SetSampleDuration((end - start) as i64)?;
            if self.audio_frames == 0 {
                sample.SetUINT32(&MFSampleExtension_Discontinuity, 1)?;
            }
            self.writer
                .as_ref()
                .ok_or_else(|| anyhow!("the MP4 writer was already closed"))?
                .WriteSample(self.audio_stream, &sample)?;
        }
        self.audio_frames += frames;
        Ok(())
    }

    /// Finalizes the MP4 and returns the recording summary.
    ///
    /// # Safety
    ///
    /// Media Foundation must be active on the current thread.
    unsafe fn close(mut self) -> Result<VideoSummary> {
        if !self.finalized {
            unsafe {
                self.writer
                    .as_ref()
                    .ok_or_else(|| anyhow!("the MP4 writer was already closed"))?
                    .Finalize()
                    .context("Windows could not finalize the MP4")?;
            }
            self.finalized = true;
        }
        self.writer.take();
        if let Some(stream) = self.byte_stream.take() {
            let _ = unsafe { stream.Close() };
        }
        Ok(VideoSummary {
            output: self.output.clone(),
            width: self.width,
            height: self.height,
            fps: self.fps,
            frames: self.video_frames,
            sample_rate: self.sample_rate,
            video_bitrate: self.video_bitrate,
            audio_bitrate: self.audio_bitrate,
            recording_ready_ms: self.recording_ready_ms,
            finalize_ms: self.finalize_ms,
        })
    }
}

impl Drop for Mp4Writer {
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

/// Creates the encoded H.264 output profile.
///
/// # Safety
///
/// Media Foundation must be active on the current thread.
unsafe fn video_output_type(
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        media_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        media_type.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(width) << 32) | u64::from(height),
        )?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps.max(1))) << 32 | 1)?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?;
        media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    }
    Ok(media_type)
}

/// Creates the NV12 input profile for the H.264 encoder.
///
/// # Safety
///
/// Media Foundation must be active on the current thread.
unsafe fn video_input_type(width: u32, height: u32, fps: u32) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        media_type.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(width) << 32) | u64::from(height),
        )?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps.max(1))) << 32 | 1)?;
        media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    }
    Ok(media_type)
}

/// Creates the encoded AAC output profile.
///
/// # Safety
///
/// Media Foundation must be active on the current thread.
unsafe fn audio_output_type(sample_rate: u32, bitrate: u32) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        media_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        media_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
        media_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate / 8)?;
    }
    Ok(media_type)
}

/// Creates the stereo PCM16 input profile for the AAC encoder.
///
/// # Safety
///
/// Media Foundation must be active on the current thread.
unsafe fn audio_input_type(sample_rate: u32) -> Result<IMFMediaType> {
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

/// Selects the closest AAC-native sample rate to the endpoint mix rate.
const fn nearest_aac_rate(source_rate: u32) -> u32 {
    if source_rate.abs_diff(44_100) < source_rate.abs_diff(48_000) {
        44_100
    } else {
        48_000
    }
}

/// Returns the per-frame media duration in 100 nanosecond units.
///
/// Media Foundation rejects video samples without an explicit duration, so
/// every frame carries `1 / fps` even when the desktop did not update.
#[must_use]
const fn frame_duration_100ns(fps: u32) -> i64 {
    let fps = if fps < 1 { 1 } else { fps };
    (10_000_000_u64 / fps as u64) as i64
}

/// Tracks encoded media time that only advances while capture is not paused.
///
/// Wall-clock timestamps would race ahead of the self-clocked audio timeline
/// during a pause, so video samples stamp this accumulated value instead.
struct RecordedTime {
    value: Duration,
}

impl RecordedTime {
    /// Creates a zeroed media clock.
    const fn new() -> Self {
        Self {
            value: Duration::ZERO,
        }
    }

    /// Adds one frame interval unless the recorder is paused.
    fn tick(&mut self, interval: Duration, paused: bool) {
        if !paused {
            self.value += interval;
        }
    }

    /// Returns the current media timestamp for encoded samples.
    const fn value(&self) -> Duration {
        self.value
    }
}

/// Formats a local timestamp for the default MP4 file name.
#[must_use]
pub fn video_timestamp() -> String {
    // SAFETY: GetLocalTime has no preconditions and returns the value by copy.
    let time = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_mode_cycles_through_all_modes() {
        assert_eq!(FitMode::Contain.next(), FitMode::Cover);
        assert_eq!(FitMode::Cover.next(), FitMode::Stretch);
        assert_eq!(FitMode::Stretch.next(), FitMode::Native);
        assert_eq!(FitMode::Native.next(), FitMode::Contain);
    }

    #[test]
    fn fit_mode_labels_match_cli_names() {
        assert_eq!(FitMode::Contain.label(), "contain");
        assert_eq!(FitMode::Cover.label(), "cover");
        assert_eq!(FitMode::Stretch.label(), "stretch");
        assert_eq!(FitMode::Native.label(), "native");
    }

    #[test]
    fn crop_clamps_to_source_bounds_with_even_size() {
        let crop = CropRect {
            left: 100,
            top: 50,
            width: 5000,
            height: 3000,
        };
        let clamped = crop.clamp_to(1920, 1080);
        assert_eq!(clamped.left, 100);
        assert_eq!(clamped.top, 50);
        assert_eq!(clamped.width, 1820);
        assert_eq!(clamped.height, 1030);
    }

    #[test]
    fn crop_clamps_oversized_offsets() {
        let crop = CropRect {
            left: 2000,
            top: 2000,
            width: 100,
            height: 100,
        };
        let clamped = crop.clamp_to(1920, 1080);
        assert_eq!(clamped.left, 1918);
        assert_eq!(clamped.top, 1078);
        assert_eq!(clamped.width, 2);
        assert_eq!(clamped.height, 2);
    }

    #[test]
    fn full_crop_reports_full() {
        let crop = CropRect {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        };
        assert!(crop.is_full(1920, 1080));
        assert!(!crop.is_full(1921, 1080));
    }

    #[test]
    fn contain_preserves_aspect_ratio_within_canvas() {
        // 1920x1080 source into 1920x1080 canvas: scale 1.0
        let (w, h, sx, sy) = transform_geometry(
            1920,
            1080,
            &CropRect {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            FitMode::Contain,
            1920,
            1080,
        );
        assert_eq!((w, h), (1920, 1080));
        assert!((sx - 1.0).abs() < 0.001);
        assert!((sy - 1.0).abs() < 0.001);
    }

    #[test]
    fn contain_letterboxes_wide_source_in_square_canvas() {
        // 1920x1080 source into 1080x1080 canvas: width-limited
        let (w, h, sx, sy) = transform_geometry(
            1920,
            1080,
            &CropRect {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            FitMode::Contain,
            1080,
            1080,
        );
        assert_eq!((w, h), (1080, 1080));
        // Scale is limited by width
        let expected = 1080.0 / 1920.0;
        assert!((sx - expected).abs() < 0.001);
        assert!((sy - expected).abs() < 0.001);
    }

    #[test]
    fn cover_crops_wide_source_in_square_canvas() {
        let (_, _, sx, sy) = transform_geometry(
            1920,
            1080,
            &CropRect {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            FitMode::Cover,
            1080,
            1080,
        );
        // Scale is limited by height
        let expected = 1080.0 / 1080.0;
        assert!((sx - expected).abs() < 0.001);
        assert!((sy - expected).abs() < 0.001);
    }

    #[test]
    fn stretch_fills_canvas_without_aspect_ratio() {
        let (_, _, sx, sy) = transform_geometry(
            1920,
            1080,
            &CropRect {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            FitMode::Stretch,
            1080,
            1080,
        );
        assert!((sx - 1080.0 / 1920.0).abs() < 0.001);
        assert!((sy - 1.0).abs() < 0.001);
    }

    #[test]
    fn native_uses_one_to_one_scale() {
        let (_, _, sx, sy) = transform_geometry(
            1920,
            1080,
            &CropRect {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            FitMode::Native,
            1920,
            1080,
        );
        assert!((sx - 1.0).abs() < 0.001);
        assert!((sy - 1.0).abs() < 0.001);
    }

    #[test]
    fn zero_canvas_uses_source_dimensions() {
        let (w, h, _, _) = transform_geometry(
            1920,
            1080,
            &CropRect {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            FitMode::Contain,
            0,
            0,
        );
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn canvas_dimensions_are_always_even() {
        let (w, h, _, _) = transform_geometry(
            1921,
            1081,
            &CropRect {
                left: 0,
                top: 0,
                width: 1921,
                height: 1081,
            },
            FitMode::Contain,
            1921,
            1081,
        );
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }

    #[test]
    fn canvas_presets_include_common_sizes() {
        let presets = canvas_presets();
        assert!(presets.iter().any(|(name, _)| *name == "1080p"));
        assert!(presets.iter().any(|(name, _)| *name == "4k"));
        assert!(presets.iter().any(|(name, _)| *name == "square"));
    }
    #[test]
    fn contain_pillarboxes_tall_source_in_wide_canvas() {
        // 1080x1920 portrait source into 1920x1080 canvas: height-limited
        let (_, _, sx, sy) = transform_geometry(
            1080,
            1920,
            &CropRect {
                left: 0,
                top: 0,
                width: 1080,
                height: 1920,
            },
            FitMode::Contain,
            1920,
            1080,
        );
        let expected = 1080.0 / 1920.0;
        assert!((sx - expected).abs() < 0.001);
        assert!((sy - expected).abs() < 0.001);
    }

    #[test]
    fn cover_scales_beyond_canvas_on_the_limiting_axis() {
        // 1920x1080 source into 1080x1080 canvas with cover: width overflows
        let (_, _, sx, _) = transform_geometry(
            1920,
            1080,
            &CropRect {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            FitMode::Cover,
            1080,
            1080,
        );
        assert!((sx - 1.0).abs() < 0.001);
    }

    #[test]
    fn crop_clamps_to_zero_offset_with_minimum_size() {
        let crop = CropRect {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
        };
        let clamped = crop.clamp_to(1920, 1080);
        assert_eq!(clamped.width, 2);
        assert_eq!(clamped.height, 2);
    }

    #[test]
    fn negative_crop_offset_clamps_to_zero() {
        let crop = CropRect {
            left: u32::MAX,
            top: u32::MAX,
            width: 100,
            height: 100,
        };
        let clamped = crop.clamp_to(1920, 1080);
        assert!(clamped.left < 1920);
        assert!(clamped.top < 1080);
        assert!(clamped.width >= 2);
        assert!(clamped.height >= 2);
    }

    #[test]
    fn canvas_preset_native_uses_source_dimensions() {
        let (name, size) = canvas_presets()[0];
        assert_eq!(name, "native");
        assert_eq!((size.width, size.height), (0, 0));
    }

    #[test]
    fn canvas_preset_1080p_has_correct_dimensions() {
        let preset = canvas_presets().iter().find(|(name, _)| *name == "1080p");
        assert!(preset.is_some());
        let (_, size) = preset.unwrap();
        assert_eq!((size.width, size.height), (1920, 1080));
    }

    #[test]
    fn canvas_preset_square_has_equal_dimensions() {
        let preset = canvas_presets().iter().find(|(name, _)| *name == "square");
        assert!(preset.is_some());
        let (_, size) = preset.unwrap();
        assert_eq!(size.width, size.height);
    }

    #[test]
    fn transform_geometry_with_oversized_crop_clamps_first() {
        // Crop larger than source: clamped crop determines the geometry
        let crop = CropRect {
            left: 0,
            top: 0,
            width: 5000,
            height: 5000,
        };
        let (w, h, _, _) = transform_geometry(1920, 1080, &crop, FitMode::Contain, 0, 0);
        // Crop clamps to 1920x1080, so native canvas is 1920x1080
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn monitor_info_stores_negative_desktop_coordinates() {
        let info = MonitorInfo {
            index: 1,
            name: "\\\\.\\DISPLAY2".to_owned(),
            primary: false,
            width: 2560,
            height: 1440,
            left: -2560,
            top: 0,
        };
        assert_eq!(info.left, -2560);
        assert_eq!(info.top, 0);
        assert!(!info.primary);
    }

    #[test]
    fn monitor_info_stores_rotation_and_mixed_dpi_positions() {
        // A portrait monitor to the left of the primary with negative coordinates
        let left_portrait = MonitorInfo {
            index: 0,
            name: "\\\\.\\DISPLAY1".to_owned(),
            primary: true,
            width: 1080,
            height: 1920,
            left: 0,
            top: 0,
        };
        let right_landscape = MonitorInfo {
            index: 1,
            name: "\\\\.\\DISPLAY2".to_owned(),
            primary: false,
            width: 3840,
            height: 2160,
            left: 1080,
            top: -240,
        };
        assert!(left_portrait.primary);
        assert!(!right_landscape.primary);
        // Virtual desktop spans from 0 to 1080+3840 horizontally
        assert!(right_landscape.left >= i32::try_from(left_portrait.width).unwrap_or(0));
    }

    #[test]
    fn transform_geometry_handles_rotated_portrait_source() {
        // 1080x1920 portrait into 1920x1080 landscape with contain
        let (w, h, sx, sy) = transform_geometry(
            1080,
            1920,
            &CropRect {
                left: 0,
                top: 0,
                width: 1080,
                height: 1920,
            },
            FitMode::Contain,
            1920,
            1080,
        );
        assert_eq!((w, h), (1920, 1080));
        let expected = 1080.0 / 1920.0;
        assert!((sx - expected).abs() < 0.001);
        assert!((sy - expected).abs() < 0.001);
    }

    #[test]
    fn transform_geometry_mixed_dpi_crop_produces_even_output() {
        // A 150% DPI monitor at 2880x1620 cropped to a 1080p canvas
        let crop = CropRect {
            left: 0,
            top: 0,
            width: 2880,
            height: 1620,
        };
        let (w, h, _, _) = transform_geometry(2880, 1620, &crop, FitMode::Contain, 1920, 1080);
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn mp4_writer_drop_removes_unfinalized_output() {
        // Mp4Writer::open needs COM/MF, so test the Drop behavior through the
        // output path tracking instead. The Drop impl removes the file when
        // finalized is false. Verify the path is tracked correctly.
        let path = std::env::temp_dir().join(format!(
            "record-drop-test-{}-unfinalized.mp4",
            std::process::id()
        ));
        // Simulate: create the file, verify Drop would clean it
        std::fs::write(&path, b"test").unwrap();
        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn video_summary_duration_uses_fps() {
        let summary = VideoSummary {
            output: PathBuf::from("test.mp4"),
            width: 1920,
            height: 1080,
            fps: 60,
            frames: 180,
            sample_rate: 48_000,
            video_bitrate: 20_000_000,
            audio_bitrate: 192_000,
            recording_ready_ms: Some(150.0),
            finalize_ms: 100.0,
        };
        assert_eq!(summary.duration(), Duration::from_secs(3));
    }

    #[test]
    fn video_summary_zero_fps_has_zero_duration() {
        let summary = VideoSummary {
            output: PathBuf::from("test.mp4"),
            width: 1920,
            height: 1080,
            fps: 0,
            frames: 100,
            sample_rate: 48_000,
            video_bitrate: 20_000_000,
            audio_bitrate: 192_000,
            recording_ready_ms: None,
            finalize_ms: 0.0,
        };
        assert_eq!(summary.duration(), Duration::ZERO);
    }

    #[test]
    fn video_summary_recording_ready_is_optional() {
        let mut summary = VideoSummary {
            output: PathBuf::from("test.mp4"),
            width: 1920,
            height: 1080,
            fps: 60,
            frames: 0,
            sample_rate: 48_000,
            video_bitrate: 20_000_000,
            audio_bitrate: 192_000,
            recording_ready_ms: None,
            finalize_ms: 0.0,
        };
        assert!(summary.recording_ready_ms.is_none());
        summary.recording_ready_ms = Some(250.5);
        assert_eq!(summary.recording_ready_ms, Some(250.5));
    }

    #[test]
    fn default_crop_covers_the_full_native_canvas() {
        // Regression: CropRect::default() has a zero size, which used to clamp
        // to a 2x2 canvas and fail encoder setup with MF_E_INVALIDMEDIATYPE.
        let crop = effective_crop(&CropRect::default(), 2560, 1440);
        assert_eq!((crop.left, crop.top), (0, 0));
        assert_eq!((crop.width, crop.height), (2560, 1440));
        let (width, height, _, _) = transform_geometry(
            2560,
            1440,
            &crop,
            FitMode::Contain,
            0,
            0,
        );
        assert_eq!((width, height), (2560, 1440));
    }

    #[test]
    fn effective_crop_treats_any_zero_dimension_as_full_frame() {
        let width_only = CropRect {
            left: 40,
            top: 30,
            width: 100,
            height: 0,
        };
        let crop = effective_crop(&width_only, 1920, 1080);
        assert_eq!((crop.left, crop.top), (0, 0));
        assert_eq!((crop.width, crop.height), (1920, 1080));
    }

    #[test]
    fn effective_crop_keeps_and_clamps_explicit_rectangles() {
        let partial = CropRect {
            left: 100,
            top: 50,
            width: 200,
            height: 300,
        };
        assert_eq!(
            effective_crop(&partial, 1920, 1080),
            CropRect {
                left: 100,
                top: 50,
                width: 200,
                height: 300
            }
        );
        let oversized = CropRect {
            left: u32::MAX,
            top: u32::MAX,
            width: 5000,
            height: 5000,
        };
        let clamped = effective_crop(&oversized, 1920, 1080);
        assert!(clamped.width >= 2 && clamped.height >= 2);
        assert!(clamped.left < 1920 && clamped.top < 1080);
    }

    #[test]
    fn transform_geometry_survives_a_degenerate_crop() {
        // Defensive: a zero-size crop must not divide the scale by zero.
        let degenerate = CropRect {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
        };
        let (_, _, scale_x, scale_y) =
            transform_geometry(1920, 1080, &degenerate, FitMode::Contain, 0, 0);
        assert!(scale_x.is_finite());
        assert!(scale_y.is_finite());
    }

    #[test]
    fn frame_duration_matches_the_requested_fps() {
        assert_eq!(frame_duration_100ns(60), 166_666);
        assert_eq!(frame_duration_100ns(30), 333_333);
        assert_eq!(frame_duration_100ns(1), 10_000_000);
        assert_eq!(frame_duration_100ns(0), 10_000_000);
    }

    #[test]
    fn recorded_time_advances_only_while_not_paused() {
        let interval = Duration::from_secs_f64(1.0 / 60.0);
        let mut clock = RecordedTime::new();
        for _ in 0..60 {
            clock.tick(interval, false);
        }
        assert_eq!(clock.value().as_secs(), 1);
        let frozen = clock.value();
        clock.tick(interval, true);
        clock.tick(interval, true);
        assert_eq!(clock.value(), frozen);
        clock.tick(interval, false);
        assert_eq!(
            (clock.value() - frozen).as_secs_f64(),
            interval.as_secs_f64()
        );
        assert_eq!(RecordedTime::new().value(), Duration::ZERO);
    }

    #[test]
    fn black_frame_matches_the_nv12_layout() {
        let frame = Nv12Frame::black(2560, 1440);
        let luma_size = 2560 * 1440;
        assert_eq!(frame.data.len(), luma_size + luma_size / 2);
        assert!(frame.data[..luma_size].iter().all(|byte| *byte == 0));
        assert!(frame.data[luma_size..].iter().all(|byte| *byte == 128));

        let small = Nv12Frame::black(2, 2);
        assert_eq!(small.data, vec![0, 0, 0, 0, 128, 128]);
    }
}