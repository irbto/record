//! Pure projection and text helpers for the integrated video editor.

use std::time::Duration;

use ratatui::{
    style::{Color, Style},
    text::{Line as TextLine, Span},
};

use crate::clip::SelectionHandle;

use super::super::super::super::{AMBER, BORDER, CYAN, GREEN, MUTED, PURPLE, utils::format_clock};
use super::super::super::{
    VideoEditor, editor_frame, editor_position_ratio, selection_end_time, selection_start_time,
    tools::EditorTool,
};

/// Maps editor state to the shared, zero-based columns inside timeline panels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::tui::video) struct TimelineColumns {
    pub(in crate::tui::video) start: usize,
    pub(in crate::tui::video) end: usize,
    pub(in crate::tui::video) playhead: usize,
    pub(in crate::tui::video) active_handle: Option<SelectionHandle>,
}

/// Optional fade regions expressed in the same columns as the waveform.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::tui::video) struct FadeColumns {
    /// Exclusive end of the fade-in region.
    pub(in crate::tui::video) fade_in_end: Option<usize>,
    /// Inclusive start of the fade-out region.
    pub(in crate::tui::video) fade_out_start: Option<usize>,
    /// Reports that the fade-in value owns the arrow keys.
    pub(in crate::tui::video) fade_in_active: bool,
    /// Reports that the fade-out value owns the arrow keys.
    pub(in crate::tui::video) fade_out_active: bool,
}

/// Returns shared handle and playhead columns for every timeline track.
pub(super) fn timeline_columns(editor: &VideoEditor, width: usize) -> TimelineColumns {
    let width = width.max(1);
    let start = ((editor.selection.normalized_start() * width as f64).floor() as usize)
        .min(width.saturating_sub(1));
    let end = ((editor.selection.normalized_end() * width as f64).ceil() as usize)
        .clamp(start + 1, width);
    let playhead = ((editor_position_ratio(editor) * width as f64).floor() as usize)
        .min(width.saturating_sub(1));
    TimelineColumns {
        start,
        end,
        playhead,
        active_handle: editor.active_tool.range_handle(),
    }
}

/// Maps a pair of fade durations into the selected timeline span.
pub(super) fn fade_columns(
    columns: TimelineColumns,
    selected_duration: Duration,
    fade_in: Duration,
    fade_out: Duration,
    fade_in_active: bool,
    fade_out_active: bool,
) -> FadeColumns {
    let selected_width = columns.end.saturating_sub(columns.start);
    let fade_width = |duration: Duration| {
        if duration.is_zero() || selected_duration.is_zero() || selected_width == 0 {
            return None;
        }
        let ratio = (duration.as_secs_f64() / selected_duration.as_secs_f64()).clamp(0.0, 1.0);
        Some(((ratio * selected_width as f64).ceil() as usize).clamp(1, selected_width))
    };
    FadeColumns {
        fade_in_end: fade_width(fade_in).map(|width| columns.start + width),
        fade_out_start: fade_width(fade_out).map(|width| columns.end - width),
        fade_in_active,
        fade_out_active,
    }
}

/// Builds waveform rows with selection, handle, playhead, and optional fade styling.
pub(in crate::tui::video) fn video_waveform_lines(
    peaks: &[f32],
    width: usize,
    height: usize,
    columns: TimelineColumns,
    fades: FadeColumns,
) -> Vec<TextLine<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let amplitudes = waveform_amplitudes(peaks, width);
    let center = height / 2;
    let fade_in_end = fades
        .fade_in_end
        .map(|end| end.clamp(columns.start, columns.end));
    let fade_out_start = fades
        .fade_out_start
        .map(|start| start.clamp(columns.start, columns.end));
    (0..height)
        .map(|row| {
            let spans = amplitudes
                .iter()
                .enumerate()
                .map(|(column, amplitude)| {
                    let at_shared_handle =
                        column == columns.start && columns.start.saturating_add(1) == columns.end;
                    let at_start = column == columns.start;
                    let at_end = column.saturating_add(1) == columns.end;
                    let at_playhead = column == columns.playhead;
                    let fade_in_edge = fade_in_end.is_some_and(|end| column + 1 == end);
                    let fade_out_edge = fade_out_start == Some(column);
                    let symbol = if at_playhead || at_shared_handle || at_start || at_end {
                        "│"
                    } else if row == center && fade_in_edge {
                        "╱"
                    } else if row == center && fade_out_edge {
                        "╲"
                    } else {
                        waveform_symbol(*amplitude, row, height, peaks.is_empty())
                    };
                    let (color, bold) = if at_playhead {
                        (Color::White, true)
                    } else if at_shared_handle {
                        (
                            if columns.active_handle.is_some() {
                                AMBER
                            } else {
                                CYAN
                            },
                            true,
                        )
                    } else if at_start {
                        (
                            if columns.active_handle == Some(SelectionHandle::Start) {
                                AMBER
                            } else {
                                CYAN
                            },
                            true,
                        )
                    } else if at_end {
                        (
                            if columns.active_handle == Some(SelectionHandle::End) {
                                AMBER
                            } else {
                                GREEN
                            },
                            true,
                        )
                    } else if !(columns.start..columns.end).contains(&column) {
                        (BORDER, false)
                    } else {
                        let fading_in = fade_in_end.is_some_and(|end| column < end);
                        let fading_out = fade_out_start.is_some_and(|start| column >= start);
                        match (fading_in, fading_out) {
                            (true, true) => (AMBER, fade_in_edge || fade_out_edge),
                            (true, false) => (
                                if fades.fade_in_active { AMBER } else { CYAN },
                                fade_in_edge,
                            ),
                            (false, true) => (
                                if fades.fade_out_active { AMBER } else { PURPLE },
                                fade_out_edge,
                            ),
                            (false, false) => (GREEN, false),
                        }
                    };
                    let style = if bold {
                        Style::default().fg(color).bold()
                    } else {
                        Style::default().fg(color)
                    };
                    Span::styled(symbol, style)
                })
                .collect::<Vec<_>>();
            TextLine::from(spans)
        })
        .collect()
}

/// Reduces a source peak envelope to one maximum amplitude per terminal column.
fn waveform_amplitudes(peaks: &[f32], width: usize) -> Vec<f32> {
    if peaks.is_empty() {
        return vec![0.0; width];
    }
    (0..width)
        .map(|column| {
            let start = column.saturating_mul(peaks.len()) / width;
            let end = (column + 1)
                .saturating_mul(peaks.len())
                .div_ceil(width)
                .clamp(start + 1, peaks.len());
            peaks[start..end]
                .iter()
                .filter(|peak| peak.is_finite())
                .map(|peak| peak.abs().clamp(0.0, 1.0))
                .fold(0.0_f32, f32::max)
        })
        .collect()
}

/// Selects a symmetric waveform or baseline glyph for one cell.
fn waveform_symbol(amplitude: f32, row: usize, height: usize, empty: bool) -> &'static str {
    let center = height / 2;
    if empty || amplitude == 0.0 {
        return if row == center { "─" } else { " " };
    }
    if row == center {
        return "█";
    }
    let center_position = (height.saturating_sub(1)) as f32 / 2.0;
    let half_height = (height as f32 / 2.0).max(1.0);
    let threshold = ((row as f32 - center_position).abs() + 0.5) / half_height;
    if amplitude >= threshold.min(1.0) {
        "█"
    } else {
        " "
    }
}

/// Builds the richest edit-tool summary that fits the full-width strip.
pub(super) fn video_editor_tool_line(editor: &VideoEditor, available: usize) -> TextLine<'static> {
    let active = editor.active_tool.label();
    let value = active_tool_value(editor);
    let (output_width, output_height) = editor_output_geometry(editor);
    let key = Style::default().fg(AMBER).bold();
    let label = Style::default().fg(MUTED);
    let active_style = Style::default().fg(PURPLE).bold();
    let value_style = Style::default().fg(Color::White).bold();
    let output_style = Style::default().fg(GREEN).bold();
    let full = TextLine::from(vec![
        Span::styled(" T ", key),
        Span::styled("range   ", label),
        Span::styled(" V ", key),
        Span::styled("video fade   ", label),
        Span::styled(" A ", key),
        Span::styled("audio fade   ", label),
        Span::styled(" C ", key),
        Span::styled("crop   ACTIVE ", label),
        Span::styled(active.to_owned(), active_style),
        Span::raw(" "),
        Span::styled(value.clone(), value_style),
        Span::styled("   OUTPUT ", label),
        Span::styled(format!("{output_width}x{output_height}"), output_style),
    ]);
    let compact = TextLine::from(vec![
        Span::styled(" T ", key),
        Span::styled("Range ·", label),
        Span::styled(" V ", key),
        Span::styled("VFade ·", label),
        Span::styled(" A ", key),
        Span::styled("AFade ·", label),
        Span::styled(" C ", key),
        Span::styled("Crop · ", label),
        Span::styled(active.to_owned(), active_style),
        Span::raw(" "),
        Span::styled(value.clone(), value_style),
        Span::styled(" · OUT ", label),
        Span::styled(format!("{output_width}x{output_height}"), output_style),
    ]);
    let tiny = TextLine::from(vec![
        Span::styled(" T/V/A/C ", key),
        Span::styled("· ", label),
        Span::styled(active.to_owned(), active_style),
        Span::raw(" "),
        Span::styled(value, value_style),
        Span::styled(" · ", label),
        Span::styled(format!("{output_width}x{output_height}"), output_style),
    ]);
    [full, compact, tiny]
        .into_iter()
        .find(|line| line.width() <= available)
        .unwrap_or_else(|| {
            TextLine::from(vec![
                Span::styled(active.to_owned(), active_style),
                Span::styled(" · ", label),
                Span::styled(format!("{output_width}x{output_height}"), output_style),
            ])
        })
}

/// Formats the value currently owned by the editor arrows.
fn active_tool_value(editor: &VideoEditor) -> String {
    match editor.active_tool {
        EditorTool::RangeStart => format_clock(selection_start_time(editor)),
        EditorTool::RangeEnd => format_clock(selection_end_time(editor)),
        EditorTool::VideoFadeIn => format_edit_duration(editor.edit_settings.video_fade_in),
        EditorTool::VideoFadeOut => format_edit_duration(editor.edit_settings.video_fade_out),
        EditorTool::AudioFadeIn => format_edit_duration(editor.edit_settings.audio_fade_in),
        EditorTool::AudioFadeOut => format_edit_duration(editor.edit_settings.audio_fade_out),
        EditorTool::CropLeft => format!("{}px", editor.edit_settings.crop_left),
        EditorTool::CropRight => format!("{}px", editor.edit_settings.crop_right),
        EditorTool::CropTop => format!("{}px", editor.edit_settings.crop_top),
        EditorTool::CropBottom => format!("{}px", editor.edit_settings.crop_bottom),
    }
}

/// Formats a fade duration compactly while retaining frame-sized values.
fn format_edit_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if duration.subsec_nanos() == 0 {
        format!("{seconds:.0}s")
    } else if duration.subsec_millis().is_multiple_of(100)
        && duration.subsec_nanos().is_multiple_of(1_000_000)
    {
        format!("{seconds:.1}s")
    } else {
        format!("{seconds:.2}s")
    }
}

/// Returns the post-crop dimensions advertised by the edit-tool strip.
fn editor_output_geometry(editor: &VideoEditor) -> (u32, u32) {
    let width = editor
        .summary
        .width
        .saturating_sub(editor.edit_settings.crop_left)
        .saturating_sub(editor.edit_settings.crop_right)
        .max(2);
    let height = editor
        .summary
        .height
        .saturating_sub(editor.edit_settings.crop_top)
        .saturating_sub(editor.edit_settings.crop_bottom)
        .max(2);
    (width, height)
}

/// Builds the richest one-line editor summary that fits inside the header.
pub(super) fn video_editor_header_line(
    editor: &VideoEditor,
    status: &str,
    status_color: Color,
    available: usize,
) -> TextLine<'static> {
    let active = editor.active_tool.label();
    let status_style = Style::default().fg(status_color).bold();
    let full = TextLine::from(vec![
        Span::styled(status.to_owned(), status_style),
        Span::raw(format!("   FRAME {}   ", editor_frame(editor))),
        Span::styled(
            format!(
                "{}x{} @ {} fps · H.264 + AAC",
                editor.summary.width, editor.summary.height, editor.summary.fps
            ),
            Style::default().fg(MUTED),
        ),
        Span::styled("   START ", Style::default().fg(CYAN).bold()),
        Span::raw(format_clock(selection_start_time(editor))),
        Span::styled("   END ", Style::default().fg(GREEN).bold()),
        Span::raw(format_clock(selection_end_time(editor))),
        Span::styled("   LENGTH ", Style::default().fg(PURPLE).bold()),
        Span::raw(format_clock(editor.selection.duration())),
        Span::styled("   ACTIVE ", Style::default().fg(MUTED)),
        Span::styled(active, Style::default().fg(AMBER).bold()),
    ]);
    let compact = TextLine::from(vec![
        Span::styled(status.to_owned(), status_style),
        Span::styled(" · F", Style::default().fg(MUTED)),
        Span::raw(editor_frame(editor).to_string()),
        Span::styled(" · ", Style::default().fg(MUTED)),
        Span::styled(
            format!(
                "{}x{}@{}fps H.264/AAC",
                editor.summary.width, editor.summary.height, editor.summary.fps
            ),
            Style::default().fg(MUTED),
        ),
        Span::styled(" · ", Style::default().fg(MUTED)),
        Span::styled(
            format_clock(selection_start_time(editor)),
            Style::default().fg(CYAN).bold(),
        ),
        Span::styled("–", Style::default().fg(MUTED)),
        Span::styled(
            format_clock(selection_end_time(editor)),
            Style::default().fg(GREEN).bold(),
        ),
        Span::styled(" · LEN ", Style::default().fg(PURPLE).bold()),
        Span::raw(format_clock(editor.selection.duration())),
        Span::styled(" · ACTIVE ", Style::default().fg(MUTED)),
        Span::styled(active, Style::default().fg(AMBER).bold()),
    ]);
    let narrow = TextLine::from(vec![
        Span::styled(status.to_owned(), status_style),
        Span::styled(" · ", Style::default().fg(MUTED)),
        Span::styled(
            format_clock(selection_start_time(editor)),
            Style::default().fg(CYAN).bold(),
        ),
        Span::styled("–", Style::default().fg(MUTED)),
        Span::styled(
            format_clock(selection_end_time(editor)),
            Style::default().fg(GREEN).bold(),
        ),
        Span::styled(" · LEN ", Style::default().fg(PURPLE).bold()),
        Span::raw(format_clock(editor.selection.duration())),
        Span::styled(" · ACTIVE ", Style::default().fg(MUTED)),
        Span::styled(active, Style::default().fg(AMBER).bold()),
    ]);
    let tiny = TextLine::from(vec![
        Span::styled(status.to_owned(), status_style),
        Span::styled(" · ", Style::default().fg(MUTED)),
        Span::styled(
            format_clock(selection_start_time(editor)),
            Style::default().fg(CYAN).bold(),
        ),
        Span::styled("–", Style::default().fg(MUTED)),
        Span::styled(
            format_clock(selection_end_time(editor)),
            Style::default().fg(GREEN).bold(),
        ),
        Span::styled(" · ", Style::default().fg(MUTED)),
        Span::styled(active, Style::default().fg(AMBER).bold()),
        Span::styled(" ACTIVE", Style::default().fg(MUTED)),
    ]);
    [full, compact, narrow, tiny]
        .into_iter()
        .find(|line| line.width() <= available)
        .unwrap_or_else(|| {
            TextLine::from(vec![
                Span::styled(status.to_owned(), status_style),
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(active, Style::default().fg(AMBER).bold()),
            ])
        })
}
