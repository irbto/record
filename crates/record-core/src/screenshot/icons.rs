//! Toolbar icons drawn from lines and simple shapes.
//!
//! Each icon is drawn into a square box at any size, so the toolbar stays
//! sharp at every display scale and needs no image files.

use super::canvas::{Area, Canvas, Point};

/// Identifies one toolbar icon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Icon {
    Select,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Pen,
    Text,
    Marker,
    Pixelate,
    Blur,
    Step,
    Spotlight,
    Eraser,
    Minus,
    Plus,
    Undo,
    Redo,
    Copy,
    Save,
    Close,
}

/// Draws an icon into a square box.
///
/// `ink` is the icon color. `paper` is the color behind the icon, which a
/// few icons use for cut-out details.
pub(crate) fn draw_icon(canvas: &mut Canvas, icon: Icon, area: Area, ink: u32, paper: u32) {
    let size = area.width().min(area.height()) as f32;
    let stroke = (size / 11.0).max(1.5);
    // Maps a position in the unit square to a pixel of the box.
    let at = |x: f32, y: f32| {
        Point::new(
            area.left + (x * (size - 1.0)).round() as i32,
            area.top + (y * (size - 1.0)).round() as i32,
        )
    };
    let part = |left: f32, top: f32, right: f32, bottom: f32| {
        let (first, second) = (at(left, top), at(right, bottom));
        Area::new(first.x, first.y, second.x + 1, second.y + 1)
    };
    let weight = stroke.round().max(1.0) as i32;
    let path = |canvas: &mut Canvas, points: &[(f32, f32)]| {
        for pair in points.windows(2) {
            canvas.stroke_line(
                at(pair[0].0, pair[0].1),
                at(pair[1].0, pair[1].1),
                stroke,
                ink,
            );
        }
    };
    match icon {
        Icon::Select => {
            for (x, y, dx, dy) in [
                (0.08, 0.08, 1.0, 1.0),
                (0.92, 0.08, -1.0, 1.0),
                (0.08, 0.92, 1.0, -1.0),
                (0.92, 0.92, -1.0, -1.0),
            ] {
                path(canvas, &[(x + 0.3 * dx, y), (x, y), (x, y + 0.3 * dy)]);
            }
        }
        Icon::Rectangle => canvas.stroke_rect(part(0.05, 0.15, 0.95, 0.85), weight, ink),
        Icon::Ellipse => canvas.stroke_ellipse(part(0.02, 0.14, 0.98, 0.86), stroke, ink),
        Icon::Line => path(canvas, &[(0.12, 0.88), (0.88, 0.12)]),
        Icon::Arrow => canvas.stroke_arrow(at(0.1, 0.9), at(0.92, 0.08), stroke, ink),
        Icon::Pen => path(
            canvas,
            &[
                (0.05, 0.7),
                (0.25, 0.3),
                (0.45, 0.7),
                (0.65, 0.3),
                (0.95, 0.62),
            ],
        ),
        Icon::Text => {
            path(
                canvas,
                &[(0.15, 0.3), (0.15, 0.12), (0.85, 0.12), (0.85, 0.3)],
            );
            path(canvas, &[(0.5, 0.12), (0.5, 0.9)]);
            path(canvas, &[(0.35, 0.9), (0.65, 0.9)]);
        }
        Icon::Marker => {
            canvas.fill_round_rect(part(0.02, 0.5, 0.98, 0.88), stroke, 0x00ff_d60a, 255);
            path(canvas, &[(0.15, 0.25), (0.85, 0.25)]);
        }
        Icon::Pixelate => {
            for row in 0..3 {
                for column in 0..3 {
                    let (x, y) = (column as f32 / 3.0, row as f32 / 3.0);
                    let alpha = if (row + column) % 2 == 0 { 255 } else { 95 };
                    canvas.fill_rect(part(x + 0.03, y + 0.03, x + 0.3, y + 0.3), ink, alpha);
                }
            }
        }
        Icon::Blur => {
            for (reach, alpha) in [(0.5, 50), (0.36, 100), (0.2, 255)] {
                canvas.fill_round_rect(
                    part(0.5 - reach, 0.5 - reach, 0.5 + reach, 0.5 + reach),
                    size,
                    ink,
                    alpha,
                );
            }
        }
        Icon::Step => {
            canvas.fill_round_rect(part(0.04, 0.04, 0.96, 0.96), size, ink, 255);
            for pair in [[(0.4, 0.38), (0.53, 0.26)], [(0.53, 0.26), (0.53, 0.74)]] {
                canvas.stroke_line(
                    at(pair[0].0, pair[0].1),
                    at(pair[1].0, pair[1].1),
                    stroke,
                    paper,
                );
            }
        }
        Icon::Spotlight => {
            canvas.fill_round_rect(part(0.0, 0.08, 1.0, 0.92), stroke, ink, 70);
            canvas.fill_rect(part(0.28, 0.3, 0.72, 0.7), ink, 255);
        }
        Icon::Eraser => {
            path(
                canvas,
                &[
                    (0.55, 0.1),
                    (0.95, 0.5),
                    (0.55, 0.9),
                    (0.3, 0.9),
                    (0.05, 0.6),
                    (0.55, 0.1),
                ],
            );
            path(canvas, &[(0.3, 0.35), (0.7, 0.75)]);
            path(canvas, &[(0.55, 0.9), (0.95, 0.9)]);
        }
        Icon::Minus => path(canvas, &[(0.2, 0.5), (0.8, 0.5)]),
        Icon::Plus => {
            path(canvas, &[(0.2, 0.5), (0.8, 0.5)]);
            path(canvas, &[(0.5, 0.2), (0.5, 0.8)]);
        }
        Icon::Undo | Icon::Redo => {
            let flip = |x: f32| if icon == Icon::Undo { x } else { 1.0 - x };
            path(
                canvas,
                &[
                    (flip(0.3), 0.85),
                    (flip(0.9), 0.85),
                    (flip(0.9), 0.4),
                    (flip(0.35), 0.4),
                ],
            );
            canvas.stroke_arrow(at(flip(0.5), 0.4), at(flip(0.05), 0.4), stroke, ink);
        }
        Icon::Copy => {
            canvas.stroke_rect(part(0.05, 0.05, 0.65, 0.65), weight, ink);
            canvas.fill_rect(part(0.32, 0.32, 0.95, 0.95), paper, 255);
            canvas.stroke_rect(part(0.32, 0.32, 0.95, 0.95), weight, ink);
        }
        Icon::Save => {
            canvas.stroke_arrow(at(0.5, 0.05), at(0.5, 0.68), stroke, ink);
            path(
                canvas,
                &[(0.08, 0.6), (0.08, 0.93), (0.92, 0.93), (0.92, 0.6)],
            );
        }
        Icon::Close => {
            path(canvas, &[(0.15, 0.15), (0.85, 0.85)]);
            path(canvas, &[(0.85, 0.15), (0.15, 0.85)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 20] = [
        Icon::Select,
        Icon::Rectangle,
        Icon::Ellipse,
        Icon::Line,
        Icon::Arrow,
        Icon::Pen,
        Icon::Text,
        Icon::Marker,
        Icon::Pixelate,
        Icon::Blur,
        Icon::Step,
        Icon::Spotlight,
        Icon::Eraser,
        Icon::Minus,
        Icon::Plus,
        Icon::Undo,
        Icon::Redo,
        Icon::Copy,
        Icon::Save,
        Icon::Close,
    ];

    #[test]
    fn every_icon_draws_inside_a_small_margin_at_every_size() {
        for size in [14, 20, 30, 40] {
            for icon in ALL {
                let margin = 6;
                let side = size + 2 * margin;
                let mut canvas = Canvas::new(side as u32, side as u32, 0);
                let area = Area::new(margin, margin, margin + size, margin + size);
                draw_icon(&mut canvas, icon, area, 0x00ff_ffff, 0x0020_2124);
                let inked = canvas.pixels().iter().filter(|pixel| **pixel != 0).count();
                assert!(inked > 8, "{icon:?} at {size} is almost empty");
                let reach = area.inflate(3);
                for y in 0..side {
                    for x in 0..side {
                        if !reach.contains(Point::new(x, y)) {
                            assert_eq!(canvas.pixel(x, y), Some(0), "{icon:?} at {size} leaks");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mirrored_icons_differ() {
        let draw = |icon| {
            let mut canvas = Canvas::new(24, 24, 0);
            draw_icon(&mut canvas, icon, Area::new(2, 2, 22, 22), 0x00ff_ffff, 0);
            canvas
        };
        assert_ne!(draw(Icon::Undo), draw(Icon::Redo));
        assert_ne!(draw(Icon::Line), draw(Icon::Arrow));
    }
}
