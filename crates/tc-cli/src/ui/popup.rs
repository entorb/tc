use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::{BAR, draw_edit};
use tc_app::fmt::Dialog;
use tc_core::cmdline::LineEdit;
/// Centered text popup; returns its outer area.
pub(super) fn popup(frame: &mut Frame, title: &str, text: &str) -> Rect {
    popup_area(frame, title, text)
}

/// Centered popup sized to its text; returns the outer area.
fn popup_area(frame: &mut Frame, title: &str, text: &str) -> Rect {
    let screen = frame.area();
    let longest = text.lines().map(UnicodeWidthStr::width).max().unwrap_or(0);
    let width = (longest + 4)
        .max(60)
        .min(usize::from(screen.width.saturating_sub(2)));
    let inner_w = width.saturating_sub(2).max(1);
    let rows: usize = text
        .lines()
        .map(|l| l.width().div_ceil(inner_w).max(1))
        .sum::<usize>()
        + 2;
    let rows = rows.max(7).min(usize::from(screen.height));
    let [h] = Layout::horizontal([Constraint::Length(width as u16)])
        .flex(Flex::Center)
        .areas(screen);
    let [area] = Layout::vertical([Constraint::Length(rows as u16)])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .title(title);
    let para = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .style(BAR)
        .block(block);
    frame.render_widget(para, area);
    area
}

/// Popup with a text field: `head` lines, the field, then `tail`.
fn popup_edit(frame: &mut Frame, title: &str, head: &str, l: &LineEdit, tail: &str) {
    let text = if head.is_empty() {
        format!("\n{tail}")
    } else {
        format!("{head}\n\n{tail}")
    };
    let inner = Block::bordered()
        .border_type(BorderType::Double)
        .inner(popup_area(frame, title, &text));
    let row = head.lines().count() as u16;
    if row < inner.height {
        let line = Rect {
            y: inner.y + row,
            height: 1,
            ..inner
        };
        draw_edit(frame, line, "", l, BAR);
    }
}

/// Bookmark-like list: up to 4 rows around the selection.
fn list_popup(frame: &mut Frame, title: &str, rows: &[String], sel: usize, empty: &str) {
    let start = (sel + 1).saturating_sub(4);
    let mut text: String = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(4)
        .map(|(i, p)| format!("{}{p}\n", if i == sel { "> " } else { "  " }))
        .collect();
    if rows.is_empty() {
        text.push_str(empty);
        text.push('\n');
    }
    popup(frame, title, &text);
}

pub(super) fn draw_dialog(frame: &mut Frame, d: &Dialog) {
    match d {
        Dialog::Text { title, text } => {
            popup(frame, title, text);
        }
        Dialog::Edit {
            title,
            head,
            line,
            tail,
        } => popup_edit(frame, title, head, line, tail),
        Dialog::List {
            title,
            rows,
            sel,
            empty,
        } => list_popup(frame, title, rows, *sel, empty),
    }
}
