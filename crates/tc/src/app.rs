//! App state and key handling, independent of the terminal.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::ui::{DATE_WIDTH, SIZE_WIDTH};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use regex::Regex;
use tc_core::archive::{Archive, Format, TempDir};
use tc_core::cmdline::{self, LineEdit};
use tc_core::edit::{self, Editor, Eol, SizeCheck};
use tc_core::eta::Estimator;
use tc_core::panel::{Entry, Panel, SortKey};
use tc_core::stats::{self, Node};
use tc_core::vfs::{self, Remote, Side};
use tc_core::{Error, find, hotlist, ops, search, view::Viewer};
use tc_remote::Prompt;

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
    /// Delete: targets and the question shown.
    Confirm(Op, Vec<PathBuf>, String),
    /// Copy/move: editable destination (a new name for a single item, or a directory).
    Transfer(Op, Vec<PathBuf>, LineEdit),
    Mkdir(LineEdit),
    /// Shift+F4: name of the file to create (if missing) and edit externally.
    NewFile(LineEdit),
    /// Quick search text (cursor jumps to the next match).
    Search(String),
    /// Quick filter text (non-matching entries hidden).
    Filter(String),
    /// Command prompt for the external editor (shown when the default is missing).
    Editor(PathBuf, LineEdit),
    /// Command line, run with Enter.
    Cmd(LineEdit),
    /// Bookmark list with the selected row.
    Hotlist(usize),
    /// Mount points, selected row and the panel Enter jumps.
    Drives(Vec<PathBuf>, usize, usize),
    Message(String),
    /// Find results, filled live by a worker thread.
    Found(Found),
    /// File Stats (ncdu-style) tree, scanned by a worker thread.
    Stats(Stats),
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
    /// Copy or move running in a worker thread, which may ask about name conflicts.
    Xfer(Xfer),
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
    /// Saved connections (Ctrl+N) with the selected row.
    Connections(usize),
    /// Connection URL being added (`None`) or edited (row).
    ConnEdit(Option<usize>, LineEdit),
    /// Connect running in a worker; may ask for a host key or password.
    Connecting(Connecting),
}

enum CMsg {
    Ask(Prompt),
    Done(tc_core::Result<(Arc<dyn Remote>, String)>),
}

pub struct Connecting {
    pub url: String,
    /// Open question and the answer typed so far (a password, shown masked).
    pub ask: Option<(Prompt, LineEdit)>,
    rx: Receiver<CMsg>,
    reply: mpsc::Sender<Option<String>>,
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
    pub text: LineEdit,
    pattern: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Encoding,
    Eol,
}

enum Done {
    Listed(tc_core::Result<Archive>),
    Extracted(tc_core::Result<(TempDir, PathBuf)>),
    /// Remote file downloaded for the editor: connection, remote path, temp copy.
    Fetched(
        Arc<dyn Remote>,
        PathBuf,
        tc_core::Result<(TempDir, PathBuf)>,
    ),
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

/// File Stats: the scanned tree and the directory being browsed.
pub struct Stats {
    pub dir: PathBuf,
    pub root: Node,
    /// Child indices from `root` down to the browsed directory.
    path: Vec<usize>,
    pub sel: usize,
    /// Show and sort by file count instead of size.
    pub by_files: bool,
    /// Running scan; `root` is empty until it finishes.
    job: Option<thread::JoinHandle<Node>>,
    pub progress: Arc<ops::Progress>,
    /// Delete question for the selected entry, shown while set.
    pub confirm: Option<String>,
    /// Connection of a remote panel the tree was scanned on.
    remote: Option<Arc<dyn Remote>>,
    /// Child of the browsed directory a running remote delete removes from the tree on success.
    deleting: Option<usize>,
}

impl Stats {
    pub fn scanning(&self) -> bool {
        self.job.is_some()
    }

    pub fn current(&self) -> &Node {
        self.path.iter().fold(&self.root, |n, &i| &n.children[i])
    }

    fn current_mut(&mut self) -> &mut Node {
        let mut n = &mut self.root;
        for &i in &self.path {
            n = &mut n.children[i];
        }
        n
    }

    /// Browsed directory below `dir`, for the title.
    pub fn path(&self) -> PathBuf {
        let mut p = self.dir.clone();
        let mut n = &self.root;
        for &i in &self.path {
            n = &n.children[i];
            p.push(&n.name);
        }
        p
    }

    fn key(&mut self, code: KeyCode, page: usize) {
        let last = self.current().children.len().saturating_sub(1);
        match code {
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(page),
            KeyCode::PageDown => self.sel = (self.sel + page).min(last),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = last,
            KeyCode::Enter | KeyCode::Right
                if self
                    .current()
                    .children
                    .get(self.sel)
                    .is_some_and(|n| n.is_dir) =>
            {
                self.path.push(self.sel);
                let by_files = self.by_files;
                stats::sort(&mut self.current_mut().children, by_files);
                self.sel = 0;
            }
            KeyCode::Backspace | KeyCode::Left => {
                if let Some(i) = self.path.pop() {
                    let by_files = self.by_files;
                    let dir = self.current_mut();
                    let name = dir.children[i].name.clone();
                    stats::sort(&mut dir.children, by_files);
                    self.sel = dir
                        .children
                        .iter()
                        .position(|n| n.name == name)
                        .unwrap_or(0);
                }
            }
            KeyCode::Tab => {
                self.by_files = !self.by_files;
                let by_files = self.by_files;
                stats::sort(&mut self.current_mut().children, by_files);
                self.sel = 0;
            }
            _ => {}
        }
    }
}

enum XMsg {
    /// Conflict: (source, existing target); the worker waits for an `Answer`.
    Ask(PathBuf, PathBuf),
    /// Finished with an error text, if any.
    Done(Option<String>),
}

#[derive(Clone, Copy)]
enum Policy {
    Overwrite,
    Skip,
    Newer,
}

enum Answer {
    One(Policy),
    /// Same policy for all further conflicts.
    All(Policy),
    Rename(String),
    Cancel,
}

pub struct Xfer {
    pub ask: Option<(PathBuf, PathBuf)>,
    /// Name being typed after `r`.
    pub rename: Option<LineEdit>,
    rx: Receiver<XMsg>,
    reply: mpsc::Sender<Answer>,
    pub progress: Arc<ops::Progress>,
    /// Fed from `App::tick`.
    pub eta: Estimator,
    started: Instant,
}

impl Xfer {
    /// Quick copies finish before a popup would only flicker.
    pub fn visible(&self) -> bool {
        self.ask.is_some() || self.started.elapsed() > Duration::from_secs(1)
    }

    fn answer(&mut self, a: Answer) {
        self.ask = None;
        self.rename = None;
        let _ = self.reply.send(a);
    }

    fn key(&mut self, key: KeyEvent) {
        let code = key.code;
        if let Some(text) = &mut self.rename {
            match code {
                KeyCode::Enter => {
                    let name = std::mem::take(&mut text.text);
                    self.answer(Answer::Rename(name));
                }
                KeyCode::Esc => self.rename = None,
                _ => {
                    edit_key(text, key);
                }
            }
        } else if let Some((_, target)) = &self.ask {
            use Policy::*;
            match code {
                KeyCode::Char('o') => self.answer(Answer::One(Overwrite)),
                KeyCode::Char('s') => self.answer(Answer::One(Skip)),
                KeyCode::Char('n') => self.answer(Answer::One(Newer)),
                KeyCode::Char('O') => self.answer(Answer::All(Overwrite)),
                KeyCode::Char('S') => self.answer(Answer::All(Skip)),
                KeyCode::Char('N') => self.answer(Answer::All(Newer)),
                KeyCode::Char('r') => {
                    let name = target.file_name().unwrap_or_default();
                    self.rename = Some(LineEdit::from(name.to_string_lossy().into_owned()));
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('c') => self.answer(Answer::Cancel),
                _ => {}
            }
        } else if code == KeyCode::Esc {
            self.progress.cancel.store(true, Ordering::Relaxed);
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

/// Edit keys shared by all text prompts; true if `key` was one of them.
fn edit_key(l: &mut LineEdit, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Left => l.left(),
        KeyCode::Right => l.right(),
        KeyCode::Home => l.home(),
        KeyCode::End => l.end(),
        KeyCode::Backspace => l.backspace(),
        KeyCode::Delete => l.delete(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => l.clear(),
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            l.insert(c)
        }
        _ => return false,
    }
    true
}

/// Pasted text goes into a one-line prompt without line breaks.
fn paste_line(l: &mut LineEdit, text: &str) {
    text.chars()
        .filter(|c| !matches!(c, '\r' | '\n'))
        .for_each(|c| l.insert(c));
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
pub const FOOTER: [(&str, KeyCode, KeyModifiers); 9] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
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
    /// Background tabs per side, in cycle order after the visible one in `panels`.
    pub tabs: [VecDeque<Panel>; 2],
    /// Position of the visible tab per side, for the title.
    pub tab: [usize; 2],
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
    /// Shortcut popup over the current mode, any key closes it.
    pub help: bool,
    pub help_scroll: usize,
    /// Last search, used by next/previous.
    pub search: Option<Regex>,
    /// Short result of the last key, shown in the viewer or editor, e.g. "not found".
    pub note: String,
    /// Editor clipboard, kept across files (Ctrl+V); the system clipboard is fed through `copied`.
    clipboard: String,
    /// Text for the main loop to put on the system clipboard (OSC 52).
    pub copied: Option<String>,
    /// Size calculation in a worker thread: directory and its progress (`scanned` = result ready).
    pub dir_job: Option<(usize, PathBuf, Arc<ops::Progress>)>,
    /// Viewer line numbers on (`l`).
    pub view_numbers: bool,
    /// Bookmarked directories and the file they are saved to (none: not persisted).
    pub hotlist: Vec<PathBuf>,
    pub hotlist_file: Option<PathBuf>,
    /// Time and cell of the last left click, to detect double clicks (terminals send none).
    last_click: Option<(Instant, u16, u16)>,
    /// Command history (oldest first) and the file it is saved to.
    pub history: Vec<String>,
    pub history_file: Option<PathBuf>,
    /// Command and directory for the main loop to run on the real terminal.
    /// The `url` of a remote connection runs it there over `ssh`.
    pub run: Option<(String, PathBuf, Option<String>)>,
    /// Entry extracted from an archive for the viewer; removed once the viewer is closed.
    view_tmp: Option<TempDir>,
    /// File Stats left for a viewer or editor; back once the mode is `Normal` again.
    pub stats: Option<Stats>,
    /// Saved connection URLs (Ctrl+N) and the file they are saved to.
    pub connections: Vec<PathBuf>,
    pub connections_file: Option<PathBuf>,
    /// Remote file open in the editor: saves are uploaded to it; the temp copy lives until the editor closes.
    edit_remote: Option<(Arc<dyn Remote>, PathBuf, TempDir)>,
}

/// Largest archive entry F3/F4 extracts to a temp file.
const VIEW_CAP: u64 = 1 << 30;
const READ_ONLY: &str = "Archives are read-only: only F5 copy out and F3/F4 view work here.";
const REMOTE: &str = "Not available on remote panels.";

impl App {
    pub fn new(left: Panel, right: Panel) -> Self {
        Self {
            panels: [left, right],
            tabs: Default::default(),
            tab: [0; 2],
            lists: Default::default(),
            areas: Default::default(),
            footer: Rect::default(),
            active: 0,
            mode: Mode::Normal,
            quit: false,
            page: 10,
            input: None,
            help: false,
            help_scroll: 0,
            search: None,
            note: String::new(),
            clipboard: String::new(),
            copied: None,
            dir_job: None,
            view_numbers: false,
            hotlist: Vec::new(),
            hotlist_file: None,
            last_click: None,
            history: Vec::new(),
            history_file: None,
            run: None,
            view_tmp: None,
            stats: None,
            connections: Vec::new(),
            connections_file: None,
            edit_remote: None,
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
        if ev.row == area.y {
            // title row: columns of the size and date fields sort by them
            let col = usize::from(ev.column - area.x).saturating_sub(1);
            let w = usize::from(area.width).saturating_sub(2);
            let key = if col >= w.saturating_sub(DATE_WIDTH) {
                SortKey::Date
            } else if col >= w.saturating_sub(DATE_WIDTH + 1 + SIZE_WIDTH) {
                SortKey::Size
            } else {
                SortKey::Name
            };
            self.active = i;
            self.panels[i].set_sort(key);
            return;
        }
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
            let is_dir = self.panels[i]
                .current()
                .is_some_and(|e| e.is_dir || Format::detect(Path::new(&e.name)).is_some());
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
        if self.help {
            return self.help_key(key.code);
        }
        let idle = matches!(self.mode, Mode::Normal | Mode::View { .. })
            || matches!(&self.mode, Mode::Stats(st) if !st.scanning())
            || matches!(self.mode, Mode::Edit { error: None, .. });
        if key.code == KeyCode::F(1) && idle && self.input.is_none() {
            self.help = true;
            self.help_scroll = 0;
            return;
        }
        if self.input.is_some() {
            return self.input_key(key);
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_mode_key(key),
            Mode::Search(text) => self.search_key(text, key),
            Mode::Filter(text) => self.filter_key(text, key),
            Mode::Editor(path, mut cmd) => match key.code {
                KeyCode::Enter => self.external_edit(path, cmd.text),
                KeyCode::Esc => {}
                _ => {
                    edit_key(&mut cmd, key);
                    self.mode = Mode::Editor(path, cmd);
                }
            },
            Mode::Connections(sel) => self.connections_key(key.code, sel),
            Mode::ConnEdit(row, mut l) => match key.code {
                KeyCode::Enter => self.conn_save(row, &l.text),
                KeyCode::Esc => self.mode = Mode::Connections(row.unwrap_or(0)),
                _ => {
                    edit_key(&mut l, key);
                    self.mode = Mode::ConnEdit(row, l);
                }
            },
            Mode::Connecting(c) => self.connecting_key(c, key),
            // no trash on remote panels: only `p`
            Mode::Confirm(op, targets, text) => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter
                    if self.panels[self.active].remote().is_none() =>
                {
                    self.delete(&targets, true)
                }
                KeyCode::Char('p' | 'P') => self.delete(&targets, false),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
                _ => self.mode = Mode::Confirm(op, targets, text),
            },
            Mode::Transfer(op, targets, mut text) => match key.code {
                KeyCode::Enter => self.run(op, &targets, &text.text),
                KeyCode::Esc => {}
                _ => {
                    edit_key(&mut text, key);
                    self.mode = Mode::Transfer(op, targets, text);
                }
            },
            Mode::Mkdir(mut name) => match key.code {
                KeyCode::Enter => self.mkdir(&name.text),
                KeyCode::Esc => {}
                _ => {
                    edit_key(&mut name, key);
                    self.mode = Mode::Mkdir(name);
                }
            },
            Mode::NewFile(mut name) => match key.code {
                KeyCode::Enter => self.new_file(&name.text),
                KeyCode::Esc => {}
                _ => {
                    edit_key(&mut name, key);
                    self.mode = Mode::NewFile(name);
                }
            },
            Mode::Cmd(mut l) => {
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Enter => return self.run_cmd(&l.text),
                    KeyCode::Up => l.prev(&self.history),
                    KeyCode::Down => l.next(&self.history),
                    _ => {
                        edit_key(&mut l, key);
                    }
                }
                self.mode = Mode::Cmd(l);
            }
            Mode::Hotlist(sel) => self.hotlist_key(key.code, sel),
            Mode::Drives(rows, sel, panel) => self.drives_key(key.code, rows, sel, panel),
            Mode::Found(f) => self.found_key(key.code, f),
            Mode::Stats(st) => self.stats_mode_key(st, key),
            Mode::Message(_) => {}
            Mode::Xfer(mut x) => {
                x.key(key);
                self.mode = Mode::Xfer(x);
            }
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
            Mode::Edit { editor, error } => self.editor_key(editor, error, key),
            Mode::View { title, view } => self.viewer_key(title, view, key),
        }
    }

    fn help_key(&mut self, code: KeyCode) {
        let page = self.page;
        match code {
            KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
            KeyCode::Down => self.help_scroll += 1,
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(page),
            KeyCode::PageDown => self.help_scroll += page,
            KeyCode::Home => self.help_scroll = 0,
            KeyCode::End => self.help_scroll = usize::MAX,
            _ => self.help = false,
        }
    }

    fn normal_mode_key(&mut self, key: KeyEvent) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::F(4) if shift => self.new_file_prompt(),
            KeyCode::F(6) if shift => self.rename(),
            KeyCode::Enter if self.default_open().is_some() => {
                if let Some(path) = self.default_open() {
                    self.open_default(path);
                }
            }
            KeyCode::F(n @ 1..=2) if alt => {
                self.mode = Mode::Drives(hotlist::mounts(), 0, usize::from(n == 2));
            }
            KeyCode::Char('.') if alt => {
                if let Err(e) = self.panels[self.active].toggle_hidden() {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            KeyCode::Char('=') if alt => {
                let panel = &self.panels[self.active];
                let path = panel.path().to_owned();
                match panel.remote().cloned() {
                    Some(r) => {
                        if let Err(e) = self.panels[1 - self.active].open_remote(r, path) {
                            self.mode = Mode::Message(e.to_string());
                        }
                    }
                    None => self.go(1 - self.active, path),
                }
            }
            KeyCode::Char('c' | 'C') if alt && shift => self.copy_paths(),
            KeyCode::Up | KeyCode::Down if shift => {
                let panel = &mut self.panels[self.active];
                let c = panel.cursor();
                // select the entry left behind
                panel.select_range(c, c);
                panel.move_cursor(if key.code == KeyCode::Up { -1 } else { 1 });
            }
            KeyCode::F(7) if alt && self.panels[self.active].remote().is_some() => {
                self.mode = Mode::Message(REMOTE.into())
            }
            KeyCode::F(7) if alt => self.ask(Ask::FindMask),
            code if key.modifiers.contains(KeyModifiers::CONTROL) => self.ctrl_key(code),
            KeyCode::Left | KeyCode::Right if alt => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Err(e) = self.panels[self.active].history_step(delta) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            KeyCode::Esc => {
                let panel = &mut self.panels[self.active];
                if panel.cancel_load() {
                    return;
                }
                panel.apply_filter("");
                if let Err(e) = panel.leave_virtual() {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            code => self.normal_key(code),
        }
    }

    fn ctrl_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('s') => self.mode = Mode::Search(String::new()),
            KeyCode::Char('f') => {
                self.mode = Mode::Filter(self.panels[self.active].filter().to_owned());
            }
            KeyCode::Char('d') if self.panels[self.active].remote().is_some() => {
                self.mode = Mode::Message(REMOTE.into())
            }
            KeyCode::Char('d') => self.hotlist_add(),
            KeyCode::Char('n') => self.mode = Mode::Connections(0),
            KeyCode::Char('b') => self.mode = Mode::Hotlist(0),
            KeyCode::Char(' ') => self.spawn_stats(),
            KeyCode::Char('y') => {
                let [a, b] = &mut self.panels;
                a.compare_with(b);
            }
            KeyCode::Char('u') => {
                self.panels.swap(0, 1);
                self.tabs.swap(0, 1);
                self.tab.swap(0, 1);
                self.lists.swap(0, 1);
                if let Some(job) = &mut self.dir_job {
                    job.0 = 1 - job.0;
                }
            }
            KeyCode::Char('r') => {
                if let Err(e) = self.panels[self.active].reload() {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            // terminals send Ctrl+\ as 0x1c, which crossterm reports as '4'
            KeyCode::Char('\\' | '4') => {
                let panel = &mut self.panels[self.active];
                let root = panel.path().ancestors().last().map(Path::to_owned);
                let result = match root {
                    Some(root) if panel.remote().is_some() => panel.cd(root),
                    Some(root) => panel.go(root),
                    None => Ok(()),
                };
                if let Err(e) = result {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            KeyCode::Insert => self.copy_paths(),
            KeyCode::Char('t' | 'w')
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Tab
            | KeyCode::BackTab => self.tab_key(code),
            KeyCode::F(n @ 3..=6) => {
                let key = [SortKey::Name, SortKey::Ext, SortKey::Date, SortKey::Size];
                self.panels[self.active].set_sort(key[usize::from(n) - 3]);
            }
            _ => {}
        }
    }

    /// Tabs per side: `panels[side]` is the visible tab, rotated through the ring in `tabs[side]`.
    fn tab_key(&mut self, code: KeyCode) {
        let side = self.active;
        let bg = &mut self.tabs[side];
        let n = bg.len() + 1;
        let next = match code {
            KeyCode::Char('t') => match self.panels[side].duplicate() {
                Ok(p) => {
                    bg.push_back(mem::replace(&mut self.panels[side], p));
                    self.tab[side] += 1;
                    self.lists[side] = ListState::default();
                    return;
                }
                Err(e) => {
                    self.mode = Mode::Message(e.to_string());
                    return;
                }
            },
            KeyCode::Char('w') => {
                let Some(p) = bg.pop_front() else { return };
                self.panels[side] = p;
                self.tab[side] %= n - 1;
                None
            }
            KeyCode::PageDown | KeyCode::Tab => bg.pop_front().map(|p| {
                self.tab[side] = (self.tab[side] + 1) % n;
                (p, false)
            }),
            _ => bg.pop_back().map(|p| {
                self.tab[side] = (self.tab[side] + n - 1) % n;
                (p, true)
            }),
        };
        if let Some((p, prev)) = next {
            let old = mem::replace(&mut self.panels[side], p);
            if prev {
                self.tabs[side].push_front(old);
            } else {
                self.tabs[side].push_back(old);
            }
        } else if code != KeyCode::Char('w') {
            return;
        }
        self.lists[side] = ListState::default();
        // the directory may have changed while the tab was hidden
        if let Err(e) = self.panels[side].reload() {
            self.mode = Mode::Message(e.to_string());
        }
    }

    fn search_key(&mut self, mut text: String, key: KeyEvent) {
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

    fn filter_key(&mut self, mut text: String, key: KeyEvent) {
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

    fn editor_key(&mut self, mut editor: Editor, error: Option<String>, key: KeyEvent) {
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
                'c' | 'x' => {
                    let text = if c.eq_ignore_ascii_case(&'c') {
                        editor.copy()
                    } else {
                        editor.cut()
                    };
                    if let Some(text) = text {
                        self.clipboard.clone_from(&text);
                        self.copied = Some(text);
                    }
                }
                'v' => editor.insert(&self.clipboard, false),
                'k' => editor.delete_line(),
                _ => {}
            },
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => editor.insert_char(c),
            _ => {}
        }
        self.mode = Mode::Edit {
            editor,
            error: None,
        };
    }

    fn viewer_key(&mut self, title: String, mut view: Viewer, key: KeyEvent) {
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
            KeyCode::Char('w') => {
                view.wrap = !view.wrap;
                Ok(())
            }
            KeyCode::Char('l') => {
                self.view_numbers = !self.view_numbers;
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

    /// Remember the command and hand it to the main loop; empty does nothing.
    fn run_cmd(&mut self, text: &str) {
        let cmd = text.trim();
        if cmd.is_empty() {
            return;
        }
        if self.stats.is_none() && self.panels[self.active].archive().is_some() {
            self.mode = Mode::Message("No commands inside an archive.".into());
            return;
        }
        cmdline::push_history(&mut self.history, cmd);
        if let Some(file) = &self.history_file
            && let Err(e) = cmdline::save_history(file, &self.history)
        {
            self.mode = Mode::Message(e.to_string());
        }
        let remote = match &self.stats {
            Some(st) => st.remote.as_ref(),
            None => self.panels[self.active].remote(),
        };
        self.run = Some((
            cmd.to_owned(),
            self.cmd_dir(),
            remote.map(|r| r.url().to_owned()),
        ));
    }

    /// Where the command line runs: the browsed File Stats directory, else the active panel.
    pub fn cmd_dir(&self) -> PathBuf {
        self.stats
            .as_ref()
            .map_or_else(|| self.panels[self.active].path().to_owned(), Stats::path)
    }

    /// After a command ran: files may have changed in both panels.
    pub fn reload_all(&mut self) {
        for p in &mut self.panels {
            if let Err(e) = p.reload() {
                self.mode = Mode::Message(e.to_string());
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

    /// Ctrl+Space: File Stats of the active panel's directory.
    fn spawn_stats(&mut self) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message("File Stats does not work in archives.".into());
            return;
        }
        let dir = panel.path().to_owned();
        let progress = Arc::new(ops::Progress::default());
        let (p, d, r) = (progress.clone(), dir.clone(), panel.remote().cloned());
        self.mode = Mode::Stats(Stats {
            remote: r.clone(),
            deleting: None,
            dir,
            root: Node::default(),
            path: Vec::new(),
            sel: 0,
            by_files: false,
            job: Some(thread::spawn(move || match r {
                Some(r) => stats::scan_remote(r.as_ref(), &vfs::rpath(&d), &p),
                None => stats::scan(&d, &p),
            })),
            progress,
            confirm: None,
        });
    }

    /// Keys in File Stats; only Esc/q/F10 (cancel) work while scanning.
    fn stats_mode_key(&mut self, mut st: Stats, key: KeyEvent) {
        if st.confirm.is_some() {
            return self.stats_confirm_key(st, key.code);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(10) => {
                st.progress.cancel.store(true, Ordering::Relaxed);
                return;
            }
            _ if st.scanning() => {}
            KeyCode::F(3 | 4) => return self.stats_open(st, key),
            KeyCode::F(8) | KeyCode::Char('d') => {
                st.confirm = st.current().children.get(st.sel).map(|n| {
                    let mut text = format!("Delete {}?", n.name.to_string_lossy());
                    if n.is_dir {
                        text.push_str("\nDirectories are deleted recursively.");
                    }
                    if st.remote.is_some() {
                        return text
                            + "\n\nNo trash on remote panels.\n[p] delete permanently   [n] no";
                    }
                    text + "\n\n[y] trash   [p] delete permanently   [n] no"
                });
            }
            KeyCode::Char(':') => {
                self.stats = Some(st);
                self.mode = Mode::Cmd(LineEdit::default());
                return;
            }
            code => st.key(code, self.page),
        }
        self.mode = Mode::Stats(st);
    }

    /// Answer to the File Stats delete question: the entry leaves the tree without a rescan.
    fn stats_confirm_key(&mut self, mut st: Stats, code: KeyCode) {
        let to_trash = match code {
            // no trash on remote panels
            KeyCode::Char('y' | 'Y') | KeyCode::Enter if st.remote.is_none() => true,
            KeyCode::Char('p' | 'P') => false,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                st.confirm = None;
                self.mode = Mode::Stats(st);
                return;
            }
            _ => {
                self.mode = Mode::Stats(st);
                return;
            }
        };
        st.confirm = None;
        let Some(name) = st.current().children.get(st.sel).map(|n| n.name.clone()) else {
            self.mode = Mode::Stats(st);
            return;
        };
        let target = st.path().join(name);
        if let Some(r) = st.remote.clone() {
            st.deleting = Some(st.sel);
            self.stats = Some(st);
            let side = Side::Remote(r);
            return self.spawn_xfer((side.clone(), side.clone()), move |_, _| {
                side.delete(&target)
            });
        }
        let result = if to_trash {
            ops::trash(&target)
        } else {
            ops::delete(&target)
        };
        if result.is_ok() {
            stats::remove(&mut st.root, &st.path, st.sel);
            st.sel = st.sel.min(st.current().children.len().saturating_sub(1));
        }
        // `tick` brings the stats back once any error message is closed
        self.stats = Some(st);
        self.finish(result.err().map(|e| e.to_string()));
    }

    /// F3 view, F4 edit, Shift+F4 external editor on the selected file of File Stats.
    fn stats_open(&mut self, st: Stats, key: KeyEvent) {
        let dir = st.current();
        let Some(path) = dir
            .children
            .get(st.sel)
            .filter(|n| !n.is_dir)
            .map(|n| (st.path().join(&n.name), n.size))
        else {
            self.mode = Mode::Stats(st);
            return;
        };
        let (path, size) = path;
        if let Some(r) = st.remote.clone() {
            self.stats = Some(st);
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                self.mode = Mode::Message(REMOTE.into());
                return;
            }
            return self.fetch(r, path, size, key.code == KeyCode::F(4));
        }
        self.stats = Some(st);
        match key.code {
            KeyCode::F(3) => self.open_viewer(path),
            _ if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.external_edit(path, "code".into());
            }
            _ => self.edit_path(path),
        }
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
            text: LineEdit::default(),
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
            _ => {
                if edit_key(&mut input.text, key) {
                    input.error = None;
                }
            }
        }
        self.input = Some(input);
    }

    /// `Ok(true)` when done, `Ok(false)` when the prompt goes on with its next step.
    fn submit(&mut self, input: &mut Input) -> Result<bool, String> {
        let text = input.text.text.clone();
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
            KeyCode::Insert => panel.toggle_select(),
            KeyCode::Char(' ') => {
                let dir = panel
                    .current()
                    .filter(|e| e.is_dir && !e.is_parent() && panel.archive().is_none())
                    .filter(|_| panel.remote().is_none())
                    .map(|e| panel.path().join(&e.name));
                panel.toggle_select();
                if let Some(dir) = dir {
                    self.calc_dir_size(dir);
                }
            }
            KeyCode::Char('+') => self.ask(Ask::Select),
            KeyCode::Char('-') => self.ask(Ask::Deselect),
            KeyCode::Char('*') => panel.invert_selection(),
            KeyCode::Char(':') => self.mode = Mode::Cmd(LineEdit::default()),
            KeyCode::Char('~') => {
                if let Some(home) = std::env::home_dir() {
                    self.go(self.active, home);
                }
            }
            KeyCode::Enter | KeyCode::Backspace => {
                let archive = panel
                    .current()
                    .filter(|e| code == KeyCode::Enter && !e.is_dir && panel.archive().is_none())
                    .filter(|_| panel.remote().is_none())
                    .filter(|e| Format::detect(Path::new(&e.name)).is_some())
                    .map(|e| panel.path().join(&e.name));
                if let Some(file) = archive {
                    return self.spawn_archive(file);
                }
                let result = if code == KeyCode::Enter {
                    panel.enter().map(drop)
                } else {
                    panel.parent()
                };
                if let Err(e) = result {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            KeyCode::F(3 | 4) if panel.archive().is_some() => self.view_archive_entry(),
            KeyCode::F(3 | 4) if panel.remote().is_some() => {
                self.fetch_remote(code == KeyCode::F(4))
            }
            KeyCode::F(7) if panel.archive().is_some() => {
                self.mode = Mode::Message(READ_ONLY.into())
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
            KeyCode::F(7) => self.mode = Mode::Mkdir(LineEdit::default()),
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

    /// Archives are only a source: nothing is written into them, nothing is removed from them.
    fn confirm(&mut self, op: Op) {
        let in_archive = |i: usize| self.panels[i].archive().is_some();
        if (in_archive(self.active) && op != Op::Copy)
            || (in_archive(1 - self.active) && op != Op::Delete)
        {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        let targets = self.panels[self.active].targets();
        match targets.as_slice() {
            [] => {}
            _ if op == Op::Delete => {
                let text = self.delete_question();
                self.mode = Mode::Confirm(op, targets, text);
            }
            [one] => {
                let name = one.file_name().unwrap_or_default();
                let text = self.other_dir().join(name).display().to_string();
                self.mode = Mode::Transfer(op, targets, LineEdit::from(text));
            }
            _ => {
                let text = self.other_dir().display().to_string();
                self.mode = Mode::Transfer(op, targets, LineEdit::from(text));
            }
        }
    }

    /// "Delete a, b, c…?" from the entries in memory, no I/O.
    fn delete_question(&self) -> String {
        let entries = self.panels[self.active].target_entries();
        let names: Vec<_> = entries
            .iter()
            .take(3)
            .map(|e| e.name.to_string_lossy())
            .collect();
        let more = if entries.len() > 3 { "…" } else { "" };
        let mut text = match entries.len() {
            1 => format!("Delete {}?", names[0]),
            n => format!("Delete {n} items: {}{more}?", names.join(", ")),
        };
        // a symlink is removed as a link, not recursed into
        if entries.iter().any(|e| e.is_dir && !e.is_symlink) {
            text.push_str("\nDirectories are deleted recursively.");
        }
        if self.panels[self.active].remote().is_some() {
            return text + "\n\nNo trash on remote panels.\n[p] delete permanently   [n] no";
        }
        text + "\n\n[y] trash   [p] delete permanently   [n] no"
    }

    fn delete(&mut self, targets: &[PathBuf], to_trash: bool) {
        let side = self.panels[self.active].side();
        if side.remote().is_some() {
            let targets = targets.to_vec();
            let sides = (side.clone(), side.clone());
            return self.spawn_xfer(sides, move |_, _| {
                targets.iter().try_for_each(|t| side.delete(t))
            });
        }
        let result = targets.iter().try_for_each(|src| {
            if to_trash {
                ops::trash(src)
            } else {
                ops::delete(src)
            }
        });
        self.finish(result.err().map(|e| e.to_string()));
    }

    /// Space on a directory: sum its size in a worker thread (a newer request cancels the older one).
    fn calc_dir_size(&mut self, dir: PathBuf) {
        if let Some((_, _, old)) = self.dir_job.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let progress = Arc::new(ops::Progress::default());
        let (p, d) = (progress.clone(), dir.clone());
        thread::spawn(move || ops::scan(&[d], &p));
        self.dir_job = Some((self.active, dir, progress));
    }

    fn go(&mut self, i: usize, path: PathBuf) {
        if let Err(e) = self.panels[i].go(path) {
            self.mode = Mode::Message(e.to_string());
        }
    }

    fn drives_key(&mut self, code: KeyCode, rows: Vec<PathBuf>, sel: usize, panel: usize) {
        let last = rows.len().saturating_sub(1);
        let mut sel = sel.min(last);
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Up => sel = sel.saturating_sub(1),
            KeyCode::Down => sel = (sel + 1).min(last),
            KeyCode::Enter if !rows.is_empty() => return self.go(panel, rows[sel].clone()),
            _ => {}
        }
        self.mode = Mode::Drives(rows, sel, panel);
    }

    /// Shift+F6: rename the cursor entry through the move prompt (bare name = same directory).
    fn rename(&mut self) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        let Some(entry) = panel.current().filter(|e| !e.is_parent()) else {
            return;
        };
        let name = entry.name.to_string_lossy().into_owned();
        let path = panel.path().join(&entry.name);
        self.mode = Mode::Transfer(Op::Move, vec![path], LineEdit::from(name));
    }

    /// Ctrl+Insert: paths of the selection or cursor entry to the system clipboard.
    fn copy_paths(&mut self) {
        let panel = &self.panels[self.active];
        let paths = panel.targets();
        if !paths.is_empty() {
            let url = panel.remote().map_or("", |r| r.url());
            let lines: Vec<_> = paths
                .iter()
                .map(|p| format!("{url}{}", p.to_string_lossy()))
                .collect();
            self.copied = Some(lines.join("\n"));
        }
    }

    /// Regular file under the cursor that Enter hands to the desktop (archives open in the panel).
    fn default_open(&self) -> Option<PathBuf> {
        self.current_file().filter(|p| Format::detect(p).is_none())
    }

    /// Start the default application detached; `xdg-open` and friends return at once.
    fn open_default(&mut self, path: PathBuf) {
        use std::process::Stdio;
        let spawned = cmdline::open_command(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(e) = spawned {
            self.mode = Mode::Message(format!("{}: {e}", path.display()));
        }
    }

    /// Shift+F4: ask for a file name, prefilled with the cursor file.
    fn new_file_prompt(&mut self) {
        if self.panels[self.active].archive().is_some() {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        if self.panels[self.active].remote().is_some() {
            self.mode = Mode::Message(REMOTE.into());
            return;
        }
        let name = self
            .current_file()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        self.mode = Mode::NewFile(LineEdit::from(name));
    }

    /// Create `name` unless it exists (never truncates), then edit it externally.
    fn new_file(&mut self, name: &str) {
        let dir = self.panels[self.active].path().to_owned();
        let path = dir.join(name);
        if name.is_empty() || path.is_dir() {
            return;
        }
        if std::fs::symlink_metadata(&path).is_err() {
            if let Err(e) = ops::touch(&dir, &OsString::from(name)) {
                self.mode = Mode::Message(e.to_string());
                return;
            }
            self.finish(None);
            self.panels[self.active].focus(OsStr::new(name));
        }
        self.external_edit(path, "code".into());
    }

    /// Text pasted by the terminal (bracketed paste): into the editor or the open prompt.
    pub fn on_paste(&mut self, text: String) {
        if let Some(input) = &mut self.input {
            return paste_line(&mut input.text, &text);
        }
        match &mut self.mode {
            Mode::Edit {
                editor,
                error: None,
            } => editor.insert(&text.replace("\r\n", "\n"), false),
            Mode::Cmd(l)
            | Mode::Transfer(_, _, l)
            | Mode::Mkdir(l)
            | Mode::NewFile(l)
            | Mode::ConnEdit(_, l) => paste_line(l, &text),
            Mode::Connecting(Connecting {
                ask: Some((Prompt::Password(_), l)),
                ..
            }) => paste_line(l, &text),
            Mode::Editor(_, l) => paste_line(l, &text),
            Mode::Xfer(Xfer {
                rename: Some(l), ..
            }) => paste_line(l, &text),
            _ => {}
        }
    }

    /// Copy or move to `text`. A relative `text` is in the active panel (rename), an absolute
    /// one on the other panel's side (local disk or its remote). A single item goes to exactly
    /// that path; several items, or a trailing separator, mean a directory.
    /// Runs in a worker; conflicts are put to the user through `Mode::Xfer`.
    fn run(&mut self, op: Op, targets: &[PathBuf], text: &str) {
        let active = &self.panels[self.active];
        let to = if Path::new(text).has_root() {
            &self.panels[1 - self.active]
        } else {
            active
        };
        let (dst_side, dest) = (to.side(), to.path().join(text));
        let into_dir = targets.len() > 1 || text.ends_with(['/', std::path::MAIN_SEPARATOR]);
        let jobs: Vec<(PathBuf, PathBuf)> = targets
            .iter()
            .map(|src| match src.file_name() {
                Some(name) if into_dir => (src.clone(), dest.join(name)),
                _ => (src.clone(), dest.clone()),
            })
            .collect();
        let arch = active.archive().cloned();
        // archive entries are extracted to local temp files first
        let src_side = if arch.is_some() {
            Side::Local
        } else {
            active.side()
        };
        let sides = (src_side.clone(), dst_side.clone());
        self.spawn_xfer(sides, move |c, ask| {
            // shortcut: needs temp space for the whole selection, upgrade to copy straight from the stream
            let (jobs, _tmp) = match arch {
                None => (jobs, None),
                Some(a) => {
                    let srcs: Vec<PathBuf> = jobs.iter().map(|(s, _)| s.clone()).collect();
                    let (tmp, out) = a.extract_all(&srcs, &c.cancel)?;
                    let targets = jobs.into_iter().map(|(_, t)| t);
                    (out.into_iter().zip(targets).collect(), Some(tmp))
                }
            };
            let srcs: Vec<PathBuf> = jobs.iter().map(|(s, _)| s.clone()).collect();
            vfs::scan(&src_side, &srcs, c);
            let local = matches!((&src_side, &dst_side), (Side::Local, Side::Local));
            let mv = op == Op::Move;
            // Stops at the first error so the user sees it before anything else happens.
            jobs.iter().try_for_each(|(src, target)| {
                if !local {
                    vfs::transfer(&src_side, src, &dst_side, target, mv, ask, c)
                } else if mv {
                    ops::move_(src, target, ask, c)
                } else {
                    ops::copy(src, target, ask, c)
                }
            })
        });
    }

    /// Run `work` in a worker with progress and the conflict dialog (`sides` decide "newer").
    fn spawn_xfer(
        &mut self,
        sides: (Side, Side),
        work: impl FnOnce(&ops::Progress, ops::Ask) -> tc_core::Result<()> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let (reply, answers) = mpsc::channel();
        let progress = Arc::new(ops::Progress::default());
        let c = progress.clone();
        thread::spawn(move || {
            let mut sticky: Option<Policy> = None;
            let (from, to) = sides;
            let mut ask = |src: &Path, target: &Path| {
                let policy = match sticky {
                    Some(p) => p,
                    None => {
                        let msg = XMsg::Ask(src.to_owned(), target.to_owned());
                        if tx.send(msg).is_err() {
                            return ops::Decision::Cancel;
                        }
                        match answers.recv() {
                            Ok(Answer::One(p)) => p,
                            Ok(Answer::All(p)) => *sticky.insert(p),
                            Ok(Answer::Rename(n)) => return ops::Decision::Rename(n.into()),
                            Ok(Answer::Cancel) | Err(_) => return ops::Decision::Cancel,
                        }
                    }
                };
                match policy {
                    Policy::Overwrite => ops::Decision::Overwrite,
                    Policy::Newer if vfs::is_newer(&from, src, &to, target) => {
                        ops::Decision::Overwrite
                    }
                    Policy::Newer | Policy::Skip => ops::Decision::Skip,
                }
            };
            let result = work(&c, &mut ask);
            let error = result.err().filter(|e| !matches!(e, Error::Cancelled));
            let _ = tx.send(XMsg::Done(error.map(|e| e.to_string())));
        });
        self.mode = Mode::Xfer(Xfer {
            ask: None,
            rename: None,
            rx,
            reply,
            progress,
            eta: Estimator::default(),
            started: Instant::now(),
        });
    }

    fn mkdir(&mut self, name: &str) {
        let panel = &self.panels[self.active];
        let side = panel.side();
        if side.remote().is_some() {
            let (dir, name) = (panel.path().to_owned(), OsString::from(name));
            let sides = (side.clone(), side.clone());
            return self.spawn_xfer(sides, move |_, _| side.mkdir_in(&dir, &name).map(drop));
        }
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
        let entry = panel
            .current()
            .filter(|e| !e.is_dir && panel.archive().is_none() && panel.remote().is_none())?;
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
                self.mode = Mode::Editor(path, LineEdit::default())
            }
            Err(e) => self.mode = Mode::Message(format!("{cmd}: {e}")),
        }
    }

    /// List an archive in a worker thread; `tick` opens it in the active panel.
    fn spawn_archive(&mut self, file: PathBuf) {
        let total = std::fs::metadata(&file).map_or(1, |m| m.len());
        let f = file.clone();
        self.spawn("Listing", file, total, false, move |progress, cancel| {
            Done::Listed(Archive::open(&f, progress, cancel))
        });
    }

    /// F3/F4 inside an archive: extract the entry to a temp file (worker thread), then view it.
    fn view_archive_entry(&mut self) {
        let panel = &self.panels[self.active];
        let (Some(arch), Some(entry)) = (panel.archive().cloned(), panel.current()) else {
            return;
        };
        let fake = panel.path().join(&entry.name);
        let Some(inner) = arch
            .inner_of(&fake)
            .filter(|_| !entry.is_dir && !entry.is_parent())
        else {
            return;
        };
        if entry.size > VIEW_CAP {
            self.mode = Mode::Message("Entry over 1 GiB: copy it out with F5 instead.".into());
            return;
        }
        let total = entry.size;
        self.spawn("Extracting", fake, total, false, move |progress, cancel| {
            Done::Extracted(arch.extract_temp(&inner, VIEW_CAP, progress, cancel))
        });
    }

    /// F3/F4 on a remote panel: download the file to a temp copy (worker), then view or edit it.
    fn fetch_remote(&mut self, edit: bool) {
        let panel = &self.panels[self.active];
        let (Some(r), Some(e)) = (panel.remote().cloned(), panel.current()) else {
            return;
        };
        if e.is_dir {
            return;
        }
        let (r, path, size) = (r, panel.path().join(&e.name), e.size);
        self.fetch(r, path, size, edit);
    }

    fn fetch(&mut self, r: Arc<dyn Remote>, path: PathBuf, size: u64, edit: bool) {
        if size > VIEW_CAP {
            self.mode = Mode::Message("File over 1 GiB: copy it with F5 instead.".into());
            return;
        }
        let title = PathBuf::from(format!("{}{}", r.url(), vfs::rpath(&path)));
        let total = size;
        self.spawn(
            "Downloading",
            title,
            total,
            false,
            move |progress, cancel| {
                let res = vfs::download_temp(&r, &path, VIEW_CAP, progress, cancel);
                if edit {
                    Done::Fetched(r, path, res)
                } else {
                    Done::Extracted(res)
                }
            },
        );
    }

    fn connections_save(&mut self) {
        if let Some(file) = &self.connections_file
            && let Err(e) = hotlist::save(file, &self.connections)
        {
            self.mode = Mode::Message(e.to_string());
        }
    }

    fn connections_key(&mut self, code: KeyCode, sel: usize) {
        let last = self.connections.len().saturating_sub(1);
        let mut sel = sel.min(last);
        let url = self
            .connections
            .get(sel)
            .map(|u| u.to_string_lossy().into_owned());
        match (code, url) {
            (KeyCode::Esc | KeyCode::Char('q'), _) => return,
            (KeyCode::Up, _) => sel = sel.saturating_sub(1),
            (KeyCode::Down, _) => sel = (sel + 1).min(last),
            (KeyCode::Enter, Some(url)) => return self.connect(url),
            (KeyCode::Insert | KeyCode::Char('a'), _) => {
                self.mode = Mode::ConnEdit(None, LineEdit::from("sftp://".to_owned()));
                return;
            }
            (KeyCode::F(4) | KeyCode::Char('e'), Some(url)) => {
                self.mode = Mode::ConnEdit(Some(sel), LineEdit::from(url));
                return;
            }
            (KeyCode::Delete, Some(_)) => {
                self.connections.remove(sel);
                self.connections_save();
                if matches!(self.mode, Mode::Message(_)) {
                    return;
                }
            }
            _ => {}
        }
        self.mode = Mode::Connections(sel);
    }

    /// Store an added or edited URL (no passwords: they are asked on connect).
    fn conn_save(&mut self, row: Option<usize>, text: &str) {
        let text = text.trim();
        if let Err(e) = tc_remote::Url::parse(text) {
            self.mode = Mode::Message(e.to_string());
            return;
        }
        let url = PathBuf::from(text);
        let sel = match row.filter(|&i| i < self.connections.len()) {
            Some(i) => {
                self.connections[i] = url;
                i
            }
            None => {
                self.connections.push(url);
                self.connections.len() - 1
            }
        };
        self.mode = Mode::Connections(sel);
        self.connections_save();
    }

    /// Connect in a worker; questions (host key, password) come back through `tick`.
    fn connect(&mut self, url: String) {
        let (tx, rx) = mpsc::channel();
        let (reply, answers) = mpsc::channel::<Option<String>>();
        let u = url.clone();
        thread::spawn(move || {
            let mut ask = |p: Prompt| {
                tx.send(CMsg::Ask(p)).ok()?;
                answers.recv().ok().flatten()
            };
            let result = tc_remote::connect(&u, &mut ask);
            let _ = tx.send(CMsg::Done(result));
        });
        self.mode = Mode::Connecting(Connecting {
            url,
            ask: None,
            rx,
            reply,
        });
    }

    /// Host key: `y` trusts, `n`/Esc cancels; password: typed, Enter sends. Esc while waiting
    /// abandons the attempt (the worker finishes on its own).
    fn connecting_key(&mut self, mut c: Connecting, key: KeyEvent) {
        let answer = match (&mut c.ask, key.code) {
            (None, KeyCode::Esc) => return,
            (Some(_), KeyCode::Esc) | (Some((Prompt::HostKey(_), _)), KeyCode::Char('n')) => {
                Some(None)
            }
            (Some((Prompt::HostKey(_), _)), KeyCode::Char('y')) => Some(Some("y".to_owned())),
            (Some((Prompt::Password(_), l)), KeyCode::Enter) => Some(Some(mem::take(&mut l.text))),
            (Some((Prompt::Password(_), l)), _) => {
                edit_key(l, key);
                None
            }
            _ => None,
        };
        if let Some(a) = answer {
            c.ask = None;
            let _ = c.reply.send(a);
        }
        self.mode = Mode::Connecting(c);
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
        if let Some(path) = self.current_file() {
            self.edit_path(path);
        }
    }

    fn edit_path(&mut self, path: PathBuf) {
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
        let upload = self
            .edit_remote
            .as_ref()
            .map(|(r, p, _)| (r.clone(), p.clone()));
        self.spawn("Saving", path, total, exit, move |progress, cancel| {
            let mut result = editor.save(to_utf8, to_lf, progress, cancel);
            if let (Ok(()), Some((r, target))) = (&result, upload) {
                // replaces the remote file via temp name + rename, like an overwrite in F5
                let p = ops::Progress::default();
                let mut over = |_: &Path, _: &Path| ops::Decision::Overwrite;
                let (src, dst) = (Side::Local, Side::Remote(r));
                result = vfs::transfer(&src, editor.path(), &dst, &target, false, &mut over, &p);
            }
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

    fn connect_tick(&mut self) {
        let Mode::Connecting(c) = &mut self.mode else {
            return;
        };
        match c.rx.try_recv() {
            Ok(CMsg::Ask(p)) => c.ask = Some((p, LineEdit::default())),
            Err(TryRecvError::Empty) => {}
            Ok(CMsg::Done(Ok((remote, dir)))) => {
                self.mode = Mode::Normal;
                if let Err(e) = self.panels[self.active].open_remote(remote, dir.into()) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            Ok(CMsg::Done(Err(Error::Cancelled))) => self.mode = Mode::Normal,
            Ok(CMsg::Done(Err(e))) => self.mode = Mode::Message(format!("{}: {e}", c.url)),
            Err(TryRecvError::Disconnected) => {
                self.mode = Mode::Message("connect worker failed".into())
            }
        }
    }

    /// Collect a finished worker result.
    pub fn tick(&mut self) {
        // dialogs keep a pending listing waiting so its error cannot replace them
        if matches!(self.mode, Mode::Normal) {
            for p in &mut self.panels {
                if let Some(Err(e)) = p.poll() {
                    self.mode = Mode::Message(e.to_string());
                }
            }
        }
        if matches!(self.mode, Mode::Normal)
            && let Some(st) = self.stats.take()
        {
            self.mode = Mode::Stats(st);
        }
        if let Some((side, dir, p)) = &self.dir_job
            && p.scanned.load(Ordering::Acquire)
        {
            let (side, dir, bytes) = (*side, dir.clone(), p.total.load(Ordering::Relaxed));
            self.dir_job = None;
            if let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) {
                // only the panel Space was pressed in, the other one may show the same directory
                let panel = &mut self.panels[side];
                if panel.path() == parent && !panel.is_virtual() && panel.archive().is_none() {
                    panel.set_dir_size(name.to_owned(), bytes);
                }
            }
        }
        if self.view_tmp.is_some() && !matches!(self.mode, Mode::View { .. }) {
            self.view_tmp = None;
        }
        // File Stats comes back right after the editor closes, so it ends the remote edit too
        if self.edit_remote.is_some() && matches!(self.mode, Mode::Normal | Mode::Stats(_)) {
            self.edit_remote = None;
        }
        if matches!(self.mode, Mode::Connecting(_)) {
            return self.connect_tick();
        }
        if let Mode::Found(f) = &mut self.mode {
            return f.pump();
        }
        if let Mode::Stats(st) = &mut self.mode
            && st
                .job
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
            && let Some(job) = st.job.take()
        {
            match job.join() {
                Ok(root) => st.root = root,
                Err(_) => self.mode = Mode::Message("File Stats scan failed.".into()),
            }
            return;
        }
        if let Mode::Xfer(x) = &mut self.mode {
            let done = x.progress.done.load(Ordering::Relaxed);
            x.eta.push(x.started.elapsed().as_secs_f64(), done as f64);
            match x.rx.try_recv() {
                Ok(XMsg::Ask(src, target)) => x.ask = Some((src, target)),
                Ok(XMsg::Done(error)) => {
                    self.mode = Mode::Normal;
                    if let Some(st) = &mut self.stats
                        && let Some(i) = st.deleting.take()
                        && error.is_none()
                    {
                        stats::remove(&mut st.root, &st.path, i);
                        st.sel = st.sel.min(st.current().children.len().saturating_sub(1));
                    }
                    self.finish(error);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.mode = Mode::Message("worker thread failed".into())
                }
            }
            return;
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
            Done::Listed(Ok(archive)) => self.panels[self.active].open_archive(archive),
            Done::Fetched(r, remote, Ok((tmp, file))) => {
                self.edit_remote = Some((r, remote, tmp));
                self.edit_path(file);
            }
            Done::Fetched(_, _, Err(Error::Cancelled)) => {}
            Done::Fetched(_, remote, Err(e)) => {
                self.mode = Mode::Message(format!("{}: {e}", remote.display()))
            }
            Done::Extracted(Ok((tmp, file))) => match Viewer::open(&file) {
                Ok(view) => {
                    self.view_tmp = Some(tmp);
                    self.mode = Mode::View {
                        title: path.display().to_string(),
                        view,
                    };
                }
                Err(e) => self.mode = Mode::Message(format!("{}: {e}", path.display())),
            },
            Done::Listed(Err(Error::Cancelled)) | Done::Extracted(Err(Error::Cancelled)) => {}
            Done::Listed(Err(e)) | Done::Extracted(Err(e)) => {
                self.mode = Mode::Message(format!("{}: {e}", path.display()))
            }
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

    fn two_panels(l: &Path, r: &Path) -> App {
        App::new(Panel::open(l).unwrap(), Panel::open(r).unwrap())
    }

    fn with(app: &mut App, code: KeyCode, mods: KeyModifiers) {
        app.on_key(KeyEvent::new(code, mods));
    }

    #[test]
    fn tabs_open_cycle_close_and_swap() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        let ctrl = KeyModifiers::CONTROL;
        with(&mut app, KeyCode::Char('t'), ctrl);
        assert_eq!((app.tabs[0].len(), app.tab[0]), (1, 1));
        app.go(0, sub.clone());
        with(&mut app, KeyCode::PageDown, ctrl);
        assert_eq!((app.panels[0].path(), app.tab[0]), (dir.path(), 0));
        with(&mut app, KeyCode::PageUp, ctrl);
        assert_eq!((app.panels[0].path(), app.tab[0]), (sub.as_path(), 1));
        with(&mut app, KeyCode::Char('u'), ctrl);
        assert_eq!(
            (app.tabs[1].len(), app.tab[1], app.tabs[0].len()),
            (1, 1, 0)
        );
        key(&mut app, KeyCode::Tab);
        with(&mut app, KeyCode::Char('w'), ctrl);
        assert_eq!((app.panels[1].path(), app.tab[1]), (dir.path(), 0));
        assert!(app.tabs[1].is_empty());
        with(&mut app, KeyCode::Char('w'), ctrl); // last tab stays
        assert_eq!(app.panels[1].path(), dir.path());
    }

    #[test]
    fn delete_confirm_text_trash_choice_and_permanent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        for n in ["a", "b", "c"] {
            std::fs::write(dir.path().join(n), "").unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        for c in "+".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        type_text(&mut app, ""); // select all files
        key(&mut app, KeyCode::Down); // "d"
        key(&mut app, KeyCode::Char(' ')); // select the dir too
        key(&mut app, KeyCode::F(8));
        let Mode::Confirm(_, _, text) = &app.mode else {
            panic!("no confirm")
        };
        assert!(text.contains("recursively") && text.contains('…'));
        key(&mut app, KeyCode::Esc);
        assert!(dir.path().join("a").exists() && dir.path().join("d").exists());
        key(&mut app, KeyCode::F(8));
        key(&mut app, KeyCode::Char('p'));
        assert!(!dir.path().join("a").exists() && !dir.path().join("d").exists());
    }

    #[test]
    fn prompt_edits_at_cursor_and_rename_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::F(7));
        for c in "ac".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Char('b'));
        key(&mut app, KeyCode::Enter);
        assert!(dir.path().join("abc").is_dir());
        key(&mut app, KeyCode::Down);
        with(&mut app, KeyCode::F(6), KeyModifiers::SHIFT);
        with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        type_text(&mut app, "xyz");
        wait(&mut app);
        assert!(dir.path().join("xyz").is_dir() && !dir.path().join("abc").exists());
    }

    #[test]
    fn swap_root_sort_click_and_hidden() {
        let (l, r) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(l.path().join(".h"), "").unwrap();
        let mut app = two_panels(l.path(), r.path());
        app.areas = [Rect::new(0, 0, 40, 10), Rect::new(40, 0, 40, 10)];
        with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(app.panels[0].path(), r.path());
        with(&mut app, KeyCode::Char('='), KeyModifiers::ALT); // right gets left's dir
        assert_eq!(app.panels[1].path(), r.path());
        with(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL);
        settle(&mut app); // listing may take over the 50 ms wait on a loaded machine
        assert!(app.panels[0].path().parent().is_none());
        with(&mut app, KeyCode::Char('.'), KeyModifiers::ALT);
        assert!(!app.panels[0].shows_hidden());
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40 + 20,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.panels[1].sort().0, SortKey::Size);
        with(&mut app, KeyCode::F(5), KeyModifiers::CONTROL); // active panel is 1 now: by date
        assert_eq!(app.panels[1].sort().0, SortKey::Date);
    }

    #[test]
    fn clipboard_and_paste() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        with(&mut app, KeyCode::Insert, KeyModifiers::CONTROL);
        assert_eq!(
            app.copied.take(),
            Some(dir.path().join("a.txt").display().to_string())
        );
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        with(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.copied.as_deref(), Some("o"));
        app.on_paste("a\r\nb".into());
        assert_eq!(rows(&mut app)[..2], ["a", "bne"]);
    }

    #[test]
    fn space_on_dir_calculates_size() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("d/e")).unwrap();
        std::fs::write(dir.path().join("d/a"), "123").unwrap();
        std::fs::write(dir.path().join("d/e/b"), "45").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // "d"
        key(&mut app, KeyCode::Char(' '));
        while app.dir_job.is_some() {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(app.panels[0].dir_size(&app.panels[0].entries()[1]), Some(5));
        assert_eq!(app.panels[0].selection_info(), (1, 5));
        // the right panel shows the same directory but was not asked
        assert_eq!(app.panels[1].dir_size(&app.panels[1].entries()[1]), None);
    }

    #[test]
    fn stats_deletes_without_rescan_and_runs_cmd_in_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("many")).unwrap();
        std::fs::write(tmp.path().join("many").join("a"), "x").unwrap();
        std::fs::write(tmp.path().join("big"), "x".repeat(100)).unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        key(&mut app, KeyCode::F(8));
        key(&mut app, KeyCode::Char('n'));
        assert!(tmp.path().join("big").exists());
        key(&mut app, KeyCode::F(8));
        key(&mut app, KeyCode::Char('p'));
        app.tick();
        assert!(!tmp.path().join("big").exists());
        match &app.mode {
            Mode::Stats(st) => {
                assert_eq!((st.root.size, st.root.files), (1, 1));
                assert_eq!(st.current().children[0].name, "many");
            }
            _ => panic!("not in stats"),
        }
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char(':'));
        type_text(&mut app, "ls");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.run, Some(("ls".into(), tmp.path().join("many"), None)));
        app.tick();
        assert!(matches!(app.mode, Mode::Stats(_)));
    }

    #[test]
    fn stats_sorts_toggles_count_and_browses() {
        let tmp = tempfile::tempdir().unwrap();
        let many = tmp.path().join("many");
        std::fs::create_dir(&many).unwrap();
        for n in ["a", "b", "c"] {
            std::fs::write(many.join(n), "x").unwrap();
        }
        std::fs::write(tmp.path().join("big"), "x".repeat(100)).unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        let top = |app: &App| match &app.mode {
            Mode::Stats(st) => st.current().children[st.sel].name.clone(),
            _ => panic!("not in stats"),
        };
        assert_eq!(top(&app), "big");
        key(&mut app, KeyCode::Tab);
        assert_eq!(top(&app), "many");
        key(&mut app, KeyCode::Enter);
        assert_eq!(top(&app), "a");
        key(&mut app, KeyCode::Backspace);
        assert_eq!(top(&app), "many");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::F(3));
        assert!(matches!(app.mode, Mode::View { .. }));
        key(&mut app, KeyCode::Esc);
        app.tick();
        assert_eq!(top(&app), "a"); // back in stats after the viewer
        key(&mut app, KeyCode::F(10));
        assert!(matches!(app.mode, Mode::Normal) && !app.quit);
    }

    #[test]
    fn f1_opens_help_any_key_closes() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(1));
        assert!(app.help);
        key(&mut app, KeyCode::Char('x'));
        assert!(!app.help && matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn command_line_runs_only_on_enter() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        for c in ":ls".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert!(app.run.is_none() && !app.quit);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.run, Some(("ls".into(), tmp.path().to_owned(), None)));
        assert_eq!(app.history, ["ls"]);
        key(&mut app, KeyCode::Char(':'));
        key(&mut app, KeyCode::Up);
        assert!(matches!(&app.mode, Mode::Cmd(l) if l.text == "ls"));
        key(&mut app, KeyCode::Esc);
        app.run = None;
        key(&mut app, KeyCode::Char(':'));
        key(&mut app, KeyCode::Enter);
        assert!(app.run.is_none());
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
    fn conflict_dialog_overwrite_all_and_rename() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(left.path().join(f), "new").unwrap();
            std::fs::write(right.path().join(f), "old").unwrap();
        }
        let mut app = App::new(
            Panel::open(left.path()).unwrap(),
            Panel::open(right.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        key(&mut app, KeyCode::Char('r'));
        key(&mut app, KeyCode::Char('2'));
        key(&mut app, KeyCode::Enter); // "a2"
        wait(&mut app);
        key(&mut app, KeyCode::Char('O')); // overwrite all
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        let read = |f| std::fs::read_to_string(right.path().join(f)).unwrap();
        assert_eq!(
            (read("a"), read("a2"), read("b")),
            ("old".into(), "new".into(), "new".into())
        );
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
        wait(&mut app);
        assert!(right.path().join("new").is_dir());
        assert!(matches!(app.mode, Mode::Normal));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Normal)); // existing dir is merged, no conflict
        // copy in the current dir under a new name (relative to the active panel)
        key(&mut app, KeyCode::F(5));
        for _ in 0..right.path().display().to_string().len() + 5 {
            key(&mut app, KeyCode::Backspace);
        }
        for c in "copy".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(left.path().join("copy").is_dir());
    }

    fn settle(app: &mut App) {
        wait(app);
        while app.panels.iter().any(|p| p.loading().is_some()) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn type_keys(app: &mut App, text: &str) {
        text.chars().for_each(|c| key(app, KeyCode::Char(c)));
    }

    /// File Stats on a remote panel: scan, view, `:` over ssh and permanent delete.
    #[test]
    fn remote_stats_view_cmd_and_delete() {
        let left = tempfile::tempdir().unwrap();
        let mem = tc_core::vfs::mem::Mem::new();
        mem.dir("/d");
        mem.put("/d/f", b"xyz");
        let mut app = two_panels(left.path(), left.path());
        app.active = 1;
        app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
        settle(&mut app);
        // `:` on the panel runs over ssh in the panel directory
        key(&mut app, KeyCode::Char(':'));
        type_keys(&mut app, "ls");
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.run,
            Some(("ls".into(), "/".into(), Some("mem://".into())))
        );
        app.run = None;
        with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        assert!(matches!(&app.mode, Mode::Stats(st) if (st.root.size, st.root.files) == (3, 1)));
        key(&mut app, KeyCode::Enter); // into "d"
        key(&mut app, KeyCode::F(3));
        settle(&mut app);
        assert!(matches!(&app.mode, Mode::View { title, .. } if title.ends_with("/d/f")));
        key(&mut app, KeyCode::Esc);
        app.tick();
        // F4 edits a temp copy and uploads it on save
        key(&mut app, KeyCode::F(4));
        settle(&mut app);
        assert!(matches!(app.mode, Mode::Edit { .. }));
        key(&mut app, KeyCode::Char('x'));
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('y'));
        wait(&mut app);
        app.tick();
        assert_eq!(mem.get("/d/f").unwrap(), b"xxyz");
        assert!(matches!(app.mode, Mode::Stats(_)) && app.edit_remote.is_none());
        key(&mut app, KeyCode::Char(':'));
        type_keys(&mut app, "ls");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(&app.run, Some((c, _, Some(u))) if c == "ls" && u == "mem://"));
        app.run = None;
        app.tick();
        key(&mut app, KeyCode::F(8));
        assert!(
            matches!(&app.mode, Mode::Stats(st) if st.confirm.as_ref().is_some_and(|t| t.contains("No trash")))
        );
        key(&mut app, KeyCode::Char('y')); // no trash: ignored
        key(&mut app, KeyCode::Char('p'));
        settle(&mut app);
        app.tick();
        assert!(mem.get("/d/f").is_none());
        assert!(
            matches!(&app.mode, Mode::Stats(st) if st.root.size == 0 && st.current().children.is_empty())
        );
        key(&mut app, KeyCode::Esc);
        app.tick();
    }

    #[test]
    fn remote_panel_copy_delete_mkdir_and_blocked_keys() {
        let left = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a"), "data").unwrap();
        let mem = tc_core::vfs::mem::Mem::new();
        let mut app = two_panels(left.path(), left.path());
        app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
        settle(&mut app);
        assert_eq!(app.panels[1].title(), "mem:///");
        // upload: the prefilled absolute path is on the other panel's side
        app.panels[0].focus(OsStr::new("a"));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(mem.get("/a").unwrap(), b"data");
        // download it back under a new name
        key(&mut app, KeyCode::Tab);
        app.panels[1].focus(OsStr::new("a"));
        key(&mut app, KeyCode::F(5));
        let Mode::Transfer(_, _, l) = &mut app.mode else {
            panic!("no transfer prompt")
        };
        l.text.push_str(".back");
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(std::fs::read(left.path().join("a.back")).unwrap(), b"data");
        // delete: no trash on remote, only `p`
        key(&mut app, KeyCode::F(8));
        assert!(matches!(&app.mode, Mode::Confirm(_, _, t) if t.contains("No trash")));
        key(&mut app, KeyCode::Char('y'));
        assert!(matches!(app.mode, Mode::Confirm(..)));
        key(&mut app, KeyCode::Char('p'));
        settle(&mut app);
        assert!(mem.get("/a").is_none());
        key(&mut app, KeyCode::F(7));
        type_keys(&mut app, "d");
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(mem.paths(), ["/", "/d"]);
        with(&mut app, KeyCode::F(7), KeyModifiers::ALT);
        assert!(matches!(&app.mode, Mode::Message(m) if m == REMOTE));
        key(&mut app, KeyCode::Esc);
        // ".." at the remote root goes back to the local directory
        app.panels[1].set_cursor(0);
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert!(app.panels[1].remote().is_none());
        assert_eq!(app.panels[1].path(), left.path());
    }

    #[test]
    fn connection_list_add_edit_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        with(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(matches!(app.mode, Mode::Connections(0)));
        key(&mut app, KeyCode::Insert);
        type_keys(&mut app, "me@h");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.connections, [PathBuf::from("sftp://me@h")]);
        key(&mut app, KeyCode::F(4));
        type_keys(&mut app, "ost");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.connections, [PathBuf::from("sftp://me@host")]);
        key(&mut app, KeyCode::Insert);
        for _ in 0..7 {
            key(&mut app, KeyCode::Backspace);
        }
        type_keys(&mut app, "nonsense");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Message(_)));
        assert_eq!(app.connections.len(), 1);
        key(&mut app, KeyCode::Esc);
        with(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Delete);
        assert!(app.connections.is_empty());
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
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

    #[test]
    fn archive_enter_view_copy_out_and_read_only() {
        use std::io::Write;
        let (left, right) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let file = std::fs::File::create(left.path().join("a.zip")).unwrap();
        let mut z = tc_core_zip(file);
        z.0.start_file("d/f.txt", z.1).unwrap();
        z.0.write_all(b"hello").unwrap();
        z.0.finish().unwrap();
        let mut app = App::new(
            Panel::open(left.path()).unwrap(),
            Panel::open(right.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter); // opens a.zip
        wait(&mut app);
        assert!(app.panels[0].archive().is_some());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter); // d
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        wait(&mut app);
        assert!(matches!(app.mode, Mode::View { .. }) && app.view_tmp.is_some());
        key(&mut app, KeyCode::Esc);
        app.tick();
        assert!(app.view_tmp.is_none());
        key(&mut app, KeyCode::F(6));
        assert!(matches!(app.mode, Mode::Message(_)));
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert_eq!(std::fs::read(right.path().join("f.txt")).unwrap(), b"hello");
    }

    fn tc_core_zip(
        f: std::fs::File,
    ) -> (zip::ZipWriter<std::fs::File>, zip::write::SimpleFileOptions) {
        (
            zip::ZipWriter::new(f),
            zip::write::SimpleFileOptions::default(),
        )
    }

    fn wait(app: &mut App) {
        while matches!(app.mode, Mode::Work(_))
            || matches!(&app.mode, Mode::Xfer(x) if x.ask.is_none())
        {
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
