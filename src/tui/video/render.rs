//! Rendering for the screen-capture interface.

mod editor;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line as TextLine, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::raster::RasterRenderer;

use super::super::{
    AMBER, BORDER, CYAN, MUTED, PURPLE, RED,
    utils::{centered, compact_path, format_clock, help_line, panel},
};
use super::{CaptureState, VideoApp};

#[cfg(test)]
pub(super) use editor::{
    FadeColumns, TimelineColumns, progress_ratio, spinner_frame, video_waveform_lines,
};

impl VideoApp {
    /// Draws the header, live status panel, footer, and any overlay.
    #[cfg(test)]
    pub(super) fn render(&mut self, frame: &mut Frame) {
        self.render_with_raster(frame, None);
    }

    /// Draws the capture interface with an optional real terminal raster.
    pub(super) fn render_with_raster(
        &mut self,
        frame: &mut Frame,
        raster: Option<&mut RasterRenderer>,
    ) {
        if self.editor.is_some() {
            self.render_video_editor(frame, raster);
            return;
        }
        let area = frame.area();
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);
        self.render_header(frame, layout[0]);
        self.render_status(frame, layout[1], raster);
        self.render_footer(frame, layout[2]);
        if self.show_help {
            self.render_help(frame, centered(area, 62, 12));
        }
    }

    /// Draws capture state, elapsed time, and the encoded format.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let (status, color) = match self.state {
            CaptureState::Starting => ("● STARTING", AMBER),
            CaptureState::Recording => ("● RECORDING", RED),
            CaptureState::Paused => ("Ⅱ PAUSED", AMBER),
            CaptureState::Previewing => ("▶ RECORDING", RED),
            CaptureState::Finalizing => ("◌ FINALIZING", AMBER),
        };
        let title = TextLine::from(vec![
            Span::styled(" record ", Style::default().fg(RED).bold()),
            Span::styled("SCREEN CAPTURE", Style::default().fg(MUTED)),
        ]);
        let geometry = if self.width > 0 {
            format!("{}x{} @ {} fps", self.width, self.height, self.fps)
        } else {
            "preparing encoder".to_owned()
        };
        let header = Paragraph::new(TextLine::from(vec![
            Span::styled(status, Style::default().fg(color).bold()),
            Span::raw(format!("   {}   ", format_clock(self.recorded))),
            Span::styled(
                format!("{geometry} · H.264 + AAC"),
                Style::default().fg(MUTED),
            ),
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

    /// Draws a Sixel-backed live video surface with one compact status row.
    fn render_status(
        &self,
        frame: &mut Frame,
        area: Rect,
        mut raster: Option<&mut RasterRenderer>,
    ) {
        let panel_block = panel(" LIVE VIDEO ", PURPLE);
        let inner = panel_block.inner(area);
        frame.render_widget(panel_block, area);
        let sections = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

        let rendered = raster
            .as_deref_mut()
            .is_some_and(|raster| raster.render(frame, sections[0]));
        if !rendered {
            let message = raster
                .as_deref()
                .and_then(RasterRenderer::error)
                .unwrap_or("waiting for the first desktop frame");
            frame.render_widget(
                Paragraph::new(message)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(MUTED)),
                sections[0],
            );
        }

        let status = self.notice.clone().unwrap_or_else(|| {
            let ready = self
                .capture_ready_ms
                .map(|milliseconds| format!(" · ready {milliseconds:.0} ms"))
                .unwrap_or_default();
            format!(
                "{} · {} kHz AAC{ready}",
                compact_path(
                    &self.output,
                    usize::from(inner.width).saturating_sub(20).max(8)
                ),
                self.sample_rate / 1_000,
            )
        });
        let color = if self.notice.is_some() { AMBER } else { MUTED };
        frame.render_widget(
            Paragraph::new(status)
                .alignment(Alignment::Center)
                .style(Style::default().fg(color)),
            sections[1],
        );
    }

    /// Draws controls and the compact output path.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let compact = area.width < 96;
        let footer = Paragraph::new(vec![TextLine::from(vec![
            Span::styled(" Ctrl+C / S ", Style::default().fg(RED).bold()),
            Span::styled(
                if compact { "stop   " } else { "save & stop   " },
                Style::default().fg(MUTED),
            ),
            Span::styled(" Space ", Style::default().fg(AMBER).bold()),
            Span::styled("pause   ", Style::default().fg(MUTED)),
            Span::styled(" ? ", Style::default().fg(CYAN).bold()),
            Span::styled("help", Style::default().fg(MUTED)),
        ])])
        .block(
            Block::new()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        );
        frame.render_widget(footer, area);
    }

    /// Draws the screen-capture help overlay.
    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            TextLine::from("Screen capture records the primary monitor and system audio."),
            TextLine::from(""),
            help_line("Ctrl+C / S / Q / Esc", "finalize the MP4 and exit"),
            help_line("Space", "pause or resume (paused time is omitted)"),
            help_line("? / Esc", "close this help"),
            TextLine::from(""),
            TextLine::from(Span::styled(
                "The MP4 finalizes on exit; keep the terminal open until it saves.",
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
                .title(TextLine::from(" SCREEN HELP ").fg(PURPLE).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(help, area);
    }
}
