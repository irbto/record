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
    edit_overlay: Option<RasterEditOverlay>,
}

/// One fully encoded terminal image returned to the drawing thread.
struct RasterResponse {
    serial: u64,
    width: u16,
    height: u16,
    edit_overlay: Option<RasterEditOverlay>,
    protocol: Protocol,
}

/// Identifies one crop guide that receives the active amber highlight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CropEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Describes inward crop margins as fractions of the complete source frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RasterEditOverlay {
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) top: f32,
    pub(crate) bottom: f32,
    pub(crate) active: Option<CropEdge>,
    pub(crate) show_crop: bool,
    pub(crate) video_gain: f32,
}

impl Default for RasterEditOverlay {
    fn default() -> Self {
        Self {
            left: 0.0,
            right: 0.0,
            top: 0.0,
            bottom: 0.0,
            active: None,
            show_crop: false,
            video_gain: 1.0,
        }
    }
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
    requested_edit_overlay: Option<RasterEditOverlay>,
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
            requested_edit_overlay: None,
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
        self.render_with_edit_overlay(frame, area, None)
    }

    /// Draws the latest raster with an optional pixel-aligned crop guide.
    pub(crate) fn render_with_edit_overlay(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        edit_overlay: Option<RasterEditOverlay>,
    ) -> bool {
        self.drain_responses();
        self.request_latest(area, edit_overlay);
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
                        self.requested_edit_overlay = response.edit_overlay;
                        self.protocol = Some(response.protocol);
                        self.error = None;
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn request_latest(&mut self, area: Rect, edit_overlay: Option<RasterEditOverlay>) {
        if self.in_flight || area.is_empty() {
            return;
        }
        let needs_frame = self.requested_serial != self.latest_serial;
        let needs_resize = self.requested_size != (area.width, area.height);
        let needs_edit_overlay = self.requested_edit_overlay != edit_overlay;
        if !needs_frame && !needs_resize && !needs_edit_overlay {
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
            edit_overlay,
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
        request.edit_overlay,
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
        edit_overlay: request.edit_overlay,
        protocol,
    })
}

/// Converts BGRA capture bytes into a centered, opaque RGBA terminal canvas.
fn letterbox_frame(
    frame: &PreviewFrame,
    width: u32,
    height: u32,
    edit_overlay: Option<RasterEditOverlay>,
) -> Result<DynamicImage> {
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
    let mut resized = DynamicImage::ImageRgba8(source)
        .resize(width, height, image::imageops::FilterType::Triangle)
        .to_rgba8();
    if let Some(edit_overlay) = edit_overlay {
        apply_video_gain(&mut resized, edit_overlay.video_gain);
        if edit_overlay.show_crop {
            draw_crop_overlay(&mut resized, edit_overlay);
        }
    }
    let mut canvas = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 255]));
    let left = i64::from(width.saturating_sub(resized.width()) / 2);
    let top = i64::from(height.saturating_sub(resized.height()) / 2);
    imageops::overlay(&mut canvas, &resized, left, top);
    Ok(DynamicImage::ImageRgba8(canvas))
}

/// Applies the video fade envelope before crop guides are drawn on top.
fn apply_video_gain(image: &mut RgbaImage, gain: f32) {
    let gain = if gain.is_finite() {
        gain.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if gain >= 1.0 {
        return;
    }
    for pixel in image.pixels_mut() {
        for channel in &mut pixel.0[..3] {
            *channel = (f32::from(*channel) * gain).round() as u8;
        }
    }
}

/// Dims discarded pixels and outlines crop boundaries on the resized picture.
fn draw_crop_overlay(image: &mut RgbaImage, overlay: RasterEditOverlay) {
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return;
    }
    let left = fraction_pixel(overlay.left, width);
    let right = width
        .saturating_sub(fraction_pixel(overlay.right, width))
        .max(left.saturating_add(1));
    let top = fraction_pixel(overlay.top, height);
    let bottom = height
        .saturating_sub(fraction_pixel(overlay.bottom, height))
        .max(top.saturating_add(1));

    for (x, y, pixel) in image.enumerate_pixels_mut() {
        if x < left || x >= right || y < top || y >= bottom {
            pixel.0[0] /= 4;
            pixel.0[1] /= 4;
            pixel.0[2] /= 4;
        }
    }

    let inactive = Rgba([34, 211, 238, 255]);
    let active = Rgba([245, 158, 11, 255]);
    let thickness = 2_u32.min(width).min(height);
    for offset in 0..thickness {
        let left_x = left.saturating_add(offset).min(width - 1);
        let right_x = right.saturating_sub(1 + offset).min(width - 1);
        let top_y = top.saturating_add(offset).min(height - 1);
        let bottom_y = bottom.saturating_sub(1 + offset).min(height - 1);
        draw_vertical(
            image,
            left_x,
            top,
            bottom,
            if overlay.active == Some(CropEdge::Left) {
                active
            } else {
                inactive
            },
        );
        draw_vertical(
            image,
            right_x,
            top,
            bottom,
            if overlay.active == Some(CropEdge::Right) {
                active
            } else {
                inactive
            },
        );
        draw_horizontal(
            image,
            top_y,
            left,
            right,
            if overlay.active == Some(CropEdge::Top) {
                active
            } else {
                inactive
            },
        );
        draw_horizontal(
            image,
            bottom_y,
            left,
            right,
            if overlay.active == Some(CropEdge::Bottom) {
                active
            } else {
                inactive
            },
        );
    }
    // Draw the selected guide last so its amber highlight remains visible at
    // intersections with the other three crop boundaries.
    for offset in 0..thickness {
        match overlay.active {
            Some(CropEdge::Left) => draw_vertical(
                image,
                left.saturating_add(offset).min(width - 1),
                top,
                bottom,
                active,
            ),
            Some(CropEdge::Right) => draw_vertical(
                image,
                right.saturating_sub(1 + offset).min(width - 1),
                top,
                bottom,
                active,
            ),
            Some(CropEdge::Top) => draw_horizontal(
                image,
                top.saturating_add(offset).min(height - 1),
                left,
                right,
                active,
            ),
            Some(CropEdge::Bottom) => draw_horizontal(
                image,
                bottom.saturating_sub(1 + offset).min(height - 1),
                left,
                right,
                active,
            ),
            None => {}
        }
    }
}

fn fraction_pixel(fraction: f32, length: u32) -> u32 {
    (fraction.clamp(0.0, 1.0) * length as f32).round() as u32
}

fn draw_vertical(image: &mut RgbaImage, x: u32, top: u32, bottom: u32, color: Rgba<u8>) {
    for y in top.min(image.height())..bottom.min(image.height()) {
        image.put_pixel(x, y, color);
    }
}

fn draw_horizontal(image: &mut RgbaImage, y: u32, left: u32, right: u32, color: Rgba<u8>) {
    for x in left.min(image.width())..right.min(image.width()) {
        image.put_pixel(x, y, color);
    }
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
        let image = letterbox_frame(&frame, 8, 8, None).unwrap().to_rgba8();
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
        assert!(letterbox_frame(&frame, 8, 4, None).is_err());
    }

    #[test]
    fn crop_overlay_dims_removed_pixels_and_highlights_the_active_edge() {
        let frame = PreviewFrame {
            bgra: vec![200; 8 * 8 * 4],
            width: 8,
            height: 8,
            time: Duration::ZERO,
        };
        let image = letterbox_frame(
            &frame,
            8,
            8,
            Some(RasterEditOverlay {
                left: 0.25,
                right: 0.25,
                top: 0.25,
                bottom: 0.25,
                active: Some(CropEdge::Left),
                show_crop: true,
                video_gain: 1.0,
            }),
        )
        .unwrap()
        .to_rgba8();
        assert!(image.get_pixel(0, 4).0[0] < 100);
        assert_eq!(image.get_pixel(2, 4).0, [245, 158, 11, 255]);
        assert_eq!(image.get_pixel(5, 4).0, [34, 211, 238, 255]);
    }

    #[test]
    fn video_fade_overlay_darkens_the_picture_without_crop_guides() {
        let frame = PreviewFrame {
            bgra: vec![200; 4 * 4 * 4],
            width: 4,
            height: 4,
            time: Duration::ZERO,
        };
        let image = letterbox_frame(
            &frame,
            4,
            4,
            Some(RasterEditOverlay {
                video_gain: 0.5,
                ..RasterEditOverlay::default()
            }),
        )
        .unwrap()
        .to_rgba8();
        assert_eq!(image.get_pixel(2, 2).0, [100, 100, 100, 255]);
    }
}
