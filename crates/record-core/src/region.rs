//! Desktop rectangles and the monitors that hold them.
//!
//! A region recording starts with a rectangle in desktop pixels. The video
//! engine records one monitor and crops in the pixels of that monitor. The
//! functions here pick the monitor and convert the rectangle. They call no
//! Windows function, so they run in tests.

use crate::screenshot::DesktopRect;

/// Smallest edge of a recorded region in pixels.
pub const MIN_REGION_EDGE: u32 = 32;

/// The place of one monitor on the desktop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MonitorArea {
    /// Index that the video engine accepts.
    pub index: u32,
    /// Left edge in desktop pixels.
    pub left: i32,
    /// Top edge in desktop pixels.
    pub top: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl MonitorArea {
    /// Returns the first column after the monitor.
    const fn right(&self) -> i64 {
        self.left as i64 + self.width as i64
    }

    /// Returns the first row after the monitor.
    const fn bottom(&self) -> i64 {
        self.top as i64 + self.height as i64
    }

    /// Returns the whole monitor as a desktop rectangle.
    #[must_use]
    pub const fn rect(&self) -> DesktopRect {
        DesktopRect {
            left: self.left,
            top: self.top,
            width: self.width,
            height: self.height,
        }
    }
}

/// One region inside one monitor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MonitorRegion {
    /// Index of the monitor.
    pub monitor: u32,
    /// Left edge in pixels of the monitor.
    pub left: u32,
    /// Top edge in pixels of the monitor.
    pub top: u32,
    /// Width in pixels. It is even.
    pub width: u32,
    /// Height in pixels. It is even.
    pub height: u32,
    /// The same region in desktop pixels.
    pub desktop: DesktopRect,
}

/// Finds the monitor that contains a desktop point.
#[must_use]
pub fn monitor_at(monitors: &[MonitorArea], x: i32, y: i32) -> Option<MonitorArea> {
    monitors.iter().copied().find(|monitor| {
        let (x, y) = (i64::from(x), i64::from(y));
        x >= i64::from(monitor.left)
            && x < monitor.right()
            && y >= i64::from(monitor.top)
            && y < monitor.bottom()
    })
}

/// Maps a desktop rectangle to the monitor that holds most of it.
///
/// The part outside that monitor is cut off. The width and the height become
/// even, because the video encoder needs that. Returns `None` when no monitor
/// holds a part of at least [`MIN_REGION_EDGE`] pixels in each direction.
#[must_use]
pub fn region_on_monitor(monitors: &[MonitorArea], region: DesktopRect) -> Option<MonitorRegion> {
    let right = i64::from(region.left) + i64::from(region.width);
    let bottom = i64::from(region.top) + i64::from(region.height);
    monitors
        .iter()
        .filter_map(|monitor| {
            let left = i64::from(region.left).max(i64::from(monitor.left));
            let top = i64::from(region.top).max(i64::from(monitor.top));
            let width = (right.min(monitor.right()) - left).max(0) as u32 & !1;
            let height = (bottom.min(monitor.bottom()) - top).max(0) as u32 & !1;
            (width >= MIN_REGION_EDGE && height >= MIN_REGION_EDGE).then(|| MonitorRegion {
                monitor: monitor.index,
                left: (left - i64::from(monitor.left)) as u32,
                top: (top - i64::from(monitor.top)) as u32,
                width,
                height,
                desktop: DesktopRect {
                    left: left as i32,
                    top: top as i32,
                    width,
                    height,
                },
            })
        })
        // The first monitor wins a tie, so the result does not depend on
        // the iteration order of `max_by_key`.
        .rev()
        .max_by_key(|found| u64::from(found.width) * u64::from(found.height))
}

/// What a video recording captures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureTarget {
    /// The whole monitor with this index.
    Monitor(u32),
    /// The whole monitor that contains this desktop point.
    Point(i32, i32),
    /// One rectangle of the desktop.
    Region(DesktopRect),
}

/// The monitor that a recording captures and the part of it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapturePlan {
    /// The monitor.
    pub monitor: MonitorArea,
    /// The recorded part. `None` records the whole monitor.
    pub region: Option<MonitorRegion>,
}

/// Resolves a capture target against the attached monitors.
///
/// A point outside every monitor selects the primary monitor, which has its
/// top left corner at the desktop origin.
pub fn plan_capture(
    monitors: &[MonitorArea],
    target: CaptureTarget,
) -> Result<CapturePlan, String> {
    let whole = |monitor: MonitorArea| CapturePlan {
        monitor,
        region: None,
    };
    match target {
        CaptureTarget::Monitor(index) => monitors
            .iter()
            .find(|monitor| monitor.index == index)
            .map(|monitor| whole(*monitor))
            .ok_or_else(|| format!("Monitor {index} is not attached")),
        CaptureTarget::Point(x, y) => monitor_at(monitors, x, y)
            .or_else(|| monitor_at(monitors, 0, 0))
            .or_else(|| monitors.first().copied())
            .map(whole)
            .ok_or_else(|| "No monitor is attached".to_owned()),
        CaptureTarget::Region(region) => {
            let found = region_on_monitor(monitors, region).ok_or_else(|| {
                format!(
                    "The region is too small or outside the desktop. Select at least \
                     {MIN_REGION_EDGE} x {MIN_REGION_EDGE} pixels."
                )
            })?;
            monitors
                .iter()
                .find(|monitor| monitor.index == found.monitor)
                .map(|monitor| CapturePlan {
                    monitor: *monitor,
                    region: Some(found),
                })
                .ok_or_else(|| "No monitor is attached".to_owned())
        }
    }
}

/// Returns the position of the mouse pointer in desktop pixels.
#[cfg(windows)]
#[must_use]
pub fn pointer_position() -> (i32, i32) {
    use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};

    let mut pointer = POINT::default();
    // SAFETY: The pointer refers to a live local. A failure leaves the
    // origin, which lies on the primary monitor.
    let _ = unsafe { GetCursorPos(&mut pointer) };
    (pointer.x, pointer.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two monitors side by side, the second one left of the primary one.
    fn monitors() -> [MonitorArea; 2] {
        [
            MonitorArea {
                index: 0,
                left: 0,
                top: 0,
                width: 2_560,
                height: 1_440,
            },
            MonitorArea {
                index: 1,
                left: -1_920,
                top: 200,
                width: 1_920,
                height: 1_080,
            },
        ]
    }

    /// Builds a desktop rectangle.
    fn rect(left: i32, top: i32, width: u32, height: u32) -> DesktopRect {
        DesktopRect {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn a_point_finds_its_monitor() {
        let monitors = monitors();
        assert_eq!(monitor_at(&monitors, 0, 0).map(|m| m.index), Some(0));
        assert_eq!(
            monitor_at(&monitors, 2_559, 1_439).map(|m| m.index),
            Some(0)
        );
        assert_eq!(monitor_at(&monitors, -1, 200).map(|m| m.index), Some(1));
        assert_eq!(monitor_at(&monitors, -1, 199), None);
        assert_eq!(monitor_at(&monitors, 2_560, 0), None);
        assert_eq!(monitors[1].rect(), rect(-1_920, 200, 1_920, 1_080));
    }

    #[test]
    fn a_region_maps_to_monitor_pixels_with_an_even_size() {
        let monitors = monitors();
        let found = region_on_monitor(&monitors, rect(100, 50, 801, 601)).unwrap();
        assert_eq!(
            (
                found.monitor,
                found.left,
                found.top,
                found.width,
                found.height
            ),
            (0, 100, 50, 800, 600)
        );
        assert_eq!(found.desktop, rect(100, 50, 800, 600));
        let second = region_on_monitor(&monitors, rect(-1_900, 300, 640, 480)).unwrap();
        assert_eq!(
            (second.monitor, second.left, second.top),
            (1, 20, 100),
            "{second:?}"
        );
        assert_eq!(second.desktop, rect(-1_900, 300, 640, 480));
    }

    #[test]
    fn a_region_across_two_monitors_keeps_the_larger_part() {
        let monitors = monitors();
        let found = region_on_monitor(&monitors, rect(-300, 400, 400, 300)).unwrap();
        assert_eq!(found.monitor, 1);
        assert_eq!((found.left, found.top), (1_620, 200));
        assert_eq!((found.width, found.height), (300, 300));
        assert_eq!(found.desktop, rect(-300, 400, 300, 300));
        // Equal parts go to the first monitor.
        let tie = region_on_monitor(&monitors, rect(-200, 400, 400, 300)).unwrap();
        assert_eq!(tie.monitor, 0);
        // The part past the edge of the monitor is cut off.
        let edge = region_on_monitor(&monitors, rect(2_000, 1_000, 5_000, 5_000)).unwrap();
        assert_eq!((edge.width, edge.height), (560, 440));
    }

    #[test]
    fn a_target_resolves_to_a_monitor_and_a_part_of_it() {
        let monitors = monitors();
        let by_index = plan_capture(&monitors, CaptureTarget::Monitor(1)).unwrap();
        assert_eq!((by_index.monitor.index, by_index.region), (1, None));
        assert!(
            plan_capture(&monitors, CaptureTarget::Monitor(7))
                .unwrap_err()
                .contains("Monitor 7")
        );
        let by_point = plan_capture(&monitors, CaptureTarget::Point(-5, 300)).unwrap();
        assert_eq!(by_point.monitor.index, 1);
        // A point outside every monitor selects the primary monitor.
        let outside = plan_capture(&monitors, CaptureTarget::Point(9_999, 9_999)).unwrap();
        assert_eq!(outside.monitor.index, 0);
        let part = plan_capture(
            &monitors,
            CaptureTarget::Region(rect(-1_000, 500, 400, 300)),
        )
        .unwrap();
        assert_eq!(part.monitor.index, 1);
        assert_eq!(
            part.region.map(|region| (region.left, region.top)),
            Some((920, 300))
        );
        assert!(
            plan_capture(&monitors, CaptureTarget::Region(rect(0, 0, 10, 10)))
                .unwrap_err()
                .contains("too small")
        );
        assert!(plan_capture(&[], CaptureTarget::Point(0, 0)).is_err());
    }

    #[test]
    fn a_region_that_is_too_small_or_off_the_desktop_gives_nothing() {
        let monitors = monitors();
        assert_eq!(region_on_monitor(&monitors, rect(10, 10, 31, 500)), None);
        assert_eq!(region_on_monitor(&monitors, rect(10, 10, 500, 8)), None);
        assert_eq!(
            region_on_monitor(&monitors, rect(9_000, 9_000, 500, 500)),
            None
        );
        assert_eq!(region_on_monitor(&[], rect(0, 0, 500, 500)), None);
        assert!(region_on_monitor(&monitors, rect(10, 10, 33, 33)).is_some());
    }
}
