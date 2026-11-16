//! The state of a plain region selection on a frozen monitor image.
//!
//! The picker window shows one monitor image and lets the user drag one
//! rectangle. There is no toolbar and no annotation. This module holds the
//! state and draws the frames. It has no Windows dependency.

use super::canvas::{Area, Canvas, Point, rgb};

/// Smallest edge of a selection in pixels. A smaller drag is a slip.
pub(crate) const MIN_EDGE: i32 = 32;
/// Color of the selection outline.
const OUTLINE: u32 = rgb(65, 214, 224);
/// Thickness of the selection outline in pixels.
const OUTLINE_WIDTH: i32 = 2;

/// What the picker window does after one input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    /// Keep the window open.
    Continue,
    /// Close the window and return this selection.
    Confirm(Area),
    /// Close the window and return nothing.
    Cancel,
}

/// One region selection in progress.
pub(crate) struct Selector {
    /// The frozen monitor image.
    base: Canvas,
    /// Where the current drag started.
    anchor: Option<Point>,
    /// The rectangle of the current or the last drag.
    selection: Option<Area>,
}

impl Selector {
    /// Starts a selection on a monitor image.
    pub(crate) const fn new(base: Canvas) -> Self {
        Self {
            base,
            anchor: None,
            selection: None,
        }
    }

    /// Returns the selection when it is large enough to record.
    fn usable(&self) -> Option<Area> {
        self.selection
            .filter(|area| area.width() >= MIN_EDGE && area.height() >= MIN_EDGE)
    }

    /// Keeps a point inside the image.
    fn inside(&self, point: Point) -> Point {
        Point::new(
            point.x.clamp(0, self.base.width() as i32 - 1),
            point.y.clamp(0, self.base.height() as i32 - 1),
        )
    }

    /// Starts a drag.
    pub(crate) fn pointer_down(&mut self, point: Point) {
        let point = self.inside(point);
        self.anchor = Some(point);
        self.selection = Some(Area::from_points(point, point));
    }

    /// Stretches the rectangle of a running drag.
    pub(crate) fn pointer_move(&mut self, point: Point) {
        if let Some(anchor) = self.anchor {
            self.selection = Some(Area::from_points(anchor, self.inside(point)));
        }
    }

    /// Ends a drag. A rectangle that is large enough confirms at once.
    pub(crate) fn pointer_up(&mut self, point: Point) -> Step {
        self.pointer_move(point);
        if self.anchor.take().is_none() {
            return Step::Continue;
        }
        match self.usable() {
            Some(area) => Step::Confirm(area),
            None => {
                self.selection = None;
                Step::Continue
            }
        }
    }

    /// Confirms the selection, as the Enter key does.
    pub(crate) fn confirm(&self) -> Step {
        self.usable().map_or(Step::Continue, Step::Confirm)
    }

    /// Draws the image with everything outside the selection dimmed.
    pub(crate) fn compose(&self, frame: &mut Canvas) {
        frame.copy_from(&self.base);
        let selection = self.selection.unwrap_or_default();
        frame.dim_outside(selection);
        if !selection.is_empty() {
            frame.stroke_rect(selection, OUTLINE_WIDTH, OUTLINE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a selector on a gray image.
    fn selector() -> Selector {
        Selector::new(Canvas::new(400, 300, rgb(200, 200, 200)))
    }

    #[test]
    fn a_drag_confirms_on_release_in_any_direction() {
        let mut selector = selector();
        selector.pointer_down(Point::new(300, 200));
        selector.pointer_move(Point::new(200, 150));
        assert_eq!(
            selector.pointer_up(Point::new(100, 100)),
            Step::Confirm(Area::new(100, 100, 301, 201))
        );
        // A release without a drag does nothing.
        assert_eq!(selector.pointer_up(Point::new(5, 5)), Step::Continue);
    }

    #[test]
    fn a_small_drag_is_dropped_and_the_window_stays() {
        let mut selector = selector();
        selector.pointer_down(Point::new(10, 10));
        assert_eq!(selector.pointer_up(Point::new(20, 200)), Step::Continue);
        assert_eq!(selector.confirm(), Step::Continue);
        selector.pointer_down(Point::new(10, 10));
        selector.pointer_move(Point::new(120, 90));
        assert_eq!(
            selector.confirm(),
            Step::Confirm(Area::new(10, 10, 121, 91))
        );
    }

    #[test]
    fn a_drag_past_the_edge_stays_inside_the_image() {
        let mut selector = selector();
        selector.pointer_down(Point::new(350, 250));
        assert_eq!(
            selector.pointer_up(Point::new(9_000, -50)),
            Step::Confirm(Area::new(350, 0, 400, 251))
        );
    }

    #[test]
    fn the_frame_dims_the_outside_and_outlines_the_selection() {
        let mut selector = selector();
        let mut frame = Canvas::new(400, 300, 0);
        selector.compose(&mut frame);
        let dimmed = frame.pixel(200, 150).unwrap();
        assert_ne!(dimmed, rgb(200, 200, 200));
        selector.pointer_down(Point::new(100, 100));
        selector.pointer_move(Point::new(299, 199));
        selector.compose(&mut frame);
        assert_eq!(frame.pixel(200, 150), Some(rgb(200, 200, 200)));
        assert_eq!(frame.pixel(100, 100), Some(OUTLINE));
        assert_eq!(frame.pixel(50, 50), Some(dimmed));
    }
}
