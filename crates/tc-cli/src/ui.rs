//! Rendering of the app state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Paragraph, Wrap};
use regex::Regex;

use ratatui::widgets::ListState;
use std::path::{Path, PathBuf};
use tc::app::{App, FindForm, Found, MENUS, Mode, RenameForm, Stats, Sync, SyncForm, menu_drop};
use tc::fmt::{
    self, DATE_WIDTH, Dialog, Mark, SIZE_WIDTH, dialog, editor_info, entry_cols, fit, found_title,
    help, input_dialog, panel_info, panel_title, stats_footer, stats_row, sync_footer, sync_row,
    sync_title,
};
use tc::fmt::{
    PROC_COLS, proc_row, procs_confirm, procs_footer, procs_header, procs_prompt, procs_title,
};
use tc_core::cmdline::LineEdit;
use tc_core::syntax::{self, Kind, Lang};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

fn draw_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let Mode::View { title, view } = &mut app.mode else {
        return;
    };
    let bottom = format!(
        " {}%  {}  {} ",
        view.percent(),
        view.encoding_name(),
        app.note
    );
    let block = themed_block().title(title.as_str()).title_bottom(bottom);
    let inner = block.inner(area);
    let height = inner.height as usize;
    app.page = height.max(1);
    // numbers only while the top line is known, see `Viewer::top_line`
    let top = view.top_line().filter(|_| app.view_numbers);
    let gutter = if top.is_some() { 7 } else { 0 };
    let lang = syntax::lang(Path::new(title.as_str()));
    // shortcut: viewer assumes no code fence open above the screen, upgrade with a fence index in `Viewer`
    let mut fence = false;
    let visible: Vec<Line> = match view.rows(height, (inner.width as usize).saturating_sub(gutter))
    {
        Ok(rows) => {
            let mut n = top.unwrap_or(0);
            rows.into_iter()
                .map(|(first, text)| {
                    let mut line =
                        highlight(text, lang, &mut fence, app.search.as_ref(), None, (0, &[]));
                    if top.is_some() {
                        let num = if first {
                            format!("{:>6} ", n + 1)
                        } else {
                            " ".repeat(7)
                        };
                        line.spans.insert(0, Span::raw(num));
                    }
                    n += u64::from(first);
                    line
                })
                .collect()
        }
        Err(e) => vec![Line::raw(e.to_string())],
    };
    frame.render_widget(Paragraph::new(visible).style(PANEL).block(block), area);
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

    match &app.mode {
        Mode::Found(f) => draw_found(frame, f, body),
        Mode::FindForm(form) => draw_find_form(frame, form),
        Mode::MultiRename(form) => draw_multi_rename(frame, form),
        Mode::SyncForm(form) => draw_sync_form(frame, form),
        Mode::Sync(s) => draw_sync(frame, s, body),
        Mode::Stats(st) => draw_stats(frame, st, None, body),
        Mode::Procs(_) => draw_procs(frame, app, body),
        Mode::Cmd(l) if let Some(st) = &app.stats => draw_stats(frame, st, Some(l), body),
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

fn draw_found(frame: &mut Frame, f: &Found, area: Rect) {
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
fn draw_find_form(frame: &mut Frame, form: &FindForm) {
    let lines = form_lines(&form.rows, form.focus, form.error.as_deref());
    draw_form(
        frame,
        " Find files ",
        lines,
        form.focus,
        fmt::FIND_FORM_KEYS,
    );
}

/// Synchronize directories options popup over the panels.
fn draw_sync_form(frame: &mut Frame, form: &SyncForm) {
    let lines = form_lines(&form.rows, form.focus, form.error.as_deref());
    let title = " Synchronize directories (left <-> right panel) ";
    draw_form(frame, title, lines, form.focus, fmt::SYNC_FORM_KEYS);
}

/// Synchronize directories: page of compared rows holding the cursor, status line, question.
fn draw_sync(frame: &mut Frame, s: &Sync, area: Rect) {
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
fn draw_multi_rename(frame: &mut Frame, form: &RenameForm) {
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
    draw_form(
        frame,
        " Multi-rename ",
        lines,
        form.focus,
        fmt::RENAME_FORM_KEYS,
    );
}

/// Form rows (focused one highlighted) and the error under them.
fn form_lines(rows: &[tc::app::FormRow], focus: usize, error: Option<&str>) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = fmt::form_lines(rows, focus)
        .into_iter()
        .map(|(text, focus)| Line::raw(text).style(if focus { BAR } else { PANEL }))
        .collect();
    if let Some(e) = error {
        lines.push(Line::raw(e.to_owned()).style(PANEL.fg(Color::LightRed)));
    }
    lines
}

/// Centered form popup: `lines`, then the key help.
fn draw_form(
    frame: &mut Frame,
    title: &str,
    mut lines: Vec<Line>,
    focus: usize,
    keys: &'static str,
) {
    lines.push(Line::raw(""));
    lines.extend(keys.lines().map(|k| Line::raw(k).style(PANEL)));
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
}

/// File Stats: entries of the browsed directory, largest first, with share and bar.
/// `cmd`: command line typed from the stats, shown in the footer row.
fn draw_stats(frame: &mut Frame, st: &Stats, cmd: Option<&LineEdit>, area: Rect) {
    let h = usize::from(area.height.saturating_sub(3)).max(1);
    let start = st.sel / h * h;
    let items = st.shown().skip(start).take(h).map(|n| stats_row(st, n));
    let footer = stats_footer(st);
    let list = List::new(items)
        .style(PANEL)
        .block(themed_block().title(format!(
            " Stats: {}{} ",
            st.path().display(),
            st.filter.tag()
        )))
        .highlight_style(BAR);
    let mut state = ListState::default().with_selected(Some(st.sel.saturating_sub(start)));
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, main, &mut state);
    match cmd {
        Some(l) => {
            let prompt = format!("{}> ", st.path().display());
            let style = Style::new().fg(Color::Gray).bg(Color::Black);
            draw_edit(frame, status, &prompt, l, style);
        }
        None => frame.render_widget(Line::raw(footer).style(BAR), status),
    }
    if let Some(text) = &st.confirm {
        popup(frame, "Delete", text);
    }
}

/// Process monitor: summary, column titles, the page of processes holding the cursor, footer.
fn draw_procs(frame: &mut Frame, app: &mut App, area: Rect) {
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let block = themed_block();
    let [head, cols, rows] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(block.inner(main));
    let h = usize::from(rows.height).max(1);
    app.page = h;
    let Mode::Procs(p) = &app.mode else {
        return;
    };
    let start = p.sel / h * h;
    let items = p
        .rows
        .iter()
        .skip(start)
        .take(h)
        .map(|&i| proc_row(&p.snap.procs[i]));
    let list = List::new(items).style(PANEL).highlight_style(BAR);
    let mut state = ListState::default().with_selected(Some(p.sel.saturating_sub(start)));
    frame.render_widget(Clear, area);
    frame.render_widget(block.title(format!(" {} ", procs_title(p))), main);
    frame.render_widget(Line::raw(procs_header(p)).style(PANEL), head);
    frame.render_widget(Line::raw(PROC_COLS).style(PANEL.bold()), cols);
    frame.render_stateful_widget(list, rows, &mut state);
    if let Some((label, l)) = procs_prompt(p) {
        let style = Style::new().fg(Color::Gray).bg(Color::Black);
        draw_edit(frame, status, label, l, style);
    } else {
        frame.render_widget(Line::raw(procs_footer(p)).style(BAR), status);
    }
    if let Some((pid, name)) = &p.confirm {
        popup(frame, "Kill", &procs_confirm(*pid, name));
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

fn draw_editor(
    frame: &mut Frame,
    editor: &mut tc_core::edit::Editor,
    (page, text_area): (&mut usize, &mut Rect),
    note: &str,
    re: Option<&Regex>,
    area: Rect,
) {
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let block = themed_block().title(editor.path().display().to_string());
    let inner = block.inner(main);
    let gutter = if editor.numbers {
        editor.line_count().to_string().len() + 1
    } else {
        0
    };
    let (height, width) = (
        inner.height as usize,
        (inner.width as usize).saturating_sub(gutter),
    );
    *page = height.max(1);
    *text_area = Rect::new(inner.x + gutter as u16, inner.y, width as u16, inner.height);
    editor.scroll_to_cursor(height, width);
    let brackets = editor.brackets();
    let lang = syntax::lang(editor.path());
    let mut fence = lang.is_some_and(|l| l.markdown) && editor.in_fence(editor.top);
    let lines: Vec<Line> = editor
        .rows(height, width)
        .into_iter()
        .map(|(l, start, row)| {
            let marks: Vec<usize> = brackets
                .iter()
                .flatten()
                .filter(|p| p.0 == l)
                .filter_map(|p| p.1.checked_sub(start))
                .collect();
            let mut line = highlight(
                row,
                lang,
                &mut fence,
                re,
                editor.selection_cols(l),
                (start, &marks),
            );
            if editor.numbers {
                // continuation rows of a wrapped line get a blank gutter
                let num = if start == 0 || !editor.wrap {
                    format!("{:>1$} ", l + 1, gutter - 1)
                } else {
                    " ".repeat(gutter)
                };
                line.spans
                    .insert(0, Span::styled(num, Style::new().fg(Color::DarkGray)));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).style(PANEL).block(block), main);
    let (dy, dx) = editor.cursor_cell(width);
    if dx < width && dy < height {
        frame.set_cursor_position((text_area.x + dx as u16, inner.y + dy as u16));
    }
    let info = editor_info(editor, note);
    frame.render_widget(Line::raw(info).style(BAR), status);
}

/// `row` in `lang` syntax colors, search hits yellow, selection reversed, see [`fmt::highlight`].
fn highlight(
    row: String,
    lang: Option<&Lang>,
    fence: &mut bool,
    re: Option<&Regex>,
    sel: Option<(usize, usize)>,
    cols: (usize, &[usize]),
) -> Line<'static> {
    let spans: Vec<Span> = fmt::highlight(&row, lang, fence, re, sel, cols)
        .into_iter()
        .map(|(t, m)| Span::styled(t, mark_style(m)))
        .collect();
    Line::from(spans)
}

fn mark_style(m: Mark) -> Style {
    let mut st = if m.hit {
        MARK
    } else {
        m.kind.map_or_else(Style::new, syntax_style)
    };
    if m.selected {
        st = st.reversed();
    }
    if m.bracket {
        st = st.bold().underlined();
    }
    st
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
    let info = panel_info(app, i, today);
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

fn popup(frame: &mut Frame, title: &str, text: &str) {
    popup_area(frame, title, text);
}

/// Centered popup sized to its text; returns the inner area.
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
    let inner = block.inner(area);
    let para = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .style(BAR)
        .block(block);
    frame.render_widget(para, area);
    inner
}

/// Popup with a text field: `head` lines, the field, then `tail`.
fn popup_edit(frame: &mut Frame, title: &str, head: &str, l: &LineEdit, tail: &str) {
    let text = if head.is_empty() {
        format!("\n{tail}")
    } else {
        format!("{head}\n\n{tail}")
    };
    let inner = popup_area(frame, title, &text);
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
fn list_popup(
    frame: &mut Frame,
    title: &str,
    rows: &[&PathBuf],
    sel: usize,
    empty: &str,
    footer: &str,
) {
    let start = (sel + 1).saturating_sub(4);
    let mut text: String = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(4)
        .map(|(i, p)| format!("{}{}\n", if i == sel { "> " } else { "  " }, p.display()))
        .collect();
    if rows.is_empty() {
        text.push_str(empty);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(footer);
    popup(frame, title, &text);
}

fn draw_dialog(frame: &mut Frame, d: &Dialog) {
    match d {
        Dialog::Text { title, text } => popup(frame, title, text),
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
            keys,
        } => list_popup(frame, title, rows, *sel, empty, keys),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use tc_core::panel::Panel;

    #[test]
    fn renders_process_monitor() {
        let tmp = tempfile::tempdir().unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        app.on_key(tc::KeyEvent::new(
            tc::KeyCode::Char('p'),
            tc::KeyModifiers::CONTROL,
        ));
        while matches!(&app.mode, Mode::Procs(p) if p.rows.is_empty()) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(screen.contains("Processes: local"));
        assert!(screen.contains("COMMAND"));
        assert!(screen.contains("F8 kill"));
        assert_eq!(app.page, 24 - 1 - 1 - 2 - 2); // status row and F-key bar
    }

    #[test]
    fn renders_panels_and_popup() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "hi").unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        app.mode = Mode::Mkdir(LineEdit::from("x".to_owned()));
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(screen.contains("file.txt"));
        assert!(screen.contains("Make directory"));
        assert!(screen.contains("F5 Copy"));
    }

    #[test]
    fn renders_editor_with_status() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.txt");
        std::fs::write(&path, "hello\nworld").unwrap();
        let editor = tc_core::edit::Editor::open(
            &path,
            &std::sync::atomic::AtomicU64::new(0),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        app.mode = Mode::Edit {
            editor,
            error: None,
        };
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(screen.contains("world"));
        assert!(screen.contains("1:1  UTF-8  LF"));
    }

    #[test]
    fn viewer_shows_find_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("v.txt"), "hello\n").unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        app.on_key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Down,
        ));
        for c in [
            ratatui::crossterm::event::KeyCode::F(3),
            ratatui::crossterm::event::KeyCode::Char('/'),
        ] {
            app.on_key(ratatui::crossterm::event::KeyEvent::from(c));
        }
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(screen.contains("Find (regex)"));
    }

    #[test]
    fn highlights_all_matches() {
        let re = tc_core::search::compile("o").unwrap();
        let line = highlight(
            "foo bar o".into(),
            None,
            &mut false,
            Some(&re),
            None,
            (0, &[]),
        );
        let marked: String = line
            .spans
            .iter()
            .filter(|s| s.style == MARK)
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(marked, "ooo");
        assert_eq!(line.to_string(), "foo bar o");
        // selection overlay and multibyte text do not panic or shift columns
        let line = highlight(
            "é o".into(),
            None,
            &mut false,
            Some(&re),
            Some((0, 2)),
            (0, &[]),
        );
        assert_eq!(line.to_string(), "é o");
    }

    fn screen(term: &Terminal<TestBackend>) -> String {
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn status_line_shows_selection_and_link() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "123").unwrap();
        std::fs::write(tmp.path().join("b"), "12345").unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.panels[0].select_mask("*", true);
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(screen(&term).contains("2 selected, 8 bytes"));
    }

    #[test]
    fn draws_every_screen_on_tiny_terminals() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("e.txt");
        std::fs::write(&file, "hello\nworld").unwrap();
        for (w, h) in [(1, 1), (10, 3), (20, 5), (80, 4)] {
            let open = || Panel::open(tmp.path()).unwrap();
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut app = App::new(open(), open());
            term.draw(|f| draw(f, &mut app)).unwrap();
            app.mode = Mode::Mkdir(LineEdit::from("x".to_owned()));
            term.draw(|f| draw(f, &mut app)).unwrap();
            app.mode = Mode::Normal;
            app.help = true;
            term.draw(|f| draw(f, &mut app)).unwrap();
            app.help = false;
            app.mode = Mode::View {
                title: "t".into(),
                view: tc_core::view::Viewer::open(&file).unwrap(),
            };
            app.view_numbers = true;
            term.draw(|f| draw(f, &mut app)).unwrap();
            let editor = tc_core::edit::Editor::open(
                &file,
                &std::sync::atomic::AtomicU64::new(0),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
            app.mode = Mode::Edit {
                editor,
                error: None,
            };
            term.draw(|f| draw(f, &mut app)).unwrap();
            app.mode = Mode::Normal;
            let alt_f7 = ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::F(7),
                ratatui::crossterm::event::KeyModifiers::ALT,
            );
            app.on_key(alt_f7);
            for _ in 0..2 {
                app.on_key(ratatui::crossterm::event::KeyEvent::from(
                    ratatui::crossterm::event::KeyCode::Enter,
                ));
            }
            term.draw(|f| draw(f, &mut app)).unwrap();
            app.mode = Mode::Normal;
            app.on_key(ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char(' '),
                ratatui::crossterm::event::KeyModifiers::CONTROL,
            ));
            term.draw(|f| draw(f, &mut app)).unwrap(); // scanning
            while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
                app.tick();
            }
            term.draw(|f| draw(f, &mut app)).unwrap();
        }
    }
}
