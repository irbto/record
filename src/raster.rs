//! Nonblocking terminal raster rendering through Sixel or another image protocol.

use std::{
    sync::Arc,
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use image::{DynamicImage, Rgba, RgbaImage, imageops};
use ratatui::{Frame, layout::Rect};
use ratatui_image::{
    Image, Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};

use crate::video::preview::PreviewFrame;

/// One frame queued for background resize and terminal-protocol encoding.
struct RasterRequest {
    serial: u64,
    frame: Arc<PreviewFrame>,
    width: u16,
    height: u16,
}

/// One fully encoded terminal image returned to the drawing thread.
struct RasterResponse {
    serial: u64,
    width: u16,
    height: u16,
    protocol: Protocol,
}

/// Owns a bounded raster encoder and the newest inexpensive render protocol.
pub(crate) struct RasterRenderer {
    requests: Sender<RasterRequest>,
    responses: Receiver<Result<RasterResponse, String>>,
    worker: Option<JoinHandle<()>>,
    latest: Option<Arc<PreviewFrame>>,
    latest_serial: u64,
    requested_serial: u64,
    requested_size: (u16, u16),
    in_flight: bool,
    protocol: Option<Protocol>,
    error: Option<String>,
}

impl RasterRenderer {
    /// Queries the active terminal and starts the bounded image encoder.
    pub(crate) fn start() -> Result<Self> {
        let picker = Picker::from_query_stdio().context("could not query terminal graphics")?;
        if matches!(picker.protocol_type(), ProtocolType::Halfblocks) {
            bail!(
                "record video needs a terminal raster protocol; Windows Terminal 1.22 or newer supports Sixel"
            );
        }

        let (request_sender, requests) = bounded::<RasterRequest>(1);
        let (responses, response_receiver) = bounded::<Result<RasterResponse, String>>(1);
        let worker = thread::Builder::new()
            .name("record-raster-preview".to_owned())
            .spawn(move || raster_worker(picker, &requests, &responses))
            .context("could not start the terminal raster encoder")?;
        Ok(Self {
            requests: request_sender,
            responses: response_receiver,
            worker: Some(worker),
            latest: None,
            latest_serial: 0,
            requested_serial: 0,
            requested_size: (0, 0),
            in_flight: false,
            protocol: None,
            error: None,
        })
    }

    /// Replaces any frame that has not yet entered raster encoding.
    pub(crate) fn set_frame(&mut self, frame: PreviewFrame) {
        self.latest_serial = self.latest_serial.wrapping_add(1);
        self.latest = Some(Arc::new(frame));
    }

    /// Draws the latest encoded raster and schedules fresher work when needed.
    pub(crate) fn render(&mut self, frame: &mut Frame, area: Rect) -> bool {
        self.drain_responses();
        self.request_latest(area);
        let Some(protocol) = &self.protocol else {
            return false;
        };
        frame.render_widget(Image::new(protocol), area);
        true
    }

    /// Returns the most recent worker failure, if any.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn drain_responses(&mut self) {
        for response in self.responses.try_iter() {
            self.in_flight = false;
            match response {
                Ok(response) => {
                    if response.serial >= self.requested_serial {
                        self.requested_serial = response.serial;
                        self.requested_size = (response.width, response.height);
                        self.protocol = Some(response.protocol);
                        self.error = None;
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn request_latest(&mut self, area: Rect) {
        if self.in_flight || area.is_empty() {
            return;
        }
        let needs_frame = self.requested_serial != self.latest_serial;
        let needs_resize = self.requested_size != (area.width, area.height);
        if !needs_frame && !needs_resize {
            return;
        }
        let Some(frame) = &self.latest else {
            return;
        };
        let request = RasterRequest {
            serial: self.latest_serial,
            frame: Arc::clone(frame),
            width: area.width,
            height: area.height,
        };
        if self.requests.try_send(request).is_ok() {
            self.in_flight = true;
        }
    }
}

impl Drop for RasterRenderer {
    fn drop(&mut self) {
        // Replacing the sender closes the worker's receive loop before joining.
        let (closed_sender, closed_receiver) = bounded(0);
        drop(closed_receiver);
        let sender = std::mem::replace(&mut self.requests, closed_sender);
        drop(sender);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn raster_worker(
    picker: Picker,
    requests: &Receiver<RasterRequest>,
    responses: &Sender<Result<RasterResponse, String>>,
) {
    while let Ok(request) = requests.recv() {
        let result = encode_request(&picker, request).map_err(|error| format!("{error:#}"));
        let _ = responses.try_send(result);
    }
}

fn encode_request(picker: &Picker, request: RasterRequest) -> Result<RasterResponse> {
    let image = letterbox_frame(
        &request.frame,
        u32::from(request.width) * u32::from(picker.font_size().width),
        u32::from(request.height) * u32::from(picker.font_size().height),
    )?;
    let protocol = picker
        .new_protocol(
            image,
            (request.width, request.height).into(),
            Resize::Fit(None),
        )
        .context("could not encode terminal raster frame")?;
    Ok(RasterResponse {
        serial: request.serial,
        width: request.width,
        height: request.height,
        protocol,
    })
}

/// Converts BGRA capture bytes into a centered, opaque RGBA terminal canvas.
fn letterbox_frame(frame: &PreviewFrame, width: u32, height: u32) -> Result<DynamicImage> {
    if frame.width == 0 || frame.height == 0 || width == 0 || height == 0 {
        bail!("terminal raster frame has an empty size");
    }
    let expected = frame.width as usize * frame.height as usize * 4;
    if frame.bgra.len() != expected {
        bail!("terminal raster frame has invalid BGRA storage");
    }
    let mut rgba = frame.bgra.clone();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    let source = RgbaImage::from_raw(frame.width, frame.height, rgba)
        .context("could not create terminal raster image")?;
    let resized = DynamicImage::ImageRgba8(source).resize(
        width,
        height,
        image::imageops::FilterType::Triangle,
    );
    let mut canvas = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 255]));
    let left = i64::from(width.saturating_sub(resized.width()) / 2);
    let top = i64::from(height.saturating_sub(resized.height()) / 2);
    imageops::overlay(&mut canvas, &resized, left, top);
    Ok(DynamicImage::ImageRgba8(canvas))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn letterbox_preserves_color_and_centers_wide_frames() {
        let mut bgra = Vec::new();
        for _ in 0..8 {
            bgra.extend_from_slice(&[20, 40, 200, 255]);
        }
        let frame = PreviewFrame {
            bgra,
            width: 4,
            height: 2,
            time: Duration::ZERO,
        };
        let image = letterbox_frame(&frame, 8, 8).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(4, 4).0, [200, 40, 20, 255]);
        assert_eq!(image.get_pixel(4, 0).0, [0, 0, 0, 255]);
        assert_eq!(image.get_pixel(4, 7).0, [0, 0, 0, 255]);
    }

    #[test]
    fn letterbox_rejects_invalid_storage() {
        let frame = PreviewFrame {
            bgra: vec![0; 7],
            width: 2,
            height: 1,
            time: Duration::ZERO,
        };
        assert!(letterbox_frame(&frame, 8, 4).is_err());
    }
}
