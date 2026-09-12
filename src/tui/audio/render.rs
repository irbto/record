//! Rendering for the system-audio interface and MP3 clip editor.

use std::{collections::VecDeque, time::Duration};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::{self, Marker},
    text::{Line as TextLine, Span},
    widgets::{
        Block, BorderType, Borders, Clear, LineGauge, Padding, Paragraph, Sparkline, Wrap,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use crate::{
    audio::SavedFileKind,
    clip::{ClipSelection, SelectionHandle, WaveformBin},
};

use super::super::{
    AMBER, BORDER, CYAN, CaptureState, GREEN, MUTED, PURPLE, RED,
    utils::{centered, compact_path, format_clock, help_line, panel},
};
use super::{App, ViewMode};

impl App {
    /// Draws the live view and all active overlays.
    pub(super) fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let saves_height = if self.clips_enabled { 5 } else { 0 };
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
            Constraint::Length(saves_height),
            Constraint::Length(3),
        ])
        .split(area);
        self.render_header(frame, layout[0]);
        self.render_visualization(frame, layout[1]);
        self.render_meters(frame, layout[2]);
        if self.clips_enabled {
            self.render_saves(frame, layout[3]);
        }
        self.render_footer(frame, layout[4]);
        if self.show_help {
            self.render_help(frame, centered(area, 66, 21));
        }
        if self.clip_prompt.is_some() {
            self.render_clip_prompt(frame, centered(area, 64, 8));
        }
        if self.clip_review.is_some() {
            let width = area.width.saturating_sub(4).min(110);
            let height = area.height.saturating_sub(2).min(26);
            self.render_clip_review(frame, centered(area, width, height));
        }
    }

    /// Draws capture state, elapsed time, format, and view mode.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let elapsed = self.elapsed();
        let (status, color) = match self.state {
            CaptureState::Starting => ("● STARTING", AMBER),
            CaptureState::Recording => ("● RECORDING", RED),
            CaptureState::Paused => ("Ⅱ PAUSED", AMBER),
            CaptureState::Previewing => ("▶ PREVIEW", GREEN),
            CaptureState::Finalizing => ("◌ FINALIZING", AMBER),
        };
        let title = TextLine::from(vec![
            Span::styled(" record ", Style::default().fg(RED).bold()),
            Span::styled("SYSTEM AUDIO", Style::default().fg(MUTED)),
        ]);
        let header = Paragraph::new(TextLine::from(vec![
            Span::styled(status, Style::default().fg(color).bold()),
            Span::raw(format!("   {elapsed}   ")),
            Span::styled(
                format!(
                    "{} kbps · {} kHz · stereo",
                    self.bitrate / 1_000,
                    self.sample_rate / 1_000
                ),
                Style::default().fg(MUTED),
            ),
            Span::raw("   "),
            Span::styled(self.view.label(), Style::default().fg(PURPLE).bold()),
        ]))
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

    /// Draws the active waveform, spectrum, or split arrangement.
    fn render_visualization(&self, frame: &mut Frame, area: Rect) {
        match self.view {
            ViewMode::Waveform => self.render_waveforms(frame, area),
            ViewMode::Spectrum => self.render_spectrum(frame, area),
            ViewMode::Split => {
                let split =
                    Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
                        .split(area);
                self.render_waveforms(frame, split[0]);
                self.render_spectrum(frame, split[1]);
            }
        }
    }

    /// Draws independent left and right live scopes.
    fn render_waveforms(&self, frame: &mut Frame, area: Rect) {
        let panels =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
        let (left, right) = self.waveform.channels();
        render_waveform(frame, panels[0], " LEFT · 6 SECOND SCOPE ", left, CYAN);
        render_waveform(frame, panels[1], " RIGHT · 6 SECOND SCOPE ", right, GREEN);
    }

    /// Draws the lazy logarithmic frequency display.
    fn render_spectrum(&self, frame: &mut Frame, area: Rect) {
        let Some(spectrum) = &self.spectrum else {
            frame.render_widget(
                Paragraph::new("Spectrum initializes on first use")
                    .alignment(Alignment::Center)
                    .block(panel(" SPECTRUM ", PURPLE)),
                area,
            );
            return;
        };
        let width = area.width.saturating_sub(2).max(1) as usize;
        let bins = spectrum.bins();
        let data = (0..width)
            .map(|column| {
                let position = column as f32 / width as f32;
                let index = ((position * position) * (bins.len() - 1) as f32) as usize;
                (bins[index] * 100.0).round() as u64
            })
            .collect::<Vec<_>>();
        let spectrum = Sparkline::default()
            .block(panel(" SPECTRUM · 20 Hz to 24 kHz ", PURPLE))
            .data(&data)
            .max(100)
            .style(Style::default().fg(PURPLE))
            .bar_set(symbols::bar::NINE_LEVELS);
        frame.render_widget(spectrum, area);
    }

    /// Draws current left and right peak meters.
    fn render_meters(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(2), Constraint::Length(2)]).split(area);
        let (left, right) = self.waveform.peaks();
        frame.render_widget(level_gauge(" L ", left, CYAN), rows[0]);
        frame.render_widget(level_gauge(" R ", right, GREEN), rows[1]);
    }

    /// Draws a window around the selected finalized file.
    fn render_saves(&self, frame: &mut Frame, area: Rect) {
        let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
        let selected = self.selected_save.unwrap_or(0);
        let maximum_start = self.saved.len().saturating_sub(visible_rows);
        let start = selected
            .saturating_add(1)
            .saturating_sub(visible_rows)
            .min(maximum_start);
        let lines = if self.saved.is_empty() {
            vec![TextLine::from(Span::styled(
                " No files are finalized yet",
                Style::default().fg(MUTED),
            ))]
        } else {
            self.saved
                .iter()
                .enumerate()
                .skip(start)
                .take(visible_rows)
                .map(|(index, file)| {
                    let marker = if Some(index) == self.selected_save {
                        ">"
                    } else {
                        " "
                    };
                    let kind = match file.kind {
                        SavedFileKind::Recording => "FILE",
                        SavedFileKind::Part => "PART",
                        SavedFileKind::Clip => "CLIP",
                    };
                    let name = file
                        .path
                        .file_name()
                        .unwrap_or(file.path.as_os_str())
                        .to_string_lossy();
                    let style = if Some(index) == self.selected_save {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(MUTED)
                    };
                    TextLine::from(vec![
                        Span::styled(format!(" {marker} ✓ {kind:<4} "), style),
                        Span::styled(name.into_owned(), style),
                        Span::styled(
                            format!("   {:.1}s", file.duration().as_secs_f64()),
                            Style::default().fg(MUTED),
                        ),
                    ])
                })
                .collect()
        };
        let title = if let Some(notice) = &self.notice {
            format!(" SAVES · {notice} ")
        } else {
            " SAVES · SESSION ".to_owned()
        };
        frame.render_widget(Paragraph::new(lines).block(panel(&title, GREEN)), area);
    }

    /// Draws controls and the compact session path.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let output = compact_path(&self.output, area.width.saturating_sub(4) as usize);
        let compact = area.width < 96;
        let mut controls = vec![
            Span::styled(" Ctrl+C / S ", Style::default().fg(RED).bold()),
            Span::styled(
                if compact { "stop   " } else { "save & stop   " },
                Style::default().fg(MUTED),
            ),
            Span::styled(" Space ", Style::default().fg(AMBER).bold()),
            Span::styled("pause   ", Style::default().fg(MUTED)),
            Span::styled(" W ", Style::default().fg(PURPLE).bold()),
            Span::styled("view   ", Style::default().fg(MUTED)),
        ];
        if self.clips_enabled {
            controls.extend([
                Span::styled(" C ", Style::default().fg(GREEN).bold()),
                Span::styled("clip   ", Style::default().fg(MUTED)),
                Span::styled(" E ", Style::default().fg(PURPLE).bold()),
                Span::styled("edit   ", Style::default().fg(MUTED)),
            ]);
        }
        controls.extend([
            Span::styled(" ? ", Style::default().fg(CYAN).bold()),
            Span::styled("help", Style::default().fg(MUTED)),
        ]);
        let footer = Paragraph::new(vec![
            TextLine::from(controls),
            TextLine::from(vec![
                Span::styled(
                    if self.clips_enabled {
                        " SESSION  "
                    } else {
                        " OUTPUT   "
                    },
                    Style::default().fg(MUTED),
                ),
                Span::styled(output, Style::default().fg(Color::White)),
            ]),
        ])
        .block(
            Block::new()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        );
        frame.render_widget(footer, area);
    }

    /// Draws the recorder help overlay.
    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            TextLine::from("record starts audio capture immediately. No menu is necessary."),
            TextLine::from(""),
            help_line("Ctrl+C / S / Q / Esc", "finalize the MP3 and exit"),
            help_line("Space", "pause or resume (paused time is omitted)"),
            help_line("W", "cycle waveform, spectrum, and split views"),
            help_line("C", "name and finalize the current session part"),
            help_line("Up / Down", "select a finalized session file"),
            help_line("E", "review and trim the selected named clip"),
            help_line("? / Esc", "close this help"),
            TextLine::from(""),
            TextLine::from(Span::styled(
                "Captures every ordinary app routed to the default Windows output.",
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
                .title(TextLine::from(" QUICK HELP ").fg(PURPLE).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(help, area);
    }

    /// Draws the clip-name prompt and current validation message.
    fn render_clip_prompt(&self, frame: &mut Frame, area: Rect) {
        let Some(prompt) = &self.clip_prompt else {
            return;
        };
        let error = prompt
            .error
            .as_deref()
            .unwrap_or("Enter saves this part as a clip. Capture continues in a new part.");
        let error_color = if prompt.error.is_some() { RED } else { MUTED };
        let input = if prompt.input.is_empty() {
            Span::styled("clip-name", Style::default().fg(MUTED))
        } else {
            Span::styled(
                prompt.input.clone(),
                Style::default().fg(Color::White).bold(),
            )
        };
        let prompt = Paragraph::new(vec![
            TextLine::from(vec![
                Span::styled(" File name  ", Style::default().fg(CYAN)),
                input,
            ]),
            TextLine::from(""),
            TextLine::from(Span::styled(error, Style::default().fg(error_color))),
            TextLine::from(Span::styled(
                "Enter save   Esc cancel",
                Style::default().fg(MUTED),
            )),
        ])
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(GREEN))
                .padding(Padding::horizontal(1))
                .title(TextLine::from(" SAVE CLIP ").fg(GREEN).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(prompt, area);
    }

    /// Draws complete clip channels, handles, times, and edit controls.
    fn render_clip_review(&self, frame: &mut Frame, area: Rect) {
        let Some(review) = &self.clip_review else {
            return;
        };
        let Some(file) = self.saved.get(review.save_index) else {
            return;
        };
        let name = file
            .path
            .file_name()
            .unwrap_or(file.path.as_os_str())
            .to_string_lossy();
        let outer = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(PURPLE))
            .title(
                TextLine::from(format!(" CLIP REVIEW · {name} "))
                    .fg(PURPLE)
                    .bold(),
            );
        let inner = outer.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(outer, area);

        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Min(5),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(inner);
        let active = match review.selection.active_handle() {
            SelectionHandle::Start => "START",
            SelectionHandle::End => "END",
        };
        let status = if self.trim_job.is_some() {
            Span::styled("  ◌ WRITING TRIMMED MP3", Style::default().fg(AMBER).bold())
        } else if self.preview.is_some() {
            Span::styled("  ▶ PLAYING SELECTION", Style::default().fg(GREEN).bold())
        } else {
            Span::raw("")
        };
        frame.render_widget(
            Paragraph::new(vec![
                TextLine::from(vec![
                    Span::styled(" START ", Style::default().fg(CYAN).bold()),
                    Span::raw(format_clip_time(
                        review.selection.start_frame() - review.selection.source_start_frame(),
                        review.selection.sample_rate(),
                    )),
                    Span::styled("   END ", Style::default().fg(GREEN).bold()),
                    Span::raw(format_clip_time(
                        review.selection.end_frame() - review.selection.source_start_frame(),
                        review.selection.sample_rate(),
                    )),
                    Span::styled("   LENGTH ", Style::default().fg(PURPLE).bold()),
                    Span::raw(format!("{:.2}s", review.selection.duration().as_secs_f64())),
                    status,
                ]),
                TextLine::from(vec![
                    Span::styled(" ACTIVE HANDLE  ", Style::default().fg(MUTED)),
                    Span::styled(active, Style::default().fg(AMBER).bold()),
                ]),
            ]),
            rows[0],
        );
        render_clip_waveform(
            frame,
            rows[1],
            " LEFT · CLIP ",
            &review.waveform.left,
            &review.selection,
            CYAN,
        );
        render_clip_waveform(
            frame,
            rows[2],
            " RIGHT · CLIP ",
            &review.waveform.right,
            &review.selection,
            GREEN,
        );
        let message = review
            .error
            .as_deref()
            .unwrap_or("Tab handle   ←/→ 0.1s   Shift+←/→ 1s   Ctrl+←/→ 0.01s");
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(if review.error.is_some() {
                RED
            } else {
                MUTED
            })),
            rows[3],
        );
        frame.render_widget(
            Paragraph::new(TextLine::from(vec![
                Span::styled(" P ", Style::default().fg(GREEN).bold()),
                Span::styled("preview   ", Style::default().fg(MUTED)),
                Span::styled(" R ", Style::default().fg(CYAN).bold()),
                Span::styled("reset   ", Style::default().fg(MUTED)),
                Span::styled(" Enter ", Style::default().fg(PURPLE).bold()),
                Span::styled("save trim   ", Style::default().fg(MUTED)),
                Span::styled(" Esc ", Style::default().fg(RED).bold()),
                Span::styled("keep original", Style::default().fg(MUTED)),
            ])),
            rows[4],
        );
    }

    /// Formats total encoded frames as an hours, minutes, and seconds clock.
    fn elapsed(&self) -> String {
        let duration = if self.sample_rate == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(self.encoded_frames as f64 / f64::from(self.sample_rate))
        };
        format_clock(duration)
    }
}

/// Draws one live channel as a braille line scope.
fn render_waveform(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    samples: &VecDeque<f32>,
    color: Color,
) {
    let maximum_points = usize::from(area.width.max(2)) * 3;
    let points = waveform_points(samples, maximum_points);
    let x_max = points.last().map_or(1.0, |point| point.0.max(1.0));
    let canvas = Canvas::default()
        .block(panel(title, color))
        .marker(Marker::Braille)
        .x_bounds([0.0, x_max])
        .y_bounds([-1.0, 1.0])
        .paint(|context| {
            context.draw(&CanvasLine::new(0.0, 0.0, x_max, 0.0, BORDER));
            for window in points.windows(2) {
                context.draw(&CanvasLine::new(
                    window[0].0,
                    window[0].1,
                    window[1].0,
                    window[1].1,
                    color,
                ));
            }
        });
    frame.render_widget(canvas, area);
}

/// Draws one full clip channel with its start and end handles.
fn render_clip_waveform(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    bins: &[WaveformBin],
    selection: &ClipSelection,
    color: Color,
) {
    if bins.is_empty() {
        frame.render_widget(
            Paragraph::new("No waveform data").block(panel(title, color)),
            area,
        );
        return;
    }
    let x_max = (bins.len().saturating_sub(1) as f64).max(1.0);
    let selection_start = selection.normalized_start() * x_max;
    let selection_end = selection.normalized_end() * x_max;
    let start_color = if selection.active_handle() == SelectionHandle::Start {
        AMBER
    } else {
        BORDER
    };
    let end_color = if selection.active_handle() == SelectionHandle::End {
        AMBER
    } else {
        BORDER
    };
    let canvas = Canvas::default()
        .block(panel(title, color))
        .marker(Marker::Braille)
        .x_bounds([0.0, x_max])
        .y_bounds([-1.0, 1.0])
        .paint(|context| {
            context.draw(&CanvasLine::new(0.0, 0.0, x_max, 0.0, BORDER));
            for (index, bin) in bins.iter().enumerate() {
                let x = if bins.len() == 1 {
                    0.0
                } else {
                    index as f64 / (bins.len() - 1) as f64 * x_max
                };
                let bin_color = if x >= selection_start && x <= selection_end {
                    color
                } else {
                    MUTED
                };
                context.draw(&CanvasLine::new(
                    x,
                    f64::from(bin.minimum.clamp(-1.0, 1.0)),
                    x,
                    f64::from(bin.maximum.clamp(-1.0, 1.0)),
                    bin_color,
                ));
            }
            context.draw(&CanvasLine::new(
                selection_start,
                -1.0,
                selection_start,
                1.0,
                start_color,
            ));
            context.draw(&CanvasLine::new(
                selection_end,
                -1.0,
                selection_end,
                1.0,
                end_color,
            ));
        });
    frame.render_widget(canvas, area);
}

/// Formats an absolute frame count with millisecond resolution.
pub(super) fn format_clip_time(frames: u64, sample_rate: u32) -> String {
    if sample_rate == 0 {
        return "00:00.000".to_owned();
    }
    let milliseconds =
        u64::try_from(u128::from(frames).saturating_mul(1_000) / u128::from(sample_rate))
            .unwrap_or(u64::MAX);
    let hours = milliseconds / 3_600_000;
    let minutes = milliseconds / 60_000 % 60;
    let seconds = milliseconds / 1_000 % 60;
    let remainder = milliseconds % 1_000;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}.{remainder:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{remainder:03}")
    }
}

/// Reduces a rolling sample history to a bounded canvas point list.
pub(super) fn waveform_points(samples: &VecDeque<f32>, maximum: usize) -> Vec<(f64, f64)> {
    if samples.is_empty() {
        return vec![(0.0, 0.0), (1.0, 0.0)];
    }
    let step = samples.len().div_ceil(maximum.max(2));
    samples
        .iter()
        .step_by(step)
        .enumerate()
        .map(|(index, sample)| (index as f64, f64::from(*sample)))
        .collect()
}

/// Creates a peak gauge with a floor of minus 60 dB.
fn level_gauge(label: &'static str, peak: f32, color: Color) -> LineGauge<'static> {
    let db = if peak < 0.001 {
        -60.0
    } else {
        (20.0 * peak.log10()).max(-60.0)
    };
    LineGauge::default()
        .block(
            Block::new()
                .borders(Borders::LEFT | Borders::RIGHT)
                .border_style(BORDER),
        )
        .label(format!("{label}{db:>4.0} dB"))
        .ratio(f64::from(peak.clamp(0.0, 1.0)))
        .filled_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .unfilled_style(Style::default().fg(BORDER))
        .filled_symbol(symbols::line::THICK_HORIZONTAL)
        .unfilled_symbol(symbols::line::HORIZONTAL)
}
