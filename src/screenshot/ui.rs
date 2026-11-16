//! The terminal hub: a capture menu beside a preview and a file history.
//!
//! The hub never captures by itself. It sends requests to a `CaptureHub`
//! and shows the reports that come back. Upload destinations are selected and
//! configured in the hub, and the choices are stored for later runs. The
//! preview uses the best image protocol of the terminal and falls back to
//! half blocks.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::Ordering},
    time::{Duration, SystemTime},
};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};
use ratatui_image::{Image, Resize, picker::Picker, protocol::Protocol};

use super::{
    CaptureMode,
    events::{CaptureHub, HubEvent, HubRequest, HubSettings},
    history::{HistoryEntry, list_history},
    upload::{
        SavedTasks, imgur_client_id, import_sxcu, list_destinations, resolve_destination,
        save_imgur_client_id, set_default_destination,
    },
};
use crate::tui::{AMBER, BORDER, CYAN, GREEN, MUTED, RED};

/// Width of the menu column in terminal cells.
const MENU_WIDTH: u16 = 40;
/// Background of the selected row.
const SELECTED: Color = Color::Rgb(30, 41, 59);
/// Bright text color.
const TEXT: Color = Color::Rgb(226, 232, 240);

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
    UploadToggle,
    Destination,
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
const ROWS: [Row; 23] = [
    Row::Heading("CAPTURE"),
    Row::Item(MenuItem::Region),
    Row::Item(MenuItem::Fullscreen),
    Row::Item(MenuItem::Window),
    Row::Item(MenuItem::Monitor),
    Row::Item(MenuItem::LastRegion),
    Row::Item(MenuItem::DelayedRegion),
    Row::Item(MenuItem::Auto),
    Row::Item(MenuItem::Recording),
    Row::Gap,
    Row::Heading("AFTER CAPTURE"),
    Row::Item(MenuItem::CopyToggle),
    Row::Item(MenuItem::SaveToggle),
    Row::Item(MenuItem::UploadToggle),
    Row::Item(MenuItem::Destination),
    Row::Gap,
    Row::Heading("FILES"),
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

/// What a text prompt collects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Prompt {
    ImgurClientId,
    ImportPath,
}

/// One row of the destination list.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Choice {
    /// Turn uploads off.
    Off,
    /// The built-in Imgur destination.
    Imgur,
    /// A `.sxcu` file in the uploaders folder.
    Named(String),
    /// Ask for a `.sxcu` path to import.
    Import,
    /// Ask for a new Imgur client ID.
    ChangeImgurId,
}

/// The dialog shown over the hub.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Dialog {
    None,
    Destinations { choices: Vec<Choice>, cursor: usize },
    Input { prompt: Prompt, text: String },
}

/// One encoded preview with the file and cell size it was made for.
struct Preview {
    path: PathBuf,
    size: (u16, u16),
    protocol: Option<Protocol>,
}

/// Complete state of the terminal hub.
struct App<H: CaptureHub> {
    settings: Arc<HubSettings>,
    hub: H,
    picker: Picker,
    cursor: usize,
    focus: Focus,
    dialog: Dialog,
    history: Vec<HistoryEntry>,
    selected: usize,
    preview: Option<Preview>,
    status: String,
    failed: bool,
    hotkeys: [bool; 3],
    auto: bool,
    capturing: bool,
    quit: bool,
    /// Writes choices to the settings folder. Tests turn this off.
    persist: bool,
}

/// Runs the hub until the user quits.
#[cfg(windows)]
pub fn run(settings: Arc<HubSettings>) -> Result<()> {
    let hub = super::hub::Hub::start(Arc::clone(&settings))?;
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let mut app = App::new(settings, hub, picker);
    ratatui::run(|terminal| app.run(terminal))
}

impl<H: CaptureHub> App<H> {
    /// Creates the hub state and reads the history of the screenshots folder.
    fn new(settings: Arc<HubSettings>, hub: H, picker: Picker) -> Self {
        let history = list_history(&settings.directory);
        Self {
            settings,
            hub,
            picker,
            cursor: 1,
            focus: Focus::Menu,
            dialog: Dialog::None,
            history,
            selected: 0,
            preview: None,
            status: "Ready".to_owned(),
            failed: false,
            hotkeys: [true; 3],
            auto: false,
            capturing: false,
            quit: false,
            persist: true,
        }
    }

    /// Draws frames and handles input until the user quits.
    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.drain_events();
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

    /// Applies every waiting report from the capture thread.
    fn drain_events(&mut self) {
        while let Ok(event) = self.hub.events().try_recv() {
            self.on_hub_event(event);
        }
    }

    /// Stores a status line. Errors use the alert color.
    fn report(&mut self, status: impl Into<String>, failed: bool) {
        self.status = status.into();
        self.failed = failed;
    }

    /// Applies one report from the capture thread.
    fn on_hub_event(&mut self, event: HubEvent) {
        match event {
            HubEvent::Ready(hotkeys) => self.hotkeys = hotkeys,
            HubEvent::Capturing => {
                self.capturing = true;
                self.report("Capturing", false);
            }
            HubEvent::Finished(summary) => {
                self.capturing = false;
                let target = summary.output.as_deref().map_or_else(
                    || "Copied to the clipboard".to_owned(),
                    |path| format!("Saved {}", file_name(path)),
                );
                let mut status = format!("{target} · {}x{}", summary.width, summary.height);
                if let Some(uploaded) = &summary.uploaded {
                    status = format!("Uploaded {}", uploaded.url);
                    if summary.copied {
                        status.push_str(" · link copied");
                    }
                } else if summary.copied && summary.output.is_some() {
                    status.push_str(" · copied");
                }
                if summary.hdr {
                    status.push_str(&format!(" · HDR at {:.0} nits", summary.sdr_white_nits));
                }
                let failed = summary.upload_error.is_some();
                if let Some(error) = &summary.upload_error {
                    status = format!("Upload failed: {error}");
                }
                if let Some(error) = summary.clipboard_error {
                    status.push_str(&format!(" · clipboard failed: {error}"));
                }
                self.report(status, failed);
                self.history = list_history(&self.settings.directory);
                self.selected = 0;
            }
            HubEvent::Cancelled => {
                self.capturing = false;
                self.report("Cancelled. No file was written.", false);
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
        if self.dialog != Dialog::None {
            self.on_dialog_key(code);
            return;
        }
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
                    'u' => MenuItem::UploadToggle,
                    'p' => MenuItem::Destination,
                    _ => return,
                };
                self.activate(item);
            }
            _ => {}
        }
    }

    /// Handles one key press while a dialog is open.
    fn on_dialog_key(&mut self, code: KeyCode) {
        match &mut self.dialog {
            Dialog::None => {}
            Dialog::Destinations { choices, cursor } => match code {
                KeyCode::Esc | KeyCode::Char('q') => self.dialog = Dialog::None,
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(choices.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    let choice = choices[*cursor].clone();
                    self.choose(choice);
                }
                _ => {}
            },
            Dialog::Input { prompt, text } => match code {
                KeyCode::Esc => self.open_destinations(),
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(character) => text.push(character),
                KeyCode::Enter => {
                    let (prompt, text) = (*prompt, text.trim().trim_matches('"').to_owned());
                    self.submit(prompt, &text);
                }
                _ => {}
            },
        }
    }

    /// Opens the destination list with the active destination selected.
    fn open_destinations(&mut self) {
        let mut choices = vec![Choice::Off, Choice::Imgur];
        choices.extend(list_destinations().into_iter().map(Choice::Named));
        choices.push(Choice::Import);
        if imgur_client_id().is_some() {
            choices.push(Choice::ChangeImgurId);
        }
        let active = self
            .settings
            .active_uploader()
            .map(|uploader| uploader.name);
        let uploading = self.settings.upload.load(Ordering::Relaxed);
        let cursor = choices
            .iter()
            .position(|choice| match (choice, &active) {
                (Choice::Off, _) => !uploading || active.is_none(),
                (Choice::Imgur, Some(name)) => name == "Imgur",
                (Choice::Named(file), Some(name)) => file == name,
                _ => false,
            })
            .unwrap_or(0);
        self.dialog = Dialog::Destinations { choices, cursor };
    }

    /// Applies one row of the destination list.
    fn choose(&mut self, choice: Choice) {
        match choice {
            Choice::Off => {
                self.settings.upload.store(false, Ordering::Relaxed);
                self.store_tasks();
                self.dialog = Dialog::None;
                self.report("Uploads are off", false);
            }
            Choice::Imgur if imgur_client_id().is_none() => self.ask(Prompt::ImgurClientId),
            Choice::Imgur => self.select_destination("imgur"),
            Choice::Named(name) => self.select_destination(&name),
            Choice::Import => self.ask(Prompt::ImportPath),
            Choice::ChangeImgurId => self.ask(Prompt::ImgurClientId),
        }
    }

    /// Opens a text prompt.
    fn ask(&mut self, prompt: Prompt) {
        self.dialog = Dialog::Input {
            prompt,
            text: String::new(),
        };
    }

    /// Applies the text of a finished prompt.
    fn submit(&mut self, prompt: Prompt, text: &str) {
        let destination = match prompt {
            Prompt::ImgurClientId => save_imgur_client_id(text).map(|()| "imgur".to_owned()),
            Prompt::ImportPath => import_sxcu(Path::new(text)),
        };
        match destination {
            Ok(destination) => self.select_destination(&destination),
            Err(error) => self.report(error, true),
        }
    }

    /// Makes a destination active, turns uploads on, and stores the choice.
    fn select_destination(&mut self, destination: &str) {
        match resolve_destination(destination) {
            Ok(uploader) => {
                let name = uploader.name.clone();
                if let Ok(mut active) = self.settings.uploader.lock() {
                    *active = Some(uploader);
                }
                self.settings.upload.store(true, Ordering::Relaxed);
                self.store_tasks();
                let stored = if self.persist {
                    set_default_destination(Some(destination))
                } else {
                    Ok(())
                };
                self.dialog = Dialog::None;
                match stored {
                    Ok(()) => self.report(format!("Uploads go to {name}"), false),
                    Err(error) => self.report(error, true),
                }
            }
            Err(error) => self.report(error, true),
        }
    }

    /// Stores the after-capture tasks for later runs.
    fn store_tasks(&self) {
        if !self.persist {
            return;
        }
        SavedTasks {
            copy: self.settings.copy.load(Ordering::Relaxed),
            save: self.settings.save.load(Ordering::Relaxed),
            upload: self.settings.upload.load(Ordering::Relaxed),
        }
        .store();
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
                self.report("A capture is already running", false);
            } else {
                if matches!(request, HubRequest::Delayed(_)) {
                    self.report("Capturing in 3 seconds", false);
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
            MenuItem::UploadToggle => {
                if self.settings.active_uploader().is_some() {
                    self.settings.upload.fetch_xor(true, Ordering::Relaxed);
                    self.store_tasks();
                } else {
                    self.open_destinations();
                }
            }
            MenuItem::Destination => self.open_destinations(),
            MenuItem::Folder => {
                let _ = std::fs::create_dir_all(&self.settings.directory);
                self.launch(Command::new("explorer").arg(&self.settings.directory));
            }
            MenuItem::Recording => {
                if let Ok(program) = std::env::current_exe() {
                    let mut command = Command::new(program);
                    command.arg("video");
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;

                        // Gives the recorder its own console window.
                        command.creation_flags(0x10);
                    }
                    self.launch(&mut command);
                }
            }
            MenuItem::OpenImage => match selected {
                Some(path) => self.launch(Command::new("explorer").arg(path)),
                None => self.report("No screenshot is selected", false),
            },
            MenuItem::CopyImage => match selected {
                Some(path) => match copy_image(&path) {
                    Ok(()) => self.report(format!("Copied {}", file_name(&path)), false),
                    Err(error) => self.report(format!("Copy failed: {error:#}"), true),
                },
                None => self.report("No screenshot is selected", false),
            },
            MenuItem::Quit => self.quit = true,
            _ => {}
        }
    }

    /// Flips one after-capture task. Copy or save always stays active.
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
        self.store_tasks();
    }

    /// Starts a helper program and reports a failure in the status line.
    fn launch(&mut self, command: &mut Command) {
        if let Err(error) = command.spawn() {
            self.report(format!("Could not start the program: {error}"), true);
        }
    }

    /// Returns the label, the shortcut text, and the switch state of an entry.
    fn label(&self, item: MenuItem) -> (String, String, Option<bool>) {
        let hotkey = |index: usize, keys: &str| {
            if self.hotkeys[index] {
                keys.to_owned()
            } else {
                format!("{keys} in use")
            }
        };
        let flag = |flag: &std::sync::atomic::AtomicBool| Some(flag.load(Ordering::Relaxed));
        let uploader = self
            .settings
            .active_uploader()
            .map(|uploader| uploader.name);
        let plain = |label: &str, keys: &str| (label.to_owned(), keys.to_owned(), None);
        match item {
            MenuItem::Region => ("Region".to_owned(), hotkey(0, "Ctrl+PrtSc"), None),
            MenuItem::Fullscreen => ("Fullscreen".to_owned(), hotkey(1, "PrtSc"), None),
            MenuItem::Window => ("Active window".to_owned(), hotkey(2, "Alt+PrtSc"), None),
            MenuItem::Monitor => plain("Active monitor", "M"),
            MenuItem::LastRegion => plain("Last region", "L"),
            MenuItem::DelayedRegion => plain("Region in 3 seconds", "D"),
            MenuItem::Auto => ("Auto capture".to_owned(), "A".to_owned(), Some(self.auto)),
            MenuItem::Recording => plain("Screen recording", "V"),
            MenuItem::CopyToggle => (
                "Copy to clipboard".to_owned(),
                String::new(),
                flag(&self.settings.copy),
            ),
            MenuItem::SaveToggle => (
                "Save to file".to_owned(),
                String::new(),
                flag(&self.settings.save),
            ),
            MenuItem::UploadToggle => (
                "Upload and copy link".to_owned(),
                "U".to_owned(),
                Some(uploader.is_some() && self.settings.upload.load(Ordering::Relaxed)),
            ),
            MenuItem::Destination => (
                format!("Destination: {}", uploader.as_deref().unwrap_or("not set")),
                "P".to_owned(),
                None,
            ),
            MenuItem::Folder => plain("Open screenshots folder", "O"),
            MenuItem::OpenImage => plain("Open selected image", "Enter"),
            MenuItem::CopyImage => plain("Copy selected image", "C"),
            MenuItem::Quit => plain("Quit", "Q"),
        }
    }

    /// Draws the complete hub.
    fn render(&mut self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let [menu, right] =
            Layout::horizontal([Constraint::Length(MENU_WIDTH), Constraint::Min(10)]).areas(body);
        let history_height = (self.history.len() as u16 + 2).clamp(3, 9);
        let [preview, history] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(history_height)]).areas(right);
        self.render_header(frame, header);
        self.render_menu(frame, menu);
        self.render_preview(frame, preview);
        self.render_history(frame, history);
        self.render_footer(frame, footer);
        match &self.dialog {
            Dialog::None => {}
            Dialog::Destinations { choices, cursor } => {
                self.render_destinations(frame, choices, *cursor);
            }
            Dialog::Input { prompt, text } => render_input(frame, *prompt, text),
        }
    }

    /// Draws the title line with the folder that receives the files.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let title = Line::from(vec![
            Span::styled(
                " record screenshot ",
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                concat!("v", env!("CARGO_PKG_VERSION")),
                Style::default().fg(MUTED),
            ),
        ]);
        frame.render_widget(Paragraph::new(title), area);
        let folder = Line::styled(
            format!("{} ", self.settings.directory.display()),
            Style::default().fg(MUTED),
        );
        frame.render_widget(Paragraph::new(folder).alignment(Alignment::Right), area);
    }

    /// Draws the status line and the key hints.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let color = if self.failed {
            RED
        } else if self.capturing {
            AMBER
        } else {
            GREEN
        };
        let status = Line::from(vec![
            Span::styled(" ● ", Style::default().fg(color)),
            Span::styled(self.status.clone(), Style::default().fg(TEXT)),
        ]);
        let hints = Line::styled(
            "Tab panels · ↑↓ move · Enter run · Q quit ",
            Style::default().fg(MUTED),
        );
        let hint_width = (hints.width() as u16).min(area.width);
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(hint_width)]).areas(area);
        frame.render_widget(Paragraph::new(status), left);
        frame.render_widget(Paragraph::new(hints), right);
    }

    /// Returns a rounded panel whose border shows the keyboard focus.
    fn panel(&self, title: String, focus: Option<Focus>) -> Block<'static> {
        let focused = focus == Some(self.focus) && self.dialog == Dialog::None;
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if focused { CYAN } else { BORDER }))
            .title(Span::styled(
                title,
                Style::default().fg(if focused { CYAN } else { MUTED }),
            ))
    }

    /// Draws the capture menu and keeps the cursor row in view.
    fn render_menu(&self, frame: &mut Frame, area: Rect) {
        let block = self.panel(" Menu ".to_owned(), Some(Focus::Menu));
        let inner = block.inner(area);
        let width = inner.width as usize;
        let lines: Vec<Line> = ROWS
            .iter()
            .enumerate()
            .map(|(index, row)| match row {
                Row::Gap => Line::default(),
                Row::Heading(text) => Line::styled(
                    format!("  {text}"),
                    Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
                ),
                Row::Item(item) => {
                    let selected = index == self.cursor;
                    let (label, shortcut, switch) = self.label(*item);
                    menu_line(
                        &label,
                        &shortcut,
                        switch,
                        selected,
                        self.focus == Focus::Menu,
                        width,
                    )
                }
            })
            .collect();
        let visible = inner.height as usize;
        let scroll = (self.cursor + 2)
            .saturating_sub(visible)
            .min(ROWS.len().saturating_sub(visible));
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((scroll as u16, 0)),
            area,
        );
    }

    /// Draws the selected screenshot.
    fn render_preview(&mut self, frame: &mut Frame, area: Rect) {
        let selected = self.history.get(self.selected).cloned();
        let title = selected.as_ref().map_or_else(
            || " Preview ".to_owned(),
            |entry| format!(" {} ", entry.name),
        );
        let block = self.panel(title, None);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(entry) = selected else {
            self.preview = None;
            let hint = vec![
                Line::default(),
                Line::styled("No screenshots yet", Style::default().fg(TEXT)),
                Line::styled("Press R to capture a region", Style::default().fg(MUTED)),
            ];
            frame.render_widget(Paragraph::new(hint).alignment(Alignment::Center), inner);
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
                    "This file has no preview",
                    Style::default().fg(MUTED),
                ))
                .alignment(Alignment::Center),
                inner,
            ),
        }
    }

    /// Draws the list of saved screenshots around the selection.
    fn render_history(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" History · {} ", self.history.len());
        let block = self.panel(title, Some(Focus::History));
        let inner = block.inner(area);
        let rows = inner.height as usize;
        let first = self
            .selected
            .saturating_sub(rows.saturating_sub(1))
            .min(self.history.len().saturating_sub(rows));
        let now = SystemTime::now();
        let lines: Vec<Line> = self
            .history
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(index, entry)| {
                menu_line(
                    &entry.name,
                    &age_text(now, entry.modified),
                    None,
                    index == self.selected,
                    self.focus == Focus::History,
                    inner.width as usize,
                )
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// Draws the destination list over the hub.
    fn render_destinations(&self, frame: &mut Frame, choices: &[Choice], cursor: usize) {
        let has_id = imgur_client_id().is_some();
        let area = centered(frame.area(), 58, choices.len() as u16 + 5);
        let block = dialog_block(" Upload destination ");
        let inner = block.inner(area);
        let mut lines: Vec<Line> = choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let (label, note) = match choice {
                    Choice::Off => ("Off", "keep images local"),
                    Choice::Imgur if has_id => ("Imgur", "ready"),
                    Choice::Imgur => ("Imgur", "needs a client ID"),
                    Choice::Named(name) => (name.as_str(), ".sxcu"),
                    Choice::Import => ("Import a .sxcu file", ""),
                    Choice::ChangeImgurId => ("Change the Imgur client ID", ""),
                };
                menu_line(
                    label,
                    note,
                    None,
                    index == cursor,
                    true,
                    inner.width as usize,
                )
            })
            .collect();
        lines.push(Line::default());
        lines.push(Line::styled(
            "  Enter select · Esc close",
            Style::default().fg(MUTED),
        ));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
}

/// Builds one selectable row: marker, optional switch, label, and a note at
/// the right edge.
fn menu_line(
    label: &str,
    note: &str,
    switch: Option<bool>,
    selected: bool,
    focused: bool,
    width: usize,
) -> Line<'static> {
    let active = selected && focused;
    let marker = if selected { "▌" } else { " " };
    let (dot, dot_color) = match switch {
        Some(true) => ("● ", GREEN),
        Some(false) => ("○ ", MUTED),
        None => ("", MUTED),
    };
    let used = 2 + dot.chars().count() + note.chars().count() + 1;
    let room = width.saturating_sub(used);
    let label: String = if label.chars().count() > room && room > 1 {
        label.chars().take(room - 1).chain(['…']).collect()
    } else {
        label.to_owned()
    };
    let base = if active {
        Style::default().bg(SELECTED)
    } else {
        Style::default()
    };
    let label_style = if active {
        base.fg(Color::White).add_modifier(Modifier::BOLD)
    } else if selected {
        base.fg(CYAN)
    } else {
        base.fg(TEXT)
    };
    Line::from(vec![
        Span::styled(marker, base.fg(CYAN)),
        Span::styled(" ", base),
        Span::styled(dot, base.fg(dot_color)),
        Span::styled(format!("{label:<room$}"), label_style),
        Span::styled(format!("{note} "), base.fg(MUTED)),
    ])
}

/// Returns a bordered block for a dialog.
fn dialog_block(title: &'static str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CYAN))
        .title(Span::styled(
            title,
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ))
}

/// Returns a rectangle of the given size in the middle of an area.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// Draws a text prompt over the hub.
fn render_input(frame: &mut Frame, prompt: Prompt, text: &str) {
    let (title, help) = match prompt {
        Prompt::ImgurClientId => (
            " Imgur client ID ",
            [
                "Register an application at",
                "api.imgur.com/oauth2/addclient",
                "and paste its client ID here.",
            ],
        ),
        Prompt::ImportPath => (
            " Import a .sxcu file ",
            [
                "Paste the path of a ShareX custom",
                "uploader file. A copy goes into the",
                "uploaders folder.",
            ],
        ),
    };
    let area = centered(frame.area(), 58, 9);
    let block = dialog_block(title);
    let room = block.inner(area).width.saturating_sub(5) as usize;
    let count = text.chars().count();
    let shown: String = text.chars().skip(count.saturating_sub(room)).collect();
    let mut lines: Vec<Line> = help
        .iter()
        .map(|line| Line::styled(format!("  {line}"), Style::default().fg(MUTED)))
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("  › ", Style::default().fg(CYAN)),
        Span::styled(shown, Style::default().fg(Color::White)),
        Span::styled("█", Style::default().fg(CYAN)),
    ]));
    lines.push(Line::default());
    lines.push(Line::styled(
        "  Enter save · Esc back",
        Style::default().fg(MUTED),
    ));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Formats how long ago a file changed, such as `5m` or `3d`.
fn age_text(now: SystemTime, modified: SystemTime) -> String {
    let seconds = now.duration_since(modified).map_or(0, |age| age.as_secs());
    match seconds {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Places a saved image on the clipboard.
fn copy_image(path: &Path) -> Result<()> {
    #[cfg(windows)]
    return super::copy_file_to_clipboard(path);
    #[cfg(not(windows))]
    anyhow::bail!("{} cannot be copied on this platform", path.display())
}

/// Returns the file name of a path for status lines.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, atomic::AtomicBool};

    use crossbeam_channel::{Receiver, Sender, unbounded};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::screenshot::{MonitorChoice, ScreenshotSummary, upload::Uploader};

    /// Records requests in place of the capture thread.
    struct FakeHub {
        events: Receiver<HubEvent>,
        requests: Mutex<Vec<HubRequest>>,
    }

    impl CaptureHub for FakeHub {
        fn events(&self) -> &Receiver<HubEvent> {
            &self.events
        }

        fn request(&self, request: HubRequest) {
            self.requests.lock().unwrap().push(request);
        }
    }

    /// Creates a hub on an empty folder with a fake capture thread.
    fn app(uploader: Option<Uploader>) -> (App<FakeHub>, Sender<HubEvent>, tempfile::TempDir) {
        let folder = tempfile::tempdir().unwrap();
        let (sender, events) = unbounded();
        let settings = Arc::new(HubSettings {
            directory: folder.path().to_path_buf(),
            monitor: MonitorChoice::Pointer,
            sdr_white_nits: None,
            copy: AtomicBool::new(true),
            save: AtomicBool::new(true),
            auto_interval: Duration::from_secs(5),
            uploader: Mutex::new(uploader),
            upload: AtomicBool::new(false),
        });
        let hub = FakeHub {
            events,
            requests: Mutex::new(Vec::new()),
        };
        let mut app = App::new(settings, hub, Picker::halfblocks());
        app.persist = false;
        (app, sender, folder)
    }

    /// Draws the hub and returns the screen as text lines.
    fn screen(app: &mut App<FakeHub>, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text: Vec<String> = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        let text = text.join("\n");
        if std::env::var_os("RECORD_UI_DUMP").is_some() {
            eprintln!("{text}\n");
        }
        text
    }

    #[test]
    fn the_hub_draws_the_menu_the_empty_preview_and_the_status() {
        let (mut app, _sender, _folder) = app(None);
        let text = screen(&mut app, 110, 30);
        for expected in [
            "record screenshot",
            "CAPTURE",
            "▌ Region",
            "Ctrl+PrtSc",
            "AFTER CAPTURE",
            "● Copy to clipboard",
            "○ Upload and copy link",
            "Destination: not set",
            "No screenshots yet",
            "● Ready",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
    }

    #[test]
    fn a_short_terminal_keeps_the_cursor_row_visible() {
        let (mut app, _sender, _folder) = app(None);
        for _ in 0..15 {
            app.on_key(KeyCode::Down);
        }
        assert_eq!(ROWS[app.cursor], Row::Item(MenuItem::Quit));
        let text = screen(&mut app, 80, 12);
        assert!(text.contains("▌ Quit"), "{text}");
        app.on_key(KeyCode::Down);
        assert_eq!(ROWS[app.cursor], Row::Item(MenuItem::Region));
    }

    #[test]
    fn keys_send_capture_requests_and_respect_a_running_capture() {
        let (mut app, sender, _folder) = app(None);
        app.on_key(KeyCode::Char('f'));
        app.on_key(KeyCode::Char('w'));
        app.on_key(KeyCode::Enter);
        sender.send(HubEvent::Capturing).unwrap();
        app.drain_events();
        app.on_key(KeyCode::Char('r'));
        app.on_key(KeyCode::Char('a'));
        assert_eq!(
            *app.hub.requests.lock().unwrap(),
            [
                HubRequest::Capture(CaptureMode::Fullscreen),
                HubRequest::Delayed(CaptureMode::Window),
                HubRequest::Capture(CaptureMode::Region),
                HubRequest::ToggleAuto,
            ]
        );
        assert_eq!(app.status, "A capture is already running");
    }

    #[test]
    fn a_finished_upload_shows_the_link_and_a_failure_shows_the_reason() {
        let (mut app, sender, folder) = app(None);
        let summary = |uploaded, upload_error| ScreenshotSummary {
            output: Some(folder.path().join("screenshot-1.png")),
            width: 800,
            height: 600,
            copied: true,
            clipboard_error: None,
            uploaded,
            upload_error,
            hdr: true,
            sdr_white_nits: 240.0,
        };
        let link = crate::screenshot::upload::UploadResult {
            url: "https://i.example.com/a.png".to_owned(),
            deletion_url: None,
        };
        sender
            .send(HubEvent::Finished(summary(Some(link), None)))
            .unwrap();
        app.drain_events();
        assert_eq!(
            app.status,
            "Uploaded https://i.example.com/a.png · link copied · HDR at 240 nits"
        );
        assert!(!app.failed);
        sender
            .send(HubEvent::Finished(summary(
                None,
                Some("HTTP status 401".to_owned()),
            )))
            .unwrap();
        app.drain_events();
        assert_eq!(app.status, "Upload failed: HTTP status 401");
        assert!(app.failed);
    }

    #[test]
    fn copy_and_save_cannot_both_be_off() {
        let (mut app, _sender, _folder) = app(None);
        app.toggle(true);
        app.toggle(false);
        let on = |flag: &AtomicBool| flag.load(Ordering::Relaxed);
        assert!(on(&app.settings.copy) && !on(&app.settings.save));
    }

    #[test]
    fn the_upload_switch_opens_the_destination_list_when_none_is_set() {
        let (mut app, _sender, _folder) = app(None);
        app.on_key(KeyCode::Char('u'));
        assert!(matches!(app.dialog, Dialog::Destinations { .. }));
        let text = screen(&mut app, 110, 30);
        for expected in [
            "Upload destination",
            "▌ Off",
            "Imgur",
            "Import a .sxcu file",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        app.on_key(KeyCode::Char('r'));
        assert!(app.hub.requests.lock().unwrap().is_empty());
        app.on_key(KeyCode::Esc);
        assert_eq!(app.dialog, Dialog::None);
        assert!(!app.quit);
    }

    #[test]
    fn the_upload_switch_flips_when_a_destination_is_set() {
        let (mut app, _sender, _folder) = app(Some(Uploader::imgur("abcdef123456")));
        app.on_key(KeyCode::Char('u'));
        assert_eq!(app.dialog, Dialog::None);
        assert!(app.settings.upload.load(Ordering::Relaxed));
        let text = screen(&mut app, 110, 30);
        assert!(text.contains("● Upload and copy link"), "{text}");
        assert!(text.contains("Destination: Imgur"), "{text}");
    }

    #[test]
    fn a_prompt_collects_text_and_escape_returns_to_the_list() {
        let (mut app, _sender, _folder) = app(None);
        app.ask(Prompt::ImportPath);
        for character in "C:\\x.sxcu".chars() {
            app.on_key(KeyCode::Char(character));
        }
        app.on_key(KeyCode::Backspace);
        let text = screen(&mut app, 110, 30);
        assert!(text.contains("› C:\\x.sxc█"), "{text}");
        app.on_key(KeyCode::Esc);
        assert!(matches!(app.dialog, Dialog::Destinations { .. }));
    }

    #[test]
    fn a_bad_import_path_keeps_the_prompt_open_and_reports_the_reason() {
        let (mut app, _sender, folder) = app(None);
        app.ask(Prompt::ImportPath);
        let missing = folder.path().join("missing.sxcu");
        for character in missing.to_string_lossy().chars() {
            app.on_key(KeyCode::Char(character));
        }
        app.on_key(KeyCode::Enter);
        assert!(matches!(app.dialog, Dialog::Input { .. }));
        assert!(
            app.failed && app.status.contains("could not read"),
            "{}",
            app.status
        );
    }

    #[test]
    fn ages_use_the_largest_whole_unit() {
        let now = SystemTime::now();
        let ago = |seconds| now - Duration::from_secs(seconds);
        assert_eq!(age_text(now, ago(5)), "now");
        assert_eq!(age_text(now, ago(125)), "2m");
        assert_eq!(age_text(now, ago(7_300)), "2h");
        assert_eq!(age_text(now, ago(200_000)), "2d");
        assert_eq!(age_text(now, now + Duration::from_secs(60)), "now");
    }
}
