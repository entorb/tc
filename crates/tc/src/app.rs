//! App state and key handling, independent of the terminal.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use tc_core::edit::{self, Editor, Eol, SizeCheck};
use tc_core::{Error, ops, panel::Panel, view::Viewer};

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
    View {
        title: String,
        view: Viewer,
    },
    Edit {
        editor: Editor,
        error: Option<String>,
    },
    /// Load or save running in a worker thread.
    Work(Job),
    /// Size prompt: `y` viewer, `n` loads into the editor when `can_load`.
    AskViewer {
        path: PathBuf,
        text: String,
        can_load: bool,
    },
    AskSave {
        editor: Editor,
        to_utf8: bool,
        exit: bool,
        step: Step,
    },
    AskExit(Editor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Encoding,
    Eol,
}

enum Done {
    Loaded(tc_core::Result<Editor>),
    Saved(Editor, tc_core::Result<()>),
}

pub struct Job {
    pub label: &'static str,
    path: PathBuf,
    rx: Receiver<Done>,
    progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    total: u64,
    exit: bool,
    started: Instant,
}

impl Job {
    /// Saves are usually instant, so their popup waits for a slow one.
    pub fn visible(&self) -> bool {
        self.label != "Saving" || self.started.elapsed() > Duration::from_secs(2)
    }

    pub fn percent(&self) -> u64 {
        (self.progress.load(Ordering::Relaxed) * 100 / self.total.max(1)).min(100)
    }
}

impl Mode {
    /// The editor shown behind prompts.
    pub fn editor_mut(&mut self) -> Option<&mut Editor> {
        match self {
            Mode::Edit { editor, .. } | Mode::AskSave { editor, .. } | Mode::AskExit(editor) => {
                Some(editor)
            }
            _ => None,
        }
    }
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
            Mode::Work(job) => {
                if key.code == KeyCode::Esc {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                self.mode = Mode::Work(job);
            }
            Mode::AskViewer {
                path,
                text,
                can_load,
            } => match key.code {
                KeyCode::Char('y' | 'Y') => self.open_viewer(path),
                KeyCode::Char('n' | 'N') if can_load => self.spawn_load(path),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
                _ => {
                    self.mode = Mode::AskViewer {
                        path,
                        text,
                        can_load,
                    }
                }
            },
            Mode::AskExit(editor) => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.next_save_prompt(editor, true, false, Step::Encoding)
                }
                KeyCode::Char('n' | 'N') => self.leave_editor(),
                KeyCode::Esc => {
                    self.mode = Mode::Edit {
                        editor,
                        error: None,
                    }
                }
                _ => self.mode = Mode::AskExit(editor),
            },
            Mode::AskSave {
                editor,
                to_utf8,
                exit,
                step,
            } => match key.code {
                KeyCode::Char(c @ ('y' | 'Y' | 'n' | 'N')) => {
                    let yes = c.eq_ignore_ascii_case(&'y');
                    match step {
                        Step::Encoding => self.next_save_prompt(editor, exit, yes, Step::Eol),
                        Step::Eol => self.spawn_save(editor, to_utf8, yes, exit),
                    }
                }
                KeyCode::Esc => {
                    self.mode = Mode::Edit {
                        editor,
                        error: None,
                    }
                }
                _ => {
                    self.mode = Mode::AskSave {
                        editor,
                        to_utf8,
                        exit,
                        step,
                    }
                }
            },
            Mode::Edit { mut editor, error } => {
                if error.is_some() {
                    self.mode = Mode::Edit {
                        editor,
                        error: None,
                    };
                    return;
                }
                let page = self.page as isize;
                match key.code {
                    KeyCode::Esc | KeyCode::F(10) => {
                        if editor.dirty() {
                            self.mode = Mode::AskExit(editor);
                        } else {
                            self.leave_editor();
                        }
                        return;
                    }
                    KeyCode::F(2) => {
                        self.next_save_prompt(editor, false, false, Step::Encoding);
                        return;
                    }
                    KeyCode::Up => editor.move_v(-1),
                    KeyCode::Down => editor.move_v(1),
                    KeyCode::PageUp => editor.move_v(-page),
                    KeyCode::PageDown => editor.move_v(page),
                    KeyCode::Left => editor.move_h(-1),
                    KeyCode::Right => editor.move_h(1),
                    KeyCode::Home => editor.home(),
                    KeyCode::End => editor.end(),
                    KeyCode::Enter => editor.insert_char('\n'),
                    KeyCode::Tab => editor.insert_char('\t'),
                    KeyCode::Backspace => editor.backspace(),
                    KeyCode::Delete => editor.delete(),
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        editor.insert_char(c)
                    }
                    _ => {}
                }
                self.mode = Mode::Edit {
                    editor,
                    error: None,
                };
            }
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
            KeyCode::F(3) => {
                if let Some(path) = self.current_file() {
                    self.open_viewer(path);
                }
            }
            KeyCode::F(4) => self.edit(),
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

    fn current_file(&self) -> Option<PathBuf> {
        let panel = &self.panels[self.active];
        let entry = panel.current().filter(|e| !e.is_dir)?;
        Some(panel.path().join(&entry.name))
    }

    fn open_viewer(&mut self, path: PathBuf) {
        self.mode = match Viewer::open(&path) {
            Ok(view) => Mode::View {
                title: path.display().to_string(),
                view,
            },
            Err(e) => Mode::Message(format!("{}: {e}", path.display())),
        };
    }

    fn edit(&mut self) {
        let Some(path) = self.current_file() else {
            return;
        };
        let ask = |text: String, can_load| Mode::AskViewer {
            path: path.clone(),
            text,
            can_load,
        };
        match edit::check_size(&path) {
            Ok(SizeCheck::Ok) => self.spawn_load(path.clone()),
            Ok(SizeCheck::Large(n)) => {
                self.mode = ask(
                    format!(
                        "File is {}, open in viewer instead?\n\n[y] viewer   [n] edit   Esc cancel",
                        edit::human(n)
                    ),
                    true,
                )
            }
            Err(e @ Error::OutOfMemory { .. }) => {
                self.mode = ask(format!("{e}. Open in viewer?\n\n[y] yes   [n] no"), false)
            }
            Err(e) => self.mode = Mode::Message(e.to_string()),
        }
    }

    /// Run `work` in a thread; `tick` collects the result.
    fn spawn(
        &mut self,
        label: &'static str,
        path: PathBuf,
        total: u64,
        exit: bool,
        work: impl FnOnce(&AtomicU64, &AtomicBool) -> Done + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let (progress, cancel) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let (p, c) = (progress.clone(), cancel.clone());
        thread::spawn(move || {
            let _ = tx.send(work(&p, &c));
        });
        self.mode = Mode::Work(Job {
            label,
            path,
            rx,
            progress,
            cancel,
            total,
            exit,
            started: Instant::now(),
        });
    }

    fn spawn_load(&mut self, path: PathBuf) {
        let total = std::fs::metadata(&path).map_or(1, |m| m.len());
        let p = path.clone();
        self.spawn("Loading", path, total, false, move |progress, cancel| {
            Done::Loaded(Editor::open(&p, progress, cancel))
        });
    }

    fn spawn_save(&mut self, mut editor: Editor, to_utf8: bool, to_lf: bool, exit: bool) {
        let (path, total) = (editor.path().to_owned(), editor.line_count() as u64);
        self.spawn("Saving", path, total, exit, move |progress, cancel| {
            let result = editor.save(to_utf8, to_lf, progress, cancel);
            Done::Saved(editor, result)
        });
    }

    /// Ask about encoding, then line ends, only where the file differs from UTF-8/LF.
    fn next_save_prompt(&mut self, editor: Editor, exit: bool, to_utf8: bool, from: Step) {
        let step = if from == Step::Encoding && !editor.is_utf8() {
            Step::Encoding
        } else if editor.eol() != Eol::Lf {
            Step::Eol
        } else {
            return self.spawn_save(editor, to_utf8, false, exit);
        };
        self.mode = Mode::AskSave {
            editor,
            to_utf8,
            exit,
            step,
        };
    }

    fn leave_editor(&mut self) {
        self.mode = Mode::Normal;
        self.finish(None);
    }

    /// Collect a finished worker result.
    pub fn tick(&mut self) {
        let Mode::Work(job) = &self.mode else {
            return;
        };
        let done = match job.rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(done) => done,
            Err(TryRecvError::Disconnected) => {
                self.mode = Mode::Message("worker thread failed".into());
                return;
            }
        };
        let (exit, path) = (job.exit, job.path.clone());
        self.mode = Mode::Normal;
        match done {
            Done::Loaded(Ok(editor)) => {
                self.mode = Mode::Edit {
                    editor,
                    error: None,
                }
            }
            Done::Loaded(Err(Error::Binary)) => self.open_viewer(path),
            Done::Loaded(Err(Error::Cancelled)) => {}
            Done::Loaded(Err(e)) => self.mode = Mode::Message(e.to_string()),
            Done::Saved(_, Ok(())) if exit => self.leave_editor(),
            Done::Saved(editor, result) => {
                let error = result.err().filter(|e| !matches!(e, Error::Cancelled));
                self.mode = Mode::Edit {
                    editor,
                    error: error.map(|e| e.to_string()),
                };
            }
        }
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

    fn wait(app: &mut App) {
        while matches!(app.mode, Mode::Work(_)) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn edit_save_exit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi  \r\n").unwrap();
        let p = Panel::open(dir.path()).unwrap();
        let mut app = App::new(p, Panel::open(dir.path()).unwrap());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Edit { .. }));
        key(&mut app, KeyCode::Char('x'));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::AskExit(_)));
        key(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            app.mode,
            Mode::AskSave {
                step: Step::Eol,
                ..
            }
        ));
        key(&mut app, KeyCode::Char('y')); // to LF
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        let text = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(text, "xhi\n");
    }

    #[test]
    fn large_file_asks_for_viewer() {
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::File::create(dir.path().join("big")).unwrap();
        f.set_len((1 << 30) + 1).unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        assert!(matches!(app.mode, Mode::AskViewer { .. }));
        key(&mut app, KeyCode::Char('y'));
        assert!(matches!(app.mode, Mode::View { .. }));
        app.mode = Mode::Normal;
        key(&mut app, KeyCode::F(4));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }
}
