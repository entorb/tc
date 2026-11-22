use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Color;
use ratatui::text::Line;
use ratatui::widgets::{Clear, List, Paragraph};

use super::popup::popup;
use super::{BAR, PANEL, themed_block};
use ratatui::widgets::ListState;
use tc_app::app::{FindForm, Found, RenameForm, SearchForm, SettingsForm, Sync, SyncForm};
use tc_app::fmt::{self, found_title, sync_footer, sync_row, sync_title};
/// Search dialog over the viewer or editor.
pub(super) fn draw_search_form(frame: &mut Frame, form: &SearchForm) -> (Rect, usize) {
    let lines = form_lines(&form.rows, form.focus, form.error.as_deref());
    draw_form(frame, " Search ", lines, form.focus)
}

pub(super) fn draw_found(frame: &mut Frame, f: &Found, area: Rect) {
    let h = usize::from(area.height.saturating_sub(2)).max(1);
    let start = f.sel / h * h; // page-wise, so only visible rows are built
    let items = f.shown().skip(start).take(h).map(fmt::found_row);
    let title = found_title(f);
    let list = List::new(items)
        .style(PANEL)
        .block(themed_block().title(title))
        .highlight_style(BAR);
    let mut st = ListState::default().with_selected(Some(f.sel.saturating_sub(start)));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut st);
}

/// Find files form popup over the panels: fields, checkboxes, the error and the keys.
pub(super) fn draw_find_form(frame: &mut Frame, form: &FindForm) -> (Rect, usize) {
    let lines = form_lines(&form.rows, form.focus, form.error.as_deref());
    draw_form(frame, " Find files ", lines, form.focus)
}

/// Synchronize directories options popup over the panels.
pub(super) fn draw_sync_form(frame: &mut Frame, form: &SyncForm) -> (Rect, usize) {
    let lines = form_lines(&form.rows, form.focus, form.error.as_deref());
    let title = " Synchronize directories (left <-> right panel) ";
    draw_form(frame, title, lines, form.focus)
}

/// Settings popup over the panels (F9 Tools).
pub(super) fn draw_settings(frame: &mut Frame, form: &SettingsForm) -> (Rect, usize) {
    let lines = form_lines(&form.rows, form.focus, None);
    draw_form(frame, " Settings ", lines, form.focus)
}

/// Synchronize directories: page of compared rows holding the cursor, status line, question.
pub(super) fn draw_sync(frame: &mut Frame, s: &Sync, area: Rect) {
    let today = jiff::ZonedDateTime::now().date();
    let h = usize::from(area.height.saturating_sub(3)).max(1);
    let start = s.sel / h * h;
    let items = s.shown().skip(start).take(h).map(|it| sync_row(it, today));
    let list = List::new(items)
        .style(PANEL)
        .block(themed_block().title(sync_title(s)))
        .highlight_style(BAR);
    let mut state = ListState::default().with_selected(Some(s.sel.saturating_sub(start)));
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, main, &mut state);
    frame.render_widget(Line::raw(sync_footer(s)).style(BAR), status);
    if let Some(text) = &s.confirm {
        popup(frame, "Synchronize", text);
    }
}

/// Multi-rename: the form, then the visible part of the old -> new list.
pub(super) fn draw_multi_rename(frame: &mut Frame, form: &RenameForm) -> (Rect, usize) {
    let mut lines = form_lines(&form.rows, form.focus, form.error.as_deref());
    lines.push(Line::raw(format!("{} files:", form.preview.len())).style(PANEL));
    let rows = form
        .preview
        .iter()
        .skip(form.scroll)
        .take(fmt::RENAME_PREVIEW_ROWS);
    lines.extend(rows.map(|(old, new)| {
        let (text, bad) = fmt::rename_row(old, new);
        Line::raw(text).style(if bad {
            PANEL.fg(Color::LightRed)
        } else {
            PANEL
        })
    }));
    lines.push(Line::raw(""));
    lines.extend(
        fmt::RENAME_FORM_KEYS
            .lines()
            .map(|k| Line::raw(k).style(PANEL)),
    );
    draw_form(frame, " Multi-rename ", lines, form.focus)
}

/// Form rows (focused one highlighted) and the error under them.
fn form_lines(
    rows: &[tc_app::app::FormRow],
    focus: usize,
    error: Option<&str>,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = fmt::form_lines(rows, focus)
        .into_iter()
        .map(|(text, focus)| Line::raw(text).style(if focus { BAR } else { PANEL }))
        .collect();
    if let Some(e) = error {
        lines.push(Line::raw(e.to_owned()).style(PANEL.fg(Color::LightRed)));
    }
    lines
}

/// Centered form popup of `lines`; returns the inner area and its first line shown.
fn draw_form(frame: &mut Frame, title: &str, lines: Vec<Line>, focus: usize) -> (Rect, usize) {
    let area = frame.area();
    let w = area.width.min(76);
    let h = (lines.len() as u16 + 2).min(area.height);
    let [col] = Layout::horizontal([Constraint::Length(w)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::vertical([Constraint::Length(h)])
        .flex(Flex::Center)
        .areas(col);
    // short terminals: scroll so the focused row stays visible
    let inner = usize::from(h.saturating_sub(2));
    let off = u16::try_from((focus + 1).saturating_sub(inner)).unwrap_or(0);
    frame.render_widget(Clear, popup);
    let block = themed_block().title(title);
    let text = Paragraph::new(lines)
        .style(PANEL)
        .block(block)
        .scroll((off, 0));
    frame.render_widget(text, popup);
    (themed_block().inner(popup), usize::from(off))
}
