//! The dashboard look, built from the palette that the terminal interface uses.

use egui::{Color32, CornerRadius, Stroke, Visuals};
use record_core::theme::{self, Rgb};

/// Converts a shared theme color to the egui color type.
#[must_use]
pub const fn color(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

/// Accent color for focus, selection, and titles.
pub const CYAN: Color32 = color(theme::CYAN);
/// Success and active state.
pub const GREEN: Color32 = color(theme::GREEN);
/// Errors and the recording indicator.
pub const RED: Color32 = color(theme::RED);
/// Work in progress.
pub const AMBER: Color32 = color(theme::AMBER);
/// Secondary text.
pub const MUTED: Color32 = color(theme::MUTED);
/// Panel outlines.
pub const BORDER: Color32 = color(theme::BORDER);
/// Primary text.
pub const TEXT: Color32 = color(theme::TEXT);
/// Background of a selected or hovered control.
pub const SELECTED: Color32 = color(theme::SELECTED);
/// Window background.
pub const BACKGROUND: Color32 = color(theme::BACKGROUND);
/// Panel background.
pub const PANEL: Color32 = color(theme::PANEL);

/// Applies the dark `record` look to an egui context.
pub fn apply(context: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = PANEL;
    visuals.faint_bg_color = PANEL;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0, CYAN);
    visuals.hyperlink_color = CYAN;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    let radius = CornerRadius::same(8);
    for (widget, fill, outline) in [
        (&mut visuals.widgets.noninteractive, PANEL, BORDER),
        (&mut visuals.widgets.inactive, PANEL, BORDER),
        (&mut visuals.widgets.hovered, SELECTED, CYAN),
        (&mut visuals.widgets.active, SELECTED, CYAN),
        (&mut visuals.widgets.open, SELECTED, CYAN),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::new(1.0, outline);
        widget.fg_stroke = Stroke::new(1.0, TEXT);
        widget.corner_radius = radius;
    }
    context.set_visuals(visuals);
    context.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.interact_size.y = 34.0;
    });
}
