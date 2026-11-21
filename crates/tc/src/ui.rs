//! Rendering of the app state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Paragraph, Wrap};
use regex::Regex;

use crate::app::{App, FOOTER, Found, Mode, Stats, Step, Xfer};
use ratatui::widgets::ListState;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use tc_core::cmdline::LineEdit;
use tc_core::edit::{Eol, human};
use tc_core::find::MAX_RESULTS;
use tc_core::panel::SortKey;
use tc_core::syntax::{self, Kind, Lang};
use tc_remote::Prompt;
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

/// Length of "YY-MM-DD HH:MM".
pub(crate) const DATE_WIDTH: usize = 14;
/// Length of "1023.9 K".
pub(crate) const SIZE_WIDTH: usize = 8;

/// `1234567` as `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `s` cut to `w` display columns with a trailing `…`, or padded with spaces to `w`.
fn fit(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    if s.width() <= w {
        out.push_str(s);
        used = s.width();
    } else {
        for c in s.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > w - 1 {
                break;
            }
            out.push(c);
            used += cw;
        }
        out.push('…');
        used += 1;
    }
    out.extend(std::iter::repeat_n(' ', w - used));
    out
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    if let Mode::View { title, view } = &mut app.mode {
        let bottom = format!(
            " {}%  {}  {}  / or f Find  n/N Next/Prev  g Goto  w Wrap  l Lines ",
            view.percent(),
            view.encoding_name(),
            app.note
        );
        let block = themed_block().title(title.as_str()).title_bottom(bottom);
        let inner = block.inner(frame.area());
        let height = inner.height as usize;
        app.page = height.max(1);
        // numbers only while the top line is known, see `Viewer::top_line`
        let top = view.top_line().filter(|_| app.view_numbers);
        let gutter = if top.is_some() { 7 } else { 0 };
        let lang = syntax::lang(Path::new(title.as_str()));
        // shortcut: viewer assumes no code fence open above the screen, upgrade with a fence index in `Viewer`
        let mut fence = false;
        let visible: Vec<Line> = match view
            .rows(height, (inner.width as usize).saturating_sub(gutter))
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
        frame.render_widget(
            Paragraph::new(visible).style(PANEL).block(block),
            frame.area(),
        );
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
        );
    } else {
        let [main, cmd, status] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        draw_cmdline(frame, app, cmd);
        let halves: [Rect; 2] = Layout::horizontal([Constraint::Fill(1); 2]).areas(main);
        app.areas = halves;
        app.page = main.height.saturating_sub(2).max(1) as usize;
        for (i, area) in halves.into_iter().enumerate() {
            draw_panel(frame, app, i, area);
        }
        app.footer = status;
        let help = FOOTER.map(|(l, ..)| l).join("  ");
        frame.render_widget(Line::raw(help).style(BAR), status);
    }

    match &app.mode {
        Mode::Confirm(op, _, text) => popup(frame, op.verb(), text),
        Mode::Transfer(op, targets, l) => popup_edit(
            frame,
            op.verb(),
            &format!(
                "{} {} item(s) to (edit name or directory):",
                op.verb(),
                targets.len()
            ),
            l,
            "Enter ok   Esc cancel",
        ),
        Mode::Mkdir(l) => popup_edit(frame, "Make directory", "", l, "Enter ok   Esc cancel"),
        Mode::NewFile(l) => popup_edit(
            frame,
            "New file (created if missing, opened in editor)",
            "",
            l,
            "Enter ok   Esc cancel",
        ),
        Mode::Editor(_, l) => popup_edit(
            frame,
            "Editor not found, enter editor command",
            "",
            l,
            "Enter ok   Esc cancel",
        ),
        Mode::Hotlist(sel) => list_popup(
            frame,
            "Hotlist",
            &app.hotlist,
            *sel,
            "(empty, Ctrl+D adds the current directory)",
            "Enter go  Del remove  Esc close",
        ),
        Mode::Drives(rows, sel, _) => list_popup(
            frame,
            "Drives",
            rows,
            *sel,
            "(no drives found)",
            "Enter go  Esc close",
        ),
        Mode::Connections(sel) => list_popup(
            frame,
            "Connections",
            &app.connections,
            *sel,
            "(empty, Insert adds one)",
            "Enter connect  Ins add  F4 edit  Del remove  Esc close",
        ),
        Mode::ConnEdit(_, l) => popup_edit(
            frame,
            "Connection",
            "sftp://user@host[:port]/dir, ftp://..., ftps://...\n(no passwords, they are asked on connect)",
            l,
            "Enter save   Esc cancel",
        ),
        Mode::Connecting(c) => match &c.ask {
            Some((Prompt::HostKey(text), _)) => popup(
                frame,
                "Host key",
                &format!("{text}\n\n[y] trust and save to known_hosts   [n] cancel"),
            ),
            Some((Prompt::Password(label), l)) => {
                let masked = LineEdit::from("*".repeat(l.text.chars().count()));
                popup_edit(frame, label, "", &masked, "Enter ok   Esc cancel");
            }
            None => popup(frame, "Connecting", &format!("{}\n\nEsc abandon", c.url)),
        },
        Mode::Found(f) => draw_found(frame, f),
        Mode::Stats(st) => draw_stats(frame, st, None),
        Mode::Cmd(l) if let Some(st) = &app.stats => draw_stats(frame, st, Some(l)),
        Mode::Message(msg) => popup(frame, "Error", &format!("{msg}\n\nany key")),
        Mode::Edit { error, .. } => {
            if let Some(msg) = error {
                popup(frame, "Error", &format!("{msg}\n\nany key"));
            }
        }
        Mode::Work(job) if job.visible() => popup(
            frame,
            job.label,
            &format!("{}%\n\nEsc cancel", job.percent()),
        ),
        Mode::Work(_) => {}
        Mode::Xfer(x) => {
            if let Some(l) = &x.rename {
                popup_edit(frame, "Rename", "", l, "Enter ok   Esc back");
            } else if let Some((_, target)) = &x.ask {
                popup(
                    frame,
                    "File exists",
                    &format!(
                        "{}\n\no overwrite  s skip  r rename  n newer\nO all  S skip all  N newer all\nEsc cancel",
                        target.display()
                    ),
                );
            } else if x.visible() {
                popup(frame, "Working", &progress_text(x));
            }
        }
        Mode::AskViewer { text, .. } => popup(frame, "Edit", text),
        Mode::AskSave { editor, step, .. } => {
            let text = match step {
                Step::Changed => "File changed on disk. Overwrite it?".to_owned(),
                Step::Encoding => format!("File is {}. Convert to UTF-8?", editor.encoding_name()),
                Step::Eol => "Convert line endings to LF?".to_owned(),
            };
            popup(
                frame,
                "Save",
                &format!("{text}\n\n[y] yes   [n] no   Esc cancel"),
            );
        }
        Mode::AskExit(_) => popup(
            frame,
            "Edit",
            "Save changes?\n\n[y] yes   [n] no   Esc cancel",
        ),
        Mode::Normal | Mode::View { .. } | Mode::Search(_) | Mode::Filter(_) | Mode::Cmd(_) => {}
    }

    draw_input(frame, app);
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

fn draw_found(frame: &mut Frame, f: &Found) {
    let area = frame.area();
    let h = usize::from(area.height.saturating_sub(2)).max(1);
    let start = f.sel / h * h; // page-wise, so only visible rows are built
    let items = f
        .list
        .iter()
        .skip(start)
        .take(h)
        .map(|e| e.name.to_string_lossy());
    let state = if f.running { "searching" } else { "done" };
    let cap = if f.list.len() >= MAX_RESULTS {
        ", capped"
    } else {
        ""
    };
    let title = format!(
        " Find: {} found, {state}{cap}  Enter panelize  Esc stop/close ",
        f.list.len()
    );
    let list = List::new(items)
        .style(PANEL)
        .block(themed_block().title(title))
        .highlight_style(BAR);
    let mut st = ListState::default().with_selected(Some(f.sel.saturating_sub(start)));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut st);
}

/// File Stats: entries of the browsed directory, largest first, with share and bar.
/// `cmd`: command line typed from the stats, shown in the footer row.
fn draw_stats(frame: &mut Frame, st: &Stats, cmd: Option<&LineEdit>) {
    const BAR_WIDTH: usize = 10;
    let area = frame.area();
    let h = usize::from(area.height.saturating_sub(3)).max(1);
    let dir = st.current();
    let value = |n: &tc_core::stats::Node| if st.by_files { n.files } else { n.size };
    let fmt = |v: u64| if st.by_files { thousands(v) } else { human(v) };
    let total = value(dir).max(1);
    let start = st.sel / h * h;
    let items = dir.children.iter().skip(start).take(h).map(|n| {
        let share = value(n) as f64 / total as f64;
        let filled = (share * BAR_WIDTH as f64).round() as usize;
        let bar = "#".repeat(filled) + &" ".repeat(BAR_WIDTH - filled.min(BAR_WIDTH));
        let slash = if n.is_dir { "/" } else { "" };
        format!(
            "{:>11} {:>5.1}% [{bar}] {}{slash}",
            fmt(value(n)),
            share * 100.0,
            n.name.to_string_lossy()
        )
    });
    let footer = if st.scanning() {
        format!(
            "scanning: {} files, {} / {} dirs  Esc/F10 cancel",
            thousands(st.progress.files.load(Ordering::Relaxed)),
            thousands(st.progress.dirs_done.load(Ordering::Relaxed)),
            thousands(st.progress.dirs.load(Ordering::Relaxed))
        )
    } else {
        let mode = if st.by_files { "count" } else { "size" };
        format!(
            "{}, {} files  Tab size/count ({mode})  Enter open  Bksp up  F1 Help  F3 View  F4 Edit  S-F4 Ext  F8 Del  : Cmd  F10 Quit",
            human(dir.size),
            thousands(dir.files)
        )
    };
    let list = List::new(items)
        .style(PANEL)
        .block(themed_block().title(format!(" Stats: {} ", st.path().display())))
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

fn draw_input(frame: &mut Frame, app: &mut App) {
    if app.help {
        let (title, text) = match app.mode {
            Mode::View { .. } => ("Help: viewer", HELP_VIEW),
            Mode::Edit { .. } => ("Help: editor", HELP_EDIT),
            Mode::Stats(_) => ("Help: file stats (ncdu)", HELP_STATS),
            _ => ("Help", HELP_MAIN),
        };
        let lines = text.lines().count();
        let [h] = Layout::horizontal([Constraint::Length(frame.area().width.min(64))])
            .flex(Flex::Center)
            .areas(frame.area());
        let [area] = Layout::vertical([Constraint::Length(
            (lines as u16 + 2).min(frame.area().height),
        )])
        .flex(Flex::Center)
        .areas(h);
        let rows = usize::from(area.height.saturating_sub(2));
        app.help_scroll = app.help_scroll.min(lines.saturating_sub(rows));
        frame.render_widget(Clear, area);
        let block = Block::bordered()
            .border_type(BorderType::Double)
            .title(title)
            .title_bottom(" Up/Down scroll  other key closes ");
        frame.render_widget(
            Paragraph::new(text)
                .scroll((app.help_scroll as u16, 0))
                .style(BAR)
                .block(block),
            area,
        );
    }
    if let Some(input) = &app.input {
        let error = input.error.as_deref().unwrap_or_default();
        popup_edit(
            frame,
            input.ask.title(),
            "",
            &input.text,
            &format!("{error}\nEnter ok   Esc cancel"),
        );
    }
}

fn draw_editor(
    frame: &mut Frame,
    editor: &mut tc_core::edit::Editor,
    (page, text_area): (&mut usize, &mut Rect),
    note: &str,
    re: Option<&Regex>,
) {
    let [main, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
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
    let (line, col) = editor.cursor;
    let (dy, dx) = editor.cursor_cell(width);
    if dx < width && dy < height {
        frame.set_cursor_position((text_area.x + dx as u16, inner.y + dy as u16));
    }
    let eol = match editor.eol() {
        Eol::Lf => "LF",
        Eol::Crlf => "CRLF",
        Eol::Mixed => "mixed",
    };
    let dirty = if editor.dirty() { " *" } else { "" };
    let info = format!(
        "{}:{}  {}  {eol}{dirty}  {note}  F2 Save  F3 Next  F4/^R Replace  F7/^F Find  ^Z Undo  Esc Exit",
        line + 1,
        col + 1,
        editor.encoding_name()
    );
    frame.render_widget(Line::raw(info).style(BAR), status);
}

/// `row` in `lang` syntax colors, with all matches of `re` marked yellow and the selected columns
/// (counted from the first column of the line, minus `left`) reversed.
// shortcut: matches the shown text, so a pattern with \t or one cut at the left edge can differ from the real match
fn highlight(
    row: String,
    lang: Option<&Lang>,
    fence: &mut bool,
    re: Option<&Regex>,
    sel: Option<(usize, usize)>,
    (left, brackets): (usize, &[usize]),
) -> Line<'static> {
    let mut styles = vec![Style::new(); row.chars().count()];
    let at = |byte: usize| row[..byte].chars().count();
    for (r, kind) in lang
        .map(|l| syntax::tokens(l, &row, fence))
        .unwrap_or_default()
    {
        styles[at(r.start)..at(r.end)].fill(syntax_style(kind));
    }
    for m in re.into_iter().flat_map(|re| re.find_iter(&row)) {
        styles[at(m.start())..at(m.end())].fill(MARK);
    }
    if let Some((a, b)) = sel {
        let end = styles.len();
        for st in &mut styles[a.saturating_sub(left).min(end)..b.saturating_sub(left).min(end)] {
            *st = st.reversed();
        }
    }
    for &c in brackets {
        if let Some(st) = styles.get_mut(c) {
            *st = st.bold().underlined();
        }
    }
    let mut spans: Vec<(String, Style)> = Vec::new();
    for (c, st) in row.chars().zip(styles) {
        match spans.last_mut() {
            Some((text, last)) if *last == st => text.push(c),
            _ => spans.push((c.to_string(), st)),
        }
    }
    Line::from(
        spans
            .into_iter()
            .map(|(t, st)| Span::styled(t, st))
            .collect::<Vec<_>>(),
    )
}

fn draw_panel(frame: &mut Frame, app: &mut App, i: usize, area: Rect) {
    let panel = &app.panels[i];
    let active = i == app.active;
    let width = area.width.saturating_sub(2) as usize;
    let today = jiff::ZonedDateTime::now().date();
    let date_of = |e: &tc_core::panel::Entry| {
        e.modified
            .and_then(|t| jiff::ZonedDateTime::try_from(t).ok())
            .map_or_else(String::new, |z| fmt_date(&z, today))
    };
    let dir_size = |e: &tc_core::panel::Entry| -> Option<String> {
        if !e.is_dir || e.is_parent() || panel.archive().is_some() {
            return None;
        }
        match &app.dir_job {
            Some((side, p, _)) if *side == i && *p == panel.path().join(&e.name) => {
                Some("...".into())
            }
            _ => panel.dir_size(e).map(human),
        }
    };
    let items: Vec<ListItem> = panel
        .entries()
        .iter()
        .map(|e| {
            let prefix = match (e.is_symlink, e.is_dir) {
                (true, true) => '~',
                (true, false) => '@',
                (false, true) => '/',
                _ => ' ',
            };
            let name = format!("{prefix}{}", e.name.to_string_lossy());
            let size = if e.is_dir {
                dir_size(e).unwrap_or_else(|| "<DIR>".to_owned())
            } else {
                human(e.size)
            };
            let date = date_of(e);
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
    let mut title = panel.title();
    let tabs = app.tabs[i].len() + 1;
    if tabs > 1 {
        title = format!("[{}/{tabs}] {title}", app.tab[i] + 1);
    }
    if panel.is_virtual() {
        title += " [found]";
    }
    if let Some(dir) = panel.loading() {
        title += &format!(" [loading {}, Esc cancels]", dir.display());
    }
    match panel.sort() {
        (SortKey::Name, false) => {}
        (key, rev) => {
            let key = format!("{key:?}").to_lowercase();
            title += &format!(" [{key}{}]", if rev { " desc" } else { "" });
        }
    }
    if !panel.shows_hidden() {
        title += " [no hidden]";
    }
    match &app.mode {
        Mode::Search(t) if active => title += &format!(" [search: {t}]"),
        Mode::Filter(t) if active => title += &format!(" [filter: {t}]"),
        _ if !panel.filter().is_empty() => title += &format!(" [filter: {}]", panel.filter()),
        _ => {}
    }
    // status line: selection sum (cached in the panel), else the cursor entry in full
    let (count, bytes) = panel.selection_info();
    let info = match panel.current().filter(|e| !e.is_parent()) {
        _ if count > 0 => format!(" {count} selected, {} bytes ", thousands(bytes)),
        Some(e) => {
            let size = match dir_size(e) {
                Some(s) => s,
                None if e.is_dir => "<DIR>".to_owned(),
                None => thousands(e.size),
            };
            let link = e
                .link
                .as_ref()
                .map_or(String::new(), |l| format!("  -> {}", l.display()));
            format!(
                " {}  {size}  {}{link} ",
                e.name.to_string_lossy(),
                date_of(e)
            )
        }
        None => String::new(),
    };
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

/// Time only for today's files, else short date and time.
fn fmt_date(z: &jiff::ZonedDateTime, today: jiff::civil::Date) -> String {
    let fmt = if z.date() == today {
        "%H:%M:%S"
    } else {
        "%y-%m-%d %H:%M"
    };
    z.strftime(fmt).to_string()
}

fn progress_text(x: &Xfer) -> String {
    let p = &x.progress;
    let (done, files) = (
        p.done.load(Ordering::Relaxed),
        p.files.load(Ordering::Relaxed),
    );
    let current = p
        .current
        .lock()
        .map(|c| c.display().to_string())
        .unwrap_or_default();
    let (total, of) = if p.scanned.load(Ordering::Acquire) {
        let (t, f) = (
            p.total.load(Ordering::Relaxed),
            p.files_total.load(Ordering::Relaxed),
        );
        (human(t), format!("{} / {f}", files.min(f)))
    } else {
        ("scanning...".to_owned(), files.to_string())
    };
    let remaining = p.total.load(Ordering::Relaxed).saturating_sub(done) as f64;
    let speed = x
        .eta
        .rate()
        .map_or("--".to_owned(), |r| format!("{}/s", human(r as u64)));
    let eta = match x.eta.eta(remaining) {
        Some(s) if p.scanned.load(Ordering::Acquire) => hms(s as u64),
        _ => "--".to_owned(),
    };
    format!(
        "{current}\n{} / {total}   files {of}\n{speed}   ETA {eta}\n\nEsc cancel",
        human(done)
    )
}

fn hms(s: u64) -> String {
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, m, s) => format!("{m}:{s:02}"),
        (h, m, s) => format!("{h}:{m:02}:{s:02}"),
    }
}

const HELP_MAIN: &str = "\
== Highlights ==
Ctrl+Space      FILE STATS (like ncdu): biggest files and dirs
                of the current tree, with % and bar
Ctrl+N          SFTP / FTP / FTPS connections
Ctrl+B          bookmarks (hotlist), Ctrl+D adds this directory

== Navigation ==
Up/Down/PgUp/PgDn/Home/End   move cursor
Tab             switch panel
Enter           open directory, archive or file
Backspace       parent directory
Alt+Left/Right  history back / forward
~               home directory
Ctrl+\\         root directory
Ctrl+R          refresh
Alt+F1 / F2     drives for left / right panel
Ctrl+U          swap panels
Ctrl+T / Ctrl+W new / close tab
Ctrl+PgDn/PgUp  next / previous tab
Alt+=           other panel to this directory
Alt+.           show / hide hidden files
Ctrl+F3..F6     sort name/ext/date/size (again: reverse)
Esc             clear filter, leave found list

== Selection ==
Insert / Space  select entry
Shift+Up/Down   select range
+ / -           select / deselect by mask
*               invert selection
Ctrl+Y          select differences to other panel

== Search ==
Ctrl+S          quick search (again: next match)
Ctrl+F          quick filter
Alt+F7          find files
:               command line

== Files ==
F3 / F4         view / edit
Shift+F4        create file, edit in external editor
Ctrl+Insert     copy paths to clipboard (Alt+Shift+C)
F5 / F6         copy / move
Shift+F6        rename
F7 / F8         make directory / delete
F10 / q         quit";

const HELP_STATS: &str = "\
Up/Down/PgUp/PgDn/Home/End  move cursor
Enter / Right  open directory
Backspace / Left  parent directory
Tab  toggle size / file count
F3  view file
F4  edit file
Shift+F4  edit in external editor
F8 / d  delete (y trash, p permanent), no rescan
:  command line in this directory
Esc / q / F10  close (while scanning: cancel)

Scan runs in up to 8 threads; footer shows files and
scanned / discovered directories.";

const HELP_VIEW: &str = "\
== Move ==
Up/Down/PgUp/PgDn/Home/End  scroll
Left/Right  scroll sideways
g  go to line or N%

== Search ==
/ or f or F7  find
n  next match
N  previous match

== View ==
w  wrap long lines
l  line numbers
Esc / q / F3 / F10  close";

const HELP_EDIT: &str = "\
== File ==
F2  save
Esc / F10  exit

== Move ==
Ctrl+Left/Right  by word
Ctrl+Home/End  start / end of file
Ctrl+G  go to line

== Search ==
F7 / Ctrl+F  find
F3  next match
Shift+F3  previous match
F4 / Ctrl+R  replace all
Alt+R  replace one by one (y n a Esc)
Alt+C  case sensitive / smart case

== Edit ==
Ctrl+Z  undo
Ctrl+Y  redo
Shift+arrows  select
Ctrl+C  copy (also system clipboard)
Ctrl+X  cut
Ctrl+V  paste (terminal paste works too)
Ctrl+K / F8  delete selection or line
Ctrl+A  select all
Ctrl+D  duplicate line
Ctrl+Backspace/Delete  delete word
Shift+Tab  unindent line / selection
Alt+W  wrap on / off
Alt+L  line numbers on / off
Mouse  click, drag, Shift+click select";

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
    rows: &[PathBuf],
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use tc_core::panel::Panel;

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
    fn date_today_shows_time_only() {
        let z = jiff::ZonedDateTime::now();
        let today = z.date();
        assert_eq!(fmt_date(&z, today), z.strftime("%H:%M:%S").to_string());
        let yesterday = today.yesterday().unwrap();
        assert_eq!(
            fmt_date(&z, yesterday),
            z.strftime("%y-%m-%d %H:%M").to_string()
        );
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

    #[test]
    fn thousands_and_fit() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("ab", 4), "ab  ");
        assert_eq!(fit("x", 0), "");
        let w = fit("日本語", 5);
        assert_eq!(w.width(), 5);
        assert!(w.trim_end().ends_with('…'));
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
