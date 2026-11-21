//! Rendering of the app state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};

use crate::app::{App, Mode};

const HELP: &str =
    "F3 View  F5 Copy  F6 Move  F7 Mkdir  F8 Delete  Ins Select  Tab Switch  F10 Quit";
/// Length of "YY-MM-DD HH:MM".
const DATE_WIDTH: usize = 14;

pub fn draw(frame: &mut Frame, app: &mut App) {
    if let Mode::View { title, view } = &mut app.mode {
        let block = Block::bordered().title(title.as_str());
        let height = block.inner(frame.area()).height as usize;
        app.page = height.max(1);
        let visible: Vec<Line> = match view.lines(height) {
            Ok(lines) => lines.into_iter().map(Line::raw).collect(),
            Err(e) => vec![Line::raw(e.to_string())],
        };
        frame.render_widget(Paragraph::new(visible).block(block), frame.area());
        return;
    }

    let [main, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let halves: [Rect; 2] = Layout::horizontal([Constraint::Fill(1); 2]).areas(main);
    app.page = main.height.saturating_sub(2).max(1) as usize;
    for (i, area) in halves.into_iter().enumerate() {
        draw_panel(frame, app, i, area);
    }
    frame.render_widget(Line::raw(HELP).reversed(), status);

    match &app.mode {
        Mode::Confirm(op, targets) => {
            let to = if matches!(op, crate::app::Op::Delete) {
                String::new()
            } else {
                format!(" to {}", app.other_dir().display())
            };
            let text = format!(
                "{} {} item(s){to}?\n\n[y] yes   [n] no",
                op.verb(),
                targets.len()
            );
            popup(frame, op.verb(), &text);
        }
        Mode::Mkdir(name) => popup(
            frame,
            "Make directory",
            &format!("{name}_\n\nEnter ok   Esc cancel"),
        ),
        Mode::Message(msg) => popup(frame, "Error", &format!("{msg}\n\nany key")),
        Mode::Normal | Mode::View { .. } => {}
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
}
