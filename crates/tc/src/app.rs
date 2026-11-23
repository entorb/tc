//! App state and key handling, independent of the terminal.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::Instant;

use crate::{KeyCode, KeyEvent, KeyModifiers};
use regex::Regex;
use tc_core::archive::{Archive, Format, TempDir};
use tc_core::cmdline::{self, LineEdit};
use tc_core::edit::Editor;
use tc_core::panel::{Panel, SortKey};
use tc_core::vfs::Remote;
use tc_core::{Error, hotlist, ops, view::Viewer};
use tc_remote::Prompt;

mod bookmarks;
mod browse_sync;
mod compare;
mod config;
mod connect;
mod editor;
mod file_stats;
mod find_files;
mod help;
mod link;
mod menu;
mod multi_rename;
mod popular;
mod proc_mon;
mod props;
mod sync;
mod tabs;
mod update;
mod widgets;
mod xfer;
pub use connect::Connecting;
pub use editor::{Ask, Input, SearchForm, Step};
pub use file_stats::Stats;
pub use find_files::{FindForm, Found, FoundSort};
pub use menu::{
    FOOTER, MENUS, MenuItem, PROCS_FOOTER, STATS_FOOTER, TOOLBAR, ToolItem, help_action, zoom_step,
};
pub use multi_rename::RenameForm;
use multi_rename::{RenameBatch, rename_rows};
pub use proc_mon::Procs;
pub use props::PropsForm;
pub use sync::{Sync, SyncForm};
pub use update::{SettingsForm, VERSION};
pub use widgets::{Filter, FormRow, ListRow, list_rows};
use widgets::{
    PresetKind, edit_key, form_checked, form_text, list_nav, paste_line, preset_key,
    rows_from_line, rows_to_line,
};
pub use xfer::{FileClip, Job, Xfer};
use xfer::{delete_question, delete_text};

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
    /// Symlink / hard link (F9 menu): two prompts, target then link path.
    Link(link::LinkAsk),
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
    /// Ctrl+Z popular directories with the selected row.
    Popular(usize),
    /// F9 menu bar: open group and selected entry.
    Menu(usize, usize),
    /// Ctrl+Y: what to select, selected row.
    Compare(usize),
    /// Mount points, selected row and the panel Enter jumps.
    Drives(Vec<PathBuf>, usize, usize),
    Message(String),
    /// Find files form (Ctrl+F): fields and checkboxes, one focused.
    FindForm(FindForm),
    /// Find results, filled live by a worker thread.
    Found(Found),
    /// Multi-rename form (Ctrl+M) with the preview.
    MultiRename(RenameForm),
    /// Synchronize directories options (Ctrl+E).
    SyncForm(SyncForm),
    /// Synchronize directories compare result, filled by a worker thread.
    Sync(Box<Sync>),
    /// File Stats (ncdu-style) tree, scanned by a worker thread.
    Stats(Stats),
    /// Process monitor, refreshed by a worker thread.
    Procs(Box<Procs>),
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
    /// Alias of the bookmark in this row being edited.
    HotlistAlias(usize, LineEdit),
    /// Connect running in a worker; may ask for a host key or password.
    Connecting(Connecting),
    /// Asks before deleting tc's files in this config directory.
    ResetConfig(PathBuf),
    /// Asks before leaving tc (F10 / q in the main panel).
    AskQuit,
    /// A newer release exists: its tag and the "check daily" checkbox.
    Update {
        tag: String,
        check: bool,
    },
    /// Settings form (F9 Tools).
    Settings(SettingsForm),
    /// File properties form (Alt+Enter): mode, owner, group.
    Props(Box<PropsForm>),
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
    /// Files on the host clipboard for Ctrl+V; `None` when the read was cancelled.
    HostFiles(Option<Vec<PathBuf>>),
    /// File properties applied (what was done before an error stays done).
    Props(tc_core::Result<()>),
}

pub struct App {
    pub panels: [Panel; 2],
    /// Free bytes + 1 per panel (0 = unknown), with the local directory they are measured for (see `free_tick`).
    pub free: [(PathBuf, Arc<AtomicU64>); 2],
    /// Background tabs per side, in cycle order after the visible one in `panels`.
    pub tabs: [VecDeque<Panel>; 2],
    /// Position of the visible tab per side, for the title.
    pub tab: [usize; 2],
    pub active: usize,
    pub mode: Mode,
    pub quit: bool,
    /// Visible rows of a panel, set by the UI, used for PgUp/PgDn.
    pub page: usize,
    /// Open text prompt of the viewer or editor.
    pub input: Option<Input>,
    /// Shortcut popup over the current mode: typing filters it, Up/Down select, Enter runs, Esc closes.
    pub help: bool,
    pub help_scroll: usize,
    /// Text typed into the help filter (case-insensitive substring of each line).
    pub help_filter: String,
    /// Font scale (Ctrl+= / Ctrl+-), saved in the settings.
    pub zoom: f32,
    /// Selected line of the filtered help text.
    pub help_sel: usize,
    /// Visible text rows of the help popup, set by the UI.
    pub help_rows: usize,
    /// Last search, used by next/previous.
    pub search: Option<Regex>,
    /// Short result of the last key, shown in the viewer or editor, e.g. "not found".
    pub note: String,
    /// Editor clipboard, kept across files (Ctrl+V); the system clipboard is fed through `copied`.
    clipboard: String,
    /// Replacement of a running one-by-one replace.
    replacing: Option<String>,
    /// Search with exact case instead of smart case (Alt+C).
    exact: bool,
    /// Search patterns are regular expressions; off, they match the text literally.
    search_regex: bool,
    /// Search dialog of the viewer / editor (F3, F7, Ctrl+F), open over the text.
    pub search_form: Option<SearchForm>,
    /// Text for the main loop to put on the system clipboard (OSC 52).
    pub copied: Option<String>,
    /// Files copied or cut with Ctrl+C / Ctrl+X, until pasted (a cut is pasted once).
    pub file_clip: Option<FileClip>,
    /// Host clipboard files seen at the last Ctrl+V: a different list is newer than `file_clip`.
    last_host: Vec<PathBuf>,
    /// Find form as last searched or cancelled; Ctrl+F reopens it.
    last_find: Option<FindForm>,
    /// Row of the last Ctrl+Y choice.
    compare_last: usize,
    /// Multi-rename settings of past runs ([`rows_to_line`]), oldest first; Ctrl+M opens with the last.
    rename_history: Vec<String>,
    /// Find files forms of past searches ([`FindForm::to_line`]), oldest first.
    finds: Vec<String>,
    syncs: Vec<String>,
    /// Patterns of the viewer / editor search dialog, oldest first.
    find_history: Vec<String>,
    /// Directory of the settings and history files (none: not persisted).
    config_dir: Option<PathBuf>,
    /// Sync form as last used; Ctrl+E reopens it.
    last_sync: Option<SyncForm>,
    /// A running sync compares again when it ends.
    sync_again: Option<sync::SyncSetup>,
    /// Find results while a result is open in the viewer or editor; shown again after it.
    found: Option<Found>,
    /// Sync rows while a file is open in the viewer or editor; shown again after it.
    sync_back: Option<Box<Sync>>,
    /// Multi-rename form while its new names are edited in the editor, and the temp file's dir.
    rename_back: Option<(RenameForm, TempDir)>,
    /// The batch being renamed, and the last one that finished completely (undo, until quit).
    rename_running: Option<RenameBatch>,
    rename_undo: Option<RenameBatch>,
    /// Saved find forms, one line each (see `FindForm::to_line`), and their file.
    searches: Vec<String>,
    searches_file: Option<PathBuf>,
    rename_presets: Vec<String>,
    rename_presets_file: Option<PathBuf>,
    /// The running transfer pastes a cut: `file_clip` is dropped once it succeeds.
    cut_pending: bool,
    /// Size calculation in a worker thread: directory and its progress (`scanned` = result ready).
    pub dir_job: Option<(usize, PathBuf, Arc<ops::Progress>)>,
    /// Viewer line numbers on (`l`).
    pub view_numbers: bool,
    /// Bookmarked directories and the file they are saved to (none: not persisted).
    pub hotlist: Vec<hotlist::Bookmark>,
    /// `/` filter of the open bookmark or connection list.
    pub list_filter: Filter,
    pub hotlist_file: Option<PathBuf>,
    /// Visited directories (frecency) and the file they are saved to.
    pub popular: Vec<tc_core::popular::Popular>,
    pub popular_file: Option<PathBuf>,
    /// Ctrl+K: navigation of the active panel is repeated in the other.
    pub sync_browse: bool,
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
    /// Daily update check on (settings), the local date it last ran, and the running check.
    update_check: bool,
    update_day: String,
    update_job: Option<thread::JoinHandle<Option<String>>>,
}

/// Largest archive entry F3/F4 extracts to a temp file.
const VIEW_CAP: u64 = 1 << 30;
const READ_ONLY: &str = "Archives are read-only: only F5 copy out and F3/F4 view work here.";
const REMOTE: &str = "Not available on remote panels.";

impl App {
    pub fn new(left: Panel, right: Panel) -> Self {
        Self {
            panels: [left, right],
            free: Default::default(),
            tabs: Default::default(),
            tab: [0; 2],
            active: 0,
            mode: Mode::Normal,
            quit: false,
            page: 10,
            input: None,
            help: false,
            help_scroll: 0,
            help_filter: String::new(),
            zoom: 1.0,
            help_sel: 0,
            help_rows: 0,
            search: None,
            note: String::new(),
            clipboard: String::new(),
            replacing: None,
            exact: false,
            search_regex: true,
            search_form: None,
            copied: None,
            file_clip: None,
            last_host: Vec::new(),
            last_find: None,
            compare_last: 0,
            rename_history: Vec::new(),
            finds: Vec::new(),
            syncs: Vec::new(),
            find_history: Vec::new(),
            config_dir: None,
            last_sync: None,
            sync_again: None,
            found: None,
            sync_back: None,
            rename_back: None,
            rename_running: None,
            rename_undo: None,
            searches: Vec::new(),
            searches_file: None,
            rename_presets: Vec::new(),
            rename_presets_file: None,
            cut_pending: false,
            dir_job: None,
            view_numbers: false,
            hotlist: Vec::new(),
            list_filter: Filter::default(),
            hotlist_file: None,
            popular: Vec::new(),
            popular_file: None,
            sync_browse: false,
            history: Vec::new(),
            history_file: None,
            run: None,
            view_tmp: None,
            stats: None,
            connections: Vec::new(),
            connections_file: None,
            edit_remote: None,
            update_check: true,
            update_day: String::new(),
            update_job: None,
        }
    }

    pub fn other_dir(&self) -> &Path {
        self.panels[1 - self.active].path()
    }

    /// The screen takes F1 and footer clicks: no prompt, dialog, help or running scan.
    pub fn idle(&self) -> bool {
        let screen = matches!(self.mode, Mode::Normal | Mode::View { .. })
            || matches!(&self.mode, Mode::Stats(st) if !st.scanning() && st.confirm.is_none())
            || matches!(&self.mode, Mode::Procs(p) if p.idle())
            || matches!(&self.mode, Mode::Sync(s) if s.idle())
            || matches!(self.mode, Mode::Edit { error: None, .. });
        screen && self.input.is_none() && !self.help
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        self.note.clear();
        if self.help {
            return self.help_key(key);
        }
        if key.code == KeyCode::F(1)
            && !key.modifiers.contains(KeyModifiers::ALT)
            && self.takes_f1()
        {
            self.help = true;
            self.help_scroll = 0;
            self.help_set_filter(String::new());
            return;
        }
        if self.input.is_some() {
            return self.input_key(key);
        }
        if self.search_form.is_some() {
            return self.search_dialog_key(key);
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
            Mode::Connections(sel) => self.connections_key(key, sel),
            Mode::ConnEdit(row, mut l) => match key.code {
                KeyCode::Enter => self.conn_save(row, &l.text),
                KeyCode::Esc => self.mode = Mode::Connections(row.unwrap_or(0)),
                _ => {
                    edit_key(&mut l, key);
                    self.mode = Mode::ConnEdit(row, l);
                }
            },
            Mode::HotlistAlias(row, l) => self.hotlist_alias_key(key, row, l),
            Mode::Connecting(c) => self.connecting_key(c, key),
            Mode::Confirm(op, targets, text) => self.confirm_key(op, targets, text, key.code),
            Mode::ResetConfig(dir) => self.reset_config_key(key.code, dir),
            Mode::AskQuit => self.ask_quit_key(key.code),
            Mode::Update { tag, check } => self.update_key(key.code, tag, check),
            Mode::Settings(form) => self.settings_key(key, form),
            Mode::Props(form) => self.props_key(key, form),
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
            Mode::Link(ask) => self.link_key(key, ask),
            Mode::NewFile(mut name) => match key.code {
                KeyCode::Enter => self.new_file(&name.text),
                KeyCode::Esc => {}
                _ => {
                    edit_key(&mut name, key);
                    self.mode = Mode::NewFile(name);
                }
            },
            Mode::Cmd(l) => self.cmd_key(l, key),
            Mode::Hotlist(sel) => self.hotlist_key(key, sel),
            Mode::Popular(sel) => self.popular_key(key, sel),
            Mode::Compare(sel) => self.compare_key(key.code, sel),
            Mode::Menu(group, sel) => self.menu_key(key.code, group, sel),
            Mode::Drives(rows, sel, panel) => self.drives_key(key.code, rows, sel, panel),
            Mode::Found(f) => self.found_key(key, f),
            Mode::FindForm(form) => self.find_form_key(key, form),
            Mode::MultiRename(form) => self.multi_rename_key(key, form),
            Mode::SyncForm(form) => self.sync_form_key(key, form),
            Mode::Sync(s) => self.sync_key(key, s),
            Mode::Stats(st) => self.stats_mode_key(st, key),
            Mode::Procs(mut p) => {
                if p.key(key, self.page) {
                    self.mode = Mode::Procs(p);
                }
            }
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
            } => self.ask_viewer_key(path, text, can_load, key.code),
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
                        Step::Changed if yes => {
                            self.save_prompts(editor, exit, to_utf8, Step::Encoding)
                        }
                        Step::Changed => {
                            self.mode = Mode::Edit {
                                editor,
                                error: None,
                            }
                        }
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

    /// Measure free space in a thread when a panel shows another local directory (a hung mount
    /// must not freeze the UI); a stale thread writes into the replaced counter.
    fn free_tick(&mut self) {
        for (p, f) in self.panels.iter().zip(&mut self.free) {
            let local = p.remote().is_none() && p.archive().is_none() && !p.is_virtual();
            let dir = if local { p.path() } else { Path::new("") };
            if f.0 != dir {
                *f = (dir.to_owned(), Arc::default());
                if local {
                    let (dir, out) = (f.0.clone(), f.1.clone());
                    thread::spawn(move || {
                        if let Some(n) = hotlist::free_space(&dir) {
                            out.store(n.saturating_add(1), Ordering::Relaxed);
                        }
                    });
                }
            }
        }
    }

    fn dir_job_tick(&mut self) {
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
    }

    fn cmd_key(&mut self, mut l: LineEdit, key: KeyEvent) {
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

    fn normal_mode_key(&mut self, key: KeyEvent) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::F(4) if shift => self.new_file_prompt(),
            KeyCode::Null if ctrl => self.link_open(alt),
            KeyCode::Null if shift => self.disconnect_all(),
            KeyCode::Null if alt => self.open_settings(),
            KeyCode::F(6) if shift => self.rename(),
            KeyCode::Enter if alt => self.props_open(),
            KeyCode::Enter if self.default_open().is_some() => {
                if let Some(path) = self.default_open() {
                    self.open_default(path);
                }
            }
            KeyCode::F(n @ 1..=2) if alt => {
                self.mode = Mode::Drives(hotlist::mounts(), 0, usize::from(n == 2));
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
            KeyCode::Char('+' | '=') => self.zoom = zoom_step(self.zoom, 1),
            KeyCode::Char('-') => self.zoom = zoom_step(self.zoom, -1),
            KeyCode::Char('s') => self.mode = Mode::Search(String::new()),
            KeyCode::Char('c') => self.clip_files(Op::Copy),
            KeyCode::Char('x') => self.clip_files(Op::Move),
            KeyCode::Char('v') => self.paste(),
            KeyCode::Char('f') => {
                let len = self.finds.len();
                let form = match self.last_find.take() {
                    Some(f) => FindForm { hist: len, ..f },
                    None => self.find_hist(len.saturating_sub(1)),
                };
                self.mode = Mode::FindForm(form);
            }
            KeyCode::Char('d') => {
                self.hotlist_add();
            }
            KeyCode::Char('g') => {
                let panel = &mut self.panels[self.active];
                let file = (panel.current())
                    .filter(|_| panel.is_virtual())
                    .map(|e| PathBuf::from(&e.name));
                if let Some(Err(e)) = file.map(|f| panel.reveal(&f)) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            KeyCode::Char('m') => self.multi_rename(),
            KeyCode::Char('o') => self.start_terminal(),
            KeyCode::Char('e') => self.sync_open(),
            KeyCode::Char('n') => self.open_list(Mode::Connections(0)),
            KeyCode::Char('b') => self.open_list(Mode::Hotlist(0)),
            KeyCode::Char('z') => self.popular_open(),
            KeyCode::Char('k') => self.toggle_browse_sync(),
            KeyCode::Char(' ') => self.spawn_stats(),
            KeyCode::Char('p') => {
                let r = self.panels[self.active].remote().cloned();
                self.mode = Mode::Procs(Box::new(Procs::new(r)));
            }
            KeyCode::Char('y') => self.compare_open(),
            KeyCode::Char('u') => {
                self.panels.swap(0, 1);
                self.tabs.swap(0, 1);
                self.tab.swap(0, 1);
                if let Some(job) = &mut self.dir_job {
                    job.0 = 1 - job.0;
                }
            }
            KeyCode::Char('r') => {
                self.free = Default::default();
                if let Err(e) = self.panels[self.active].reload() {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            KeyCode::Char('\\') => {
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
        self.free = Default::default();
        for p in &mut self.panels {
            if let Err(e) = p.reload() {
                self.mode = Mode::Message(e.to_string());
            }
        }
    }

    /// Keys in the results: Esc stops a running search, then closes; Enter panelizes;
    /// F3/F4 open the selected file at its match, Ctrl+G jumps to it in the panel.
    fn ask_quit_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.quit = true,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
            _ => self.mode = Mode::AskQuit,
        }
    }

    /// Back to the sync screen after a viewer or editor opened from it closed.
    fn restore_sync_back(&mut self) {
        if matches!(self.mode, Mode::Normal)
            && let Some(s) = self.sync_back.take()
        {
            self.mode = Mode::Sync(s);
        }
    }

    fn normal_key(&mut self, code: KeyCode) {
        let page = self.page as isize;
        let panel = &mut self.panels[self.active];
        // Left goes up like Backspace; Right opens only dirs and archives, never runs a file
        let code = match code {
            KeyCode::Left => KeyCode::Backspace,
            KeyCode::Right
                if panel
                    .current()
                    .is_some_and(|e| e.is_dir || Format::detect(Path::new(&e.name)).is_some()) =>
            {
                KeyCode::Enter
            }
            c => c,
        };
        match code {
            KeyCode::Char('q') => self.mode = Mode::AskQuit,
            KeyCode::F(10) => self.quit = true,
            KeyCode::Null => self.ask_reset_config(),
            KeyCode::F(9) => self.mode = Mode::Menu(0, 0),
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
            KeyCode::Char('/') => self.mode = Mode::Filter(panel.filter().to_owned()),
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
                let step =
                    browse_sync::step(panel, code == KeyCode::Enter).filter(|_| self.sync_browse);
                let result = if code == KeyCode::Enter {
                    panel.enter().map(drop)
                } else {
                    panel.parent()
                };
                match (result, step) {
                    (Err(e), _) => self.mode = Mode::Message(e.to_string()),
                    (Ok(()), Some(step)) => self.browse_follow(step),
                    _ => {}
                }
            }
            KeyCode::F(2) if panel.selection_info().0 > 1 => self.multi_rename(),
            KeyCode::F(2) => self.rename(),
            KeyCode::F(3 | 4) if panel.archive().is_some() => self.view_archive_entry(),
            KeyCode::F(3 | 4) if panel.remote().is_some() => {
                self.fetch_remote(code == KeyCode::F(4))
            }
            KeyCode::F(7) if panel.archive().is_some() => {
                self.mode = Mode::Message(READ_ONLY.into())
            }
            KeyCode::F(3 | 4)
                if self
                    .current_file()
                    .is_some_and(|p| cmdline::opens_externally(&p)) =>
            {
                if let Some(path) = self.current_file() {
                    match code {
                        KeyCode::F(3) => self.open_preview(path),
                        _ => self.open_default(path),
                    }
                }
            }
            KeyCode::F(3 | 4) => {
                if let Some(path) = self.current_file() {
                    let line = self.panels[self.active].hit_line(path.as_os_str());
                    self.open_at(path, line, code == KeyCode::F(4));
                }
            }
            KeyCode::F(5) => self.confirm(Op::Copy),
            KeyCode::F(6) => self.confirm(Op::Move),
            KeyCode::F(8) => self.confirm(Op::Delete),
            KeyCode::F(7) => self.mode = Mode::Mkdir(LineEdit::default()),
            _ => {}
        }
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
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
            KeyCode::Up => sel = sel.saturating_sub(1),
            KeyCode::Down => sel = (sel + 1).min(last),
            KeyCode::Enter if !rows.is_empty() => return self.go(panel, rows[sel].clone()),
            _ => {}
        }
        self.mode = Mode::Drives(rows, sel, panel);
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
        self.spawn_detached(path, cmdline::open_command);
    }

    /// F3 on media and office files: the host's quick preview.
    fn open_preview(&mut self, path: PathBuf) {
        self.spawn_detached(path, cmdline::view_command);
    }

    fn spawn_detached(&mut self, path: PathBuf, command: fn(&Path) -> std::process::Command) {
        use std::process::Stdio;
        let spawned = command(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(e) = spawned {
            self.mode = Mode::Message(format!("{}: {e}", path.display()));
        }
    }

    /// Ctrl+O: system terminal in the active local directory, detached and reaped in the background.
    fn start_terminal(&mut self) {
        use std::process::Stdio;
        let panel = &self.panels[self.active];
        if panel.remote().is_some() || panel.archive().is_some() || panel.is_virtual() {
            self.mode = Mode::Message("Terminal: local directories only.".into());
            return;
        }
        let Some(mut cmd) = cmdline::terminal_command(panel.path()) else {
            self.mode =
                Mode::Message("Terminal: no terminal emulator found (set $TERMINAL).".into());
            return;
        };
        match cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(e) => self.mode = Mode::Message(format!("Terminal: {e}")),
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
            | Mode::Link(link::LinkAsk { line: l, .. })
            | Mode::NewFile(l)
            | Mode::HotlistAlias(_, l)
            | Mode::ConnEdit(_, l) => paste_line(l, &text),
            Mode::Connecting(Connecting {
                ask: Some((Prompt::Password(_), l)),
                ..
            }) => paste_line(l, &text),
            Mode::Editor(_, l) => paste_line(l, &text),
            Mode::FindForm(f) => f.paste(&text),
            Mode::MultiRename(f) => f.paste(&text),
            Mode::SyncForm(f) => f.paste(&text),
            Mode::Xfer(Xfer {
                rename: Some(l), ..
            }) => paste_line(l, &text),
            _ => {}
        }
    }

    /// True where keys go to the file panel (no prompt, dialog or help open).
    pub fn panel_keys(&self) -> bool {
        matches!(self.mode, Mode::Normal) && self.idle()
    }

    fn current_file(&self) -> Option<PathBuf> {
        let panel = &self.panels[self.active];
        let entry = panel
            .current()
            .filter(|e| !e.is_dir && panel.archive().is_none() && panel.remote().is_none())?;
        Some(panel.path().join(&entry.name))
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

    /// Join a finished File Stats scan; true when one was handled.
    fn stats_job_tick(&mut self) -> bool {
        let Mode::Stats(st) = &mut self.mode else {
            return false;
        };
        if !st
            .job
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return false;
        }
        let Some(job) = st.job.take() else {
            return false;
        };
        match job.join() {
            Ok(root) => st.scanned(root),
            // a failed rescan keeps the old tree
            Err(_) if st.splice.is_some() => {
                st.splice = None;
                if let Mode::Stats(st) = mem::replace(
                    &mut self.mode,
                    Mode::Message("File Stats scan failed.".into()),
                ) {
                    self.stats = Some(st);
                }
            }
            Err(_) => self.mode = Mode::Message("File Stats scan failed.".into()),
        }
        true
    }

    /// Collect a finished worker result.
    pub fn tick(&mut self) {
        self.free_tick();
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
        if matches!(self.mode, Mode::Normal)
            && let Some(f) = self.found.take()
        {
            self.mode = Mode::Found(f);
        }
        self.popular_tick();
        self.restore_sync_back();
        self.restore_rename_back();
        self.update_tick();
        if matches!(self.mode, Mode::Normal)
            && let Some(setup) = self.sync_again.take()
        {
            self.sync_spawn(setup);
        }
        if matches!(self.mode, Mode::Sync(_)) {
            return self.sync_tick();
        }
        self.dir_job_tick();
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
        if let Mode::Procs(p) = &mut self.mode
            && let Some(e) = p.tick()
        {
            self.mode = Mode::Message(format!("Processes: {e}"));
            return;
        }
        if self.stats_job_tick() {
            return;
        }
        if matches!(self.mode, Mode::Xfer(_)) {
            return self.xfer_tick();
        }
        let Mode::Work(job) = &self.mode else {
            return;
        };
        let done = match job.rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(done) => done,
            Err(TryRecvError::Disconnected) => {
                self.rename_running = None;
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
            Done::HostFiles(Some(host)) => self.paste_with(host),
            Done::HostFiles(None) => {}
            Done::Props(result) => {
                let error = result.err().filter(|e| !matches!(e, Error::Cancelled));
                self.finish(error.map(|e| e.to_string()));
            }
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
mod tests;
