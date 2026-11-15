//! The terminal hub: a capture menu beside a preview and a file history.
//!
//! The hub never captures by itself. It sends requests to the hub thread and
//! shows the reports that come back. The preview uses the best image protocol
//! of the terminal and falls back to half blocks.

use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use ratatui_image::{Image, Resize, picker::Picker, protocol::Protocol};

use super::{
    CaptureMode, copy_file_to_clipboard,
    history::{HistoryEntry, list_history},
    hub::{Hub, HubEvent, HubRequest, HubSettings},
};
use crate::tui::{AMBER, BORDER, CYAN, GREEN, MUTED, RED};

/// Process creation flag that gives a child its own console window.
const CREATE_NEW_CONSOLE: u32 = 0x10;
/// Width of the menu column in terminal cells.
const MENU_WIDTH: u16 = 40;

/// One selectable menu entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuItem {
    Region,
    Fullscreen,
    Window,
    Monitor,
    LastRegion,
    DelayedRegion,
    Auto,
    Recording,
    CopyToggle,
    SaveToggle,
    Folder,
    OpenImage,
    CopyImage,
    Quit,
}

/// One menu line: a group heading, an entry, or a blank spacer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Row {
    Heading(&'static str),
    Item(MenuItem),
    Gap,
}

/// The menu in display order.
const ROWS: [Row; 21] = [
    Row::Heading("Capture"),
    Row::Item(MenuItem::Region),
    Row::Item(MenuItem::Fullscreen),
    Row::Item(MenuItem::Window),
    Row::Item(MenuItem::Monitor),
    Row::Item(MenuItem::LastRegion),
    Row::Item(MenuItem::DelayedRegion),
    Row::Item(MenuItem::Auto),
    Row::Item(MenuItem::Recording),
    Row::Gap,
    Row::Heading("After capture tasks"),
    Row::Item(MenuItem::CopyToggle),
    Row::Item(MenuItem::SaveToggle),
    Row::Gap,
    Row::Heading("Tools"),
    Row::Item(MenuItem::Folder),
    Row::Item(MenuItem::OpenImage),
    Row::Item(MenuItem::CopyImage),
    Row::Gap,
    Row::Item(MenuItem::Quit),
    Row::Gap,
];

/// The panel that receives the arrow keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Menu,
    History,
}

/// One encoded preview with the file and cell size it was made for.
struct Preview {
    path: PathBuf,
    size: (u16, u16),
    protocol: Option<Protocol>,
}

/// Complete state of the terminal hub.
struct App {
    settings: Arc<HubSettings>,
    hub: Hub,
    picker: Picker,
    cursor: usize,
    focus: Focus,
    history: Vec<HistoryEntry>,
    selected: usize,
    preview: Option<Preview>,
    status: String,
    failed: bool,
    hotkeys: [bool; 3],
    auto: bool,
    capturing: bool,
    quit: bool,
}

/// Runs the hub until the user quits.
pub fn run(settings: Arc<HubSettings>) -> Result<()> {
    let hub = Hub::start(Arc::clone(&settings))?;
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let history = list_history(&settings.directory);
    let mut app = App {
        settings,
        hub,
        picker,
        cursor: 1,
        focus: Focus::Menu,
        history,
        selected: 0,
        preview: None,
        status: "Ready".to_owned(),
        failed: false,
        hotkeys: [false; 3],
        auto: false,
        capturing: false,
        quit: false,
    };
    ratatui::run(|terminal| app.run(terminal))
}

impl App {
    /// Draws frames and handles input until the user quits.
    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            while let Ok(event) = self.hub.events().try_recv() {
                self.on_hub_event(event);
            }
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            {
                self.on_key(key.code);
            }
        }
        Ok(())
    }

    /// Stores a status line. Errors use the alert color.
    fn report(&mut self, status: String, failed: bool) {
        self.status = status;
        self.failed = failed;
    }

    /// Applies one report from the capture thread.
    fn on_hub_event(&mut self, event: HubEvent) {
        match event {
            HubEvent::Ready(hotkeys) => self.hotkeys = hotkeys,
            HubEvent::Capturing => {
                self.capturing = true;
                self.report("Capturing".to_owned(), false);
            }
            HubEvent::Finished(summary) => {
                self.capturing = false;
                let target = summary.output.as_deref().map_or_else(
                    || "Copied to the clipboard".to_owned(),
                    |path| format!("Saved {}", file_name(path)),
                );
                let mut status = format!("{target} · {}x{}", summary.width, summary.height);
                if summary.copied && summary.output.is_some() {
                    status.push_str(" · copied");
                }
                if summary.hdr {
                    status.push_str(&format!(" · HDR at {:.0} nits", summary.sdr_white_nits));
                }
                if let Some(error) = summary.clipboard_error {
                    status.push_str(&format!(" · clipboard failed: {error}"));
                }
                self.report(status, false);
                self.history = list_history(&self.settings.directory);
                self.selected = 0;
            }
            HubEvent::Cancelled => {
                self.capturing = false;
                self.report("Cancelled. No file was written.".to_owned(), false);
            }
            HubEvent::Failed(error) => {
                self.capturing = false;
                self.report(format!("Capture failed: {error}"), true);
            }
            HubEvent::Auto(active) => {
                self.auto = active;
                let seconds = self.settings.auto_interval.as_secs_f64();
                self.report(
                    if active {
                        format!("Auto capture every {seconds} s")
                    } else {
                        "Auto capture stopped".to_owned()
                    },
                    false,
                );
            }
            HubEvent::QuitRequested => self.quit = true,
        }
    }

    /// Handles one key press.
    fn on_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.focus = match self.focus {
                    Focus::Menu => Focus::History,
                    Focus::History => Focus::Menu,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => self.step(false),
            KeyCode::Down | KeyCode::Char('j') => self.step(true),
            KeyCode::Enter => match (self.focus, ROWS[self.cursor]) {
                (Focus::Menu, Row::Item(item)) => self.activate(item),
                _ => self.activate(MenuItem::OpenImage),
            },
            KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
            KeyCode::Char(character) => {
                let item = match character.to_ascii_lowercase() {
                    'r' => MenuItem::Region,
                    'f' => MenuItem::Fullscreen,
                    'w' => MenuItem::Window,
                    'm' => MenuItem::Monitor,
                    'l' => MenuItem::LastRegion,
                    'd' => MenuItem::DelayedRegion,
                    'a' => MenuItem::Auto,
                    'v' => MenuItem::Recording,
                    'c' => MenuItem::CopyImage,
                    'o' => MenuItem::Folder,
                    _ => return,
                };
                self.activate(item);
            }
            _ => {}
        }
    }

    /// Moves the menu cursor or the history selection by one entry.
    fn step(&mut self, forward: bool) {
        match self.focus {
            Focus::History => {
                let last = self.history.len().saturating_sub(1);
                self.selected = if forward {
                    (self.selected + 1).min(last)
                } else {
                    self.selected.saturating_sub(1)
                };
            }
            Focus::Menu => {
                let mut index = self.cursor;
                loop {
                    index = if forward {
                        (index + 1) % ROWS.len()
                    } else {
                        (index + ROWS.len() - 1) % ROWS.len()
                    };
                    if matches!(ROWS[index], Row::Item(_)) {
                        break;
                    }
                }
                self.cursor = index;
            }
        }
    }

    /// Runs one menu entry.
    fn activate(&mut self, item: MenuItem) {
        let capture = |mode| HubRequest::Capture(mode);
        let request = match item {
            MenuItem::Region => Some(capture(CaptureMode::Region)),
            MenuItem::Fullscreen => Some(capture(CaptureMode::Fullscreen)),
            MenuItem::Monitor => Some(capture(CaptureMode::Monitor)),
            MenuItem::LastRegion => Some(capture(CaptureMode::LastRegion)),
            // From the menu, the active window is this terminal. The delay
            // gives the user time to switch to the window they want.
            MenuItem::Window => Some(HubRequest::Delayed(CaptureMode::Window)),
            MenuItem::DelayedRegion => Some(HubRequest::Delayed(CaptureMode::Region)),
            MenuItem::Auto => Some(HubRequest::ToggleAuto),
            _ => None,
        };
        if let Some(request) = request {
            if self.capturing && request != HubRequest::ToggleAuto {
                self.report("A capture is already running".to_owned(), false);
            } else {
                if matches!(request, HubRequest::Delayed(_)) {
                    self.report("Capturing in 3 seconds".to_owned(), false);
                }
                self.hub.request(request);
            }
            return;
        }
        let selected = self
            .history
            .get(self.selected)
            .map(|entry| entry.path.clone());
        match item {
            MenuItem::CopyToggle => self.toggle(true),
            MenuItem::SaveToggle => self.toggle(false),
            MenuItem::Folder => {
                let _ = std::fs::create_dir_all(&self.settings.directory);
                self.launch(Command::new("explorer").arg(&self.settings.directory));
            }
            MenuItem::Recording => {
                if let Ok(program) = std::env::current_exe() {
                    self.launch(
                        Command::new(program)
                            .arg("video")
                            .creation_flags(CREATE_NEW_CONSOLE),
                    );
                }
            }
            MenuItem::OpenImage => match selected {
                Some(path) => self.launch(Command::new("explorer").arg(path)),
                None => self.report("No screenshot is selected".to_owned(), false),
            },
            MenuItem::CopyImage => match selected {
                Some(path) => match copy_file_to_clipboard(&path) {
                    Ok(()) => self.report(format!("Copied {}", file_name(&path)), false),
                    Err(error) => self.report(format!("Copy failed: {error:#}"), true),
                },
                None => self.report("No screenshot is selected".to_owned(), false),
            },
            MenuItem::Quit => self.quit = true,
            _ => {}
        }
    }

    /// Flips one after-capture task. At least one task always stays active.
    fn toggle(&mut self, copy: bool) {
        let (changed, other) = if copy {
            (&self.settings.copy, &self.settings.save)
        } else {
            (&self.settings.save, &self.settings.copy)
        };
        let active = !changed.load(Ordering::Relaxed);
        changed.store(active, Ordering::Relaxed);
        if !active {
            other.store(true, Ordering::Relaxed);
        }
    }

    /// Starts a helper program and reports a failure in the status line.
    fn launch(&mut self, command: &mut Command) {
        if let Err(error) = command.spawn() {
            self.report(format!("Could not start the program: {error}"), true);
        }
    }

    /// Returns the label and the shortcut text of one menu entry.
    fn label(&self, item: MenuItem) -> (String, String) {
        let hotkey = |index: usize, keys: &str| {
            if self.hotkeys[index] {
                keys.to_owned()
            } else {
                format!("{keys} in use")
            }
        };
        let check = |active: bool| if active { "[x]" } else { "[ ]" };
        let (label, shortcut) = match item {
            MenuItem::Region => ("Region".to_owned(), hotkey(0, "Ctrl+PrtSc")),
            MenuItem::Fullscreen => ("Fullscreen".to_owned(), hotkey(1, "PrtSc")),
            MenuItem::Window => ("Active window".to_owned(), hotkey(2, "Alt+PrtSc")),
            MenuItem::Monitor => ("Active monitor".to_owned(), "M".to_owned()),
            MenuItem::LastRegion => ("Last region".to_owned(), "L".to_owned()),
            MenuItem::DelayedRegion => ("Region in 3 seconds".to_owned(), "D".to_owned()),
            MenuItem::Auto => (
                format!("Auto capture: {}", if self.auto { "on" } else { "off" }),
                "A".to_owned(),
            ),
            MenuItem::Recording => ("Screen recording...".to_owned(), "V".to_owned()),
            MenuItem::CopyToggle => (
                format!(
                    "{} Copy image to clipboard",
                    check(self.settings.copy.load(Ordering::Relaxed))
                ),
                String::new(),
            ),
            MenuItem::SaveToggle => (
                format!(
                    "{} Save image to file",
                    check(self.settings.save.load(Ordering::Relaxed))
                ),
                String::new(),
            ),
            MenuItem::Folder => ("Screenshots folder...".to_owned(), "O".to_owned()),
            MenuItem::OpenImage => ("Open selected image".to_owned(), "Enter".to_owned()),
            MenuItem::CopyImage => ("Copy selected image".to_owned(), "C".to_owned()),
            MenuItem::Quit => ("Quit".to_owned(), "Q".to_owned()),
        };
        (label, shortcut)
    }

    /// Draws the complete hub.
    fn render(&mut self, frame: &mut Frame) {
        let [body, status] =
            Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).areas(frame.area());
        let [menu, right] =
            Layout::horizontal([Constraint::Length(MENU_WIDTH), Constraint::Min(10)]).areas(body);
        let history_height = (self.history.len() as u16 + 2).clamp(3, 9);
        let [preview, history] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(history_height)]).areas(right);
        self.render_menu(frame, menu);
        self.render_preview(frame, preview);
        self.render_history(frame, history);
        let color = if self.failed {
            RED
        } else if self.capturing {
            AMBER
        } else {
            GREEN
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {} ", self.status), Style::default().fg(color)),
                Span::styled(
                    " Tab switches panels · Enter runs · Q quits",
                    Style::default().fg(MUTED),
                ),
            ])),
            status,
        );
    }

    /// Returns a bordered panel whose border shows the keyboard focus.
    fn panel(&self, title: String, focus: Option<Focus>) -> Block<'static> {
        let color = if focus == Some(self.focus) {
            CYAN
        } else {
            BORDER
        };
        Block::bordered()
            .title(title)
            .border_style(Style::default().fg(color))
    }

    /// Draws the capture menu.
    fn render_menu(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" record screenshot {} ", env!("CARGO_PKG_VERSION"));
        let block = self.panel(title, Some(Focus::Menu));
        let width = block.inner(area).width as usize;
        let lines: Vec<Line> = ROWS
            .iter()
            .enumerate()
            .map(|(index, row)| match row {
                Row::Gap => Line::default(),
                Row::Heading(text) => Line::styled(
                    format!(" {text}"),
                    Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
                ),
                Row::Item(item) => {
                    let (label, shortcut) = self.label(*item);
                    let room = width.saturating_sub(4 + shortcut.chars().count());
                    let text = format!("   {label:<room$}{shortcut} ");
                    let style = if index == self.cursor && self.focus == Focus::Menu {
                        Style::default().fg(CYAN).add_modifier(Modifier::REVERSED)
                    } else if index == self.cursor {
                        Style::default().fg(CYAN)
                    } else {
                        Style::default()
                    };
                    Line::styled(text, style)
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// Draws the selected screenshot.
    fn render_preview(&mut self, frame: &mut Frame, area: Rect) {
        let selected = self.history.get(self.selected).cloned();
        let title = selected.as_ref().map_or_else(
            || " No screenshots yet ".to_owned(),
            |entry| format!(" {} ", entry.name),
        );
        let block = self.panel(title, None);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(entry) = selected else {
            self.preview = None;
            let hint = Line::styled("Press R to capture a region.", Style::default().fg(MUTED));
            frame.render_widget(Paragraph::new(hint), inner);
            return;
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let size = (inner.width, inner.height);
        let stale = self
            .preview
            .as_ref()
            .is_none_or(|preview| preview.path != entry.path || preview.size != size);
        if stale {
            let protocol = image::open(&entry.path).ok().and_then(|image| {
                self.picker
                    .new_protocol(image, size.into(), Resize::Fit(None))
                    .ok()
            });
            self.preview = Some(Preview {
                path: entry.path.clone(),
                size,
                protocol,
            });
        }
        match self
            .preview
            .as_ref()
            .and_then(|preview| preview.protocol.as_ref())
        {
            Some(protocol) => frame.render_widget(Image::new(protocol), inner),
            None => frame.render_widget(
                Paragraph::new(Line::styled(
                    "This file has no preview.",
                    Style::default().fg(MUTED),
                )),
                inner,
            ),
        }
    }

    /// Draws the list of saved screenshots around the selection.
    fn render_history(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" History · {} ", self.settings.directory.display());
        let block = self.panel(title, Some(Focus::History));
        let rows = block.inner(area).height as usize;
        let first = self
            .selected
            .saturating_sub(rows.saturating_sub(1))
            .min(self.history.len().saturating_sub(rows));
        let lines: Vec<Line> = self
            .history
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(index, entry)| {
                let style = if index == self.selected && self.focus == Focus::History {
                    Style::default().fg(CYAN).add_modifier(Modifier::REVERSED)
                } else if index == self.selected {
                    Style::default().fg(CYAN)
                } else {
                    Style::default()
                };
                Line::styled(format!(" {} ", entry.name), style)
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
}

/// Returns the file name of a path for status lines.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
