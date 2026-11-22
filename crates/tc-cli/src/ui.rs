//! Rendering of the app state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Paragraph};

use tc_app::app::{App, MENUS, Mode, menu_drop};
use tc_app::fmt::{
    DATE_WIDTH, SIZE_WIDTH, dialog, entry_cols, fit, help, input_dialog, panel_info, panel_title,
};
use tc_core::cmdline::LineEdit;
use tc_core::syntax::Kind;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
mod viewer;
use self::viewer::{draw_editor, draw_view};
mod popup;
#[cfg(test)]
mod tests;
use self::popup::draw_dialog;
mod monitor;
use self::monitor::{draw_procs, draw_stats};
mod forms;
use self::forms::{
    draw_find_form, draw_found, draw_multi_rename, draw_settings, draw_sync, draw_sync_form,
};

// Norton Commander look: blue panels, light-cyan files, white dirs, cyan cursor bar.
const PANEL: Style = Style::new().fg(Color::LightCyan).bg(Color::Blue);
const BAR: Style = Style::new().fg(Color::Black).bg(Color::Cyan);
/// Cursor bar of the inactive panel: background only, entry colors stay readable.
const BAR_INACTIVE: Style = Style::new().bg(Color::DarkGray);
/// Marked files and search hits.
const MARK: Style = Style::new().fg(Color::Rgb(255, 255, 0)).bold();

fn syntax_style(kind: Kind) -> Style {
    match kind {
        Kind::Keyword => Style::new().fg(Color::White).bold(),
        Kind::Str => Style::new().fg(Color::LightGreen),
        Kind::Number => Style::new().fg(Color::LightMagenta),
        Kind::Comment => Style::new().fg(Color::Gray),
        Kind::Bold => Style::new().bold(),
        Kind::Italic => Style::new().italic(),
    }
}

/// Border and fill of viewer, editor and find list, same look as the panels.
fn themed_block() -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Double)
        .style(PANEL)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let full = matches!(
        app.mode,
        Mode::View { .. }
            | Mode::Edit { .. }
            | Mode::AskSave { .. }
            | Mode::AskExit(_)
            | Mode::Found(_)
            | Mode::Sync(_)
            | Mode::Stats(_)
            | Mode::Procs(_)
    ) || (matches!(app.mode, Mode::Cmd(_)) && app.stats.is_some());
    let [body, bar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    if full {
        draw_footer(frame, app, bar);
    }
    if matches!(app.mode, Mode::View { .. }) {
        draw_view(frame, app, body);
        draw_input(frame, app);
        return;
    }

    if let Some(editor) = app.mode.editor_mut() {
        draw_editor(
            frame,
            editor,
            (&mut app.page, &mut app.edit_text),
            &app.note,
            app.search.as_ref(),
            body,
        );
    } else {
        let [bar, main, cmd, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        draw_menu_bar(frame, app, bar);
        draw_cmdline(frame, app, cmd);
        let halves: [Rect; 2] = Layout::horizontal([Constraint::Fill(1); 2]).areas(main);
        app.areas = halves;
        app.page = main.height.saturating_sub(2).max(1) as usize;
        for (i, area) in halves.into_iter().enumerate() {
            draw_panel(frame, app, i, area);
        }
        draw_footer(frame, app, status);
    }

    if let Mode::Menu(g, sel) = app.mode {
        draw_menu_drop(frame, g, sel);
    }

    if let Mode::Stats(st) = &mut app.mode {
        app.stats_map = draw_stats(frame, st, None, body);
    } else if let Mode::Cmd(l) = &app.mode
        && let Some(st) = &mut app.stats
    {
        app.stats_map = draw_stats(frame, st, Some(l), body);
    }

    match &app.mode {
        Mode::Found(f) => draw_found(frame, f, body),
        Mode::FindForm(form) => (app.form_area, app.form_scroll) = draw_find_form(frame, form),
        Mode::MultiRename(form) => {
            (app.form_area, app.form_scroll) = draw_multi_rename(frame, form)
        }
        Mode::SyncForm(form) => (app.form_area, app.form_scroll) = draw_sync_form(frame, form),
        Mode::Settings(form) => (app.form_area, app.form_scroll) = draw_settings(frame, form),
        Mode::Sync(s) => draw_sync(frame, s, body),
        Mode::Stats(_) => {}
        Mode::Procs(_) => draw_procs(frame, app, body),
        Mode::Cmd(_) if app.stats.is_some() => {}
        _ => {
            if let Some(d) = dialog(app) {
                draw_dialog(frame, &d);
            }
        }
    }

    draw_input(frame, app);
}

/// F-key bar of the current screen in `area`; clicks on it are mapped by `App::footer_click`.
fn draw_footer(frame: &mut Frame, app: &mut App, area: Rect) {
    app.footer = area;
    let bar = app
        .footer_keys()
        .iter()
        .map(|k| k.0)
        .collect::<Vec<_>>()
        .join("  ");
    frame.render_widget(Line::raw(bar).style(BAR), area);
}

/// Top row: group titles, the open one highlighted.
fn draw_menu_bar(frame: &mut Frame, app: &App, area: Rect) {
    let open = if let Mode::Menu(g, _) = app.mode {
        Some(g)
    } else {
        None
    };
    let mut spans = Vec::new();
    for (g, (title, _)) in MENUS.iter().enumerate() {
        let st = if open == Some(g) { PANEL } else { BAR };
        spans.push(Span::styled(format!(" {title} "), st));
    }
    frame.render_widget(Line::from(spans).style(BAR), area);
}

fn draw_menu_drop(frame: &mut Frame, group: usize, sel: usize) {
    let area = menu_drop(group).intersection(frame.area());
    frame.render_widget(Clear, area);
    let rows: Vec<Line> = MENUS[group]
        .1
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let st = if i == sel { PANEL } else { BAR };
            Line::styled(
                format!(" {:<w$} ", m.0, w = usize::from(area.width) - 4),
                st,
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(rows)
            .style(BAR)
            .block(Block::bordered().border_type(BorderType::Double)),
        area,
    );
}

/// `dir> text` row.
fn draw_cmdline(frame: &mut Frame, app: &App, area: Rect) {
    let prompt = format!("{}> ", app.panels[app.active].path().display());
    let style = Style::new().fg(Color::Gray).bg(Color::Black);
    match &app.mode {
        Mode::Cmd(l) => draw_edit(frame, area, &prompt, l, style),
        _ => frame.render_widget(Line::raw(prompt).style(style), area),
    }
}

/// `prompt` and the text of `l` in one row; scrolls so the cursor stays visible, and sets the terminal cursor.
fn draw_edit(frame: &mut Frame, area: Rect, prompt: &str, l: &LineEdit, style: Style) {
    let before = prompt.width() + l.text[..l.cur].width();
    let skip = (before + 1).saturating_sub(usize::from(area.width));
    let mut passed = 0;
    let shown: String = format!("{prompt}{}", l.text)
        .chars()
        .skip_while(|c| {
            let skipping = passed < skip;
            passed += c.width().unwrap_or(0);
            skipping
        })
        .collect();
    frame.render_widget(Line::raw(shown).style(style), area);
    let col = before.saturating_sub(skip);
    if col < usize::from(area.width) && area.height > 0 {
        frame.set_cursor_position((area.x + col as u16, area.y));
    }
}

fn draw_input(frame: &mut Frame, app: &mut App) {
    if app.help {
        let title = help(&app.mode).0;
        let text = app.help_entries();
        let lines = text.len();
        let [h] = Layout::horizontal([Constraint::Length(frame.area().width.min(72))])
            .flex(Flex::Center)
            .areas(frame.area());
        let [area] = Layout::vertical([Constraint::Length(
            (lines as u16 + 2).min(frame.area().height),
        )])
        .flex(Flex::Center)
        .areas(h);
        let rows = usize::from(area.height.saturating_sub(2));
        app.help_rows = rows;
        app.help_scroll = app.help_scroll.min(lines.saturating_sub(rows));
        frame.render_widget(Clear, area);
        app.help_area = Block::bordered().inner(area);
        let block = Block::bordered()
            .border_type(BorderType::Double)
            .title(title)
            .title_bottom(format!(
                " Find: {}_  Up/Down select  Enter run  Esc close ",
                app.help_filter
            ));
        let shown: Vec<Line> = text
            .into_iter()
            .enumerate()
            .map(|(i, (l, action))| {
                let sel = i == app.help_sel && action.is_some();
                Line::raw(l).style(if sel {
                    BAR.add_modifier(Modifier::REVERSED)
                } else {
                    BAR
                })
            })
            .collect();
        frame.render_widget(
            Paragraph::new(shown)
                .scroll((app.help_scroll as u16, 0))
                .block(block),
            area,
        );
    }
    if let Some(input) = &app.input {
        draw_dialog(frame, &input_dialog(input));
    }
}

fn draw_panel(frame: &mut Frame, app: &mut App, i: usize, area: Rect) {
    let panel = &app.panels[i];
    let active = i == app.active;
    let width = area.width.saturating_sub(2) as usize;
    let today = jiff::ZonedDateTime::now().date();
    let items: Vec<ListItem> = panel
        .entries()
        .iter()
        .map(|e| {
            let [name, size, date] = entry_cols(app, i, e, today);
            let pad = width.saturating_sub(SIZE_WIDTH + DATE_WIDTH + 2);
            let row = format!(
                "{} {size:>SIZE_WIDTH$} {date:>DATE_WIDTH$}",
                fit(&name, pad)
            );
            let style = if panel.is_selected(e) {
                MARK
            } else if e.is_symlink {
                Style::new().fg(Color::Magenta)
            } else if e.is_dir {
                Style::new().fg(Color::White).bold()
            } else {
                PANEL
            };
            ListItem::new(row).style(style)
        })
        .collect();
    let title = panel_title(app, i);
    let info = panel_info(app, i);
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .title(title)
        .title_bottom(info)
        .style(PANEL);
    let highlight = if active { BAR } else { BAR_INACTIVE };
    let list = List::new(items).block(block).highlight_style(highlight);
    let state = &mut app.lists[i];
    state.select(Some(panel.cursor()));
    frame.render_stateful_widget(list, area, state);
}
