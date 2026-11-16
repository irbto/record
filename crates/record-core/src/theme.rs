//! Colors shared by every user interface of `record`.
//!
//! The terminal program and the desktop program convert these values to the
//! color type of their own framework, so both show the same palette.

/// One opaque color as red, green, and blue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Accent color for focus, selection, and titles.
pub const CYAN: Rgb = Rgb(65, 214, 224);
/// Success and active state.
pub const GREEN: Rgb = Rgb(72, 214, 147);
/// Errors and the recording indicator.
pub const RED: Rgb = Rgb(247, 75, 103);
/// Work in progress and warnings.
pub const AMBER: Rgb = Rgb(244, 183, 73);
/// Secondary accent.
pub const PURPLE: Rgb = Rgb(172, 122, 255);
/// Secondary text.
pub const MUTED: Rgb = Rgb(126, 139, 158);
/// Panel outlines.
pub const BORDER: Rgb = Rgb(55, 65, 82);
/// Primary text.
pub const TEXT: Rgb = Rgb(226, 232, 240);
/// Background of a selected row.
pub const SELECTED: Rgb = Rgb(30, 41, 59);
/// Window background.
pub const BACKGROUND: Rgb = Rgb(13, 17, 23);
/// Panel background.
pub const PANEL: Rgb = Rgb(22, 27, 38);
