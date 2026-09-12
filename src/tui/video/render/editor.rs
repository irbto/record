//! Rendering for the integrated MP4 clip editor.

mod presentation;

use std::time::Duration;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols,
    text::{Line as TextLine, Span},
    widgets::{Block, BorderType, Borders, Clear, LineGauge, Padding, Paragraph, Wrap},
};

use crate::{clip::SelectionHandle, raster::RasterRenderer, video::clip::TrimProgress};

pub(in crate::tui::video) use presentation::video_waveform_lines;
#[cfg(test)]
pub(in crate::tui::video) use presentation::{FadeColumns, TimelineColumns};
use presentation::{
    fade_columns, timeline_columns, video_editor_header_line, video_editor_tool_line,
};

use super::super::super::{
    AMBER, BORDER, CYAN, GREEN, MUTED, PURPLE, RED,
    utils::{centered, help_line, panel},
};
use super::super::{VideoApp, VideoEditor, tools::EditorTool, waveform::EditorWaveform};

impl VideoApp {
    /// Draws the integrated frame-selection editor around the raster picture.
    pub(super) fn render_video_editor(
        &mut self,
        frame: &mut Frame,
        mut raster: Option<&mut RasterRenderer>,
    ) {
        let area = frame.area();
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(4),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);
        self.render_video_editor_header(frame, layout[0]);

        let preview_block = panel(" FRAME PREVIEW ", PURPLE);
        let preview_area = preview_block.inner(layout[1]);
        frame.render_widget(preview_block, layout[1]);
        let edit_overlay = self
            .editor
            .as_ref()
            .and_then(VideoEditor::raster_edit_overlay);
        let rendered = raster.as_deref_mut().is_some_and(|raster| {
            raster.render_with_edit_overlay(frame, preview_area, edit_overlay)
        });
        if !rendered {
            let message = self
                .editor
                .as_ref()
                .and_then(|editor| editor.error.as_deref())
                .or_else(|| raster.as_deref().and_then(RasterRenderer::error))
                .unwrap_or("Decoding the selected frame");
            frame.render_widget(
                Paragraph::new(message).alignment(Alignment::Center).style(
                    Style::default().fg(
                        if self
                            .editor
                            .as_ref()
                            .is_some_and(|editor| editor.error.is_some())
                        {
                            RED
                        } else {
                            MUTED
                        },
                    ),
                ),
                preview_area,
            );
        }

        self.render_video_selection(frame, layout[2]);
        self.render_video_waveform(frame, layout[3]);
        self.render_video_tool_strip(frame, layout[4]);
        self.render_video_editor_footer(frame, layout[5]);
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.trim_job.is_some())
        {
            self.render_video_trim_loader(frame, centered(area, 68, 8));
        } else if self.show_help {
            self.render_video_editor_help(frame, centered(area, 70, 18));
        }
    }

    /// Draws selected bounds, current frame time, and editor state.
    fn render_video_editor_header(&self, frame: &mut Frame, area: Rect) {
        let Some(editor) = &self.editor else {
            return;
        };
        let (status, color) = if editor.trim_job.is_some() {
            ("◌ SAVING VIDEO", AMBER)
        } else if editor.error.is_some() {
            ("✗ ERROR", RED)
        } else if editor.playing {
            ("▶ PLAYING", GREEN)
        } else {
            ("Ⅱ SELECTING", PURPLE)
        };
        let name = editor
            .summary
            .output
            .file_name()
            .unwrap_or(editor.summary.output.as_os_str())
            .to_string_lossy();
        let title = TextLine::from(vec![
            Span::styled(" record ", Style::default().fg(GREEN).bold()),
            Span::styled("VIDEO CLIP ", Style::default().fg(MUTED)),
            Span::styled(name.into_owned(), Style::default().fg(PURPLE)),
        ]);
        let available = usize::from(area.width.saturating_sub(2));
        let header = Paragraph::new(video_editor_header_line(editor, status, color, available))
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(BORDER))
                    .title(title),
            )
            .alignment(Alignment::Center);
        frame.render_widget(header, area);
    }

    /// Draws the full source timeline with explicit start and end handles.
    fn render_video_selection(&self, frame: &mut Frame, area: Rect) {
        let Some(editor) = &self.editor else {
            return;
        };
        let block = panel(" CLIP RANGE ", CYAN);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }
        let width = usize::from(inner.width.max(1));
        let columns = timeline_columns(editor, width);
        let fades = fade_columns(
            columns,
            editor.selection.duration(),
            editor.edit_settings.video_fade_in,
            editor.edit_settings.video_fade_out,
            editor.active_tool == EditorTool::VideoFadeIn,
            editor.active_tool == EditorTool::VideoFadeOut,
        );
        let spans = (0..width)
            .map(|column| {
                if column == columns.start && columns.start + 1 == columns.end {
                    let symbol = match columns.active_handle {
                        Some(SelectionHandle::Start) => "▌",
                        Some(SelectionHandle::End) => "▐",
                        None => "│",
                    };
                    Span::styled(
                        symbol,
                        Style::default()
                            .fg(if columns.active_handle.is_some() {
                                AMBER
                            } else {
                                CYAN
                            })
                            .bold(),
                    )
                } else if column == columns.start {
                    let active = columns.active_handle == Some(SelectionHandle::Start);
                    Span::styled(
                        if active { "▌" } else { "▏" },
                        Style::default()
                            .fg(if active { AMBER } else { CYAN })
                            .bold(),
                    )
                } else if column + 1 == columns.end {
                    let active = columns.active_handle == Some(SelectionHandle::End);
                    Span::styled(
                        if active { "▐" } else { "▕" },
                        Style::default()
                            .fg(if active { AMBER } else { GREEN })
                            .bold(),
                    )
                } else if column == columns.playhead {
                    Span::styled("◆", Style::default().fg(Color::White).bold())
                } else if fades.fade_in_end == Some(column + 1) {
                    Span::styled(
                        "╱",
                        Style::default()
                            .fg(if fades.fade_in_active { AMBER } else { CYAN })
                            .bold(),
                    )
                } else if fades.fade_out_start == Some(column) {
                    Span::styled(
                        "╲",
                        Style::default()
                            .fg(if fades.fade_out_active { AMBER } else { PURPLE })
                            .bold(),
                    )
                } else if (columns.start..columns.end).contains(&column) {
                    Span::styled("━", Style::default().fg(GREEN))
                } else {
                    Span::styled("─", Style::default().fg(BORDER))
                }
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(TextLine::from(spans)), inner);
    }

    /// Draws an aligned audio-envelope track below the clip range.
    fn render_video_waveform(&self, frame: &mut Frame, area: Rect) {
        let Some(editor) = &self.editor else {
            return;
        };
        let waveform = editor.waveform.as_ref();
        let peaks = waveform.and_then(EditorWaveform::peaks).unwrap_or(&[]);
        let title = if editor.audio_preview_error.is_some() {
            " WAVEFORM · AUDIO PREVIEW UNAVAILABLE ".to_owned()
        } else {
            match waveform.and_then(EditorWaveform::status) {
                Some(status) => format!(" WAVEFORM · {status} "),
                None => " WAVEFORM ".to_owned(),
            }
        };
        let block = panel(&title, PURPLE);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }
        let columns = timeline_columns(editor, usize::from(inner.width));
        let fades = fade_columns(
            columns,
            editor.selection.duration(),
            editor.edit_settings.audio_fade_in,
            editor.edit_settings.audio_fade_out,
            editor.active_tool == EditorTool::AudioFadeIn,
            editor.active_tool == EditorTool::AudioFadeOut,
        );
        let lines = video_waveform_lines(
            peaks,
            usize::from(inner.width),
            usize::from(inner.height),
            columns,
            fades,
        );
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// Draws direct tool shortcuts, the active value, and export geometry.
    fn render_video_tool_strip(&self, frame: &mut Frame, area: Rect) {
        let Some(editor) = &self.editor else {
            return;
        };
        let available = usize::from(area.width.saturating_sub(2));
        frame.render_widget(
            Paragraph::new(video_editor_tool_line(editor, available))
                .alignment(Alignment::Center)
                .block(panel(" EDIT TOOL ", AMBER)),
            area,
        );
    }

    /// Draws the compact control map and any actionable editor error.
    fn render_video_editor_footer(&self, frame: &mut Frame, area: Rect) {
        let Some(editor) = &self.editor else {
            return;
        };
        let compact = area.width < 85;
        if editor.trim_job.is_some() {
            let (saving, safety) = if area.width < 68 {
                (
                    "Saving video… Please wait.",
                    "The original is safe until saving finishes.",
                )
            } else {
                (
                    "Saving is in progress. Keep this window open.",
                    "Your original stays in place until the trimmed video is complete.",
                )
            };
            frame.render_widget(
                Paragraph::new(vec![
                    TextLine::from(Span::styled(saving, Style::default().fg(AMBER).bold())),
                    TextLine::from(Span::styled(safety, Style::default().fg(MUTED))),
                ])
                .block(
                    Block::new()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(BORDER)),
                ),
                area,
            );
            return;
        }
        let error = editor.error.as_deref();
        let play_action = if editor.playing {
            "pause"
        } else if editor.resume_pending {
            "resume"
        } else {
            "play"
        };
        let first = if let Some(error) = error {
            TextLine::from(Span::styled(error.to_owned(), Style::default().fg(RED)))
        } else if compact {
            TextLine::from(vec![
                Span::styled(" Tab/⇧Tab ", Style::default().fg(AMBER).bold()),
                Span::styled("tools  ", Style::default().fg(MUTED)),
                Span::styled(" T/V/A/C ", Style::default().fg(AMBER).bold()),
                Span::styled("group  ", Style::default().fg(MUTED)),
                Span::styled(" ←/→ ", Style::default().fg(CYAN).bold()),
                Span::styled("adjust  ", Style::default().fg(MUTED)),
                Span::styled(" Space ", Style::default().fg(GREEN).bold()),
                Span::styled(play_action, Style::default().fg(MUTED)),
            ])
        } else {
            TextLine::from(vec![
                Span::styled(" Tab / Shift+Tab ", Style::default().fg(AMBER).bold()),
                Span::styled("tools   ", Style::default().fg(MUTED)),
                Span::styled(" T / V / A / C ", Style::default().fg(AMBER).bold()),
                Span::styled("groups   ", Style::default().fg(MUTED)),
                Span::styled(" ←/→ ", Style::default().fg(CYAN).bold()),
                Span::styled("adjust   ", Style::default().fg(MUTED)),
                Span::styled(" Space ", Style::default().fg(GREEN).bold()),
                Span::styled(play_action, Style::default().fg(MUTED)),
            ])
        };
        let second = if compact {
            TextLine::from(vec![
                Span::styled(" Bksp ", Style::default().fg(GREEN).bold()),
                Span::styled("start  ", Style::default().fg(MUTED)),
                Span::styled(" R ", Style::default().fg(CYAN).bold()),
                Span::styled("all  ", Style::default().fg(MUTED)),
                Span::styled(" Enter ", Style::default().fg(PURPLE).bold()),
                Span::styled("apply  ", Style::default().fg(MUTED)),
                Span::styled(" Esc ", Style::default().fg(RED).bold()),
                Span::styled("original  ", Style::default().fg(MUTED)),
                Span::styled(" ? ", Style::default().fg(AMBER).bold()),
                Span::styled("help", Style::default().fg(MUTED)),
            ])
        } else {
            TextLine::from(vec![
                Span::styled(" Backspace ", Style::default().fg(GREEN).bold()),
                Span::styled("restart   ", Style::default().fg(MUTED)),
                Span::styled(" R ", Style::default().fg(CYAN).bold()),
                Span::styled("reset all edits   ", Style::default().fg(MUTED)),
                Span::styled(" Enter ", Style::default().fg(PURPLE).bold()),
                Span::styled("apply edits   ", Style::default().fg(MUTED)),
                Span::styled(" Esc ", Style::default().fg(RED).bold()),
                Span::styled("keep original   ", Style::default().fg(MUTED)),
                Span::styled(" ? ", Style::default().fg(AMBER).bold()),
                Span::styled("help", Style::default().fg(MUTED)),
            ])
        };
        frame.render_widget(
            Paragraph::new(vec![first, second]).block(
                Block::new()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(BORDER)),
            ),
            area,
        );
    }

    /// Draws integrated editor help without mentioning a review command.
    fn render_video_editor_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            TextLine::from("Choose the range, fades, and crop to apply to this recording."),
            TextLine::from(""),
            help_line("Tab / Shift+Tab", "select the next or previous edit tool"),
            help_line(
                "T / V / A / C",
                "select range, video fade, audio fade, or crop",
            ),
            help_line(
                "Arrow keys",
                "adjust the selected tool in its screen direction",
            ),
            help_line("No modifier / Shift", "0.1 / 1 second; crop 2 / 20 pixels"),
            help_line("Ctrl + arrows", "one frame; crop remains 2 pixels"),
            help_line("Space / P", "play or pause the selected frames"),
            help_line("Backspace", "return to the start marker"),
            help_line("R / Home", "reset the range, fades, and crop"),
            help_line("Enter", "apply the range, effects, and crop to the MP4"),
            help_line("Esc", "keep the original MP4 and finish"),
            TextLine::from(""),
            TextLine::from(Span::styled(
                "Frame decoding and MP4 saving run on background threads.",
                Style::default().fg(MUTED),
            )),
        ])
        .wrap(Wrap { trim: true })
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(PURPLE))
                .padding(Padding::uniform(1))
                .title(TextLine::from(" VIDEO CLIP HELP ").fg(PURPLE).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(help, area);
    }

    /// Draws an animated, staged save panel while the replacement worker runs.
    fn render_video_trim_loader(&self, frame: &mut Frame, area: Rect) {
        let Some(job) = self
            .editor
            .as_ref()
            .and_then(|editor| editor.trim_job.as_ref())
        else {
            return;
        };
        let progress = *job
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let elapsed = job.started.elapsed();
        let compact = area.width < 68;
        let (message, detail, ratio) = match progress {
            TrimProgress::Preparing => (
                "Preparing your video…",
                if compact {
                    "Getting the selected frames ready."
                } else {
                    "Closing the preview and getting the selected frames ready."
                },
                None,
            ),
            TrimProgress::Processing {
                completed_frames,
                total_frames,
            } => (
                "Processing the selected video…",
                if compact {
                    "The original stays safe while this runs."
                } else {
                    "Your original stays untouched until the new file is ready."
                },
                Some(progress_ratio(completed_frames, total_frames)),
            ),
            TrimProgress::Finishing => (
                "Finishing the video file…",
                if compact {
                    "Finishing the new video file."
                } else {
                    "Putting the final video and audio together."
                },
                None,
            ),
            TrimProgress::Saving => (
                "Saving the trimmed video…",
                if compact {
                    "Replacing the original safely."
                } else {
                    "Replacing the original only after the new file is complete."
                },
                None,
            ),
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(AMBER))
            .padding(Padding::horizontal(2))
            .title(TextLine::from(" SAVING YOUR VIDEO ").fg(AMBER).bold());
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .flex(Flex::Center)
        .split(inner);
        frame.render_widget(
            Paragraph::new(TextLine::from(vec![
                Span::styled(
                    format!("{} ", spinner_frame(elapsed)),
                    Style::default().fg(AMBER).bold(),
                ),
                Span::styled(message, Style::default().fg(Color::White).bold()),
            ]))
            .alignment(Alignment::Center),
            rows[0],
        );
        if let Some(ratio) = ratio {
            frame.render_widget(
                LineGauge::default()
                    .ratio(ratio)
                    .label(format!("{:.0}%", ratio * 100.0))
                    .filled_style(Style::default().fg(GREEN).bold())
                    .unfilled_style(Style::default().fg(BORDER))
                    .filled_symbol(symbols::line::THICK_HORIZONTAL)
                    .unfilled_symbol(symbols::line::HORIZONTAL),
                rows[1],
            );
        }
        frame.render_widget(
            Paragraph::new(detail)
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center),
            rows[2],
        );
        frame.render_widget(
            Paragraph::new(format!("Elapsed time: {}", format_loader_time(elapsed)))
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center),
            rows[3],
        );
    }
}

/// Selects one deterministic animation frame from an elapsed duration.
pub(in crate::tui::video) fn spinner_frame(elapsed: Duration) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let index = (elapsed.as_millis() / 80 % FRAMES.len() as u128) as usize;
    FRAMES[index]
}

/// Returns a bounded completion ratio for a determinate background stage.
pub(in crate::tui::video) fn progress_ratio(completed: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (completed as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// Formats loader elapsed time without implying an estimated completion time.
fn format_loader_time(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
