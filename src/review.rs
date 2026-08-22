//! Standalone review player for finished MP4 recordings.
//!
//! Opens one recorded file through the Media Foundation preview worker,
//! decodes pictures off the drawing thread, and scrubs them with the
//! half-block renderer. The screen shares the recorder's visual language:
//! rounded panels, the shared palette, a gauge timeline, and the same
//! keyboard modifier scheme as the clip editor.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line as TextLine, Span},
    widgets::{Block, BorderType, Borders, Clear, LineGauge, Padding, Paragraph, Wrap},
};

use crate::{
    tui::{
        AMBER, BORDER, CYAN, GREEN, MUTED, PURPLE, RED, centered, compact_path, format_clock, panel,
    },
    video::preview::{
        HalfblockImage, PreviewCommand, PreviewEvent, PreviewFormat, PreviewFrame,
        spawn_preview_worker,
    },
};

/// How often the interface redraws while scrubbing or playing.
const TICK: Duration = Duration::from_millis(33);

/// Runs the full-screen review interface until the viewer quits.
///
/// # Errors
///
/// Returns any terminal or decoder-thread startup failure.
pub fn run(path: PathBuf) -> Result<()> {
    let (commands, events) = spawn_preview_worker(path.clone())?;
    let mut app = ReviewApp {
        path,
        commands,
        format: None,
        frame: None,
        playing: false,
        position: Duration::ZERO,
        awaiting_picture: false,
        failed: None,
        reached_end: false,
        show_help: false,
    };
    ratatui::run(|terminal| app.run(terminal, &events))
}

/// Owns all interface state for one reviewed file.
struct ReviewApp {
    /// Contains the MP4 that this session displays.
    path: PathBuf,
    /// Sends step, seek, and close requests to the decoder thread.
    commands: Sender<PreviewCommand>,
    /// Contains the announced stream once the worker opened the file.
    format: Option<PreviewFormat>,
    /// Contains the newest decoded picture.
    frame: Option<PreviewFrame>,
    /// Advances the playhead while true.
    playing: bool,
    /// Contains the media time that the timeline shows.
    position: Duration,
    /// Reports whether exactly one decode request is in flight.
    awaiting_picture: bool,
    /// Contains the decoder failure message, if any.
    failed: Option<String>,
    /// Reports whether playback reached the end of the stream.
    reached_end: bool,
    /// Reports whether the help overlay is open.
    show_help: bool,
}

impl ReviewApp {
    /// Draws frames and handles input until the viewer quits.
    fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: &Receiver<PreviewEvent>,
    ) -> Result<()> {
        let mut last_tick = Instant::now();
        loop {
            self.drain_events(events);
            self.advance_playback(last_tick.elapsed());
            last_tick = Instant::now();
            self.request_next_picture();
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(TICK)? && !self.handle_event(event::read()?) {
                break;
            }
        }
        let _ = self.commands.send(PreviewCommand::Close);
        Ok(())
    }

    /// Applies every pending decoder event without blocking.
    fn drain_events(&mut self, events: &Receiver<PreviewEvent>) {
        for event in events.try_iter() {
            match event {
                PreviewEvent::Ready(format) => self.format = Some(format),
                PreviewEvent::Frame(frame) => {
                    self.awaiting_picture = false;
                    self.position = frame.time;
                    self.frame = Some(*frame);
                    self.reached_end = false;
                }
                PreviewEvent::End => {
                    self.awaiting_picture = false;
                    self.playing = false;
                    self.reached_end = true;
                    if let Some(format) = self.format {
                        self.position = format.duration;
                    }
                }
                PreviewEvent::Failed(message) => {
                    self.awaiting_picture = false;
                    self.playing = false;
                    self.failed = Some(message);
                }
            }
        }
    }

    /// Moves the playhead forward in real time while playing.
    fn advance_playback(&mut self, delta: Duration) {
        if !self.playing || self.failed.is_some() {
            return;
        }
        let total = self.format.map_or(Duration::ZERO, |format| format.duration);
        self.position = (self.position + delta).min(total);
    }

    /// Keeps exactly one decode request outstanding while playing.
    fn request_next_picture(&mut self) {
        if !self.playing || self.awaiting_picture || self.reached_end || self.failed.is_some() {
            return;
        }
        match self.commands.try_send(PreviewCommand::Step) {
            Ok(()) => self.awaiting_picture = true,
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                self.failed = Some("The preview decoder stopped.".to_owned());
                self.playing = false;
            }
        }
    }

    /// Routes one terminal event; returns false when the viewer quits.
    fn handle_event(&mut self, event: Event) -> bool {
        let Event::Key(key) = event else {
            return true;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return true;
        }
        if matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return false;
        }
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.show_help = false;
            }
            return true;
        }
        match key.code {
            KeyCode::Char('q' | 'Q' | 's' | 'S') | KeyCode::Esc => return false,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(' ') => self.toggle_playback(),
            KeyCode::Char('r' | 'R') | KeyCode::Home => self.seek(Duration::ZERO),
            KeyCode::Left | KeyCode::Right => {
                let direction = if key.code == KeyCode::Left { -1 } else { 1 };
                let target = nudge_position(
                    self.position,
                    direction,
                    key.modifiers,
                    self.format.map_or(30.0, |format| format.fps),
                    self.duration(),
                );
                self.seek(target);
            }
            _ => {}
        }
        true
    }

    /// Starts, stops, or restarts playback of the selection.
    fn toggle_playback(&mut self) {
        if self.failed.is_some() {
            return;
        }
        if self.reached_end {
            self.seek(Duration::ZERO);
            self.playing = true;
            self.reached_end = false;
            return;
        }
        self.playing = !self.playing;
    }

    /// Jumps the playhead and asks the decoder for that exact picture.
    fn seek(&mut self, time: Duration) {
        let target = time.min(self.duration());
        self.position = target;
        self.reached_end = false;
        self.awaiting_picture = true;
        let _ = self.commands.try_send(PreviewCommand::Seek(target));
    }

    /// Returns the announced duration or zero before the file opens.
    fn duration(&self) -> Duration {
        self.format.map_or(Duration::ZERO, |format| format.duration)
    }

    /// Draws the header, preview, timeline, footer, and any overlay.
    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(area);
        self.render_header(frame, layout[0]);
        self.render_preview(frame, layout[1]);
        self.render_timeline(frame, layout[2]);
        self.render_footer(frame, layout[3]);
        if self.show_help {
            self.render_help(frame, centered(area, 62, 14));
        }
    }

    /// Draws capture state, elapsed media time, and the encoded format.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let (status, color) = if self.failed.is_some() {
            ("\u{2717} ERROR", RED)
        } else if self.format.is_none() {
            ("\u{25cb} OPENING", AMBER)
        } else if self.reached_end {
            ("\u{25cf} END", PURPLE)
        } else if self.playing {
            ("\u{25b6} PLAYING", GREEN)
        } else {
            ("\u{2161} PAUSED", AMBER)
        };
        let geometry = self.format.map_or_else(
            || "opening".to_owned(),
            |format| format!("{}x{} @ {:.0} fps", format.width, format.height, format.fps),
        );
        let name = self
            .path
            .file_name()
            .unwrap_or(self.path.as_os_str())
            .to_string_lossy();
        let title = TextLine::from(vec![
            Span::styled(" record ", Style::default().fg(GREEN).bold()),
            Span::styled(" REVIEW ", Style::default().fg(MUTED)),
            Span::styled(name.into_owned(), Style::default().fg(PURPLE)),
        ]);
        let header = Paragraph::new(TextLine::from(vec![
            Span::styled(status, Style::default().fg(color).bold()),
            Span::raw(format!("   {} ", format_clock(self.position))),
            Span::raw("/ "),
            Span::raw(format_clock(self.duration())),
            Span::styled(
                format!("   {geometry} \u{00b7} H.264"),
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

    /// Draws the half-block picture, or progress and error placeholders.
    fn render_preview(&mut self, frame: &mut Frame, area: Rect) {
        let panel_block = panel(" PREVIEW \u{00b7} HALF-BLOCK ", PURPLE);
        if let Some(message) = &self.failed {
            frame.render_widget(
                Paragraph::new(message.clone())
                    .style(Style::default().fg(RED))
                    .alignment(Alignment::Center)
                    .block(panel_block),
                area,
            );
            return;
        }
        let inner = panel_block.inner(area);
        frame.render_widget(panel_block, area);
        let Some(preview_frame) = &self.frame else {
            frame.render_widget(
                Paragraph::new("Decoding first picture")
                    .style(Style::default().fg(MUTED))
                    .alignment(Alignment::Center),
                inner,
            );
            return;
        };
        frame.render_widget(HalfblockImage::new(preview_frame), inner);
    }

    /// Draws the playhead gauge across the whole recording.
    fn render_timeline(&self, frame: &mut Frame, area: Rect) {
        let total = self.duration();
        let ratio = if total.is_zero() {
            0.0
        } else {
            self.position.as_secs_f64() / total.as_secs_f64()
        };
        let gauge = LineGauge::default()
            .label(format!(
                "{} / {}",
                format_clock(self.position),
                format_clock(total)
            ))
            .ratio(ratio.clamp(0.0, 1.0))
            .filled_style(Style::default().fg(GREEN).add_modifier(Modifier::BOLD))
            .unfilled_style(Style::default().fg(BORDER));
        frame.render_widget(gauge, area);
    }

    /// Draws controls and the compact output path.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let output = compact_path(
            &self.path,
            usize::from(area.width.saturating_sub(12)).max(8),
        );
        let footer = Paragraph::new(vec![
            TextLine::from(vec![
                Span::styled(" Space ", Style::default().fg(GREEN).bold()),
                Span::styled("play   ", Style::default().fg(MUTED)),
                Span::styled(" \u{2190}/\u{2192} ", Style::default().fg(CYAN).bold()),
                Span::styled("1s   ", Style::default().fg(MUTED)),
                Span::styled(" Shift ", Style::default().fg(CYAN).bold()),
                Span::styled("10s   ", Style::default().fg(MUTED)),
                Span::styled(" Ctrl ", Style::default().fg(CYAN).bold()),
                Span::styled("1 frame   ", Style::default().fg(MUTED)),
                Span::styled(" R ", Style::default().fg(PURPLE).bold()),
                Span::styled("restart   ", Style::default().fg(MUTED)),
                Span::styled(" ? ", Style::default().fg(AMBER).bold()),
                Span::styled("help   ", Style::default().fg(MUTED)),
                Span::styled(" Esc ", Style::default().fg(RED).bold()),
                Span::styled("quit", Style::default().fg(MUTED)),
            ]),
            TextLine::from(vec![
                Span::styled(" OUTPUT   ", Style::default().fg(MUTED)),
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

    /// Draws the review help overlay.
    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            TextLine::from("Review plays a finalized MP4 through the half-block renderer."),
            TextLine::from(""),
            help_line("Space", "play, pause, or replay from the start"),
            help_line("\u{2190} / \u{2192}", "seek by one second"),
            help_line("Shift + \u{2190}/\u{2192}", "seek by ten seconds"),
            help_line("Ctrl + \u{2190}/\u{2192}", "seek by exactly one frame"),
            help_line("R / Home", "return to the first picture"),
            help_line("? / Esc", "close this help"),
            TextLine::from(""),
            TextLine::from(Span::styled(
                "Decoding runs on a background thread; the interface never blocks.",
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
                .title(TextLine::from(" REVIEW HELP ").fg(PURPLE).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(help, area);
    }
}

/// Moves a playhead by the modifier-selected step size.
fn nudge_position(
    current: Duration,
    direction: i32,
    modifiers: KeyModifiers,
    fps: f64,
    total: Duration,
) -> Duration {
    let seconds = if modifiers.contains(KeyModifiers::SHIFT) {
        10.0
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        (1.0 / fps.max(1.0)).min(1.0)
    } else {
        1.0
    };
    let change = seconds * f64::from(direction);
    let next = current.as_secs_f64() + change;
    Duration::try_from_secs_f64(next.clamp(0.0, total.as_secs_f64())).unwrap_or(Duration::ZERO)
}

/// Creates one aligned key row for the help overlay.
fn help_line(key: &'static str, description: &'static str) -> TextLine<'static> {
    TextLine::from(vec![
        Span::styled(format!("{key:<24}"), Style::default().fg(CYAN).bold()),
        Span::raw(description),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn test_app() -> (ReviewApp, Receiver<PreviewCommand>) {
        let (commands, receiver) = bounded(8);
        let app = ReviewApp {
            path: PathBuf::from("recording-20260822-120000.mp4"),
            commands,
            format: Some(PreviewFormat {
                width: 2560,
                height: 1440,
                fps: 60.0,
                duration: Duration::from_secs(90),
            }),
            frame: None,
            playing: false,
            position: Duration::from_secs(30),
            awaiting_picture: false,
            failed: None,
            reached_end: false,
            show_help: false,
        };
        (app, receiver)
    }

    fn press(app: &mut ReviewApp, code: KeyCode, modifiers: KeyModifiers) -> bool {
        app.handle_event(Event::Key(KeyEvent::new(code, modifiers)))
    }

    #[test]
    fn nudges_move_by_the_modifier_selected_step() {
        let position = Duration::from_secs(30);
        assert_eq!(
            nudge_position(
                position,
                1,
                KeyModifiers::NONE,
                60.0,
                Duration::from_secs(90)
            ),
            Duration::from_secs(31)
        );
        assert_eq!(
            nudge_position(
                position,
                -1,
                KeyModifiers::NONE,
                60.0,
                Duration::from_secs(90)
            ),
            Duration::from_secs(29)
        );
        assert_eq!(
            nudge_position(
                position,
                -1,
                KeyModifiers::SHIFT,
                60.0,
                Duration::from_secs(90)
            ),
            Duration::from_secs(20)
        );
        let frame_step = nudge_position(
            position,
            1,
            KeyModifiers::CONTROL,
            60.0,
            Duration::from_secs(90),
        );
        assert_eq!(frame_step, Duration::from_secs_f64(30.0 + 1.0 / 60.0));
    }

    #[test]
    fn nudges_clamp_to_media_bounds() {
        let total = Duration::from_secs(90);
        assert_eq!(
            nudge_position(Duration::from_secs(89), 1, KeyModifiers::SHIFT, 30.0, total),
            total
        );
        assert_eq!(
            nudge_position(Duration::from_secs(5), -1, KeyModifiers::SHIFT, 30.0, total),
            Duration::ZERO
        );
    }

    #[test]
    fn space_toggles_playback_and_end_replays_from_start() {
        let (mut app, _commands) = test_app();
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.playing);
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!app.playing);

        app.reached_end = true;
        app.playing = false;
        app.position = app.duration();
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.playing);
        assert!(!app.reached_end);
        assert_eq!(app.position, Duration::ZERO);
    }

    #[test]
    fn arrow_keys_seek_and_clamp() {
        let (mut app, commands) = test_app();
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.position, Duration::from_secs(31));
        assert_eq!(
            commands.try_recv().unwrap(),
            PreviewCommand::Seek(Duration::from_secs(31))
        );
        app.position = Duration::from_secs(90);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.position, Duration::from_secs(90));
    }

    #[test]
    fn decoder_events_drive_state_transitions() {
        let (sender, receiver) = bounded(8);
        let (mut app, _commands) = test_app();
        app.playing = true;
        sender.send(PreviewEvent::End).unwrap();
        app.drain_events(&receiver);
        assert!(!app.playing);
        assert!(app.reached_end);
        assert_eq!(app.position, Duration::from_secs(90));

        app.reached_end = false;
        sender
            .send(PreviewEvent::Failed("decoder stopped".to_owned()))
            .unwrap();
        app.drain_events(&receiver);
        assert_eq!(app.failed.as_deref(), Some("decoder stopped"));
        assert!(!app.playing);
    }

    #[test]
    fn quit_keys_stop_the_viewer() {
        let (mut app, _commands) = test_app();
        assert!(press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!press(&mut app, KeyCode::Esc, KeyModifiers::NONE));

        let (mut app, _commands) = test_app();
        assert!(!press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE));

        let (mut app, _commands) = test_app();
        assert!(!press(&mut app, KeyCode::Char('C'), KeyModifiers::CONTROL));
    }

    #[test]
    fn help_overlay_opens_and_closes() {
        let (mut app, _commands) = test_app();
        assert!(press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(app.show_help);
        assert!(press(&mut app, KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_help);
    }

    #[test]
    fn review_view_renders_header_preview_and_timeline() {
        let (mut app, _commands) = test_app();
        let mut bgra = Vec::with_capacity(32 * 16 * 4);
        for _ in 0..32 * 16 {
            bgra.extend_from_slice(&[40, 200, 80, 255]);
        }
        app.frame = Some(PreviewFrame {
            bgra,
            width: 32,
            height: 16,
            time: Duration::ZERO,
        });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("REVIEW"));
        assert!(content.contains("PREVIEW"));
        assert!(content.contains("PAUSED"));
        assert!(content.contains("\u{2580}"));
        assert!(content.contains("Space"));
    }

    #[test]
    fn failures_render_inside_the_preview_panel() {
        let (mut app, _commands) = test_app();
        app.failed = Some("could not open file".to_owned());
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("ERROR"));
        assert!(content.contains("could not open file"));
    }
}
