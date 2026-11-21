//! App state and key handling, independent of the terminal.

use std::ffi::OsString;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use regex::Regex;
use tc_core::edit::{self, Editor, Eol, SizeCheck};
use tc_core::panel::{Entry, Panel};
use tc_core::{Error, find, hotlist, ops, search, view::Viewer};

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
    /// Copy/move: editable destination (a new name for a single item, or a directory).
    Transfer(Op, Vec<PathBuf>, String),
    Mkdir(String),
    /// Quick search text (cursor jumps to the next match).
    Search(String),
    /// Quick filter text (non-matching entries hidden).
    Filter(String),
    /// Command prompt for the external editor (shown when the default is missing).
    Editor(PathBuf, String),
    /// Bookmark list with the selected row.
    Hotlist(usize),
    Message(String),
    /// Find results, filled live by a worker thread.
    Found(Found),
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

/// What a text prompt in the viewer or editor asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Find,
    Goto,
    Replace,
    /// Second step of replace, `Input::pattern` holds the first answer.
    With,
    /// File panel: select (`+`) or deselect (`-`) files by mask.
    Select,
    Deselect,
    /// Find files: name mask, then content regex (`pattern` holds the mask).
    FindMask,
    FindText,
}

impl Ask {
    pub fn title(self) -> &'static str {
        match self {
            Ask::Find => "Find (regex)",
            Ask::Goto => "Go to line (N% in viewer)",
            Ask::Replace => "Replace (regex)",
            Ask::With => "Replace with ($1 = group)",
            Ask::Select => "Select files (mask, default *)",
            Ask::Deselect => "Deselect files (mask, default *)",
            Ask::FindMask => "Find files: name mask (default *)",
            Ask::FindText => "Find files: content regex (empty = any)",
        }
    }
}

pub struct Input {
    pub ask: Ask,
    pub text: String,
    pattern: String,
    pub error: Option<String>,
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

/// Results of a running or finished file search.
pub struct Found {
    pub list: Vec<Entry>,
    pub sel: usize,
    pub running: bool,
    rx: Receiver<Entry>,
    cancel: Arc<AtomicBool>,
}

impl Found {
    fn pump(&mut self) {
        for _ in 0..5000 {
            match self.rx.try_recv() {
                Ok(e) => self.list.push(e),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.running = false;
                    return;
                }
            }
        }
    }
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

/// Footer bar items: label, key sent when clicked.
pub const FOOTER: [(&str, KeyCode, KeyModifiers); 8] = [
    ("F3 View", KeyCode::F(3), KeyModifiers::NONE),
    ("F4 Edit", KeyCode::F(4), KeyModifiers::NONE),
    ("S-F4 Ext", KeyCode::F(4), KeyModifiers::SHIFT),
    ("F5 Copy", KeyCode::F(5), KeyModifiers::NONE),
    ("F6 Move", KeyCode::F(6), KeyModifiers::NONE),
    ("F7 Mkdir", KeyCode::F(7), KeyModifiers::NONE),
    ("F8 Delete", KeyCode::F(8), KeyModifiers::NONE),
    ("F10 Quit", KeyCode::F(10), KeyModifiers::NONE),
];

pub struct App {
    pub panels: [Panel; 2],
    /// Scroll state per panel, kept across frames so the list does not jump.
    pub lists: [ListState; 2],
    /// Screen area of each panel, set by the UI, used for mouse clicks.
    pub areas: [Rect; 2],
    /// Screen area of the footer bar, set by the UI, used for mouse clicks.
    pub footer: Rect,
    pub active: usize,
    pub mode: Mode,
    pub quit: bool,
    /// Visible rows of a panel, set by the UI, used for PgUp/PgDn.
    pub page: usize,
    /// Open text prompt of the viewer or editor.
    pub input: Option<Input>,
    /// Last search, used by next/previous.
    pub search: Option<Regex>,
    /// Short result of the last key, shown in the viewer or editor, e.g. "not found".
    pub note: String,
    /// Editor clipboard, kept across files.
    // shortcut: not the system clipboard, upgrade when pasting to other programs matters
    clipboard: String,
    /// Bookmarked directories and the file they are saved to (none: not persisted).
    pub hotlist: Vec<PathBuf>,
    pub hotlist_file: Option<PathBuf>,
    /// Time and cell of the last left click, to detect double clicks (terminals send none).
    last_click: Option<(Instant, u16, u16)>,
}

impl App {
    pub fn new(left: Panel, right: Panel) -> Self {
        Self {
            panels: [left, right],
            lists: Default::default(),
            areas: Default::default(),
            footer: Rect::default(),
            active: 0,
            mode: Mode::Normal,
            quit: false,
            page: 10,
            input: None,
            search: None,
            note: String::new(),
            clipboard: String::new(),
            hotlist: Vec::new(),
            hotlist_file: None,
            last_click: None,
        }
    }

    pub fn other_dir(&self) -> &Path {
        self.panels[1 - self.active].path()
    }

    /// Wheel / touchpad scroll in viewer, editor and active file list (editor: moves the cursor, the window follows).
    pub fn on_mouse(&mut self, ev: MouseEvent) {
        if ev.kind == MouseEventKind::Down(MouseButton::Left) {
            return self.click(ev);
        }
        let lines = match ev.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return,
        };
        if self.input.is_some() {
            return;
        }
        match &mut self.mode {
            Mode::View { view, .. } => {
                if let Err(e) = view.scroll(lines) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            Mode::Edit {
                editor,
                error: None,
            } => editor.move_v(lines),
            _ => {}
        }
    }

    /// Left click on a file row moves the cursor there (and activates the panel); with Shift it selects the range from the old cursor.
    fn click(&mut self, ev: MouseEvent) {
        if self.input.is_some() || !matches!(self.mode, Mode::Normal) {
            return;
        }
        if self.footer.contains((ev.column, ev.row).into()) {
            let mut x = self.footer.x;
            for (label, code, mods) in FOOTER {
                let end = x + label.len() as u16;
                if (x..end).contains(&ev.column) {
                    return self.on_key(KeyEvent::new(code, mods));
                }
                x = end + 2;
            }
            return;
        }
        let Some(i) = (0..2).find(|&i| self.areas[i].contains((ev.column, ev.row).into())) else {
            return;
        };
        let area = self.areas[i];
        // first row is the border
        let Some(row) = usize::from(ev.row - area.y).checked_sub(1) else {
            return;
        };
        let idx = self.lists[i].offset() + row;
        if idx >= self.panels[i].entries().len()
            || row >= usize::from(area.height.saturating_sub(2))
        {
            return;
        }
        self.active = i;
        let panel = &mut self.panels[i];
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            panel.select_range(panel.cursor(), idx);
            return;
        }
        panel.set_cursor(idx);
        let now = Instant::now();
        let double = self.last_click.is_some_and(|(t, c, r)| {
            (c, r) == (ev.column, ev.row) && now.duration_since(t) < Duration::from_millis(400)
        });
        self.last_click = (!double).then_some((now, ev.column, ev.row));
        if double {
            let is_dir = self.panels[i].current().is_some_and(|e| e.is_dir);
            let code = if is_dir {
                KeyCode::Enter
            } else {
                KeyCode::F(3)
            };
            self.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        self.note.clear();
        if self.input.is_some() {
            return self.input_key(key);
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal
                if key.code == KeyCode::F(4) && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                if let Some(path) = self.current_file() {
                    self.external_edit(path, "code".into());
                }
            }
            Mode::Normal
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    && matches!(key.code, KeyCode::Up | KeyCode::Down) =>
            {
                let panel = &mut self.panels[self.active];
                let c = panel.cursor();
                // select the entry left behind
                panel.select_range(c, c);
                panel.move_cursor(if key.code == KeyCode::Up { -1 } else { 1 });
            }
            Mode::Normal
                if key.code == KeyCode::F(7) && key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.ask(Ask::FindMask);
            }
            Mode::Normal if key.modifiers.contains(KeyModifiers::CONTROL) => match key.code {
                KeyCode::Char('s') => self.mode = Mode::Search(String::new()),
                KeyCode::Char('f') => {
                    self.mode = Mode::Filter(self.panels[self.active].filter().to_owned());
                }
                KeyCode::Char('d') => self.hotlist_add(),
                KeyCode::Char('b') => self.mode = Mode::Hotlist(0),
                KeyCode::Char('y') => {
                    let [a, b] = &mut self.panels;
                    a.compare_with(b);
                }
                _ => {}
            },
            Mode::Normal
                if key.modifiers.contains(KeyModifiers::ALT)
                    && matches!(key.code, KeyCode::Left | KeyCode::Right) =>
            {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Err(e) = self.panels[self.active].history_step(delta) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            Mode::Normal if key.code == KeyCode::Esc => {
                let panel = &mut self.panels[self.active];
                panel.apply_filter("");
                if let Err(e) = panel.leave_virtual() {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            Mode::Normal => self.normal_key(key.code),
            Mode::Search(mut text) => {
                let panel = &mut self.panels[self.active];
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => return,
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        panel.search(&text, 1);
                    }
                    KeyCode::Backspace => {
                        text.pop();
                        panel.search(&text, 0);
                    }
                    KeyCode::Char(c) => {
                        text.push(c);
                        panel.search(&text, 0);
                    }
                    _ => {}
                }
                self.mode = Mode::Search(text);
            }
            Mode::Filter(mut text) => {
                let panel = &mut self.panels[self.active];
                match key.code {
                    KeyCode::Enter => return,
                    KeyCode::Esc => {
                        panel.apply_filter("");
                        return;
                    }
                    KeyCode::Backspace => {
                        text.pop();
                        panel.apply_filter(&text);
                    }
                    KeyCode::Char(c) => {
                        text.push(c);
                        panel.apply_filter(&text);
                    }
                    _ => {}
                }
                self.mode = Mode::Filter(text);
            }
            Mode::Editor(path, mut cmd) => match key.code {
                KeyCode::Enter => self.external_edit(path, cmd),
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    cmd.pop();
                    self.mode = Mode::Editor(path, cmd);
                }
                KeyCode::Char(c) => {
                    cmd.push(c);
                    self.mode = Mode::Editor(path, cmd);
                }
                _ => self.mode = Mode::Editor(path, cmd),
            },
            Mode::Confirm(op, targets) => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.delete(&targets),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
                _ => self.mode = Mode::Confirm(op, targets),
            },
            Mode::Transfer(op, targets, mut text) => match key.code {
                KeyCode::Enter => self.run(op, &targets, &text),
                KeyCode::Esc => {}
                code => {
                    match code {
                        KeyCode::Backspace => drop(text.pop()),
                        KeyCode::Char(c) => text.push(c),
                        _ => {}
                    }
                    self.mode = Mode::Transfer(op, targets, text);
                }
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
            Mode::Hotlist(sel) => self.hotlist_key(key.code, sel),
            Mode::Found(f) => self.found_key(key.code, f),
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
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let prompt = matches!(key.code, KeyCode::F(3 | 4 | 7))
                    || (ctrl && matches!(key.code, KeyCode::Char('g' | 'f' | 'r')));
                if matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                ) {
                    editor.select(shift);
                }
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
                    _ if prompt => {
                        self.mode = Mode::Edit {
                            editor,
                            error: None,
                        };
                        match key.code {
                            KeyCode::F(3) => self.find_or_ask(!shift),
                            KeyCode::F(4) | KeyCode::Char('r') => self.ask(Ask::Replace),
                            KeyCode::F(7) | KeyCode::Char('f') => self.ask(Ask::Find),
                            _ => self.ask(Ask::Goto),
                        }
                        return;
                    }
                    KeyCode::Up => editor.move_v(-1),
                    KeyCode::Down => editor.move_v(1),
                    KeyCode::PageUp => editor.move_v(-page),
                    KeyCode::PageDown => editor.move_v(page),
                    KeyCode::Left if ctrl => editor.move_word(false),
                    KeyCode::Right if ctrl => editor.move_word(true),
                    KeyCode::Home if ctrl => editor.goto_idx(0),
                    KeyCode::End if ctrl => editor.goto_idx(usize::MAX),
                    KeyCode::Left => editor.move_h(-1),
                    KeyCode::Right => editor.move_h(1),
                    KeyCode::Home => editor.home(),
                    KeyCode::End => editor.end(),
                    KeyCode::Enter => editor.insert_char('\n'),
                    KeyCode::Tab => editor.insert_char('\t'),
                    KeyCode::Backspace => editor.backspace(),
                    KeyCode::Delete => editor.delete(),
                    KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                        'z' => editor.undo(),
                        'y' => editor.redo(),
                        'c' => {
                            self.clipboard = editor.copy().unwrap_or(mem::take(&mut self.clipboard))
                        }
                        'x' => {
                            self.clipboard = editor.cut().unwrap_or(mem::take(&mut self.clipboard))
                        }
                        'v' => editor.insert(&self.clipboard, false),
                        'k' => editor.delete_line(),
                        _ => {}
                    },
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
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
                    KeyCode::Left | KeyCode::Right => {
                        view.scroll_h(if key.code == KeyCode::Left { -8 } else { 8 });
                        Ok(())
                    }
                    KeyCode::Char('e') => {
                        view.next_encoding();
                        Ok(())
                    }
                    KeyCode::Char('/' | 'f' | 'n' | 'N' | 'g') | KeyCode::F(7) => {
                        self.mode = Mode::View { title, view };
                        match key.code {
                            KeyCode::Char('n') => self.find_or_ask(true),
                            KeyCode::Char('N') => self.find_or_ask(false),
                            KeyCode::Char('g') => self.ask(Ask::Goto),
                            _ => self.ask(Ask::Find),
                        }
                        return;
                    }
                    _ => Ok(()),
                };
                self.mode = match result {
                    Ok(()) => Mode::View { title, view },
                    Err(e) => Mode::Message(format!("{title}: {e}")),
                };
            }
        }
    }

    fn hotlist_add(&mut self) {
        let path = self.panels[self.active].path().to_owned();
        if !self.hotlist.contains(&path) {
            self.hotlist.push(path);
            self.hotlist_save();
        }
    }

    fn hotlist_save(&mut self) {
        if let Some(file) = &self.hotlist_file
            && let Err(e) = hotlist::save(file, &self.hotlist)
        {
            self.mode = Mode::Message(e.to_string());
        }
    }

    fn hotlist_key(&mut self, code: KeyCode, sel: usize) {
        let last = self.hotlist.len().saturating_sub(1);
        let mut sel = sel.min(last);
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Up => sel = sel.saturating_sub(1),
            KeyCode::Down => sel = (sel + 1).min(last),
            KeyCode::Enter if !self.hotlist.is_empty() => {
                let path = self.hotlist[sel].clone();
                if let Err(e) = self.panels[self.active].go(path) {
                    self.mode = Mode::Message(e.to_string());
                }
                return;
            }
            KeyCode::Delete if !self.hotlist.is_empty() => {
                self.hotlist.remove(sel);
                self.hotlist_save();
                if matches!(self.mode, Mode::Message(_)) {
                    return;
                }
            }
            _ => {}
        }
        self.mode = Mode::Hotlist(sel);
    }

    /// Search the active panel's directory in a worker thread, results arrive via `tick`.
    fn spawn_find(&mut self, mask: String, re: Option<Regex>) {
        let root = self.panels[self.active].path().to_owned();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        thread::spawn(move || {
            find::find(&root, &mask, re.as_ref(), &c, |e| drop(tx.send(e)));
        });
        self.mode = Mode::Found(Found {
            list: Vec::new(),
            sel: 0,
            running: true,
            rx,
            cancel,
        });
    }

    /// Keys in the results: Esc stops a running search, then closes; Enter panelizes.
    fn found_key(&mut self, code: KeyCode, mut f: Found) {
        let page = self.page;
        let last = f.list.len().saturating_sub(1);
        match code {
            KeyCode::Esc if f.running => f.cancel.store(true, Ordering::Relaxed),
            KeyCode::Esc => return,
            KeyCode::Enter => {
                f.cancel.store(true, Ordering::Relaxed);
                if !f.list.is_empty() {
                    self.panels[self.active].panelize(f.list);
                }
                return;
            }
            KeyCode::Up => f.sel = f.sel.saturating_sub(1),
            KeyCode::Down => f.sel = (f.sel + 1).min(last),
            KeyCode::PageUp => f.sel = f.sel.saturating_sub(page),
            KeyCode::PageDown => f.sel = (f.sel + page).min(last),
            KeyCode::Home => f.sel = 0,
            KeyCode::End => f.sel = last,
            _ => {}
        }
        self.mode = Mode::Found(f);
    }

    fn ask(&mut self, ask: Ask) {
        self.input = Some(Input {
            ask,
            text: String::new(),
            pattern: String::new(),
            error: None,
        });
    }

    fn find_or_ask(&mut self, forward: bool) {
        if self.search.is_some() {
            self.find(forward);
        } else {
            self.ask(Ask::Find);
        }
    }

    /// Select or show the next match of the last search in the viewer or editor.
    fn find(&mut self, forward: bool) {
        let Some(re) = &self.search else {
            return;
        };
        let found = match &mut self.mode {
            Mode::Edit { editor, .. } => Ok(editor.find(re, forward)),
            Mode::View { view, .. } => view.search(re, forward),
            _ => return,
        };
        match found {
            Ok(true) => {}
            Ok(false) => self.note = "not found".into(),
            Err(e) => self.mode = Mode::Message(e.to_string()),
        }
    }

    fn goto(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        let err = |_| "not a number".to_owned();
        match &mut self.mode {
            Mode::Edit { editor, .. } => editor.goto_line(text.parse().map_err(err)?),
            Mode::View { view, .. } => match text.strip_suffix('%') {
                Some(p) => view.goto_percent(p.trim().parse().map_err(err)?),
                None => view.goto_line(text.parse().map_err(err)?),
            }
            .map_err(|e| e.to_string())?,
            _ => {}
        }
        Ok(())
    }

    /// Key in the open prompt; Enter runs it, errors keep it open.
    fn input_key(&mut self, key: KeyEvent) {
        let Some(mut input) = self.input.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => match self.submit(&mut input) {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => input.error = Some(e),
            },
            KeyCode::Backspace => {
                input.text.pop();
                input.error = None;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.text.push(c);
                input.error = None;
            }
            _ => {}
        }
        self.input = Some(input);
    }

    /// `Ok(true)` when done, `Ok(false)` when the prompt goes on with its next step.
    fn submit(&mut self, input: &mut Input) -> Result<bool, String> {
        let text = input.text.clone();
        let compile = |p: &str| search::compile(p).map_err(|e| e.to_string());
        match input.ask {
            Ask::Goto => self.goto(&text)?,
            Ask::Select | Ask::Deselect => {
                let on = input.ask == Ask::Select;
                self.panels[self.active].select_mask(&text, on);
            }
            Ask::FindMask => {
                input.pattern = text;
                input.text.clear();
                input.ask = Ask::FindText;
                return Ok(false);
            }
            Ask::FindText => {
                let re = if text.is_empty() {
                    None
                } else {
                    Some(compile(&text)?)
                };
                self.spawn_find(mem::take(&mut input.pattern), re);
            }
            Ask::Find => {
                // empty answer repeats the last search
                if !text.is_empty() {
                    self.search = Some(compile(&text)?);
                } else if self.search.is_none() {
                    return Err("empty pattern".into());
                }
                self.find(true);
            }
            Ask::Replace => {
                compile(&text)?;
                input.pattern = text;
                input.text.clear();
                input.ask = Ask::With;
                return Ok(false);
            }
            Ask::With => {
                let re = compile(&input.pattern)?;
                if let Mode::Edit { editor, .. } = &mut self.mode {
                    self.note = format!("{} replaced", editor.replace_all(&re, &text));
                }
                self.search = Some(re);
            }
        }
        Ok(true)
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
            KeyCode::Char('+') => self.ask(Ask::Select),
            KeyCode::Char('-') => self.ask(Ask::Deselect),
            KeyCode::Char('*') => panel.invert_selection(),
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

    #[cfg(test)]
    fn mode_view_percent(&self) -> u64 {
        match &self.mode {
            Mode::View { view, .. } => view.percent(),
            _ => panic!("not in viewer"),
        }
    }

    fn confirm(&mut self, op: Op) {
        let targets = self.panels[self.active].targets();
        match targets.as_slice() {
            [] => {}
            _ if op == Op::Delete => self.mode = Mode::Confirm(op, targets),
            [one] => {
                let name = one.file_name().unwrap_or_default();
                let text = self.other_dir().join(name).display().to_string();
                self.mode = Mode::Transfer(op, targets, text);
            }
            _ => {
                let text = self.other_dir().display().to_string();
                self.mode = Mode::Transfer(op, targets, text);
            }
        }
    }

    fn delete(&mut self, targets: &[PathBuf]) {
        let result = targets.iter().try_for_each(|src| ops::delete(src));
        self.finish(result.err().map(|e| e.to_string()));
    }

    /// Copy or move to `text`, relative to the active panel. A single item goes to
    /// exactly that path (rename); several items, or a trailing separator, mean a directory.
    fn run(&mut self, op: Op, targets: &[PathBuf], text: &str) {
        let dest = self.panels[self.active].path().join(text);
        let into_dir = targets.len() > 1 || text.ends_with(['/', std::path::MAIN_SEPARATOR]);
        // Stops at the first error so the user sees it before anything else happens.
        let result = targets.iter().try_for_each(|src| {
            let target = match src.file_name() {
                Some(name) if into_dir => dest.join(name),
                _ => dest.clone(),
            };
            match op {
                Op::Copy => ops::copy(src, &target),
                _ => ops::move_(src, &target),
            }
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

    /// Start `cmd path` detached; a missing command asks for another one.
    // shortcut: no terminal suspend, so console editors (vim) do not work, upgrade if wanted
    fn external_edit(&mut self, path: PathBuf, cmd: String) {
        use std::process::{Command, Stdio};
        let cmd = cmd.trim().to_string();
        let spawned = Command::new(&cmd)
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.mode = Mode::Editor(path, String::new())
            }
            Err(e) => self.mode = Mode::Message(format!("{cmd}: {e}")),
        }
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
        if let Mode::Found(f) = &mut self.mode {
            return f.pump();
        }
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
    fn hotlist_add_jump_delete_and_history() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = App::new(Panel::open(&sub).unwrap(), Panel::open(tmp.path()).unwrap());
        let file = tmp.path().join("cfg").join("hotlist");
        app.hotlist_file = Some(file.clone());
        let ctrl =
            |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        ctrl(&mut app, 'd');
        ctrl(&mut app, 'd'); // no duplicate
        assert_eq!(hotlist::load(&file).unwrap(), [app.panels[0].path()]);
        key(&mut app, KeyCode::Tab);
        ctrl(&mut app, 'b');
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.panels[1].path(), sub);
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.panels[1].path(), tmp.path());
        // vanished dir: error popup, panel unchanged
        std::fs::remove_dir(&sub).unwrap();
        ctrl(&mut app, 'b');
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Message(_)));
        assert_eq!(app.panels[1].path(), tmp.path());
        key(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'b');
        key(&mut app, KeyCode::Delete);
        assert!(app.hotlist.is_empty() && hotlist::load(&file).unwrap().is_empty());
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
        assert!(matches!(app.mode, Mode::Transfer(Op::Copy, _, _)));
        key(&mut app, KeyCode::Enter);
        assert!(right.path().join("new").is_dir());
        assert!(matches!(app.mode, Mode::Normal));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Message(_))); // already exists
        key(&mut app, KeyCode::Char(' '));
        // copy in the current dir under a new name (relative to the active panel)
        key(&mut app, KeyCode::F(5));
        for _ in 0..right.path().display().to_string().len() + 5 {
            key(&mut app, KeyCode::Backspace);
        }
        for c in "copy".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        assert!(left.path().join("copy").is_dir());
    }

    #[test]
    fn shift_arrows_and_clicks_select() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a", "b", "c", "d"] {
            std::fs::write(dir.path().join(n), "").unwrap();
        }
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        app.areas = [Rect::new(0, 0, 40, 10), Rect::new(40, 0, 40, 10)];
        let shift = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        app.on_key(shift(KeyCode::Down)); // ".." is not selectable
        app.on_key(shift(KeyCode::Down));
        assert_eq!(app.panels[0].targets().len(), 1);
        let click = |app: &mut App, column, row, modifiers| {
            app.on_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers,
            })
        };
        click(&mut app, 45, 3, KeyModifiers::NONE); // row 3 = entry 2 ("b") in right panel
        assert_eq!((app.active, app.panels[1].cursor()), (1, 2));
        click(&mut app, 45, 5, KeyModifiers::SHIFT); // entries 2..=4
        assert_eq!(app.panels[1].targets().len(), 3);
        click(&mut app, 45, 1, KeyModifiers::NONE); // ".." twice quickly: enters the parent
        let before = app.panels[1].path().to_path_buf();
        click(&mut app, 45, 1, KeyModifiers::NONE);
        assert_ne!(app.panels[1].path(), before);
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

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            key(app, KeyCode::Char(c));
        }
        key(app, KeyCode::Enter);
    }

    fn rows(app: &mut App) -> Vec<String> {
        app.mode.editor_mut().unwrap().visible(5, 30)
    }

    #[test]
    fn editor_find_replace_undo_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo one\n").unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);

        key(&mut app, KeyCode::F(7));
        type_text(&mut app, "(");
        assert_eq!(
            app.input
                .as_ref()
                .unwrap()
                .error
                .as_deref()
                .map(str::is_empty),
            Some(false)
        );
        key(&mut app, KeyCode::Backspace);
        assert!(app.input.as_ref().unwrap().error.is_none());
        type_text(&mut app, "ONE"); // smart case: uppercase is exact, no match
        assert!(app.input.is_none());
        assert_eq!(app.note, "not found");
        key(&mut app, KeyCode::F(3)); // next with the last search
        key(&mut app, KeyCode::F(7));
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::F(7));
        type_text(&mut app, "one");
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (1, 4));
        key(&mut app, KeyCode::F(3));
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (0, 0));

        let ctrl =
            |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        ctrl(&mut app, 'c'); // match is selected
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::End);
        ctrl(&mut app, 'v');
        assert_eq!(rows(&mut app), ["one", "two oneone", ""]);
        ctrl(&mut app, 'z');
        assert_eq!(rows(&mut app), ["one", "two one", ""]);

        key(&mut app, KeyCode::F(4));
        type_text(&mut app, "o(n)e");
        assert!(app.input.is_some());
        type_text(&mut app, "[${1}]");
        assert_eq!(app.note, "2 replaced");
        assert_eq!(rows(&mut app), ["[n]", "two [n]", ""]);

        ctrl(&mut app, 'g');
        type_text(&mut app, "2");
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (1, 0));
    }

    #[test]
    fn viewer_search_and_goto() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nfoo\nc\nfoo\n").unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        key(&mut app, KeyCode::Char('f')); // same as '/'
        type_text(&mut app, "foo");
        let top = |app: &mut App| match &mut app.mode {
            Mode::View { view, .. } => view.lines(1, 10).unwrap(),
            _ => panic!("not in viewer"),
        };
        assert_eq!(top(&mut app), ["foo"]);
        assert_eq!(app.mode_view_percent(), 28);
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(app.mode_view_percent(), 71);
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(app.note, "not found");
        key(&mut app, KeyCode::Char('N'));
        assert_eq!(app.mode_view_percent(), 28);
        key(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "1");
        assert_eq!(app.mode_view_percent(), 0);
        key(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "x%");
        assert!(app.input.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn find_then_panelize_and_back() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "world\n").unwrap();
        std::fs::write(dir.path().join("c.rs"), "hello\n").unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        app.on_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::ALT));
        type_text(&mut app, "*.txt");
        type_text(&mut app, "hello");
        while matches!(&app.mode, Mode::Found(f) if f.running) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(matches!(&app.mode, Mode::Found(f) if f.list.len() == 1));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal) && app.panels[0].is_virtual());
        assert_eq!(app.panels[0].targets(), [dir.path().join("a.txt")]);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Esc); // back to the directory
        assert!(!app.panels[0].is_virtual());
        // Esc cancels a running search, a second Esc closes
        app.on_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::ALT));
        type_text(&mut app, "");
        type_text(&mut app, "");
        key(&mut app, KeyCode::Esc);
        while matches!(&app.mode, Mode::Found(f) if f.running) {
            app.tick();
        }
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }
}
