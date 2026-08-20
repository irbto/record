use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use anyhow::Result;
use crossbeam_channel::Receiver;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::{self, Marker},
    text::{Line as TextLine, Span},
    widgets::{
        Block, BorderType, Borders, Clear, LineGauge, Padding, Paragraph, Sparkline, Wrap,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use crate::{
    audio::{AudioEvent, RecordingSummary},
    spectrum::Spectrum,
    waveform::Waveform,
};

const CYAN: Color = Color::Rgb(65, 214, 224);
const GREEN: Color = Color::Rgb(72, 214, 147);
const RED: Color = Color::Rgb(247, 75, 103);
const AMBER: Color = Color::Rgb(244, 183, 73);
const PURPLE: Color = Color::Rgb(172, 122, 255);
const MUTED: Color = Color::Rgb(126, 139, 158);
const BORDER: Color = Color::Rgb(55, 65, 82);

#[derive(Clone, Copy, Debug, Default)]
enum ViewMode {
    #[default]
    Waveform,
    Spectrum,
    Split,
}

impl ViewMode {
    const fn next(self) -> Self {
        match self {
            Self::Waveform => Self::Spectrum,
            Self::Spectrum => Self::Split,
            Self::Split => Self::Waveform,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Waveform => "WAVE",
            Self::Spectrum => "SPECTRUM",
            Self::Split => "SPLIT",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum CaptureState {
    #[default]
    Starting,
    Recording,
    Paused,
    Finalizing,
}

pub fn run(
    events: &Receiver<AudioEvent>,
    worker: &JoinHandle<Result<RecordingSummary>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    output: PathBuf,
) -> Result<()> {
    let mut app = App::new(stop, paused, output);
    ratatui::run(|terminal| app.run(terminal, events, worker))
}

struct App {
    state: CaptureState,
    view: ViewMode,
    waveform: Waveform,
    spectrum: Option<Spectrum>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    output: PathBuf,
    encoded_frames: u64,
    sample_rate: u32,
    bitrate: u32,
    show_help: bool,
}

impl App {
    fn new(stop: Arc<AtomicBool>, paused: Arc<AtomicBool>, output: PathBuf) -> Self {
        Self {
            state: CaptureState::Starting,
            view: ViewMode::Waveform,
            waveform: Waveform::new(48_000 * 6),
            spectrum: None,
            stop,
            paused,
            output,
            encoded_frames: 0,
            sample_rate: 48_000,
            bitrate: 320_000,
            show_help: false,
        }
    }

    fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: &Receiver<AudioEvent>,
        worker: &JoinHandle<Result<RecordingSummary>>,
    ) -> Result<()> {
        loop {
            self.drain_audio_events(events);
            if worker.is_finished() {
                break;
            }
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(33))? {
                self.handle_event(event::read()?);
            }
            if self.stop.load(Ordering::Relaxed) {
                self.state = CaptureState::Finalizing;
            }
        }
        self.drain_audio_events(events);
        terminal.draw(|frame| self.render(frame))?;
        Ok(())
    }

    fn drain_audio_events(&mut self, events: &Receiver<AudioEvent>) {
        let mut samples_changed = false;
        for event in events.try_iter() {
            match event {
                AudioEvent::Started {
                    sample_rate,
                    bitrate,
                    ..
                } => {
                    self.sample_rate = sample_rate;
                    self.bitrate = bitrate;
                    self.state = CaptureState::Recording;
                }
                AudioEvent::Samples {
                    left,
                    right,
                    encoded_frames,
                } => {
                    self.waveform.push(&left, &right);
                    samples_changed = true;
                    self.encoded_frames = encoded_frames;
                }
                AudioEvent::Finalizing => self.state = CaptureState::Finalizing,
            }
        }
        if samples_changed && let Some(spectrum) = &mut self.spectrum {
            let (wave_left, wave_right) = self.waveform.channels();
            spectrum.update(
                wave_left
                    .iter()
                    .zip(wave_right)
                    .map(|(left, right)| (left + right) * 0.5),
            );
        }
    }

    fn handle_event(&mut self, event: Event) {
        let Event::Key(key) = event else {
            return;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.show_help = false;
            }
            return;
        }
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('s' | 'q'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.stop.store(true, Ordering::Relaxed);
                self.state = CaptureState::Finalizing;
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                ..
            } if !matches!(
                self.state,
                CaptureState::Starting | CaptureState::Finalizing
            ) =>
            {
                let paused = !self.paused.load(Ordering::Relaxed);
                self.paused.store(paused, Ordering::Relaxed);
                self.state = if paused {
                    CaptureState::Paused
                } else {
                    CaptureState::Recording
                };
            }
            KeyEvent {
                code: KeyCode::Char('w'),
                ..
            } => {
                self.view = self.view.next();
                if matches!(self.view, ViewMode::Spectrum | ViewMode::Split) {
                    self.spectrum.get_or_insert_with(|| Spectrum::new(2_048));
                }
            }
            KeyEvent {
                code: KeyCode::Char('?'),
                ..
            } => self.show_help = true,
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
            Constraint::Length(3),
        ])
        .split(area);
        self.render_header(frame, layout[0]);
        self.render_visualization(frame, layout[1]);
        self.render_meters(frame, layout[2]);
        self.render_footer(frame, layout[3]);
        if self.show_help {
            self.render_help(frame, centered(area, 62, 17));
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let elapsed = self.elapsed();
        let (status, color) = match self.state {
            CaptureState::Starting => ("● STARTING", AMBER),
            CaptureState::Recording => ("● RECORDING", RED),
            CaptureState::Paused => ("Ⅱ PAUSED", AMBER),
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

    fn render_waveforms(&self, frame: &mut Frame, area: Rect) {
        let panels =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
        let (left, right) = self.waveform.channels();
        render_waveform(frame, panels[0], " LEFT · 6 SECOND SCOPE ", left, CYAN);
        render_waveform(frame, panels[1], " RIGHT · 6 SECOND SCOPE ", right, GREEN);
    }

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

    fn render_meters(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(2), Constraint::Length(2)]).split(area);
        let (left, right) = self.waveform.peaks();
        frame.render_widget(level_gauge(" L ", left, CYAN), rows[0]);
        frame.render_widget(level_gauge(" R ", right, GREEN), rows[1]);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let output = compact_path(&self.output, area.width.saturating_sub(4) as usize);
        let footer = Paragraph::new(vec![
            TextLine::from(vec![
                Span::styled(" Ctrl+C / S ", Style::default().fg(RED).bold()),
                Span::styled("save & stop   ", Style::default().fg(MUTED)),
                Span::styled(" Space ", Style::default().fg(AMBER).bold()),
                Span::styled("pause   ", Style::default().fg(MUTED)),
                Span::styled(" W ", Style::default().fg(PURPLE).bold()),
                Span::styled("view   ", Style::default().fg(MUTED)),
                Span::styled(" ? ", Style::default().fg(CYAN).bold()),
                Span::styled("help", Style::default().fg(MUTED)),
            ]),
            TextLine::from(vec![
                Span::styled(" OUTPUT  ", Style::default().fg(MUTED)),
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

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            TextLine::from("record starts audio capture immediately. No menu is necessary."),
            TextLine::from(""),
            help_line("Ctrl+C / S / Q / Esc", "finalize the MP3 and exit"),
            help_line("Space", "pause or resume (paused time is omitted)"),
            help_line("W", "cycle waveform, spectrum, and split views"),
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

    fn elapsed(&self) -> String {
        let duration = if self.sample_rate == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(self.encoded_frames as f64 / f64::from(self.sample_rate))
        };
        let total = duration.as_secs();
        format!(
            "{:02}:{:02}:{:02}",
            total / 3_600,
            total / 60 % 60,
            total % 60
        )
    }
}

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

fn waveform_points(samples: &VecDeque<f32>, maximum: usize) -> Vec<(f64, f64)> {
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

fn panel(title: &str, color: Color) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(TextLine::from(title).fg(color).bold())
}

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

fn help_line(key: &'static str, description: &'static str) -> TextLine<'static> {
    TextLine::from(vec![
        Span::styled(format!("{key:<24}"), Style::default().fg(CYAN).bold()),
        Span::raw(description),
    ])
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let horizontal = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .split(area)[0];
    Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .split(horizontal)[0]
}

fn compact_path(path: &Path, maximum: usize) -> String {
    let text = path.display().to_string();
    if text.chars().count() <= maximum {
        return text;
    }
    let tail = text
        .chars()
        .rev()
        .take(maximum.saturating_sub(3))
        .collect::<String>();
    format!("...{}", tail.chars().rev().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacts_from_the_front_to_keep_the_filename() {
        let result = compact_path(Path::new("C:/very/long/path/recording.mp3"), 18);
        assert!(result.starts_with("..."));
        assert!(result.ends_with("recording.mp3"));
    }

    #[test]
    fn waveform_downsamples_to_a_bounded_size() {
        let samples = (0..10_000)
            .map(|index| index as f32)
            .collect::<VecDeque<_>>();
        assert!(waveform_points(&samples, 100).len() <= 100);
    }
}
