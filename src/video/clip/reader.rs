//! Media Foundation source-reader setup and decoded sample access.

use std::{path::Path, ptr, time::Duration};

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFSourceReader, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
            MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
            MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
            MF_MT_SUBTYPE, MF_SOURCE_READER_ALL_STREAMS,
            MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
            MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MFAudioFormat_PCM,
            MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
            MFMediaType_Video, MFVideoFormat_NV12,
        },
        System::{Com::StructuredStorage::PROPVARIANT, Variant::VT_I8},
    },
    core::{GUID, PCWSTR},
};

use super::{hundred_nanos, wide_path};
use crate::video::VideoSummary;

/// One decoded Media Foundation sample and its original presentation time.
pub(super) struct DecodedSample {
    pub(super) timestamp: i64,
    pub(super) bytes: Vec<u8>,
}

/// Owns the two decoded streams selected from a finished recording.
pub(super) struct ClipReader {
    reader: IMFSourceReader,
    pub(super) video_stride: usize,
}

impl ClipReader {
    /// Opens H.264 as NV12 and AAC as stereo PCM16.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the current thread.
    pub(super) unsafe fn open(path: &Path, summary: &VideoSummary) -> Result<Self> {
        let absolute = std::path::absolute(path)
            .with_context(|| format!("could not resolve {}", path.display()))?;
        let wide = wide_path(&absolute);
        let mut attributes_slot = None;
        unsafe { MFCreateAttributes(&mut attributes_slot, 4)? };
        let attributes = attributes_slot.context("could not create Media Foundation attributes")?;
        unsafe {
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        }
        let reader = unsafe { MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attributes) }
            .with_context(|| format!("could not open {} for trimming", path.display()))?;
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)?;
            reader.SetStreamSelection(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, true)?;
        }

        let video = unsafe { MFCreateMediaType()? };
        unsafe {
            video.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            video.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            video.SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(summary.width) << 32) | u64::from(summary.height),
            )?;
            video.SetUINT64(&MF_MT_FRAME_RATE, u64::from(summary.fps.max(1)) << 32 | 1)?;
            reader.SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                None,
                &video,
            )?;
        }
        let current_video =
            unsafe { reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)? };
        let video_stride = unsafe { current_video.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
            .map_or(summary.width as usize, |stride| stride as usize);

        let audio = unsafe { MFCreateMediaType()? };
        unsafe {
            audio.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            audio.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            audio.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            audio.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, summary.sample_rate)?;
            audio.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            audio.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            audio.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, summary.sample_rate * 4)?;
            reader.SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                None,
                &audio,
            )?;
        }
        Ok(Self {
            reader,
            video_stride,
        })
    }

    /// Seeks both selected streams to the requested media time.
    pub(super) unsafe fn seek(&mut self, time: Duration) -> Result<()> {
        let mut position = PROPVARIANT::default();
        unsafe {
            (*position.Anonymous.Anonymous).vt = VT_I8;
            (*position.Anonymous.Anonymous).Anonymous.hVal = hundred_nanos(time);
            self.reader.SetCurrentPosition(&GUID::zeroed(), &position)?;
        }
        Ok(())
    }

    /// Reads the next decoded NV12 sample.
    pub(super) unsafe fn read_video(&mut self) -> Result<Option<DecodedSample>> {
        unsafe { self.read(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) }
    }

    /// Reads the next decoded PCM sample.
    pub(super) unsafe fn read_audio(&mut self) -> Result<Option<DecodedSample>> {
        unsafe { self.read(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32) }
    }

    /// Reads through stream ticks until a sample or end-of-stream flag arrives.
    unsafe fn read(&mut self, stream: u32) -> Result<Option<DecodedSample>> {
        loop {
            let mut flags = 0_u32;
            let mut timestamp = 0_i64;
            let mut sample = None;
            unsafe {
                self.reader.ReadSample(
                    stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )?;
            }
            if let Some(sample) = sample {
                let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
                let mut data = ptr::null_mut();
                let mut length = 0_u32;
                unsafe { buffer.Lock(&mut data, None, Some(&mut length))? };
                let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec();
                unsafe { buffer.Unlock()? };
                return Ok(Some(DecodedSample { timestamp, bytes }));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
        }
    }
}
