//! Software drawing on one `0x00RRGGBB` pixel buffer.
//!
//! The buffer layout equals a top-down 32-bit Windows DIB. The overlay window
//! can therefore present it without a conversion. All drawing clips to the
//! buffer, so callers can pass coordinates that leave the screen.

/// One pixel position. Positions can lie outside a canvas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Point {
    /// Horizontal position in pixels from the left edge.
    pub(crate) x: i32,
    /// Vertical position in pixels from the top edge.
    pub(crate) y: i32,
}

impl Point {
    /// Creates a point.
    #[must_use]
    pub(crate) const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// One rectangle with an exclusive right and bottom edge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Area {
    /// First column inside the rectangle.
    pub(crate) left: i32,
    /// First row inside the rectangle.
    pub(crate) top: i32,
    /// First column after the rectangle.
    pub(crate) right: i32,
    /// First row after the rectangle.
    pub(crate) bottom: i32,
}

impl Area {
    /// Creates a rectangle from its edges.
    #[must_use]
    pub(crate) const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Creates the rectangle that contains both pixels, in any drag direction.
    #[must_use]
    pub(crate) fn from_points(first: Point, second: Point) -> Self {
        Self {
            left: first.x.min(second.x),
            top: first.y.min(second.y),
            right: first.x.max(second.x) + 1,
            bottom: first.y.max(second.y) + 1,
        }
    }

    /// Returns the width in pixels. An inverted rectangle has no width.
    #[must_use]
    pub(crate) const fn width(&self) -> i32 {
        if self.right > self.left {
            self.right - self.left
        } else {
            0
        }
    }

    /// Returns the height in pixels. An inverted rectangle has no height.
    #[must_use]
    pub(crate) const fn height(&self) -> i32 {
        if self.bottom > self.top {
            self.bottom - self.top
        } else {
            0
        }
    }

    /// Reports whether the rectangle contains no pixel.
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Reports whether the rectangle contains the pixel.
    #[must_use]
    pub(crate) const fn contains(&self, point: Point) -> bool {
        point.x >= self.left && point.x < self.right && point.y >= self.top && point.y < self.bottom
    }

    /// Returns the part of this rectangle that lies inside `bounds`.
    #[must_use]
    pub(crate) fn intersect(&self, bounds: &Self) -> Self {
        Self {
            left: self.left.max(bounds.left),
            top: self.top.max(bounds.top),
            right: self.right.min(bounds.right),
            bottom: self.bottom.min(bounds.bottom),
        }
    }

    /// Returns this rectangle moved by the given distance.
    #[must_use]
    pub(crate) const fn offset(&self, dx: i32, dy: i32) -> Self {
        Self {
            left: self.left + dx,
            top: self.top + dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }

    /// Returns this rectangle grown by `amount` pixels on every side.
    #[must_use]
    pub(crate) const fn inflate(&self, amount: i32) -> Self {
        Self {
            left: self.left - amount,
            top: self.top - amount,
            right: self.right + amount,
            bottom: self.bottom + amount,
        }
    }
}

/// One alpha coverage bitmap, such as rendered text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Mask {
    /// Width in pixels.
    pub(crate) width: u32,
    /// Height in pixels.
    pub(crate) height: u32,
    /// One coverage byte for each pixel, in row order.
    pub(crate) coverage: Vec<u8>,
}

/// Packs three 8-bit channels into a `0x00RRGGBB` value.
#[must_use]
pub(crate) const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    ((red as u32) << 16) | ((green as u32) << 8) | blue as u32
}

/// Mixes `color` over `under` with an 8-bit opacity.
#[must_use]
fn mix(under: u32, color: u32, alpha: u32) -> u32 {
    let inverse = 255 - alpha;
    let channel = |shift: u32| {
        let below = (under >> shift) & 0xff;
        let above = (color >> shift) & 0xff;
        ((below * inverse + above * alpha + 127) / 255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Converts a coverage fraction to an 8-bit opacity.
fn coverage_alpha(coverage: f32) -> u32 {
    (coverage.clamp(0.0, 1.0) * 255.0 + 0.5) as u32
}

/// One owned image in `0x00RRGGBB` pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl Canvas {
    /// Creates a canvas filled with one color.
    #[must_use]
    pub(crate) fn new(width: u32, height: u32, color: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![color & 0x00ff_ffff; width as usize * height as usize],
        }
    }

    /// Wraps existing pixels. The length must equal `width * height`.
    #[must_use]
    pub(crate) fn from_pixels(width: u32, height: u32, pixels: Vec<u32>) -> Option<Self> {
        (pixels.len() == width as usize * height as usize).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Returns the width in pixels.
    #[must_use]
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the height in pixels.
    #[must_use]
    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    /// Returns all pixels in row order.
    #[must_use]
    pub(crate) fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Returns the rectangle that covers the complete canvas.
    #[must_use]
    pub(crate) const fn bounds(&self) -> Area {
        Area::new(0, 0, self.width as i32, self.height as i32)
    }

    /// Returns one pixel, or `None` outside the canvas.
    #[must_use]
    pub(crate) fn pixel(&self, x: i32, y: i32) -> Option<u32> {
        self.bounds()
            .contains(Point::new(x, y))
            .then(|| self.pixels[y as usize * self.width as usize + x as usize])
    }

    /// Replaces all pixels with those of an equally sized canvas.
    pub(crate) fn copy_from(&mut self, source: &Self) {
        if self.width == source.width && self.height == source.height {
            self.pixels.copy_from_slice(&source.pixels);
        } else {
            self.clone_from(source);
        }
    }

    /// Returns one row slice limited to the given columns.
    fn row_mut(&mut self, y: i32, left: i32, right: i32) -> &mut [u32] {
        let start = y as usize * self.width as usize;
        &mut self.pixels[start + left as usize..start + right as usize]
    }

    /// Mixes a color into one pixel when it lies inside the canvas.
    fn blend_pixel(&mut self, x: i32, y: i32, color: u32, alpha: u32) {
        if alpha == 0 || !self.bounds().contains(Point::new(x, y)) {
            return;
        }
        let index = y as usize * self.width as usize + x as usize;
        self.pixels[index] = mix(self.pixels[index], color, alpha.min(255));
    }

    /// Fills a rectangle with a color at the given 8-bit opacity.
    pub(crate) fn fill_rect(&mut self, area: Area, color: u32, alpha: u8) {
        let area = area.intersect(&self.bounds());
        if area.is_empty() || alpha == 0 {
            return;
        }
        for y in area.top..area.bottom {
            let row = self.row_mut(y, area.left, area.right);
            if alpha == 255 {
                row.fill(color & 0x00ff_ffff);
            } else {
                for pixel in row {
                    *pixel = mix(*pixel, color, u32::from(alpha));
                }
            }
        }
    }

    /// Fills a rectangle with antialiased round corners.
    ///
    /// A radius of half the shorter side gives a pill or a circle.
    pub(crate) fn fill_round_rect(&mut self, area: Area, radius: f32, color: u32, alpha: u8) {
        if area.is_empty() || alpha == 0 {
            return;
        }
        let limit = area.width().min(area.height()) as f32 / 2.0;
        let radius = radius.clamp(0.0, limit);
        let visible = area.intersect(&self.bounds());
        // Centers of the corner circles, in pixel center coordinates.
        let (near_x, far_x) = (
            area.left as f32 + radius - 0.5,
            area.right as f32 - radius - 0.5,
        );
        let (near_y, far_y) = (
            area.top as f32 + radius - 0.5,
            area.bottom as f32 - radius - 0.5,
        );
        for y in visible.top..visible.bottom {
            let dy = (near_y - y as f32).max(y as f32 - far_y).max(0.0);
            for x in visible.left..visible.right {
                let dx = (near_x - x as f32).max(x as f32 - far_x).max(0.0);
                let coverage = if dx > 0.0 && dy > 0.0 {
                    radius + 0.5 - (dx * dx + dy * dy).sqrt()
                } else {
                    1.0
                };
                let opacity = coverage_alpha(coverage) * u32::from(alpha) / 255;
                self.blend_pixel(x, y, color, opacity);
            }
        }
    }

    /// Draws a rectangle outline whose strokes lie inside `area`.
    pub(crate) fn stroke_rect(&mut self, area: Area, thickness: i32, color: u32) {
        let thickness = thickness
            .max(1)
            .min((area.width() + 1) / 2)
            .min((area.height() + 1) / 2);
        if area.is_empty() {
            return;
        }
        let Area {
            left,
            top,
            right,
            bottom,
        } = area;
        self.fill_rect(Area::new(left, top, right, top + thickness), color, 255);
        self.fill_rect(
            Area::new(left, bottom - thickness, right, bottom),
            color,
            255,
        );
        self.fill_rect(
            Area::new(left, top + thickness, left + thickness, bottom - thickness),
            color,
            255,
        );
        self.fill_rect(
            Area::new(
                right - thickness,
                top + thickness,
                right,
                bottom - thickness,
            ),
            color,
            255,
        );
    }

    /// Draws an antialiased line with round ends.
    pub(crate) fn stroke_line(&mut self, from: Point, to: Point, thickness: f32, color: u32) {
        let half = (thickness.max(1.0)) / 2.0;
        let reach = half.ceil() as i32 + 1;
        let area = Area::from_points(from, to)
            .inflate(reach)
            .intersect(&self.bounds());
        let (ax, ay) = (from.x as f32, from.y as f32);
        let (dx, dy) = ((to.x - from.x) as f32, (to.y - from.y) as f32);
        let length_squared = dx * dx + dy * dy;
        for y in area.top..area.bottom {
            for x in area.left..area.right {
                let (px, py) = (x as f32 - ax, y as f32 - ay);
                let along = if length_squared > 0.0 {
                    ((px * dx + py * dy) / length_squared).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (ox, oy) = (px - along * dx, py - along * dy);
                let distance = (ox * ox + oy * oy).sqrt();
                let alpha = coverage_alpha(half + 0.5 - distance);
                self.blend_pixel(x, y, color, alpha);
            }
        }
    }

    /// Draws an antialiased ellipse outline inside `area`.
    pub(crate) fn stroke_ellipse(&mut self, area: Area, thickness: f32, color: u32) {
        if area.is_empty() {
            return;
        }
        let half = thickness.max(1.0) / 2.0;
        let radius_x = (area.width() as f32 / 2.0 - half).max(0.5);
        let radius_y = (area.height() as f32 / 2.0 - half).max(0.5);
        let center_x = (area.left + area.right - 1) as f32 / 2.0;
        let center_y = (area.top + area.bottom - 1) as f32 / 2.0;
        let clipped = area.inflate(1).intersect(&self.bounds());
        for y in clipped.top..clipped.bottom {
            for x in clipped.left..clipped.right {
                let (px, py) = (x as f32 - center_x, y as f32 - center_y);
                let value = (px / radius_x).powi(2) + (py / radius_y).powi(2) - 1.0;
                let slope = 2.0
                    * ((px / (radius_x * radius_x)).powi(2) + (py / (radius_y * radius_y)).powi(2))
                        .sqrt();
                if slope <= f32::EPSILON {
                    continue;
                }
                let distance = value.abs() / slope;
                let alpha = coverage_alpha(half + 0.5 - distance);
                self.blend_pixel(x, y, color, alpha);
            }
        }
    }

    /// Fills an antialiased triangle.
    pub(crate) fn fill_triangle(&mut self, corners: [(f32, f32); 3], color: u32) {
        let [a, b, c] = corners;
        let winding = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if winding.abs() < f32::EPSILON {
            return;
        }
        let sign = winding.signum();
        let min_x = a.0.min(b.0).min(c.0).floor() as i32 - 1;
        let max_x = a.0.max(b.0).max(c.0).ceil() as i32 + 1;
        let min_y = a.1.min(b.1).min(c.1).floor() as i32 - 1;
        let max_y = a.1.max(b.1).max(c.1).ceil() as i32 + 1;
        let area = Area::new(min_x, min_y, max_x + 1, max_y + 1).intersect(&self.bounds());
        let edges = [(a, b), (b, c), (c, a)];
        for y in area.top..area.bottom {
            for x in area.left..area.right {
                let mut inside = f32::MAX;
                for (start, end) in edges {
                    let (ex, ey) = (end.0 - start.0, end.1 - start.1);
                    let length = (ex * ex + ey * ey).sqrt().max(f32::EPSILON);
                    let cross = ex * (y as f32 - start.1) - ey * (x as f32 - start.0);
                    inside = inside.min(sign * cross / length);
                }
                self.blend_pixel(x, y, color, coverage_alpha(inside + 0.5));
            }
        }
    }

    /// Draws a line that ends in a filled arrow head at `to`.
    pub(crate) fn stroke_arrow(&mut self, from: Point, to: Point, thickness: f32, color: u32) {
        let (dx, dy) = ((to.x - from.x) as f32, (to.y - from.y) as f32);
        let length = (dx * dx + dy * dy).sqrt();
        if length < 1.0 {
            return;
        }
        let (ux, uy) = (dx / length, dy / length);
        let head = (thickness * 4.0 + 6.0).min(length);
        let wing = head * 0.55;
        let (tip_x, tip_y) = (to.x as f32, to.y as f32);
        let (base_x, base_y) = (tip_x - ux * head, tip_y - uy * head);
        let shaft_end = Point::new(
            (tip_x - ux * head * 0.8).round() as i32,
            (tip_y - uy * head * 0.8).round() as i32,
        );
        if length > head {
            self.stroke_line(from, shaft_end, thickness, color);
        }
        self.fill_triangle(
            [
                (tip_x, tip_y),
                (base_x - uy * wing, base_y + ux * wing),
                (base_x + uy * wing, base_y - ux * wing),
            ],
            color,
        );
    }

    /// Replaces a rectangle with square blocks of their average color.
    pub(crate) fn pixelate(&mut self, area: Area, block: i32) {
        let area = area.intersect(&self.bounds());
        let block = block.max(2);
        let mut top = area.top;
        while top < area.bottom {
            let bottom = (top + block).min(area.bottom);
            let mut left = area.left;
            while left < area.right {
                let right = (left + block).min(area.right);
                let mut sums = [0_u64; 3];
                for y in top..bottom {
                    for pixel in self.row_mut(y, left, right) {
                        sums[0] += u64::from((*pixel >> 16) & 0xff);
                        sums[1] += u64::from((*pixel >> 8) & 0xff);
                        sums[2] += u64::from(*pixel & 0xff);
                    }
                }
                let count = ((bottom - top) * (right - left)) as u64;
                let average = sums.map(|sum| ((sum + count / 2) / count) as u8);
                let color = rgb(average[0], average[1], average[2]);
                for y in top..bottom {
                    self.row_mut(y, left, right).fill(color);
                }
                left = right;
            }
            top = bottom;
        }
    }

    /// Blurs a rectangle with three box passes, which approximates a Gaussian.
    pub(crate) fn blur(&mut self, area: Area, radius: i32) {
        let area = area.intersect(&self.bounds());
        if area.is_empty() || radius < 1 {
            return;
        }
        let (width, height) = (area.width() as usize, area.height() as usize);
        let mut region = Vec::with_capacity(width * height);
        for y in area.top..area.bottom {
            region.extend_from_slice(self.row_mut(y, area.left, area.right));
        }
        let mut scratch = vec![0_u32; region.len()];
        let radius = radius as usize;
        for _ in 0..3 {
            for row in 0..height {
                box_blur_line(&region, &mut scratch, row * width, 1, width, radius);
            }
            for column in 0..width {
                box_blur_line(&scratch, &mut region, column, width, height, radius);
            }
        }
        for (row, y) in (area.top..area.bottom).enumerate() {
            self.row_mut(y, area.left, area.right)
                .copy_from_slice(&region[row * width..(row + 1) * width]);
        }
    }

    /// Tints a rectangle like a marker pen. Dark text stays readable.
    pub(crate) fn highlight(&mut self, area: Area, color: u32) {
        let area = area.intersect(&self.bounds());
        for y in area.top..area.bottom {
            for pixel in self.row_mut(y, area.left, area.right) {
                let channel = |shift: u32| {
                    let tint = 96 + ((color >> shift) & 0xff) * 159 / 255;
                    (((*pixel >> shift) & 0xff) * tint / 255) << shift
                };
                *pixel = channel(16) | channel(8) | channel(0);
            }
        }
    }

    /// Darkens every pixel outside `keep`.
    pub(crate) fn dim_outside(&mut self, keep: Area) {
        let keep = keep.intersect(&self.bounds());
        let width = self.width as usize;
        for (y, row) in self.pixels.chunks_exact_mut(width.max(1)).enumerate() {
            let y = y as i32;
            if keep.is_empty() || y < keep.top || y >= keep.bottom {
                dim(row);
            } else {
                dim(&mut row[..keep.left as usize]);
                dim(&mut row[keep.right as usize..]);
            }
        }
    }

    /// Draws a coverage mask in one color with its top-left corner at `origin`.
    pub(crate) fn draw_mask(&mut self, origin: Point, mask: &Mask, color: u32) {
        let placed = Area::new(
            origin.x,
            origin.y,
            origin.x + mask.width as i32,
            origin.y + mask.height as i32,
        );
        let visible = placed.intersect(&self.bounds());
        for y in visible.top..visible.bottom {
            let mask_row = (y - origin.y) as usize * mask.width as usize;
            for x in visible.left..visible.right {
                let alpha = mask.coverage[mask_row + (x - origin.x) as usize];
                self.blend_pixel(x, y, color, u32::from(alpha));
            }
        }
    }

    /// Copies another image into this one with its top-left corner at `origin`.
    pub(crate) fn paste(&mut self, source: &Self, origin: Point) {
        let placed = Area::new(
            origin.x,
            origin.y,
            origin.x + source.width as i32,
            origin.y + source.height as i32,
        );
        let visible = placed.intersect(&self.bounds());
        if visible.is_empty() {
            return;
        }
        let source_width = source.width as usize;
        for y in visible.top..visible.bottom {
            let start = (y - origin.y) as usize * source_width + (visible.left - origin.x) as usize;
            let row = &source.pixels[start..start + visible.width() as usize];
            self.row_mut(y, visible.left, visible.right)
                .copy_from_slice(row);
        }
    }

    /// Copies the part of `area` inside the canvas into a new canvas.
    #[must_use]
    pub(crate) fn crop(&self, area: Area) -> Self {
        let area = area.intersect(&self.bounds());
        if area.is_empty() {
            return self.clone();
        }
        let width = self.width as usize;
        let mut pixels = Vec::with_capacity(area.width() as usize * area.height() as usize);
        for y in area.top..area.bottom {
            let start = y as usize * width;
            pixels.extend_from_slice(
                &self.pixels[start + area.left as usize..start + area.right as usize],
            );
        }
        Self {
            width: area.width() as u32,
            height: area.height() as u32,
            pixels,
        }
    }

    /// Returns this image turned clockwise by the given number of quarter turns.
    #[must_use]
    pub(crate) fn rotated(&self, quarter_turns: u32) -> Self {
        let (width, height) = (self.width as usize, self.height as usize);
        let turns = quarter_turns % 4;
        if turns == 0 {
            return self.clone();
        }
        let (new_width, new_height) = if turns == 2 {
            (width, height)
        } else {
            (height, width)
        };
        let mut pixels = vec![0_u32; self.pixels.len()];
        for y in 0..height {
            for x in 0..width {
                let (new_x, new_y) = match turns {
                    1 => (height - 1 - y, x),
                    2 => (width - 1 - x, height - 1 - y),
                    _ => (y, width - 1 - x),
                };
                pixels[new_y * new_width + new_x] = self.pixels[y * width + x];
            }
        }
        Self {
            width: new_width as u32,
            height: new_height as u32,
            pixels,
        }
    }

    /// Returns the image as packed 8-bit RGB bytes for an encoder.
    #[must_use]
    pub(crate) fn to_rgb_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.pixels.len() * 3);
        for pixel in &self.pixels {
            bytes.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
        }
        bytes
    }
}

/// Darkens a run of pixels to 45 percent brightness.
fn dim(pixels: &mut [u32]) {
    for pixel in pixels {
        let half = (*pixel >> 1) & 0x007f_7f7f;
        let sixteenth = (*pixel >> 4) & 0x000f_0f0f;
        *pixel = half - sixteenth;
    }
}

/// Averages one row or column with a sliding window that repeats edge pixels.
fn box_blur_line(
    source: &[u32],
    target: &mut [u32],
    start: usize,
    stride: usize,
    length: usize,
    radius: usize,
) {
    let at = |index: usize| source[start + index.min(length - 1) * stride];
    let window = (2 * radius + 1) as u32;
    let mut sums = [0_u32; 3];
    let add = |sums: &mut [u32; 3], pixel: u32, times: u32| {
        sums[0] += ((pixel >> 16) & 0xff) * times;
        sums[1] += ((pixel >> 8) & 0xff) * times;
        sums[2] += (pixel & 0xff) * times;
    };
    add(&mut sums, at(0), radius as u32 + 1);
    for index in 1..=radius {
        add(&mut sums, at(index), 1);
    }
    for index in 0..length {
        let rounded = sums.map(|sum| (sum + window / 2) / window);
        target[start + index * stride] = (rounded[0] << 16) | (rounded[1] << 8) | rounded[2];
        let entering = at(index + radius + 1);
        let leaving = at(index.saturating_sub(radius));
        sums[0] = sums[0] + ((entering >> 16) & 0xff) - ((leaving >> 16) & 0xff);
        sums[1] = sums[1] + ((entering >> 8) & 0xff) - ((leaving >> 8) & 0xff);
        sums[2] = sums[2] + (entering & 0xff) - (leaving & 0xff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: u32 = 0x00ff_ffff;
    const RED: u32 = 0x00ff_0000;

    /// Creates a canvas whose left half is black and right half is white.
    fn split_canvas() -> Canvas {
        let mut canvas = Canvas::new(40, 20, 0);
        canvas.fill_rect(Area::new(20, 0, 40, 20), WHITE, 255);
        canvas
    }

    #[test]
    fn area_from_points_accepts_any_drag_direction() {
        let area = Area::from_points(Point::new(9, 7), Point::new(2, 3));
        assert_eq!(area, Area::new(2, 3, 10, 8));
        assert_eq!((area.width(), area.height()), (8, 5));
        assert!(area.contains(Point::new(9, 7)));
        assert!(!area.contains(Point::new(10, 7)));
    }

    #[test]
    fn fill_clips_to_the_canvas() {
        let mut canvas = Canvas::new(4, 4, 0);
        canvas.fill_rect(Area::new(-5, -5, 2, 2), RED, 255);
        assert_eq!(canvas.pixel(1, 1), Some(RED));
        assert_eq!(canvas.pixel(2, 2), Some(0));
        canvas.fill_rect(Area::new(0, 0, 4, 4), WHITE, 128);
        assert_eq!(canvas.pixel(3, 3), Some(0x0080_8080));
    }

    #[test]
    fn round_rect_cuts_its_corners_and_fills_its_middle() {
        let mut canvas = Canvas::new(40, 20, 0);
        canvas.fill_round_rect(Area::new(0, 0, 40, 20), 8.0, RED, 255);
        assert_eq!(canvas.pixel(0, 0), Some(0));
        assert_eq!(canvas.pixel(39, 19), Some(0));
        assert_eq!(canvas.pixel(0, 10), Some(RED));
        assert_eq!(canvas.pixel(20, 0), Some(RED));
        assert_eq!(canvas.pixel(4, 4), Some(RED));
        let mut dot = Canvas::new(11, 11, 0);
        dot.fill_round_rect(Area::new(0, 0, 11, 11), 99.0, WHITE, 255);
        assert_eq!(dot.pixel(5, 5), Some(WHITE));
        assert_eq!(dot.pixel(0, 5), Some(WHITE));
        let corner = dot.pixel(1, 1).unwrap() & 0xff;
        assert!((1..255).contains(&corner), "corner value {corner}");
        dot.fill_round_rect(Area::new(0, 0, 11, 11), 2.0, RED, 128);
        assert_eq!(dot.pixel(5, 5), Some(0x00ff_7f7f));
    }

    #[test]
    fn rectangle_outline_leaves_its_interior() {
        let mut canvas = Canvas::new(20, 20, 0);
        canvas.stroke_rect(Area::new(2, 2, 18, 18), 3, RED);
        assert_eq!(canvas.pixel(2, 2), Some(RED));
        assert_eq!(canvas.pixel(4, 10), Some(RED));
        assert_eq!(canvas.pixel(17, 17), Some(RED));
        assert_eq!(canvas.pixel(5, 5), Some(0));
        assert_eq!(canvas.pixel(1, 1), Some(0));
    }

    #[test]
    fn line_covers_its_path_and_only_its_width() {
        let mut canvas = Canvas::new(40, 40, 0);
        canvas.stroke_line(Point::new(5, 20), Point::new(35, 20), 5.0, RED);
        assert_eq!(canvas.pixel(20, 20), Some(RED));
        assert_eq!(canvas.pixel(20, 22), Some(RED));
        assert_eq!(canvas.pixel(20, 24), Some(0));
        assert_eq!(canvas.pixel(1, 20), Some(0));
        canvas.stroke_line(Point::new(-50, -50), Point::new(90, 90), 3.0, WHITE);
        assert_eq!(canvas.pixel(10, 10), Some(WHITE));
    }

    #[test]
    fn ellipse_outline_touches_its_bounds_but_not_its_center() {
        let mut canvas = Canvas::new(60, 40, 0);
        canvas.stroke_ellipse(Area::new(5, 5, 55, 35), 3.0, RED);
        assert_eq!(canvas.pixel(30, 6), Some(RED));
        assert_eq!(canvas.pixel(6, 20), Some(RED));
        assert_eq!(canvas.pixel(30, 20), Some(0));
        assert_eq!(canvas.pixel(5, 5), Some(0));
    }

    #[test]
    fn arrow_head_is_wider_than_its_shaft() {
        let mut canvas = Canvas::new(80, 40, 0);
        canvas.stroke_arrow(Point::new(5, 20), Point::new(70, 20), 3.0, RED);
        assert_eq!(canvas.pixel(30, 20), Some(RED));
        assert_eq!(canvas.pixel(30, 24), Some(0));
        assert_eq!(canvas.pixel(60, 24), Some(RED));
        assert_eq!(canvas.pixel(75, 20), Some(0));
    }

    #[test]
    fn pixelate_averages_each_block() {
        let mut canvas = split_canvas();
        canvas.pixelate(Area::new(16, 0, 24, 8), 8);
        assert_eq!(canvas.pixel(16, 0), Some(0x0080_8080));
        assert_eq!(canvas.pixel(23, 7), Some(0x0080_8080));
        assert_eq!(canvas.pixel(15, 0), Some(0));
        assert_eq!(canvas.pixel(24, 0), Some(WHITE));
    }

    #[test]
    fn blur_softens_an_edge_and_keeps_flat_color() {
        let mut canvas = split_canvas();
        canvas.blur(Area::new(0, 0, 40, 20), 3);
        let edge = canvas.pixel(20, 10).unwrap() & 0xff;
        assert!((100..=156).contains(&edge), "edge value {edge}");
        assert_eq!(canvas.pixel(0, 10), Some(0));
        assert_eq!(canvas.pixel(39, 10), Some(WHITE));
        let mut flat = Canvas::new(16, 16, 0x0012_3456);
        flat.blur(Area::new(2, 2, 14, 14), 4);
        assert!(flat.pixels().iter().all(|pixel| *pixel == 0x0012_3456));
    }

    #[test]
    fn highlight_tints_white_and_keeps_black() {
        let mut canvas = split_canvas();
        canvas.highlight(Area::new(0, 0, 40, 20), 0x00ff_ff00);
        assert_eq!(canvas.pixel(0, 0), Some(0));
        assert_eq!(canvas.pixel(30, 0), Some(0x00ff_ff60));
    }

    #[test]
    fn dim_keeps_the_selected_area() {
        let mut canvas = Canvas::new(10, 10, WHITE);
        canvas.dim_outside(Area::new(2, 2, 5, 5));
        assert_eq!(canvas.pixel(3, 3), Some(WHITE));
        assert_eq!(canvas.pixel(0, 3), Some(0x0070_7070));
        assert_eq!(canvas.pixel(5, 3), Some(0x0070_7070));
        assert_eq!(canvas.pixel(3, 9), Some(0x0070_7070));
    }

    #[test]
    fn mask_blends_by_coverage_and_clips() {
        let mut canvas = Canvas::new(4, 4, 0);
        let mask = Mask {
            width: 2,
            height: 2,
            coverage: vec![255, 128, 0, 255],
        };
        canvas.draw_mask(Point::new(3, 3), &mask, WHITE);
        assert_eq!(canvas.pixel(3, 3), Some(WHITE));
        canvas.draw_mask(Point::new(0, 0), &mask, WHITE);
        assert_eq!(canvas.pixel(1, 0), Some(0x0080_8080));
        assert_eq!(canvas.pixel(0, 1), Some(0));
    }

    #[test]
    fn paste_places_and_clips_the_source() {
        let mut canvas = Canvas::new(4, 4, 0);
        let patch = Canvas::new(3, 3, RED);
        canvas.paste(&patch, Point::new(2, -1));
        assert_eq!(canvas.pixel(2, 0), Some(RED));
        assert_eq!(canvas.pixel(3, 1), Some(RED));
        assert_eq!(canvas.pixel(1, 0), Some(0));
        assert_eq!(canvas.pixel(2, 2), Some(0));
        canvas.paste(&patch, Point::new(40, 40));
    }

    #[test]
    fn crop_and_rotation_move_pixels_to_the_expected_places() {
        let mut canvas = Canvas::new(3, 2, 0);
        canvas.fill_rect(Area::new(0, 0, 1, 1), RED, 255);
        let turned = canvas.rotated(1);
        assert_eq!((turned.width(), turned.height()), (2, 3));
        assert_eq!(turned.pixel(1, 0), Some(RED));
        assert_eq!(canvas.rotated(2).pixel(2, 1), Some(RED));
        assert_eq!(canvas.rotated(3).pixel(0, 2), Some(RED));
        let cropped = canvas.crop(Area::new(0, 0, 2, 1));
        assert_eq!((cropped.width(), cropped.height()), (2, 1));
        assert_eq!(cropped.to_rgb_bytes(), vec![255, 0, 0, 0, 0, 0]);
    }
}
