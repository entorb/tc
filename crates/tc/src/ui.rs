//! Rendering of the app state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};
use regex::Regex;

use crate::app::{App, Mode, Step};
use tc_core::edit::Eol;

const HELP: &str = "F3 View  F4 Edit  S-F4 Ext  F5 Copy  F6 Move  F7 Mkdir  F8 Delete  Ins Select  Tab Switch  F10 Quit";
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
        let [main, status] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
        let halves: [Rect; 2] = Layout::horizontal([Constraint::Fill(1); 2]).areas(main);
        app.areas = halves;
        app.page = main.height.saturating_sub(2).max(1) as usize;
        for (i, area) in halves.into_iter().enumerate() {
            draw_panel(frame, app, i, area);
        }
        frame.render_widget(Line::raw(HELP).reversed(), status);
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
        Mode::Normal | Mode::View { .. } => {}
    }

    draw_input(frame, app);
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
    let title = panel.path().display().to_string();
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
