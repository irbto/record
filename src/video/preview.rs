//! Native MP4 frame decoding for raster previews and frame selection.
//!
//! A helper thread opens the finalized MP4 with the native Media Foundation
//! Source Reader, decodes H.264 pictures to RGB, downsamples them to a
//! bounded size, and ships them across a bounded channel. The terminal
//! thread never blocks on decode work; the raster module presents these
//! pictures through the terminal's graphics protocol.

use std::{
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr, thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            self as mf, IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
            MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READER_ALL_STREAMS,
            MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
            MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM, MFCreateAttributes,
            MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Video, MFVideoFormat_RGB32,
        },
        System::{Com::StructuredStorage::PROPVARIANT, Variant::VT_I8},
    },
    core::{GUID, PCWSTR},
};

use crate::audio::windows::{ComGuard, MediaFoundationGuard};

/// The longest frame edge that the worker keeps after downsampling a decoded
/// picture. Terminal panels are far smaller than source captures, so this cap
/// bounds channel traffic without visible loss after the final cell sampling.
const MAX_PREVIEW_EDGE: u32 = 960;

/// How long the worker waits for another command before rechecking its state.
const IDLE_POLL: Duration = Duration::from_millis(250);

/// Describes the stream that the decoder reports after opening one MP4.
#[derive(Clone, Copy, Debug)]
pub struct PreviewFormat {
    /// Encoded frame width in pixels.
    pub width: u32,
    /// Encoded frame height in pixels.
    pub height: u32,
    /// Declared frames per second, falling back to 30 when unspecified.
    pub fps: f64,
    /// Total media duration.
    pub duration: Duration,
}

/// Contains one downsampled BGRA picture ready for raster rendering.
pub struct PreviewFrame {
    /// Packed BGRA bytes in top-down rows of exactly `width` pixels.
    pub bgra: Vec<u8>,
    /// Picture width in pixels after downsampling.
    pub width: u32,
    /// Picture height in pixels after downsampling.
    pub height: u32,
    /// Presentation timestamp inside the source MP4.
    pub time: Duration,
}

/// A low-frequency command from the interface to the decoder thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewCommand {
    /// Decode the first picture at or after this media time.
    Seek(Duration),
    /// Decode the next sequential picture after the last delivered one.
    Step,
    /// Stop the worker and release the file.
    Close,
}

/// An event from the decoder thread to the interface.
pub enum PreviewEvent {
    /// The file opened successfully and describes its stream.
    Ready(PreviewFormat),
    /// One decoded picture.
    Frame(Box<PreviewFrame>),
    /// The stream reached its end.
    End,
    /// The worker stopped with an error message for the interface.
    Failed(String),
}

/// Spawns the decoder thread that owns every COM and Media Foundation object
/// for one MP4 file. Dropping the command sender stops the worker.
pub fn spawn_preview_worker(
    path: PathBuf,
) -> Result<(
    Sender<PreviewCommand>,
    Receiver<PreviewEvent>,
    thread::JoinHandle<()>,
)> {
    let (commands, command_receiver) = bounded(8);
    let (event_sender, events) = bounded(2);
    let worker = thread::Builder::new()
        .name("record-video-preview".to_owned())
        .spawn(move || run_reader(&path, &command_receiver, &event_sender))
        .context("could not start the video preview thread")?;
    Ok((commands, events, worker))
}

/// Opens the file, announces its format, and serves commands until close.
fn run_reader(path: &Path, commands: &Receiver<PreviewCommand>, events: &Sender<PreviewEvent>) {
    let outcome = run_reader_inner(path, commands, events);
    if let Err(error) = outcome {
        let _ = events.try_send(PreviewEvent::Failed(format!("{error:#}")));
    }
}

/// Runs the decoder loop and translates any failure into a `Failed` event.
fn run_reader_inner(
    path: &Path,
    commands: &Receiver<PreviewCommand>,
    events: &Sender<PreviewEvent>,
) -> Result<()> {
    let _com = ComGuard::new()?;
    let _media_foundation = MediaFoundationGuard::new()?;
    let mut reader = unsafe { FrameReader::open(path)? };
    let _ = events.try_send(PreviewEvent::Ready(reader.format()));
    loop {
        let command = match commands.recv_timeout(IDLE_POLL) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match command {
            PreviewCommand::Close => break,
            PreviewCommand::Seek(time) => {
                deliver_picture(events, unsafe { reader.seek(time) })?;
            }
            PreviewCommand::Step => {
                deliver_picture(events, unsafe { reader.read_next() })?;
            }
        }
    }
    Ok(())
}

/// Sends one decode result and surfaces hard failures to the caller.
fn deliver_picture(
    events: &Sender<PreviewEvent>,
    picture: Result<Option<PreviewFrame>>,
) -> Result<()> {
    match picture {
        Ok(Some(frame)) => {
            let _ = events.try_send(PreviewEvent::Frame(Box::new(frame)));
            Ok(())
        }
        Ok(None) => {
            let _ = events.try_send(PreviewEvent::End);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Owns the native source reader and the decoded stream description.
struct FrameReader {
    reader: IMFSourceReader,
    width: u32,
    height: u32,
    fps: f64,
    duration: Duration,
    stride: usize,
    bottom_up: bool,
    ended: bool,
}

impl FrameReader {
    /// Opens the MP4 and selects decoded RGB output for the video stream.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the calling thread.
    unsafe fn open(path: &Path) -> Result<Self> {
        let absolute = std::path::absolute(path)
            .with_context(|| format!("could not resolve {}", path.display()))?;
        let wide: Vec<u16> = absolute.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut attributes_slot = None;
        unsafe { MFCreateAttributes(&mut attributes_slot, 4)? };
        let attributes = attributes_slot.context("could not create Media Foundation attributes")?;
        unsafe {
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        }
        let reader = unsafe { MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attributes) }
            .with_context(|| format!("could not open {}", path.display()))?;
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)?;
        }

        let native =
            unsafe { reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)? };
        let packed_size = unsafe { native.GetUINT64(&MF_MT_FRAME_SIZE) }
            .context("the video stream has no frame size")?;
        let width = (packed_size >> 32) as u32;
        let height = packed_size as u32;
        if width == 0 || height == 0 {
            anyhow::bail!("the video stream has an empty frame size");
        }
        let fps = unsafe { native.GetUINT64(&MF_MT_FRAME_RATE) }.map_or(30.0, |packed| {
            let numerator = (packed >> 32) as f64;
            let denominator = (packed & 0xFFFF_FFFF) as f64;
            if denominator > 0.0 {
                numerator / denominator
            } else {
                30.0
            }
        });

        let output = unsafe { MFCreateMediaType() }?;
        unsafe {
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                None,
                &output,
            )?;
        }
        // RGB media types may use a negative default stride, which stores the
        // rows bottom-up. Honor whatever stride the negotiated type declares.
        let current =
            unsafe { reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)? };
        let declared_stride = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
            .ok()
            .map(|stride| stride as i32)
            .unwrap_or(width as i32 * 4);
        let bottom_up = declared_stride < 0;
        let stride = declared_stride.unsigned_abs() as usize;

        let duration = unsafe {
            reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
        }
        .ok()
        .and_then(propvariant_duration)
        .unwrap_or(Duration::ZERO);

        Ok(Self {
            reader,
            width,
            height,
            fps,
            duration,
            stride,
            bottom_up,
            ended: false,
        })
    }

    /// Returns the announced stream description.
    fn format(&self) -> PreviewFormat {
        PreviewFormat {
            width: self.width,
            height: self.height,
            fps: self.fps,
            duration: self.duration,
        }
    }

    /// Decodes the first picture at or after `time`.
    ///
    /// Seeking lands on a keyframe at or before the target and then decodes
    /// forward until the timestamps reach it, which keeps scrubbing exact.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the calling thread.
    unsafe fn seek(&mut self, time: Duration) -> Result<Option<PreviewFrame>> {
        let mut position = PROPVARIANT::default();
        // SAFETY: Each write touches one union field that matches the variant
        // tag set immediately before it. The explicit deref bypasses the
        // ManuallyDrop wrapper without running any destructor.
        unsafe {
            (*position.Anonymous.Anonymous).vt = VT_I8;
            (*position.Anonymous.Anonymous).Anonymous.hVal = hundred_nanos(time);
            self.reader.SetCurrentPosition(&GUID::zeroed(), &position)?;
        }
        self.ended = false;
        loop {
            let Some(frame) = (unsafe { self.read_one()? }) else {
                return Ok(None);
            };
            if frame.time >= time {
                return Ok(Some(frame));
            }
        }
    }

    /// Decodes the next sequential picture.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the calling thread.
    unsafe fn read_next(&mut self) -> Result<Option<PreviewFrame>> {
        unsafe { self.read_one() }
    }

    /// Reads one sample, skipping gaps until a picture or the end arrives.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the calling thread.
    unsafe fn read_one(&mut self) -> Result<Option<PreviewFrame>> {
        while !self.ended {
            let mut flags: u32 = 0;
            let mut timestamp: i64 = 0;
            let mut sample: Option<mf::IMFSample> = None;
            unsafe {
                self.reader.ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.ended = true;
            }
            let Some(sample) = sample else {
                continue;
            };
            let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
            let mut data = ptr::null_mut::<u8>();
            let mut length: u32 = 0;
            unsafe { buffer.Lock(&mut data, None, Some(&mut length))? };
            let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec();
            unsafe { buffer.Unlock()? };
            let top_down =
                copy_top_down(&bytes, self.width, self.height, self.stride, self.bottom_up);
            let bgra = resize_bgra_to_preview(&top_down, self.width, self.height);
            let time = Duration::from_nanos(timestamp.max(0) as u64 * 100);
            return Ok(Some(PreviewFrame {
                bgra,
                width: preview_width(self.width, self.height),
                height: preview_height(self.width, self.height),
                time,
            }));
        }
        Ok(None)
    }
}

/// Packs a duration as the 100-nanosecond units that Media Foundation uses.
fn hundred_nanos(time: Duration) -> i64 {
    i64::try_from(time.as_nanos() / 100).unwrap_or(i64::MAX)
}

/// Reads a 100-nanosecond duration out of a presentation attribute.
fn propvariant_duration(value: PROPVARIANT) -> Option<Duration> {
    // SAFETY: The first union read selects the variant tag; the second reads
    // the integer field only when that tag identifies an 8-bit signed value.
    let raw = unsafe {
        if value.Anonymous.Anonymous.vt != VT_I8 {
            return None;
        }
        value.Anonymous.Anonymous.Anonymous.hVal
    };
    u64::try_from(raw)
        .ok()
        .and_then(|hundred_ns| hundred_ns.checked_mul(100))
        .map(Duration::from_nanos)
}

/// Copies packed BGRA rows into a contiguous top-down layout.
///
/// RGB media types may report a negative stride, which flips the row order.
/// Rows shorter than the stride are padded toward the right edge so the
/// sampler never reads across rows.
fn copy_top_down(
    source: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    bottom_up: bool,
) -> Vec<u8> {
    let row_bytes = width as usize * 4;
    if stride < row_bytes || source.len() < stride * height.max(1) as usize {
        return source.to_vec();
    }
    let mut top_down = vec![0_u8; row_bytes * height as usize];
    for row in 0..height as usize {
        let source_row = if bottom_up {
            height as usize - 1 - row
        } else {
            row
        };
        let start = source_row * stride;
        top_down[row * row_bytes..(row + 1) * row_bytes]
            .copy_from_slice(&source[start..start + row_bytes]);
    }
    top_down
}

/// Returns the downsampled preview width for a source size.
fn preview_width(source_width: u32, source_height: u32) -> u32 {
    let longest = source_width.max(source_height);
    if longest <= MAX_PREVIEW_EDGE || longest == 0 {
        return source_width;
    }
    let scale = f64::from(MAX_PREVIEW_EDGE) / f64::from(longest);
    ((f64::from(source_width) * scale).round() as u32).max(1)
}

/// Returns the downsampled preview height for a source size.
fn preview_height(source_width: u32, source_height: u32) -> u32 {
    let longest = source_width.max(source_height);
    if longest <= MAX_PREVIEW_EDGE || longest == 0 {
        return source_height;
    }
    let scale = f64::from(MAX_PREVIEW_EDGE) / f64::from(longest);
    ((f64::from(source_height) * scale).round() as u32).max(1)
}

/// Box-filter resamples packed BGRA pixels to `target_width` by
/// `target_height`, averaging every source block behind one target pixel.
///
/// This runs twice per displayed picture: once in the worker against the
/// capture resolution, and once in the widget against the terminal grid.
pub(crate) fn resize_bgra(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    let sw = usize::try_from(source_width.max(1)).unwrap_or(1);
    let sh = usize::try_from(source_height.max(1)).unwrap_or(1);
    let tw = usize::try_from(target_width.max(1)).unwrap_or(1);
    let th = usize::try_from(target_height.max(1)).unwrap_or(1);
    let mut target = vec![0_u8; tw * th * 4];
    for ty in 0..th {
        let y_start = ty * sh / th;
        let y_end = ((ty + 1) * sh / th).max(y_start + 1);
        for tx in 0..tw {
            let x_start = tx * sw / tw;
            let x_end = ((tx + 1) * sw / tw).max(x_start + 1);
            let (mut blue, mut green, mut red) = (0_usize, 0_usize, 0_usize);
            let mut count = 0_usize;
            for sy in y_start..y_end.min(sh) {
                let row = sy * sw * 4;
                for sx in x_start..x_end.min(sw) {
                    let offset = row + sx * 4;
                    blue += usize::from(source[offset]);
                    green += usize::from(source[offset + 1]);
                    red += usize::from(source[offset + 2]);
                    count += 1;
                }
            }
            if count == 0 {
                continue;
            }
            let offset = (ty * tw + tx) * 4;
            target[offset] = ((blue + count / 2) / count) as u8;
            target[offset + 1] = ((green + count / 2) / count) as u8;
            target[offset + 2] = ((red + count / 2) / count) as u8;
            target[offset + 3] = 255;
        }
    }
    target
}

/// Downscales one full-resolution picture to the bounded preview size.
fn resize_bgra_to_preview(source: &[u8], width: u32, height: u32) -> Vec<u8> {
    resize_bgra(
        source,
        width,
        height,
        preview_width(width, height),
        preview_height(width, height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Mp4Writer, Nv12Frame, check_video_support};

    #[test]
    fn resize_keeps_a_uniform_color_uniform() {
        let mut source = Vec::with_capacity(40 * 30 * 4);
        for _ in 0..40 * 30 {
            source.extend_from_slice(&[90_u8, 160, 20, 255]);
        }
        let resized = resize_bgra(&source, 40, 30, 10, 10);
        for pixel in resized.as_chunks::<4>().0 {
            assert_eq!(pixel, &[90, 160, 20, 255]);
        }
    }

    #[test]
    fn resize_averages_the_source_block_behind_one_pixel() {
        // Top-left red, top-right green, bottom-left blue, bottom-right white.
        let mut source = Vec::new();
        source.extend_from_slice(&[0, 0, 255, 255]);
        source.extend_from_slice(&[0, 255, 0, 255]);
        source.extend_from_slice(&[255, 0, 0, 255]);
        source.extend_from_slice(&[255, 255, 255, 255]);
        let resized = resize_bgra(&source, 2, 2, 1, 1);
        assert_eq!(&resized[0..3], &[128, 128, 128]);
        assert_eq!(resized[3], 255);
    }

    #[test]
    fn propvariant_duration_reads_signed_hundred_nanoseconds() {
        let mut value = PROPVARIANT::default();
        // SAFETY: The union write matches the variant tag read by the helper.
        unsafe {
            (*value.Anonymous.Anonymous).vt = VT_I8;
            (*value.Anonymous.Anonymous).Anonymous.hVal = 30_000_000;
        }
        assert_eq!(propvariant_duration(value), Some(Duration::from_secs(3)));
        let mut empty = PROPVARIANT::default();
        // SAFETY: The union write matches the variant tag read by the helper.
        unsafe {
            (*empty.Anonymous.Anonymous).vt = windows::Win32::System::Variant::VT_EMPTY;
        }
        assert_eq!(propvariant_duration(empty), None);
    }

    #[test]
    fn copy_top_down_flips_bottom_up_rows_and_pads_stride() {
        let width = 2;
        let height = 2;
        let stride = 12_usize;
        let row = |bytes: [u8; 8]| {
            let mut padded = bytes.to_vec();
            padded.resize(stride, 0xEE);
            padded
        };
        let top_row = row([1, 1, 1, 1, 1, 1, 1, 1]);
        let bottom_row = row([9, 9, 9, 9, 9, 9, 9, 9]);
        let mut source = bottom_row.clone();
        source.extend(top_row.clone());
        let flipped = copy_top_down(&source, width, height, stride, true);
        assert_eq!(&flipped[..8], &[1_u8; 8]);
        assert_eq!(&flipped[8..16], &[9_u8; 8]);
        let straight = copy_top_down(
            &(top_row
                .iter()
                .chain(bottom_row.iter())
                .copied()
                .collect::<Vec<_>>()),
            width,
            height,
            stride,
            false,
        );
        assert_eq!(&straight[..8], &[1_u8; 8]);
        assert_eq!(&straight[8..16], &[9_u8; 8]);
    }

    #[cfg(windows)]
    #[test]
    fn opens_a_recorded_mp4_and_decodes_its_first_picture() {
        if check_video_support().is_err() {
            return;
        }
        // The writer and reader need COM and Media Foundation on this thread,
        // exactly like the production capture path.
        let _com = crate::audio::windows::ComGuard::new().unwrap();
        let _media_foundation = crate::audio::windows::MediaFoundationGuard::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.mp4");
        // SAFETY: COM and Media Foundation are initialized inside the call
        // sites that Mp4Writer documents; this test runs them sequentially.
        let mut writer = unsafe {
            Mp4Writer::open(
                &path, 64, 48, 30, 2_000_000, 48_000, 192_000, true, None, 0.0,
            )
        }
        .unwrap();
        for index in 0..4_u32 {
            let frame = Nv12Frame::black(64, 48);
            // SAFETY: The writer owns live Media Foundation objects.
            unsafe {
                writer
                    .write_video_frame(&frame, Duration::from_millis(u64::from(index) * 33))
                    .unwrap();
            }
        }
        // SAFETY: Finalizes the sink exactly once.
        unsafe { writer.close().unwrap() };

        let (commands, events, worker) = spawn_preview_worker(path).unwrap();
        let mut format = None;
        while format.is_none() {
            match events.recv_timeout(Duration::from_secs(10)).unwrap() {
                PreviewEvent::Ready(announced) => format = Some(announced),
                PreviewEvent::Failed(message) => panic!("decoder failed: {message}"),
                _ => {}
            }
        }
        let announced = format.unwrap();
        assert_eq!(announced.width, 64);
        assert_eq!(announced.height, 48);

        commands.send(PreviewCommand::Seek(Duration::ZERO)).unwrap();
        loop {
            match events.recv_timeout(Duration::from_secs(10)).unwrap() {
                PreviewEvent::Frame(frame) => {
                    assert_eq!(frame.width, 64);
                    assert_eq!(frame.height, 48);
                    assert_eq!(frame.bgra.len(), 64 * 48 * 4);
                    let pixels = frame.bgra.as_chunks::<4>().0;
                    let average: usize = pixels.iter().map(|pixel| usize::from(pixel[2])).sum();
                    assert!(average / pixels.len() < 24, "black frame stayed dark");
                    break;
                }
                PreviewEvent::Failed(message) => panic!("decoder failed: {message}"),
                _ => {}
            }
        }
        commands.send(PreviewCommand::Close).unwrap();
        worker.join().unwrap();
    }
}
