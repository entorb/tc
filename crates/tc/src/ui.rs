//! Rendering of the app state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};
use regex::Regex;

use crate::app::{App, FOOTER, Found, Mode, Step, Xfer};
use ratatui::widgets::ListState;
use std::sync::atomic::Ordering;
use tc_core::edit::{Eol, human};
use tc_core::find::MAX_RESULTS;

/// Length of "YY-MM-DD HH:MM".
const DATE_WIDTH: usize = 14;

pub fn draw(frame: &mut Frame, app: &mut App) {
    if let Mode::View { title, view } = &mut app.mode {
        let bottom = format!(
            " {}%  {}  {}  / or f Find  n/N Next/Prev  g Goto  e Encoding ",
            view.percent(),
            view.encoding_name(),
            app.note
        );
        let block = Block::bordered().title(title.as_str()).title_bottom(bottom);
        let inner = block.inner(frame.area());
        let height = inner.height as usize;
        app.page = height.max(1);
        let visible: Vec<Line> = match view.lines(height, inner.width as usize) {
            Ok(lines) => lines
                .into_iter()
                .map(|l| highlight(l, app.search.as_ref(), None, 0))
                .collect(),
            Err(e) => vec![Line::raw(e.to_string())],
        };
        frame.render_widget(Paragraph::new(visible).block(block), frame.area());
        draw_input(frame, app);
        return;
    }

    if let Some(editor) = app.mode.editor_mut() {
        draw_editor(frame, editor, &mut app.page, &app.note, app.search.as_ref());
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
        frame.render_widget(Line::raw(help).reversed(), status);
    }

    match &app.mode {
        Mode::Confirm(op, targets) => {
            let text = format!(
                "{} {} item(s)?\n\n[y] yes   [n] no",
                op.verb(),
                targets.len()
            );
            popup(frame, op.verb(), &text);
        }
        Mode::Transfer(op, targets, text) => popup(
            frame,
            op.verb(),
            &format!(
                "{} {} item(s) to (edit name or directory):\n{text}_\n\nEnter ok   Esc cancel",
                op.verb(),
                targets.len()
            ),
        ),
        Mode::Mkdir(name) => popup(
            frame,
            "Make directory",
            &format!("{name}_\n\nEnter ok   Esc cancel"),
        ),
        Mode::Editor(_, cmd) => popup(
            frame,
            "Editor not found, enter editor command",
            &format!("{cmd}_\n\nEnter ok   Esc cancel"),
        ),
        Mode::Hotlist(sel) => {
            let rows = &app.hotlist;
            let start = (sel + 1).saturating_sub(4);
            let mut text: String = rows
                .iter()
                .enumerate()
                .skip(start)
                .take(4)
                .map(|(i, p)| format!("{}{}\n", if i == *sel { "> " } else { "  " }, p.display()))
                .collect();
            if rows.is_empty() {
                text.push_str("(empty, Ctrl+D adds the current directory)\n");
            }
            text.push_str("\nEnter go  Del remove  Esc close");
            popup(frame, "Hotlist", &text);
        }
        Mode::Found(f) => draw_found(frame, f),
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
            if let Some(text) = &x.rename {
                popup(frame, "Rename", &format!("{text}_\n\nEnter ok   Esc back"));
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

/// `dir> text` row; a long line shows its tail so the cursor stays visible.
fn draw_cmdline(frame: &mut Frame, app: &App, area: Rect) {
    let prompt = format!("{}> ", app.panels[app.active].path().display());
    let (text, cur) = match &app.mode {
        Mode::Cmd(l) => (l.text.as_str(), Some(l.text[..l.cur].chars().count())),
        _ => ("", None),
    };
    let line = format!("{prompt}{text}");
    let skip =
        (prompt.chars().count() + text.chars().count() + 1).saturating_sub(usize::from(area.width));
    let shown: String = line.chars().skip(skip).collect();
    frame.render_widget(Line::raw(shown), area);
    if let Some(c) = cur {
        let col = (prompt.chars().count() + c).saturating_sub(skip);
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
        .block(Block::bordered().title(title))
        .highlight_style(Style::new().reversed());
    let mut st = ListState::default().with_selected(Some(f.sel - start));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut st);
}

fn draw_input(frame: &mut Frame, app: &App) {
    if let Some(input) = &app.input {
        let error = input.error.as_deref().unwrap_or_default();
        popup(
            frame,
            input.ask.title(),
            &format!("{}_\n{error}\nEnter ok   Esc cancel", input.text),
        );
    }
}

fn draw_editor(
    frame: &mut Frame,
    editor: &mut tc_core::edit::Editor,
    page: &mut usize,
    note: &str,
    re: Option<&Regex>,
) {
    let [main, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let block = Block::bordered().title(editor.path().display().to_string());
    let inner = block.inner(main);
    let (height, width) = (inner.height as usize, inner.width as usize);
    *page = height.max(1);
    editor.scroll_to_cursor(height, width);
    let lines: Vec<Line> = editor
        .visible(height, width)
        .into_iter()
        .enumerate()
        .map(|(i, row)| highlight(row, re, editor.selection_cols(editor.top + i), editor.left))
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), main);
    let (line, col) = editor.cursor;
    frame.set_cursor_position((
        inner.x + (col - editor.left) as u16,
        inner.y + (line - editor.top) as u16,
    ));
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
    frame.render_widget(Line::raw(info).reversed(), status);
}

/// `row` with all matches of `re` marked yellow and the selected columns
/// (counted from the first column of the line, minus `left`) reversed.
// shortcut: matches the shown text, so a pattern with \t or one cut at the left edge can differ from the real match
fn highlight(
    row: String,
    re: Option<&Regex>,
    sel: Option<(usize, usize)>,
    left: usize,
) -> Line<'static> {
    let mut styles = vec![Style::new(); row.chars().count()];
    let at = |byte: usize| row[..byte].chars().count();
    for m in re.into_iter().flat_map(|re| re.find_iter(&row)) {
        styles[at(m.start())..at(m.end())].fill(Style::new().fg(Color::Black).bg(Color::Yellow));
    }
    if let Some((a, b)) = sel {
        let end = styles.len();
        for st in &mut styles[a.saturating_sub(left).min(end)..b.saturating_sub(left).min(end)] {
            *st = st.reversed();
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
    let items: Vec<ListItem> = panel
        .entries()
        .iter()
        .map(|e| {
            let prefix = if e.is_dir { "/" } else { " " };
            let name = format!("{prefix}{}", e.name.to_string_lossy());
            let size = if e.is_dir {
                "<DIR>".to_owned()
            } else {
                e.size.to_string()
            };
            let date = e
                .modified
                .and_then(|t| jiff::ZonedDateTime::try_from(t).ok())
                .map_or_else(String::new, |z| fmt_date(&z, today));
            let pad = width.saturating_sub(size.len() + DATE_WIDTH + 2);
            let row = format!("{name:<pad$.pad$} {size} {date:>DATE_WIDTH$}");
            let style = if panel.is_selected(e) {
                Style::new().fg(Color::Yellow).bold()
            } else if e.is_symlink {
                Style::new().fg(Color::Cyan)
            } else if e.is_dir {
                Style::new().fg(Color::Blue).bold()
            } else {
                Style::new()
            };
            ListItem::new(row).style(style)
        })
        .collect();
    let mut title = panel.path().display().to_string();
    if panel.is_virtual() {
        title += " [found]";
    }
    match &app.mode {
        Mode::Search(t) if active => title += &format!(" [search: {t}]"),
        Mode::Filter(t) if active => title += &format!(" [filter: {t}]"),
        _ if !panel.filter().is_empty() => title += &format!(" [filter: {}]", panel.filter()),
        _ => {}
    }
    let border = if active {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new()
    };
    let block = Block::bordered().title(title).border_style(border);
    let highlight = if active {
        Style::new().reversed()
    } else {
        Style::new()
    };
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

fn popup(frame: &mut Frame, title: &str, text: &str) {
    let width = frame.area().width.min(60);
    let [h] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::vertical([Constraint::Length(7)])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, area);
    let para = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(title));
    frame.render_widget(para, area);
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
        app.mode = Mode::Mkdir("x".into());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
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
            .map(|c| c.symbol())
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
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("Find (regex)"));
    }

    #[test]
    fn highlights_all_matches() {
        let re = tc_core::search::compile("o").unwrap();
        let line = highlight("foo bar o".into(), Some(&re), None, 0);
        let marked: String = line
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(Color::Yellow))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(marked, "ooo");
        assert_eq!(line.to_string(), "foo bar o");
        // selection overlay and multibyte text do not panic or shift columns
        let line = highlight("é o".into(), Some(&re), Some((0, 2)), 0);
        assert_eq!(line.to_string(), "é o");
    }
}
