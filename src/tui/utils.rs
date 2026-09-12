//! Shared formatting and layout helpers for the terminal interfaces.

use std::{path::Path, time::Duration};

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line as TextLine, Span},
    widgets::{Block, BorderType, Borders},
};

use super::{BORDER, CYAN};

/// Formats a duration as an hours, minutes, and seconds clock.
pub(crate) fn format_clock(duration: Duration) -> String {
    let total = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        total / 3_600,
        total / 60 % 60,
        total % 60
    )
}

/// Creates the common rounded panel style.
pub(crate) fn panel(title: &str, color: Color) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(TextLine::from(title).fg(color).bold())
}

/// Creates one aligned key row for the help overlay.
pub(super) fn help_line(key: &'static str, description: &'static str) -> TextLine<'static> {
    TextLine::from(vec![
        Span::styled(format!("{key:<24}"), Style::default().fg(CYAN).bold()),
        Span::raw(description),
    ])
}

/// Centers a rectangle and clamps it to the terminal area.
pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let horizontal = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .split(area)[0];
    Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .split(horizontal)[0]
}

/// Keeps the end of a path within a display-width character limit.
pub(crate) fn compact_path(path: &Path, maximum: usize) -> String {
    if maximum == 0 {
        return String::new();
    }
    let text = path.display().to_string();
    if text.chars().count() <= maximum {
        return text;
    }
    if maximum <= 3 {
        return ".".repeat(maximum);
    }
    let tail = text
        .chars()
        .rev()
        .take(maximum.saturating_sub(3))
        .collect::<String>();
    format!("...{}", tail.chars().rev().collect::<String>())
}
