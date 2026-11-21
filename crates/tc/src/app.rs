//! App state and key handling, independent of the terminal.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::widgets::ListState;
use tc_core::{ops, panel::Panel, view::Viewer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Copy,
    Move,
    Delete,
}

impl Op {
    pub fn verb(self) -> &'static str {
        match self {
            Op::Copy => "Copy",
            Op::Move => "Move",
            Op::Delete => "Delete",
        }
    }
}

pub enum Mode {
    Normal,
    Confirm(Op, Vec<PathBuf>),
    Mkdir(String),
    Message(String),
    View { title: String, view: Viewer },
}

pub struct App {
    pub panels: [Panel; 2],
    /// Scroll state per panel, kept across frames so the list does not jump.
    pub lists: [ListState; 2],
    pub active: usize,
    pub mode: Mode,
    pub quit: bool,
    /// Visible rows of a panel, set by the UI, used for PgUp/PgDn.
    pub page: usize,
}

impl App {
    pub fn new(left: Panel, right: Panel) -> Self {
        Self {
            panels: [left, right],
            lists: Default::default(),
            active: 0,
            mode: Mode::Normal,
            quit: false,
            page: 10,
        }
    }

    pub fn other_dir(&self) -> &Path {
        self.panels[1 - self.active].path()
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_key(key.code),
            Mode::Confirm(op, targets) => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.run(op, &targets),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
                _ => self.mode = Mode::Confirm(op, targets),
            },
            Mode::Mkdir(mut name) => match key.code {
                KeyCode::Enter => self.mkdir(&name),
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    name.pop();
                    self.mode = Mode::Mkdir(name);
                }
                KeyCode::Char(c) => {
                    name.push(c);
                    self.mode = Mode::Mkdir(name);
                }
                _ => self.mode = Mode::Mkdir(name),
            },
            Mode::Message(_) => {}
            Mode::View { title, mut view } => {
                let page = self.page as isize;
                let result = match key.code {
                    KeyCode::Esc | KeyCode::F(3 | 10) | KeyCode::Char('q') => return,
                    KeyCode::Up => view.scroll(-1),
                    KeyCode::Down => view.scroll(1),
                    KeyCode::PageUp => view.scroll(-page),
                    KeyCode::PageDown => view.scroll(page),
                    KeyCode::Home => {
                        view.home();
                        Ok(())
                    }
                    KeyCode::End => view.end(self.page),
                    _ => Ok(()),
                };
                self.mode = match result {
                    Ok(()) => Mode::View { title, view },
                    Err(e) => Mode::Message(format!("{title}: {e}")),
                };
            }
        }
    }

    fn normal_key(&mut self, code: KeyCode) {
        let page = self.page as isize;
        let panel = &mut self.panels[self.active];
        match code {
            KeyCode::Char('q') | KeyCode::F(10) => self.quit = true,
            KeyCode::Tab => self.active = 1 - self.active,
            KeyCode::Up => panel.move_cursor(-1),
            KeyCode::Down => panel.move_cursor(1),
            KeyCode::PageUp => panel.move_cursor(-page),
            KeyCode::PageDown => panel.move_cursor(page),
            KeyCode::Home => panel.move_cursor(isize::MIN),
            KeyCode::End => panel.move_cursor(isize::MAX),
            KeyCode::Insert | KeyCode::Char(' ') => panel.toggle_select(),
            KeyCode::Enter | KeyCode::Backspace => {
                let result = if code == KeyCode::Enter {
                    panel.enter().map(drop)
                } else {
                    panel.parent()
                };
                if let Err(e) = result {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            KeyCode::F(3) => self.view(),
            KeyCode::F(5) => self.confirm(Op::Copy),
            KeyCode::F(6) => self.confirm(Op::Move),
            KeyCode::F(8) => self.confirm(Op::Delete),
            KeyCode::F(7) => self.mode = Mode::Mkdir(String::new()),
            _ => {}
        }
    }

    fn confirm(&mut self, op: Op) {
        let targets = self.panels[self.active].targets();
        if !targets.is_empty() {
            self.mode = Mode::Confirm(op, targets);
        }
    }

    /// Stops at the first error so the user sees it before anything else happens.
    fn run(&mut self, op: Op, targets: &[PathBuf]) {
        let dst = self.other_dir().to_owned();
        let result = targets.iter().try_for_each(|src| match op {
            Op::Copy => ops::copy(src, &dst),
            Op::Move => ops::move_(src, &dst),
            Op::Delete => ops::delete(src),
        });
        self.finish(result.err().map(|e| e.to_string()));
    }

    fn mkdir(&mut self, name: &str) {
        let panel = &self.panels[self.active];
        let result = ops::mkdir(panel.path(), &OsString::from(name));
        self.finish(result.err().map(|e| e.to_string()));
    }

    fn finish(&mut self, error: Option<String>) {
        let mut errors: Vec<String> = error.into_iter().collect();
        for panel in &mut self.panels {
            if let Err(e) = panel.reload() {
                errors.push(e.to_string());
            }
        }
        if !errors.is_empty() {
            self.mode = Mode::Message(errors.join("\n"));
        }
    }

    fn view(&mut self) {
        let panel = &self.panels[self.active];
        let Some(entry) = panel.current().filter(|e| !e.is_dir) else {
            return;
        };
        let path = panel.path().join(&entry.name);
        self.mode = match Viewer::open(&path) {
            Ok(view) => Mode::View {
                title: path.display().to_string(),
                view,
            },
            Err(e) => Mode::Message(format!("{}: {e}", path.display())),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn mkdir_then_copy_to_other_panel() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Panel::open(left.path()).unwrap(),
            Panel::open(right.path()).unwrap(),
        );
        key(&mut app, KeyCode::F(7));
        for c in "new".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        assert!(left.path().join("new").is_dir());
        assert_eq!(app.panels[0].current().unwrap().name, "..");
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(5));
        assert!(matches!(app.mode, Mode::Confirm(Op::Copy, _)));
        key(&mut app, KeyCode::Char('y'));
        assert!(right.path().join("new").is_dir());
        assert!(matches!(app.mode, Mode::Normal));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Char('y'));
        assert!(matches!(app.mode, Mode::Message(_))); // already exists
    }
}
