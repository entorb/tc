//! App state and key handling, independent of the terminal.

use std::borrow::Cow;
use std::cmp;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::fmt::{self, DATE_WIDTH, SIZE_WIDTH};
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
use tc_core::find::Hit;
use tc_core::panel::{Entry, Panel, SortKey};
use tc_core::procs::{self, Proc, ProcSort, Sampler, Snapshot};
use tc_core::stats::{self, Node};
use tc_core::vfs::{self, Remote, Side};
use tc_core::{Error, find, hotlist, ops, rename, search, view::Viewer};
use tc_remote::Prompt;

mod config;
mod sync;
pub use sync::{Sync, SyncForm};

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
    /// F9 menu bar: open group and selected entry.
    Menu(usize, usize),
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
    /// Connect running in a worker; may ask for a host key or password.
    Connecting(Connecting),
    /// Asks before deleting tc's files in this config directory.
    ResetConfig(PathBuf),
    /// Asks before leaving tc (F10 / q in the main panel).
    AskQuit,
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
        }
    }
}

pub struct Input {
    pub ask: Ask,
    pub text: LineEdit,
    pattern: String,
    /// Replace asks for each match instead of replacing all.
    each: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The file changed on disk since it was opened.
    Changed,
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
    /// Files on the host clipboard for Ctrl+V; `None` when the read was cancelled.
    HostFiles(Option<Vec<PathBuf>>),
}

/// Files copied (Ctrl+C) or cut (Ctrl+X) from a panel, pasted with Ctrl+V into the active one.
#[derive(Clone)]
pub struct FileClip {
    pub op: Op,
    pub paths: Vec<PathBuf>,
    pub side: Side,
}

/// What Ctrl+F3..F6 order the find results by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FoundSort {
    Path,
    Name,
    Date,
    Size,
}

/// Case-insensitive comparison of two names (no allocation for UTF-8 ones).
fn cmp_lower(a: &OsStr, b: &OsStr) -> cmp::Ordering {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// Order of two hits by `key`, ties by path.
fn cmp_hits(a: &Hit, b: &Hit, key: FoundSort) -> cmp::Ordering {
    let (a, b) = (&a.entry, &b.entry);
    let by = match key {
        FoundSort::Path => cmp::Ordering::Equal,
        FoundSort::Name => cmp_lower(
            Path::new(&a.name).file_name().unwrap_or_default(),
            Path::new(&b.name).file_name().unwrap_or_default(),
        ),
        FoundSort::Date => a.modified.cmp(&b.modified),
        FoundSort::Size => {
            let size = |e: &Entry| if e.is_dir { 0 } else { e.size };
            size(a).cmp(&size(b))
        }
    };
    by.then_with(|| cmp_lower(&a.name, &b.name))
}

/// Results of a running or finished file search.
pub struct Found {
    pub list: Vec<Hit>,
    /// Indices into `list` of the shown results (all, or those matching `filter`).
    view: Vec<usize>,
    pub filter: Filter,
    /// Order of `view` and whether reversed; `None` keeps the arrival order.
    pub sort: Option<(FoundSort, bool)>,
    /// Row in `view`.
    pub sel: usize,
    pub running: bool,
    /// Search on a remote panel: results are not panelized.
    pub remote: bool,
    /// Why a remote search stopped early (the server cannot run `find`, ...).
    pub error: Arc<Mutex<Option<String>>>,
    /// Content regex of the search: highlighted when a result opens in the viewer or editor.
    re: Option<Regex>,
    /// What Ctrl+R searches again.
    query: find::Query,
    rx: Receiver<Hit>,
    cancel: Arc<AtomicBool>,
}

// Multi-rename form row labels.
const R_NAME: &str = "Name mask";
const R_EXT: &str = "Extension mask";
const R_FIND: &str = "Find";
const R_REPLACE: &str = "Replace with";
const R_REGEX: &str = "Find is a regex";
const R_CASE_SENS: &str = "Find case-sensitive";
const R_CASE: &str = "Case";
const R_START: &str = "Counter start";
const R_STEP: &str = "Counter step";
const R_WIDTH: &str = "Counter digits";
const R_PRESET: &str = "Preset";
const RENAME_NAMES_FILE: &str = "new-names.txt";
const NAMES_NOTE: &str = "New names from the list; edit a field to use the masks again.";
const CASES: [(&str, rename::Case); 5] = [
    ("unchanged", rename::Case::Keep),
    ("lower", rename::Case::Lower),
    ("UPPER", rename::Case::Upper),
    ("First letter", rename::Case::First),
    ("Every Word", rename::Case::Words),
];
const CASE_NAMES: [&str; 5] = [CASES[0].0, CASES[1].0, CASES[2].0, CASES[3].0, CASES[4].0];

/// A finished multi-rename: where it ran and what it renamed, for Ctrl+Z.
#[derive(Clone)]
struct RenameBatch {
    // shortcut: a remote side's session stays open in the undo memory, upgrade by clearing it on disconnect
    side: Side,
    dir: PathBuf,
    pairs: Vec<(String, String)>,
}

/// Wording of one kind of saved form lines (see [`preset_key`]).
struct PresetKind {
    /// Note when the list is empty.
    none: &'static str,
    /// Label of the name row.
    label: &'static str,
    /// What one entry is called in "No ... x.".
    one: &'static str,
    /// What the file holds in "Cannot save ...".
    all: &'static str,
}

const SEARCH_KIND: PresetKind = PresetKind {
    none: "No saved searches yet: F2 saves one.",
    label: "Saved search",
    one: "saved search",
    all: "searches",
};

const RENAME_KIND: PresetKind = PresetKind {
    none: "No presets yet: F2 saves one.",
    label: R_PRESET,
    one: "preset",
    all: "presets",
};

/// F2 saves `line` (`name\t...`) under `name`, F3 returns the line after `name`'s (cycling),
/// F8 deletes `name`; the file is written at once. Returns the line to load and the note.
fn preset_key(
    list: &mut Vec<String>,
    file: Option<&Path>,
    code: KeyCode,
    name: &str,
    line: String,
    kind: &PresetKind,
) -> (Option<String>, String) {
    let pos = list.iter().position(|l| l.split('\t').next() == Some(name));
    let (load, note) = match (code, pos) {
        (KeyCode::F(3), _) if list.is_empty() => (None, kind.none.to_owned()),
        (KeyCode::F(3), _) => {
            let next = list[pos.map_or(0, |i| (i + 1) % list.len())].clone();
            let note = format!("Loaded {}.", next.split('\t').next().unwrap_or_default());
            (Some(next), note)
        }
        _ if name.is_empty() => (None, format!("Type a name in \"{}\" first.", kind.label)),
        (KeyCode::F(2), Some(i)) => {
            list[i] = line;
            (None, format!("Saved {name}."))
        }
        (KeyCode::F(2), None) => {
            list.push(line);
            (None, format!("Saved {name}."))
        }
        (_, Some(i)) => {
            list.remove(i);
            (None, format!("Deleted {name}."))
        }
        (_, None) => (None, format!("No {} {name}.", kind.one)),
    };
    if let Some(Err(e)) = file.map(|f| cmdline::save_history(f, list))
        && code != KeyCode::F(3)
    {
        return (None, format!("Cannot save {}: {e}", kind.all));
    }
    (load, note)
}

/// Multi-rename form rows in focus order, with the defaults (name unchanged).
fn rename_rows() -> Vec<FormRow> {
    let text = |label, v: &str| FormRow::Text(label, LineEdit::from(v.to_owned()));
    vec![
        text(R_NAME, "[N]"),
        text(R_EXT, "[E]"),
        text(R_FIND, ""),
        text(R_REPLACE, ""),
        FormRow::Check(R_REGEX, false),
        FormRow::Check(R_CASE_SENS, false),
        FormRow::Choice(R_CASE, &CASE_NAMES, 0),
        text(R_START, "1"),
        text(R_STEP, "1"),
        text(R_WIDTH, "1"),
        text(R_PRESET, ""),
    ]
}

/// Multi-rename (Ctrl+M): form, the items (selection in panel order) and their new names.
pub struct RenameForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    dir: PathBuf,
    items: Vec<(OsString, Option<std::time::SystemTime>)>,
    /// Old name and the new one, or why there is none (empty: the form has an error).
    pub preview: Vec<(String, Result<String, String>)>,
    /// First preview row shown.
    pub scroll: usize,
    pub error: Option<String>,
    /// Position in the settings history for Alt+Up/Down (= its length: not from history).
    hist: usize,
    /// New names pasted as a list, one per item; they replace the masks.
    names: Option<Vec<String>>,
}

impl RenameForm {
    fn spec(&self) -> Result<rename::Spec, String> {
        let rows = &self.rows;
        let num = |label: &str| {
            let t = form_text(rows, label).trim();
            t.parse::<i64>()
                .map_err(|_| format!("{label}: not a number: {t}"))
        };
        let width = num(R_WIDTH)?;
        let case = rows.iter().find_map(|r| match r {
            FormRow::Choice(R_CASE, _, i) => Some(CASES[*i].1),
            _ => None,
        });
        Ok(rename::Spec {
            name_mask: form_text(rows, R_NAME).into(),
            ext_mask: form_text(rows, R_EXT).into(),
            find: form_text(rows, R_FIND).into(),
            replace: form_text(rows, R_REPLACE).into(),
            regex: form_checked(rows, R_REGEX),
            case_sensitive: form_checked(rows, R_CASE_SENS),
            case: case.unwrap_or_default(),
            start: num(R_START)?,
            step: num(R_STEP)?,
            width: usize::try_from(width)
                .ok()
                .filter(|w| (1..=20).contains(w))
                .ok_or(format!("{R_WIDTH}: 1 to 20"))?,
        })
    }

    /// New names for the form as it is; the first problem goes to `error`.
    fn refresh(&mut self) {
        let rule = self.spec().and_then(rename::Spec::compile);
        self.preview = (self.items.iter().enumerate())
            .map(|(i, (name, modified))| {
                let new = match (&self.names, &rule) {
                    (Some(n), _) => rename::explicit(name, &n[i]),
                    (None, Ok(r)) => r.new_name(name, *modified, i),
                    (None, Err(_)) => Err(String::new()),
                };
                (name.to_string_lossy().into_owned(), new)
            })
            .collect();
        self.error = match rule {
            _ if self.names.is_some() => self.pairs().err().or(Some(NAMES_NOTE.into())),
            Err(e) => Some(e),
            Ok(_) => self.pairs().err(),
        };
    }

    /// Renames to run, or why not.
    fn pairs(&self) -> Result<Vec<(String, String)>, String> {
        let mut old = Vec::new();
        let mut new = Vec::new();
        for (o, n) in &self.preview {
            match n {
                Ok(n) => new.push(n.clone()),
                Err(e) => return Err(format!("{o}: {e}")),
            }
            // a new name exists only for UTF-8 old names, so this is the real name
            old.push(o.clone());
        }
        rename::plan(&old, &new)
    }

    /// One line of the presets file: `name`, then the [`rows_to_line`] of the form.
    fn to_line(&self, name: &str) -> String {
        format!(
            "{}\t{}",
            name.replace('\t', " "),
            rows_to_line(&self.rows, R_PRESET)
        )
    }

    /// Set the rows from a preset line (name first); the focus stays.
    fn load_line(&mut self, line: &str) {
        self.names = None;
        self.rows = rename_rows();
        rows_from_line(&mut self.rows, line);
        if let Some((name, _)) = line.split_once('\t')
            && let Some(FormRow::Text(_, l)) = self.rows.last_mut()
        {
            l.text = name.into();
            l.end();
        }
        self.refresh();
    }

    /// Pasted text with line breaks is a list of new names; else it goes into the field.
    fn paste(&mut self, text: &str) {
        if text.contains('\n') {
            let lines = text.lines().take(self.items.len() + 1);
            return self.set_names(lines.map(str::to_owned).collect());
        }
        if let FormRow::Text(_, l) = &mut self.rows[self.focus] {
            paste_line(l, text);
        }
        self.names = None;
        self.refresh();
    }

    /// Use `lines` as the new names, one per item; a wrong count is an error and changes nothing.
    fn set_names(&mut self, lines: Vec<String>) {
        if lines.len() == self.items.len() {
            self.names = Some(lines);
            self.refresh();
        } else {
            self.error = Some(format!(
                "Pasted {} names for {} files.",
                lines.len(),
                self.items.len()
            ));
        }
    }
}

// Find form row labels; also the keys of saved searches.
const F_NAME: &str = "Name (masks)";
const F_NAME_RE: &str = "Name is a regex";
const F_NAME_CASE: &str = "Name case-sensitive";
const F_SKIP_FILES: &str = "Exclude files";
const F_CONTENT_ON: &str = "Search file contents";
const F_CONTENT: &str = "Content";
const F_REGEX: &str = "Content is a regex";
const F_CASE: &str = "Content case-sensitive";
const F_WORD: &str = "Whole words";
const F_NOT: &str = "Not containing";
const F_EVERY: &str = "List every match";
const F_SKIP_DIRS: &str = "Exclude dirs";
const F_HIDDEN: &str = "Skip hidden";
const F_GITIGNORE: &str = "Use .gitignore";
const F_FOLLOW: &str = "Follow symlinks";
const F_DEPTH: &str = "Max depth";
const F_MIN: &str = "Min size";
const F_MAX: &str = "Max size";
const F_AFTER: &str = "Modified after";
const F_BEFORE: &str = "Modified before";
const F_AGE: &str = "Not older than";
const F_EXTS: &str = "Extensions";
const F_SAVED: &str = "Saved search";

/// Find files form rows in focus order.
fn find_rows() -> Vec<FormRow> {
    let text = |label| FormRow::Text(label, LineEdit::default());
    let check = |label| FormRow::Check(label, false);
    vec![
        text(F_NAME),
        check(F_NAME_RE),
        check(F_NAME_CASE),
        text(F_SKIP_FILES),
        check(F_CONTENT_ON),
        text(F_CONTENT),
        check(F_REGEX),
        check(F_CASE),
        check(F_WORD),
        check(F_NOT),
        check(F_EVERY),
        text(F_SKIP_DIRS),
        check(F_HIDDEN),
        check(F_GITIGNORE),
        check(F_FOLLOW),
        text(F_DEPTH),
        text(F_MIN),
        text(F_MAX),
        text(F_AFTER),
        text(F_BEFORE),
        text(F_AGE),
        text(F_EXTS),
        text(F_SAVED),
    ]
}

/// One line of a form: a text field, a checkbox or a choice (options, selected).
#[derive(Clone)]
pub enum FormRow {
    Text(&'static str, LineEdit),
    Check(&'static str, bool),
    Choice(&'static str, &'static [&'static str], usize),
}

impl FormRow {
    fn label(&self) -> &'static str {
        match self {
            FormRow::Text(l, _) | FormRow::Check(l, _) | FormRow::Choice(l, _, _) => l,
        }
    }

    /// Typing edits a text field, Space toggles a checkbox, Space/Left/Right cycle a choice.
    fn key(&mut self, code: KeyCode, ctrl: bool) {
        match (self, code) {
            (FormRow::Check(_, on), KeyCode::Char(' ')) => *on = !*on,
            (FormRow::Choice(_, opts, i), KeyCode::Char(' ') | KeyCode::Right) => {
                *i = (*i + 1) % opts.len();
            }
            (FormRow::Choice(_, opts, i), KeyCode::Left) => *i = (*i + opts.len() - 1) % opts.len(),
            (FormRow::Text(_, l), KeyCode::Char(c)) if !ctrl => l.insert(c),
            (FormRow::Text(_, l), KeyCode::Backspace) => l.backspace(),
            (FormRow::Text(_, l), KeyCode::Delete) => l.delete(),
            (FormRow::Text(_, l), KeyCode::Left) => l.left(),
            (FormRow::Text(_, l), KeyCode::Right) => l.right(),
            (FormRow::Text(_, l), KeyCode::Home) => l.home(),
            (FormRow::Text(_, l), KeyCode::End) => l.end(),
            _ => {}
        }
    }
}

/// Form rows as tab separated `label=value` (checkboxes `1`/`0`, choices their index),
/// without the `skip` row; tabs inside values become spaces.
fn rows_to_line(rows: &[FormRow], skip: &str) -> String {
    let parts: Vec<String> = (rows.iter().filter(|r| r.label() != skip))
        .map(|row| {
            let value = match row {
                FormRow::Text(_, l) => l.text.replace('\t', " "),
                FormRow::Check(_, on) => if *on { "1" } else { "0" }.into(),
                FormRow::Choice(_, _, i) => i.to_string(),
            };
            format!("{}={value}", row.label())
        })
        .collect();
    parts.join("\t")
}

/// Set rows from a [`rows_to_line`] line; unknown labels are ignored, missing ones keep their value.
fn rows_from_line(rows: &mut [FormRow], line: &str) {
    let values: HashMap<&str, &str> = line.split('\t').filter_map(|p| p.split_once('=')).collect();
    for row in rows {
        let Some(&v) = values.get(row.label()) else {
            continue;
        };
        match row {
            FormRow::Text(_, l) => {
                l.text = v.into();
                l.end();
            }
            FormRow::Check(_, on) => *on = v == "1",
            FormRow::Choice(_, opts, i) => {
                *i = v.parse().ok().filter(|&v| v < opts.len()).unwrap_or(0);
            }
        }
    }
}

fn form_text<'a>(rows: &'a [FormRow], label: &str) -> &'a str {
    match rows.iter().find(|r| r.label() == label) {
        Some(FormRow::Text(_, l)) => &l.text,
        _ => "",
    }
}

fn form_checked(rows: &[FormRow], label: &str) -> bool {
    rows.iter()
        .any(|r| matches!(r, FormRow::Check(l, true) if *l == label))
}

/// Find files form: rows, the focused one, and the error (or note) of the last try.
#[derive(Clone)]
pub struct FindForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub error: Option<String>,
    /// Position in the find history for Alt+Up/Down (= its length: not from history).
    hist: usize,
}

impl FindForm {
    pub fn new() -> Self {
        FindForm {
            rows: find_rows(),
            focus: 0,
            error: None,
            hist: 0,
        }
    }

    fn text(&self, label: &str) -> &str {
        form_text(&self.rows, label)
    }

    fn checked(&self, label: &str) -> bool {
        form_checked(&self.rows, label)
    }

    /// Typing and space toggle on the focused row.
    fn key(&mut self, code: KeyCode, ctrl: bool) {
        self.rows[self.focus].key(code, ctrl);
        self.tick_content();
    }

    /// Pasted text goes into the focused text field.
    fn paste(&mut self, text: &str) {
        if let FormRow::Text(_, l) = &mut self.rows[self.focus] {
            paste_line(l, text);
        }
        self.tick_content();
    }

    /// Typed content text turns content search on, so it is not silently ignored.
    fn tick_content(&mut self) {
        if self.text(F_CONTENT).is_empty() {
            return;
        }
        for row in &mut self.rows {
            if let FormRow::Check(F_CONTENT_ON, on) = row {
                *on = true;
            }
        }
    }

    /// One line of the saved searches file: `name`, then the [`rows_to_line`] of the form.
    fn to_line(&self, name: &str) -> String {
        format!(
            "{}\t{}",
            name.replace('\t', " "),
            rows_to_line(&self.rows, F_SAVED)
        )
    }

    /// The form a saved line describes; unknown labels are ignored, missing ones stay empty.
    fn from_line(line: &str) -> Self {
        let mut form = FindForm::new();
        let (name, rest) = line.split_once('\t').unwrap_or((line, ""));
        rows_from_line(&mut form.rows, rest);
        if let Some(FormRow::Text(_, l)) = form.rows.iter_mut().find(|r| r.label() == F_SAVED) {
            l.text = name.into();
            l.end();
        }
        form
    }

    /// The search the fields describe; the first field that does not parse is the error.
    fn query(&self, exact: bool, remote: bool) -> Result<find::Query, String> {
        let name = self.text(F_NAME).trim();
        let name_case = exact || self.checked(F_NAME_CASE);
        let name_re = if self.checked(F_NAME_RE) {
            Some(search::compile_case(name, name_case).map_err(|e| format!("Name: {e}"))?)
        } else {
            None
        };
        let size = |label: &str| match self.text(label).trim() {
            "" => Ok(None),
            t => find::parse_size(t).map(Some),
        };
        let date = |label: &str, end: bool| match self.text(label).trim() {
            "" => Ok(None),
            t => find::parse_date(t, end).map(Some),
        };
        let age = match self.text(F_AGE).trim() {
            "" => None,
            t => Some(find::parse_age(t)?),
        };
        let max_depth = match self.text(F_DEPTH).trim() {
            "" => None,
            t => Some(
                t.parse::<usize>()
                    .ok()
                    .filter(|&d| d > 0)
                    .ok_or_else(|| format!("not a depth (1 = this directory only): {t}"))?,
            ),
        };
        let q = find::Query {
            mask: if name_re.is_some() { "" } else { name }.into(),
            name_re,
            mask_case: name_case,
            exclude_files: self.text(F_SKIP_FILES).trim().into(),
            content: self.content(exact)?,
            exclude: find::parse_list(self.text(F_SKIP_DIRS)),
            min_size: size(F_MIN)?,
            max_size: size(F_MAX)?,
            after: date(F_AFTER, false)?.max(age),
            before: date(F_BEFORE, true)?,
            exts: find::parse_exts(self.text(F_EXTS)),
            max_depth,
            skip_hidden: self.checked(F_HIDDEN),
            gitignore: self.checked(F_GITIGNORE),
            follow_links: self.checked(F_FOLLOW),
        };
        match q.remote_unsupported() {
            Some(e) if remote => Err(e.into()),
            _ => Ok(q),
        }
    }

    /// Content search of the form; `None` while its checkbox is off.
    fn content(&self, exact: bool) -> Result<Option<find::Content>, String> {
        if !self.checked(F_CONTENT_ON) {
            return Ok(None);
        }
        let text = self.text(F_CONTENT);
        if text.is_empty() {
            return Err("Content search needs text in its field.".into());
        }
        let mut c = find::Content::new(
            text,
            !self.checked(F_REGEX),
            self.checked(F_WORD),
            exact || self.checked(F_CASE),
        )?;
        c.negate = self.checked(F_NOT);
        c.every = self.checked(F_EVERY) && !c.negate;
        Ok(Some(c))
    }
}

impl Default for FindForm {
    fn default() -> Self {
        Self::new()
    }
}

impl Found {
    /// Shown results.
    pub fn shown(&self) -> impl Iterator<Item = &Hit> {
        self.view.iter().map(|&i| &self.list[i])
    }

    pub fn shown_len(&self) -> usize {
        self.view.len()
    }

    fn matches(&self, h: &Hit) -> bool {
        self.filter.matches(&h.entry.name.to_string_lossy())
    }

    fn rebuild(&mut self) {
        self.view = (0..self.list.len())
            .filter(|&i| self.matches(&self.list[i]))
            .collect();
        self.sort_view();
        self.sel = self.sel.min(self.view.len().saturating_sub(1));
    }

    /// Orders `view` by `sort`, the cursor stays on the same hit.
    fn resort(&mut self) {
        let cur = self.view.get(self.sel).copied();
        self.sort_view();
        if let Some(i) = cur.and_then(|c| self.view.iter().position(|&v| v == c)) {
            self.sel = i;
        }
    }

    fn sort_view(&mut self) {
        let Some((key, rev)) = self.sort else { return };
        let list = &self.list;
        self.view.sort_by(|&a, &b| {
            let o = cmp_hits(&list[a], &list[b], key);
            if rev { o.reverse() } else { o }
        });
    }

    /// Sorts by `key`; the same key again reverses.
    fn set_sort(&mut self, key: FoundSort) {
        self.sort = Some((key, self.sort == Some((key, false))));
        self.resort();
    }

    fn pump(&mut self) {
        let mut pushed = false;
        for _ in 0..5000 {
            match self.rx.try_recv() {
                Ok(e) => {
                    if self.matches(&e) {
                        self.view.push(self.list.len());
                        pushed = true;
                    }
                    self.list.push(e);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.running = false;
                    break;
                }
            }
        }
        if pushed {
            self.resort();
        }
    }
}

/// File Stats: the scanned tree and the directory being browsed.
pub struct Stats {
    pub dir: PathBuf,
    pub root: Node,
    /// Child indices from `root` down to the browsed directory.
    path: Vec<usize>,
    /// Row in `view`.
    pub sel: usize,
    /// Indices into the browsed directory's children shown (all, or those matching `filter`).
    view: Vec<usize>,
    pub filter: Filter,
    /// Show file count (value, share, bar) instead of size.
    pub by_files: bool,
    pub order: stats::Order,
    /// Reversed `order`.
    pub rev: bool,
    /// Path of the running rescan to swap into `root`; `None` for the first scan.
    splice: Option<Vec<usize>>,
    /// Running scan; `root` is empty until it finishes.
    job: Option<thread::JoinHandle<Node>>,
    pub progress: Arc<ops::Progress>,
    /// Delete question for the selected entry, shown while set.
    pub confirm: Option<String>,
    /// Connection of a remote panel the tree was scanned on.
    remote: Option<Arc<dyn Remote>>,
    /// Names in the browsed directory a running remote delete removes, and how many it has
    /// deleted so far.
    deleting: Option<(Vec<OsString>, Arc<AtomicUsize>)>,
    /// Marked names of the browsed directory.
    marked: HashSet<OsString>,
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

    /// Entry under the cursor.
    pub fn selected(&self) -> Option<&Node> {
        self.view
            .get(self.sel)
            .map(|&i| &self.current().children[i])
    }

    /// Shown entries of the browsed directory.
    pub fn shown(&self) -> impl Iterator<Item = &Node> {
        let dir = self.current();
        self.view.iter().map(|&i| &dir.children[i])
    }

    /// Index in the browsed directory's children of the entry under the cursor.
    fn selected_index(&self) -> Option<usize> {
        self.view.get(self.sel).copied()
    }

    pub fn is_marked(&self, n: &Node) -> bool {
        self.marked.contains(&n.name)
    }

    /// Children indices a delete acts on: the shown marked entries, else the selected one.
    fn targets(&self) -> Vec<usize> {
        let dir = self.current();
        let marked: Vec<_> = (self.view.iter().copied())
            .filter(|&i| self.marked.contains(&dir.children[i].name))
            .collect();
        if marked.is_empty() {
            self.selected_index().into_iter().collect()
        } else {
            marked
        }
    }

    /// Ask to delete the targets.
    fn delete_ask(&mut self) {
        let dir = self.current();
        let nodes: Vec<_> = self
            .targets()
            .into_iter()
            .map(|i| &dir.children[i])
            .collect();
        if nodes.is_empty() {
            return;
        }
        let names: Vec<_> = nodes
            .iter()
            .take(3)
            .map(|n| n.name.to_string_lossy())
            .collect();
        let size = nodes.iter().map(|n| n.size).sum();
        let question = delete_question(&names, nodes.len(), Some(size));
        let recursive = nodes.iter().any(|n| n.is_dir);
        self.confirm = Some(delete_text(question, recursive, self.remote.is_some()));
    }

    /// Remove deleted entries of the browsed directory from the tree.
    fn drop_names(&mut self, names: &[OsString]) {
        for name in names {
            if let Some(i) = self.current().children.iter().position(|n| &n.name == name) {
                stats::remove(&mut self.root, &self.path, i);
            }
        }
        self.marked.retain(|m| !names.contains(m));
        self.rebuild();
    }

    /// Recompute the shown entries after a sort, a filter or a change of the tree.
    fn rebuild(&mut self) {
        let filter = &self.filter;
        self.view = (self.current().children.iter().enumerate())
            .filter(|(_, n)| filter.matches(&n.name.to_string_lossy()))
            .map(|(i, _)| i)
            .collect();
        self.sel = self.sel.min(self.view.len().saturating_sub(1));
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
        let last = self.view.len().saturating_sub(1);
        match code {
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(page),
            KeyCode::PageDown => self.sel = (self.sel + page).min(last),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = last,
            KeyCode::Enter | KeyCode::Right if self.selected().is_some_and(|n| n.is_dir) => {
                self.marked.clear();
                if let Some(i) = self.selected_index() {
                    self.path.push(i);
                }
                self.filter = Filter::default();
                self.resort();
            }
            KeyCode::Backspace | KeyCode::Left => {
                self.marked.clear();
                if let Some(i) = self.path.pop() {
                    let name = self.current().children[i].name.clone();
                    self.filter = Filter::default();
                    self.resort();
                    self.sel = self
                        .current()
                        .children
                        .iter()
                        .position(|n| n.name == name)
                        .unwrap_or(0);
                }
            }
            KeyCode::Tab => self.by_files = !self.by_files,
            KeyCode::Insert | KeyCode::Char(' ') => self.toggle_mark(last),
            KeyCode::Char('n') => self.set_order(stats::Order::Name),
            KeyCode::Char('s') => self.set_order(stats::Order::Size),
            KeyCode::Char('C') => self.set_order(stats::Order::Count),
            _ => {}
        }
    }

    /// Ctrl+R: scan the browsed directory again in a worker and swap it into the tree.
    fn rescan(&mut self) {
        self.progress = Arc::new(ops::Progress::default());
        self.splice = Some(self.path.clone());
        self.job = Some(spawn_scan(
            self.path(),
            self.remote.clone(),
            self.progress.clone(),
        ));
        self.marked.clear();
    }

    /// A finished scan: the first one is the tree, a rescan replaces the browsed directory.
    fn scanned(&mut self, node: Node) {
        let Some(path) = self.splice.take() else {
            self.root = node;
            self.rebuild();
            return;
        };
        // the cursor row indexes the old children, so read its name before they are replaced
        let name = self.selected().map(|n| n.name.clone());
        stats::replace(&mut self.root, &path, node);
        self.resort();
        self.select_name(name);
    }

    /// Mark or unmark the selected entry and move down.
    fn toggle_mark(&mut self, last: usize) {
        if let Some(name) = self.selected().map(|n| n.name.clone())
            && !self.marked.remove(&name)
        {
            self.marked.insert(name);
        }
        self.sel = (self.sel + 1).min(last);
    }

    /// Same order again reverses it; the cursor stays on its entry.
    fn set_order(&mut self, order: stats::Order) {
        let name = self.selected().map(|n| n.name.clone());
        self.rev = self.order == order && !self.rev;
        self.order = order;
        self.resort();
        self.select_name(name);
    }

    /// Puts the cursor on the shown entry called `name`, if any.
    fn select_name(&mut self, name: Option<OsString>) {
        if let Some(i) = name.and_then(|name| {
            self.view
                .iter()
                .position(|&i| self.current().children[i].name == name)
        }) {
            self.sel = i;
        }
    }

    /// Sorts the browsed directory and puts the cursor on its first shown entry.
    fn resort(&mut self) {
        let (order, rev) = (self.order, self.rev);
        stats::sort(&mut self.current_mut().children, order, rev);
        self.sel = 0;
        self.rebuild();
    }
}

/// Scan `dir` (on `remote` if set) in a worker.
fn spawn_scan(
    dir: PathBuf,
    remote: Option<Arc<dyn Remote>>,
    p: Arc<ops::Progress>,
) -> thread::JoinHandle<Node> {
    thread::spawn(move || match remote {
        Some(r) => stats::scan_remote(r.as_ref(), &dir, &p),
        None => stats::scan(&dir, &p),
    })
}

/// Process monitor (Ctrl+P): latest snapshot, sorted and filtered, refreshed by a worker.
pub struct Procs {
    /// Connection of the remote panel it was opened on; `None`: this computer.
    pub remote: Option<Arc<dyn Remote>>,
    sampler: Sampler,
    /// Time between snapshots, CPU % is averaged over it.
    pub every: Duration,
    /// Interval being typed (`s`), in seconds.
    pub every_edit: Option<LineEdit>,
    pub snap: Snapshot,
    /// Indices into `snap.procs` matching the filter, in sort order.
    pub rows: Vec<usize>,
    pub sel: usize,
    pub sort: ProcSort,
    pub rev: bool,
    pub filter: Filter,
    /// Kill question for (pid, name), shown while set.
    pub confirm: Option<(u32, String)>,
    kill: Option<thread::JoinHandle<tc_core::Result<()>>>,
    /// Result of the last kill, shown in the footer until the next key.
    pub status: String,
}

impl Procs {
    fn new(remote: Option<Arc<dyn Remote>>) -> Self {
        Procs {
            sampler: procs::sample(remote.clone(), procs::INTERVAL),
            every: procs::INTERVAL,
            every_edit: None,
            remote,
            snap: Snapshot::default(),
            rows: Vec::new(),
            sel: 0,
            sort: ProcSort::Cpu,
            rev: false,
            filter: Filter::default(),
            confirm: None,
            kill: None,
            status: String::new(),
        }
    }

    /// No prompt or question open.
    pub fn idle(&self) -> bool {
        !self.filter.editing && self.every_edit.is_none() && self.confirm.is_none()
    }

    /// Process under the cursor.
    pub fn current(&self) -> Option<&Proc> {
        self.rows.get(self.sel).map(|&i| &self.snap.procs[i])
    }

    /// Re-sort and re-filter; the cursor keeps its row, so the list does not jump under it.
    fn refresh(&mut self) {
        procs::sort(&mut self.snap.procs, self.sort, self.rev);
        let filter = self.filter.line.text.to_lowercase();
        self.rows = (0..self.snap.procs.len())
            .filter(|&i| procs::matches(&self.snap.procs[i], &filter))
            .collect();
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    /// Takes the newest snapshot and a finished kill; `Some(error)` when sampling failed.
    fn tick(&mut self) -> Option<String> {
        let mut new = None;
        while let Ok(r) = self.sampler.rx.try_recv() {
            match r {
                Ok(s) => new = Some(s),
                Err(e) => return Some(e.to_string()),
            }
        }
        if let Some(s) = new {
            self.snap = s;
            self.refresh();
        }
        if self
            .kill
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
            && let Some(job) = self.kill.take()
        {
            self.status = match job.join() {
                Ok(Ok(())) => "signal sent".into(),
                Ok(Err(e)) => e.to_string(),
                Err(_) => "kill failed".into(),
            };
        }
        None
    }

    /// `false` closes the monitor.
    fn key(&mut self, key: KeyEvent, page: usize) -> bool {
        self.status.clear();
        if self.confirm.is_some() {
            self.confirm_key(key.code);
            return true;
        }
        if let Some(l) = self.every_edit.take() {
            self.every_key(l, key);
            return true;
        }
        if self.filter.key(key) {
            self.refresh();
            return true;
        }
        let last = self.rows.len().saturating_sub(1);
        let sort = |p: &mut Self, key| {
            p.rev = p.sort == key && !p.rev;
            p.sort = key;
        };
        match key.code {
            // sample now: the next snapshot comes right away
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sampler.set_interval(self.every);
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(10) => return false,
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(page),
            KeyCode::PageDown => self.sel = (self.sel + page).min(last),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = last,
            KeyCode::Char('c') => sort(self, ProcSort::Cpu),
            KeyCode::Char('m') => sort(self, ProcSort::Mem),
            KeyCode::Char('p') => sort(self, ProcSort::Pid),
            KeyCode::Char('n') => sort(self, ProcSort::Name),
            KeyCode::Char('f') => self.filter.editing = true,
            KeyCode::Char('s') => {
                self.every_edit = Some(LineEdit::from(self.every.as_secs_f64().to_string()));
            }
            KeyCode::F(8) | KeyCode::Char('k') => {
                self.confirm = self.current().map(|p| (p.pid, p.name.clone()));
            }
            _ => {}
        }
        self.refresh();
        true
    }

    /// Typing the update interval: Enter sets it (0.5 to 3600 s), Esc keeps the old one.
    fn every_key(&mut self, mut l: LineEdit, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => match l.text.trim().replace(',', ".").parse::<f64>() {
                Ok(secs) if (0.5..=3600.0).contains(&secs) => {
                    self.every = Duration::from_secs_f64(secs);
                    self.sampler.set_interval(self.every);
                }
                _ => self.status = "update interval: 0.5 to 3600 seconds".into(),
            },
            _ => {
                edit_key(&mut l, key);
                self.every_edit = Some(l);
            }
        }
    }

    /// Answer to the kill question: `t` SIGTERM, `k` SIGKILL, sent in a worker (slow links).
    fn confirm_key(&mut self, code: KeyCode) {
        let force = match code {
            KeyCode::Char('t' | 'T') | KeyCode::Enter => false,
            KeyCode::Char('k' | 'K') => true,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.confirm = None;
                return;
            }
            _ => return,
        };
        let Some((pid, _)) = self.confirm.take() else {
            return;
        };
        let r = self.remote.clone();
        self.kill = Some(thread::spawn(move || procs::kill(r.as_deref(), pid, force)));
        self.status = format!(
            "sending SIG{} to {pid}",
            if force { "KILL" } else { "TERM" }
        );
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

/// `/` filter of a list screen: case-insensitive substring, typed live; Enter keeps it,
/// Esc clears it.
#[derive(Default)]
pub struct Filter {
    pub line: LineEdit,
    pub editing: bool,
}

impl Filter {
    pub fn active(&self) -> bool {
        !self.line.text.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        !self.active() || name.to_lowercase().contains(&self.line.text.to_lowercase())
    }

    /// Filter part of a key: typing, `/` to start, Esc to clear an active filter.
    /// True when the key was used and the rows need a rebuild.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if self.editing {
            match key.code {
                KeyCode::Enter => self.editing = false,
                KeyCode::Esc => *self = Self::default(),
                _ => {
                    edit_key(&mut self.line, key);
                }
            }
            return true;
        }
        match key.code {
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Esc if self.active() => self.line = LineEdit::default(),
            _ => return false,
        }
        true
    }

    /// Title suffix: ` [filter: x]`, with `_` while typing.
    pub fn tag(&self) -> String {
        match (self.active(), self.editing) {
            (false, false) => String::new(),
            (_, editing) => format!(
                " [filter: {}{}]",
                self.line.text,
                if editing { "_" } else { "" }
            ),
        }
    }
}

/// Delete question with its answers; remote panels have no trash.
/// "Delete a?" or "Delete n items (size): a, b, c…?" from the first names and the total count.
fn delete_question(names: &[Cow<str>], count: usize, size: Option<u64>) -> String {
    if count == 1 {
        return format!("Delete {}?", names[0]);
    }
    let more = if count > names.len() { "…" } else { "" };
    let size = size.map_or_else(String::new, |s| format!(" ({})", edit::human(s)));
    format!("Delete {count} items{size}: {}{more}?", names.join(", "))
}

fn delete_text(mut question: String, recursive: bool, remote: bool) -> String {
    if recursive {
        question.push_str("\nDirectories are deleted recursively.");
    }
    if remote {
        return question + "\n\nNo trash on remote panels.\n[p] Delete permanently   [Esc] Cancel";
    }
    question + "\n\n[y] Trash   [p] Delete permanently   [Esc] Cancel"
}

/// Indices of the `rows` a list shows.
pub fn list_rows(rows: &[PathBuf], filter: &Filter) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| filter.matches(&rows[i].to_string_lossy()))
        .collect()
}

/// Filter and Up/Down keys of a path list; `sel` is a row of `rows`. True when `key` was used.
fn list_nav(filter: &mut Filter, rows: &[PathBuf], sel: &mut usize, key: KeyEvent) -> bool {
    if filter.key(key) {
        *sel = list_rows(rows, filter).first().copied().unwrap_or(0);
        return true;
    }
    let view = list_rows(rows, filter);
    let pos = view.iter().position(|&i| i == *sel).unwrap_or(0);
    let pos = match key.code {
        KeyCode::Up => pos.saturating_sub(1),
        KeyCode::Down => (pos + 1).min(view.len().saturating_sub(1)),
        _ => return false,
    };
    *sel = view.get(pos).copied().unwrap_or(0);
    true
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

/// Footer bar items of one screen: label, key sent when clicked.
type Footer = [(&'static str, KeyCode, KeyModifiers)];

const fn f(label: &'static str, n: u8) -> (&'static str, KeyCode, KeyModifiers) {
    (label, KeyCode::F(n), KeyModifiers::NONE)
}

const CLOSE: (&str, KeyCode, KeyModifiers) = f("F10 Close", 10);
const HELP: (&str, KeyCode, KeyModifiers) = f("F1 Help", 1);

const VIEW_FOOTER: [(&str, KeyCode, KeyModifiers); 7] = [
    HELP,
    f("F4 Edit", 4),
    f("F7 Search", 7),
    ("g Goto", KeyCode::Char('g'), KeyModifiers::NONE),
    ("w Wrap", KeyCode::Char('w'), KeyModifiers::NONE),
    ("l Lines", KeyCode::Char('l'), KeyModifiers::NONE),
    CLOSE,
];

const EDIT_FOOTER: [(&str, KeyCode, KeyModifiers); 7] = [
    HELP,
    f("F2 Save", 2),
    f("F3 Next", 3),
    f("F4 Replace", 4),
    f("F7 Search", 7),
    f("F8 Del line", 8),
    CLOSE,
];

const FOUND_FOOTER: [(&str, KeyCode, KeyModifiers); 7] = [
    HELP,
    f("F3 View", 3),
    f("F4 Edit", 4),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("Enter Panel", KeyCode::Enter, KeyModifiers::NONE),
    ("Ctrl+G Go to", KeyCode::Char('g'), KeyModifiers::CONTROL),
    CLOSE,
];

const SYNC_FOOTER: [(&str, KeyCode, KeyModifiers); 6] = [
    HELP,
    ("Enter Sync", KeyCode::Enter, KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("Ctrl+R Compare", KeyCode::Char('r'), KeyModifiers::CONTROL),
    ("Space Skip", KeyCode::Char(' '), KeyModifiers::NONE),
    CLOSE,
];

const FIND_FORM_FOOTER: [(&str, KeyCode, KeyModifiers); 6] = [
    HELP,
    ("Enter Start", KeyCode::Enter, KeyModifiers::NONE),
    f("F2 Save", 2),
    f("F3 Load", 3),
    f("F8 Del", 8),
    CLOSE,
];

const FORM_FOOTER: [(&str, KeyCode, KeyModifiers); 3] = [
    HELP,
    ("Enter Start", KeyCode::Enter, KeyModifiers::NONE),
    CLOSE,
];

/// Footer bar items: label, key sent when clicked.
pub const FOOTER: [(&str, KeyCode, KeyModifiers); 10] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
    ("F3 View", KeyCode::F(3), KeyModifiers::NONE),
    ("F4 Edit", KeyCode::F(4), KeyModifiers::NONE),
    ("S-F4 Ext", KeyCode::F(4), KeyModifiers::SHIFT),
    ("F5 Copy", KeyCode::F(5), KeyModifiers::NONE),
    ("F6 Move", KeyCode::F(6), KeyModifiers::NONE),
    ("F7 Mkdir", KeyCode::F(7), KeyModifiers::NONE),
    ("F8 Delete", KeyCode::F(8), KeyModifiers::NONE),
    ("F9 Menu", KeyCode::F(9), KeyModifiers::NONE),
    ("F10 Quit", KeyCode::F(10), KeyModifiers::NONE),
];

const C: KeyModifiers = KeyModifiers::CONTROL;
const A: KeyModifiers = KeyModifiers::ALT;
const S: KeyModifiers = KeyModifiers::SHIFT;
const N: KeyModifiers = KeyModifiers::NONE;

/// One menu entry: label with its key, key sent when chosen.
pub type MenuItem = (&'static str, KeyCode, KeyModifiers);

/// F9 menu bar like mc, for terminals that swallow keys (macOS): group title and its entries.
pub const MENUS: [(&str, &[MenuItem]); 5] = [
    (
        "File",
        &[
            ("View                F3", KeyCode::F(3), N),
            ("Edit                F4", KeyCode::F(4), N),
            ("Edit external       Shift+F4", KeyCode::F(4), S),
            ("Copy                F5", KeyCode::F(5), N),
            ("Move                F6", KeyCode::F(6), N),
            ("Rename              Shift+F6", KeyCode::F(6), S),
            ("Multi-rename        Ctrl+M", KeyCode::Char('m'), C),
            ("Make directory      F7", KeyCode::F(7), N),
            ("Delete              F8", KeyCode::F(8), N),
            ("Copy paths          Ctrl+Insert", KeyCode::Insert, C),
            ("Copy files          Ctrl+C", KeyCode::Char('c'), C),
            ("Cut files           Ctrl+X", KeyCode::Char('x'), C),
            ("Paste files         Ctrl+V", KeyCode::Char('v'), C),
            ("Quit                F10", KeyCode::F(10), N),
        ],
    ),
    (
        "Go",
        &[
            ("Home directory      ~", KeyCode::Char('~'), N),
            ("Root directory      Ctrl+\\", KeyCode::Char('\\'), C),
            ("Refresh             Ctrl+R", KeyCode::Char('r'), C),
            ("Drives left         Alt+F1", KeyCode::F(1), A),
            ("Drives right        Alt+F2", KeyCode::F(2), A),
            ("History back        Alt+Left", KeyCode::Left, A),
            ("History forward     Alt+Right", KeyCode::Right, A),
            ("Bookmarks           Ctrl+B", KeyCode::Char('b'), C),
            ("Add bookmark        Ctrl+D", KeyCode::Char('d'), C),
            ("Connections         Ctrl+N", KeyCode::Char('n'), C),
        ],
    ),
    (
        "Select",
        &[
            ("Select by mask      +", KeyCode::Char('+'), N),
            ("Deselect by mask    -", KeyCode::Char('-'), N),
            ("Invert selection    *", KeyCode::Char('*'), N),
            ("Select differences  Ctrl+Y", KeyCode::Char('y'), C),
        ],
    ),
    (
        "Tools",
        &[
            ("Help                F1", KeyCode::F(1), N),
            ("File stats (ncdu)   Ctrl+Space", KeyCode::Char(' '), C),
            ("Processes (top)     Ctrl+P", KeyCode::Char('p'), C),
            ("Find files          Ctrl+F", KeyCode::Char('f'), C),
            ("Synchronize dirs    Ctrl+E", KeyCode::Char('e'), C),
            ("Go to found file    Ctrl+G", KeyCode::Char('g'), C),
            ("Quick search        Ctrl+S", KeyCode::Char('s'), C),
            ("Quick filter        /", KeyCode::Char('/'), N),
            ("Command line        :", KeyCode::Char(':'), N),
            // no shortcut: `Null` is never typed, only sent by this entry and F1 help
            ("Delete config files", KeyCode::Null, N),
        ],
    ),
    (
        "Panel",
        &[
            ("Swap panels         Ctrl+U", KeyCode::Char('u'), C),
            ("Other panel here    Alt+=", KeyCode::Char('='), A),
            ("Hidden files        Alt+.", KeyCode::Char('.'), A),
            ("New tab             Ctrl+T", KeyCode::Char('t'), C),
            ("Close tab           Ctrl+W", KeyCode::Char('w'), C),
        ],
    ),
];

fn menu_title_width(group: usize) -> u16 {
    MENUS[group].0.len() as u16 + 2
}

/// Column of a menu title in the bar (shared by the UI and mouse clicks).
pub fn menu_x(group: usize) -> u16 {
    (0..group).map(menu_title_width).sum()
}

/// Screen area of an open menu group (below the bar, with border).
pub fn menu_drop(group: usize) -> Rect {
    let items = MENUS[group].1;
    let w = items.iter().map(|i| i.0.len()).max().unwrap_or(0) as u16 + 4;
    Rect::new(menu_x(group), 1, w, items.len() as u16 + 2)
}

/// Key a main-help line describes (its first column equals a [`MENUS`] key), if any.
pub fn help_action(line: &str) -> Option<(KeyCode, KeyModifiers)> {
    let col = line.split("  ").next()?.trim();
    MENUS
        .iter()
        .flat_map(|g| g.1)
        .find(|m| m.0.rsplit("  ").next().is_some_and(|k| k.trim() == col))
        .map(|m| (m.1, m.2))
}

/// F-key bar of File Stats (labels and the keys they send).
pub const STATS_FOOTER: [(&str, KeyCode, KeyModifiers); 9] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
    ("F3 View", KeyCode::F(3), KeyModifiers::NONE),
    ("F4 Edit", KeyCode::F(4), KeyModifiers::NONE),
    ("S-F4 Ext", KeyCode::F(4), KeyModifiers::SHIFT),
    ("F8 Delete", KeyCode::F(8), KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("Tab Size/Count", KeyCode::Tab, KeyModifiers::NONE),
    (": Cmd", KeyCode::Char(':'), KeyModifiers::NONE),
    ("F10 Close", KeyCode::F(10), KeyModifiers::NONE),
];

/// F-key bar of the process monitor.
pub const PROCS_FOOTER: [(&str, KeyCode, KeyModifiers); 5] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("s Interval", KeyCode::Char('s'), KeyModifiers::NONE),
    ("F8 Kill", KeyCode::F(8), KeyModifiers::NONE),
    ("F10 Close", KeyCode::F(10), KeyModifiers::NONE),
];

/// Toolbar button: icon, tooltip, key sent when clicked.
pub type ToolItem = (&'static str, &'static str, KeyCode, KeyModifiers);

/// tc-gui toolbar groups (navigation, tools).
/// Icons must exist in egui's `emoji-icon-font`; the rest stays in the F9 menu.
pub const TOOLBAR: [&[ToolItem]; 2] = [
    &[
        ("⏴", "History back (Alt+Left)", KeyCode::Left, A),
        ("⏵", "History forward (Alt+Right)", KeyCode::Right, A),
        ("⏶", "Parent directory (Backspace)", KeyCode::Backspace, N),
        ("🏠", "Home directory (~)", KeyCode::Char('~'), N),
        ("⟳", "Refresh (Ctrl+R)", KeyCode::Char('r'), C),
        ("👁", "Hidden files (Alt+.)", KeyCode::Char('.'), A),
        ("★", "Bookmarks (Ctrl+B)", KeyCode::Char('b'), C),
        ("✚", "New tab (Ctrl+T)", KeyCode::Char('t'), C),
        ("🖧", "Connect to server (Ctrl+N)", KeyCode::Char('n'), C),
    ],
    &[
        ("⌨", "Run command (:)", KeyCode::Char(':'), N),
        ("🗠", "File stats, ncdu (Ctrl+Space)", KeyCode::Char(' '), C),
        ("🖥", "Process monitor (Ctrl+P)", KeyCode::Char('p'), C),
        ("🔍", "Find files (Ctrl+F)", KeyCode::Char('f'), C),
        (
            "⬌",
            "Synchronize directories (Ctrl+E)",
            KeyCode::Char('e'),
            C,
        ),
        ("🖊", "Multi-rename (Ctrl+M)", KeyCode::Char('m'), C),
        (
            "⚖",
            "Compare panels, select differences (Ctrl+Y)",
            KeyCode::Char('y'),
            C,
        ),
    ],
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
    /// Screen area of the help text (set by the CLI UI), for clicks.
    pub help_area: Rect,
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
    /// Text area of the editor, set by the UI, used for mouse clicks.
    pub edit_text: Rect,
    /// Text for the main loop to put on the system clipboard (OSC 52).
    pub copied: Option<String>,
    /// Files copied or cut with Ctrl+C / Ctrl+X, until pasted (a cut is pasted once).
    pub file_clip: Option<FileClip>,
    /// Host clipboard files seen at the last Ctrl+V: a different list is newer than `file_clip`.
    last_host: Vec<PathBuf>,
    /// Find form as last searched or cancelled; Ctrl+F reopens it.
    last_find: Option<FindForm>,
    /// Multi-rename settings of past runs ([`rows_to_line`]), oldest first; Ctrl+M opens with the last.
    rename_history: Vec<String>,
    /// Find files forms of past searches ([`FindForm::to_line`]), oldest first.
    finds: Vec<String>,
    syncs: Vec<String>,
    /// Patterns of the viewer / editor Find and Replace prompts, oldest first.
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
    pub hotlist: Vec<PathBuf>,
    /// `/` filter of the open bookmark or connection list.
    pub list_filter: Filter,
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
            help_area: Rect::default(),
            active: 0,
            mode: Mode::Normal,
            quit: false,
            page: 10,
            input: None,
            help: false,
            help_scroll: 0,
            help_filter: String::new(),
            help_sel: 0,
            help_rows: 0,
            search: None,
            note: String::new(),
            clipboard: String::new(),
            replacing: None,
            exact: false,
            edit_text: Rect::default(),
            copied: None,
            file_clip: None,
            last_host: Vec::new(),
            last_find: None,
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
        if self.help {
            if ev.kind == MouseEventKind::Down(MouseButton::Left)
                && self.help_area.contains((ev.column, ev.row).into())
            {
                let row = usize::from(ev.row - self.help_area.y);
                self.help_click(self.help_scroll + row);
            }
            return;
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left) && self.footer_click(ev) {
            return;
        }
        if let MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) =
            ev.kind
            && self.input.is_none()
            && self.replacing.is_none()
            && let Mode::Edit {
                editor,
                error: None,
            } = &mut self.mode
        {
            let a = self.edit_text;
            if a.contains((ev.column, ev.row).into()) {
                let extend = ev.kind != MouseEventKind::Down(MouseButton::Left)
                    || ev.modifiers.contains(KeyModifiers::SHIFT);
                editor.click(
                    usize::from(ev.row - a.y),
                    usize::from(ev.column - a.x),
                    usize::from(a.width),
                    extend,
                );
            }
            return;
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left) {
            if self.input.is_none()
                && matches!(self.mode, Mode::Normal | Mode::Menu(..))
                && self.menu_click(ev)
            {
                return;
            }
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
            Mode::Procs(p) if p.idle() => {
                let last = p.rows.len().saturating_sub(1);
                p.sel = p.sel.saturating_add_signed(lines).min(last);
            }
            // the panel under the pointer scrolls, the active panel stays
            Mode::Normal => {
                let at = (ev.column, ev.row).into();
                let i = (0..2).find(|&i| self.areas[i].contains(at));
                self.panels[i.unwrap_or(self.active)].move_cursor(lines);
            }
            _ => {}
        }
    }

    /// Footer labels send keys: on idle screens, and on the find results and forms.
    pub fn footer_active(&self) -> bool {
        self.idle()
            || (self.input.is_none()
                && matches!(
                    self.mode,
                    Mode::Found(_) | Mode::FindForm(_) | Mode::MultiRename(_) | Mode::SyncForm(_)
                ))
    }

    /// Click on a footer label sends its key; only where the screen takes keys that way.
    fn footer_click(&mut self, ev: MouseEvent) -> bool {
        if !self.footer.contains((ev.column, ev.row).into()) || !self.footer_active() {
            return false;
        }
        let mut x = self.footer.x;
        for &(label, code, mods) in self.footer_keys() {
            let end = x + label.len() as u16;
            if (x..end).contains(&ev.column) {
                self.on_key(KeyEvent::new(code, mods));
                break;
            }
            x = end + 2;
        }
        true
    }

    /// Left click on a file row moves the cursor there (and activates the panel); with Shift it selects the range from the old cursor.
    fn click(&mut self, ev: MouseEvent) {
        if self.input.is_some() || !matches!(self.mode, Mode::Normal) {
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
            return self.sort_by(i, key);
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
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        let now = Instant::now();
        let double = !shift
            && self.last_click.is_some_and(|(t, c, r)| {
                (c, r) == (ev.column, ev.row) && now.duration_since(t) < Duration::from_millis(400)
            });
        if !shift {
            self.last_click = (!double).then_some((now, ev.column, ev.row));
        }
        self.click_entry(i, idx, shift, double);
    }

    /// Sort panel `i` by `key` (same key again reverses) and make it active.
    pub fn sort_by(&mut self, i: usize, key: SortKey) {
        self.active = i;
        self.panels[i].set_sort(key);
    }

    /// Click on entry `idx` of panel `i`: cursor there, Shift selects the range from the old cursor, double click opens.
    pub fn click_entry(&mut self, i: usize, idx: usize, shift: bool, double: bool) {
        if self.input.is_some() || !matches!(self.mode, Mode::Normal) {
            return;
        }
        self.active = i;
        let panel = &mut self.panels[i];
        if shift {
            panel.select_range(panel.cursor(), idx);
            return;
        }
        panel.set_cursor(idx);
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

    /// F-key bar of the current screen.
    pub fn footer_keys(&self) -> &'static Footer {
        match &self.mode {
            Mode::View { .. } => &VIEW_FOOTER,
            Mode::Edit { .. } | Mode::AskSave { .. } | Mode::AskExit(_) => &EDIT_FOOTER,
            Mode::Found(_) => &FOUND_FOOTER,
            Mode::Sync(_) => &SYNC_FOOTER,
            Mode::FindForm(_) => &FIND_FORM_FOOTER,
            Mode::MultiRename(_) | Mode::SyncForm(_) => &FORM_FOOTER,
            Mode::Stats(_) => &STATS_FOOTER,
            Mode::Cmd(_) if self.stats.is_some() => &STATS_FOOTER,
            Mode::Procs(_) => &PROCS_FOOTER,
            _ => &FOOTER,
        }
    }

    /// F1 opens the help here: the footer screens plus lists and the conflict question.
    fn takes_f1(&self) -> bool {
        self.footer_active()
            || (self.input.is_none()
                && !self.help
                && match &self.mode {
                    Mode::Hotlist(_) | Mode::Connections(_) | Mode::Drives(..) => true,
                    Mode::Xfer(x) => x.ask.is_some() && x.rename.is_none(),
                    _ => false,
                })
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
            Mode::Connecting(c) => self.connecting_key(c, key),
            Mode::Confirm(op, targets, text) => self.confirm_key(op, targets, text, key.code),
            Mode::ResetConfig(dir) => self.reset_config_key(key.code, dir),
            Mode::AskQuit => self.ask_quit_key(key.code),
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
            Mode::Cmd(l) => self.cmd_key(l, key),
            Mode::Hotlist(sel) => self.hotlist_key(key, sel),
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

    // no trash on remote panels: only `p`
    fn confirm_key(&mut self, op: Op, targets: Vec<PathBuf>, text: String, code: KeyCode) {
        match code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter
                if self.panels[self.active].remote().is_none() =>
            {
                self.delete(&targets, true)
            }
            KeyCode::Char('p' | 'P') => self.delete(&targets, false),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
            _ => self.mode = Mode::Confirm(op, targets, text),
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

    fn ask_viewer_key(&mut self, path: PathBuf, text: String, can_load: bool, code: KeyCode) {
        match code {
            KeyCode::Char('y' | 'Y') => self.open_viewer(path),
            KeyCode::Char('n' | 'N') if can_load => self.spawn_load(path, None),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
            _ => {
                self.mode = Mode::AskViewer {
                    path,
                    text,
                    can_load,
                }
            }
        }
    }

    /// Help lines as shown (matching the filter, all when it is empty) with the key each runs
    /// (only the main help has keys). Built once per frame or key, not per line.
    pub fn help_entries(&self) -> Vec<(&'static str, Option<(KeyCode, KeyModifiers)>)> {
        let (_, text) = fmt::help(&self.mode);
        let main = text == fmt::HELP_MAIN;
        let f = self.help_filter.to_lowercase();
        text.lines()
            .filter(|l| f.is_empty() || l.to_lowercase().contains(&f))
            .map(|l| (l, if main { help_action(l) } else { None }))
            .collect()
    }

    /// Key a shown help line runs.
    pub fn help_action_at(&self, line: usize) -> Option<(KeyCode, KeyModifiers)> {
        self.help_entries().get(line).and_then(|e| e.1)
    }

    /// Click on help text line `line`: close the help and run the action the line describes.
    pub fn help_click(&mut self, line: usize) {
        if let Some((c, m)) = self.help_action_at(line) {
            self.help = false;
            self.on_key(KeyEvent::new(c, m));
        }
    }

    /// New filter text: selects the first key line and scrolls to the top.
    fn help_set_filter(&mut self, filter: String) {
        self.help_filter = filter;
        self.help_scroll = 0;
        self.help_sel = self
            .help_entries()
            .iter()
            .position(|e| e.1.is_some())
            .unwrap_or(0);
    }

    /// Moves the selection to the next key line in `dir`, or scrolls when there are none.
    fn help_move(&mut self, dir: isize) {
        let sel = self.help_sel as isize;
        let next = self
            .help_entries()
            .iter()
            .enumerate()
            .filter(|(i, e)| e.1.is_some() && (*i as isize - sel).signum() == dir)
            .map(|(i, _)| i)
            .min_by_key(|&i| (i as isize - sel).abs());
        match next {
            Some(i) => {
                self.help_sel = i;
                self.help_follow();
            }
            None if dir < 0 => self.help_scroll = self.help_scroll.saturating_sub(1),
            None => self.help_scroll += 1,
        }
    }

    /// Keeps the selected line inside the popup.
    fn help_follow(&mut self) {
        let rows = self.help_rows.max(1);
        if self.help_sel < self.help_scroll {
            self.help_scroll = self.help_sel;
        } else if self.help_sel >= self.help_scroll + rows {
            self.help_scroll = self.help_sel + 1 - rows;
        }
    }

    fn help_key(&mut self, key: KeyEvent) {
        let page = self.page;
        let typing = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc if !self.help_filter.is_empty() => self.help_set_filter(String::new()),
            KeyCode::Esc | KeyCode::F(1) | KeyCode::F(10) => self.help = false,
            KeyCode::Enter if self.help_action_at(self.help_sel).is_some() => {
                self.help_click(self.help_sel)
            }
            KeyCode::Enter => self.help = false,
            KeyCode::Backspace => {
                let mut f = self.help_filter.clone();
                f.pop();
                self.help_set_filter(f);
            }
            KeyCode::Char(c) if typing => {
                let mut f = self.help_filter.clone();
                f.push(c);
                self.help_set_filter(f);
            }
            KeyCode::Up => self.help_move(-1),
            KeyCode::Down => self.help_move(1),
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(page),
            KeyCode::PageDown => self.help_scroll += page,
            KeyCode::Home => self.help_scroll = 0,
            KeyCode::End => self.help_scroll = usize::MAX,
            _ => {}
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
            KeyCode::Char('d') if self.panels[self.active].remote().is_some() => {
                self.mode = Mode::Message(REMOTE.into())
            }
            KeyCode::Char('d') => self.hotlist_add(),
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
            KeyCode::Char('e') => self.sync_open(),
            KeyCode::Char('n') => self.open_list(Mode::Connections(0)),
            KeyCode::Char('b') => self.open_list(Mode::Hotlist(0)),
            KeyCode::Char(' ') => self.spawn_stats(),
            KeyCode::Char('p') => {
                let r = self.panels[self.active].remote().cloned();
                self.mode = Mode::Procs(Box::new(Procs::new(r)));
            }
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

    /// Directory names of one side's tabs in cycle order; `self.tab[side]` is the visible one.
    pub fn tab_names(&self, side: usize) -> Vec<String> {
        let (bg, p) = (&self.tabs[side], self.tab[side]);
        let n = bg.len() + 1;
        (0..n)
            .map(|k| {
                let panel = if k == p {
                    &self.panels[side]
                } else {
                    &bg[(k + n - p - 1) % n]
                };
                panel
                    .path()
                    .file_name()
                    .map_or_else(|| panel.title(), |name| name.to_string_lossy().into_owned())
            })
            .collect()
    }

    /// Shows tab `k` of `side` (click on a tab button).
    pub fn select_tab(&mut self, side: usize, k: usize) {
        self.active = side;
        let n = self.tabs[side].len() + 1;
        for _ in 0..(k + n - self.tab[side]) % n {
            self.tab_key(KeyCode::PageDown);
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
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.replacing.is_some() {
            self.mode = Mode::Edit {
                editor,
                error: None,
            };
            match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.replace_step(true),
                KeyCode::Char('n' | 'N' | ' ') => self.replace_step(false),
                KeyCode::Char('a' | 'A') => {
                    while self.replacing.is_some() {
                        self.replace_step(true);
                    }
                }
                _ => self.replacing = None,
            }
            return;
        }
        let save =
            key.code == KeyCode::F(2) || (ctrl && matches!(key.code, KeyCode::Char('s' | 'S')));
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
            _ if save => {
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
                    KeyCode::F(7) if shift => self.find_or_ask(true),
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
            KeyCode::Char('w') if alt => editor.wrap = !editor.wrap,
            KeyCode::Char('l') if alt => editor.numbers = !editor.numbers,
            KeyCode::Char('c') if alt => self.toggle_case(),
            KeyCode::Char('r') if alt => {
                self.mode = Mode::Edit {
                    editor,
                    error: None,
                };
                self.ask(Ask::Replace);
                if let Some(i) = &mut self.input {
                    i.each = true;
                }
                return;
            }
            KeyCode::Left if ctrl => editor.move_word(false),
            KeyCode::Right if ctrl => editor.move_word(true),
            KeyCode::Home if ctrl => editor.goto_idx(0),
            KeyCode::End if ctrl => editor.goto_idx(usize::MAX),
            KeyCode::Left => editor.move_h(-1),
            KeyCode::Right => editor.move_h(1),
            KeyCode::Home => editor.home(),
            KeyCode::End => editor.end(),
            KeyCode::Enter => editor.newline(),
            KeyCode::BackTab => editor.unindent(),
            KeyCode::Tab if shift => editor.unindent(),
            KeyCode::Tab => editor.insert_char('\t'),
            KeyCode::Backspace if ctrl => editor.delete_word(false),
            KeyCode::Delete if ctrl => editor.delete_word(true),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::F(8) => editor.delete_line(),
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
                'a' => editor.select_all(),
                'd' => editor.duplicate_line(),
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

    /// Alt+C in the viewer and editor: searches match case exactly, or ignore it without capitals.
    fn toggle_case(&mut self) {
        self.exact = !self.exact;
        self.note = if self.exact {
            "case sensitive"
        } else {
            "smart case"
        }
        .into();
    }

    /// F4 in the viewer: the editor on the same local file, at the top line.
    fn view_to_editor(&mut self, title: String, view: Viewer) {
        if self.view_tmp.is_some() || self.edit_remote.is_some() {
            self.note = "edit: local files only".into();
            return self.mode = Mode::View { title, view };
        }
        let line = view.top_line().map(|n| n + 1);
        self.edit_path_at(view.path().to_owned(), line);
    }

    // Alt+W, Alt+L, Ctrl+F and Ctrl+G act like their plain letters: modifiers are not checked
    fn viewer_key(&mut self, title: String, mut view: Viewer, key: KeyEvent) {
        let page = self.page as isize;
        let result = match key.code {
            KeyCode::Esc | KeyCode::F(3 | 10) | KeyCode::Char('q') => return,
            KeyCode::F(4) => return self.view_to_editor(title, view),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.toggle_case();
                Ok(())
            }
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
                    KeyCode::F(7) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        self.find_or_ask(true)
                    }
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

    fn menu_key(&mut self, code: KeyCode, group: usize, sel: usize) {
        let n = MENUS.len();
        let len = MENUS[group].1.len();
        match code {
            KeyCode::Esc | KeyCode::F(9 | 10) => {}
            KeyCode::Enter => self.menu_run(group, sel),
            KeyCode::Left => self.mode = Mode::Menu((group + n - 1) % n, 0),
            KeyCode::Right => self.mode = Mode::Menu((group + 1) % n, 0),
            KeyCode::Up => self.mode = Mode::Menu(group, (sel + len - 1) % len),
            KeyCode::Down => self.mode = Mode::Menu(group, (sel + 1) % len),
            KeyCode::Home => self.mode = Mode::Menu(group, 0),
            KeyCode::End => self.mode = Mode::Menu(group, len - 1),
            _ => self.mode = Mode::Menu(group, sel),
        }
    }

    pub fn menu_run(&mut self, group: usize, sel: usize) {
        self.mode = Mode::Normal;
        let (_, c, m) = MENUS[group].1[sel];
        self.on_key(KeyEvent::new(c, m));
    }

    /// Left click while the menu bar is shown (row 0) or open. True if the click was used.
    fn menu_click(&mut self, ev: MouseEvent) -> bool {
        let (col, row) = (ev.column, ev.row);
        let open = if let Mode::Menu(g, _) = self.mode {
            Some(g)
        } else {
            None
        };
        if row == 0
            && let Some(g) = (0..MENUS.len()).find(|&g| {
                let x = menu_x(g);
                (x..x + menu_title_width(g)).contains(&col)
            })
        {
            self.mode = if open == Some(g) {
                Mode::Normal
            } else {
                Mode::Menu(g, 0)
            };
            return true;
        }
        let Some(g) = open else { return false };
        let r = menu_drop(g);
        if r.contains((col, row).into()) {
            // rows inside the border are entries
            if row > r.y && row + 1 < r.y + r.height && col > r.x && col + 1 < r.x + r.width {
                self.menu_run(g, usize::from(row - r.y - 1));
            }
        } else {
            self.mode = Mode::Normal;
        }
        true
    }

    /// Bookmark or connection list, shown without a filter.
    fn open_list(&mut self, mode: Mode) {
        self.list_filter = Filter::default();
        self.mode = mode;
    }

    fn hotlist_key(&mut self, key: KeyEvent, sel: usize) {
        let mut sel = sel.min(self.hotlist.len().saturating_sub(1));
        if list_nav(&mut self.list_filter, &self.hotlist, &mut sel, key) {
            self.mode = Mode::Hotlist(sel);
            return;
        }
        let shown = list_rows(&self.hotlist, &self.list_filter).contains(&sel);
        match key.code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
            KeyCode::Enter if shown => {
                let path = self.hotlist[sel].clone();
                if let Err(e) = self.panels[self.active].go(path) {
                    self.mode = Mode::Message(e.to_string());
                }
                return;
            }
            KeyCode::Delete if shown => {
                self.hotlist.remove(sel);
                self.hotlist_save();
                sel = list_rows(&self.hotlist, &self.list_filter)
                    .first()
                    .copied()
                    .unwrap_or(0);
                if matches!(self.mode, Mode::Message(_)) {
                    return;
                }
            }
            _ => {}
        }
        self.mode = Mode::Hotlist(sel);
    }

    /// Search the active panel's directory in a worker thread, results arrive via `tick`.
    fn spawn_find(&mut self, q: find::Query) {
        let panel = &self.panels[self.active];
        let remote = panel.remote().cloned();
        let root = panel.path().to_owned();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (c, err) = (cancel.clone(), error.clone());
        let is_remote = remote.is_some();
        let re = q.content.as_ref().map(|c| c.re.clone());
        let query = q.clone();
        thread::spawn(move || match remote {
            None => find::find(&root, &q, &c, |h| drop(tx.send(h))),
            Some(r) => {
                let send = |entry| drop(tx.send(Hit { entry, line: None }));
                let res = find::find_remote(r.as_ref(), &vfs::rpath(&root), &q, &c, send);
                if let (Err(e), Ok(mut slot)) = (res, err.lock()) {
                    *slot = Some(e.to_string());
                }
            }
        });
        self.mode = Mode::Found(Found {
            list: Vec::new(),
            view: Vec::new(),
            filter: Filter::default(),
            sort: None,
            query,
            sel: 0,
            running: true,
            remote: is_remote,
            error,
            re,
            rx,
            cancel,
        });
    }

    /// Find files form: Enter searches, Tab/Up/Down move, Space toggles, Esc cancels.
    fn find_form_key(&mut self, key: KeyEvent, mut form: FindForm) {
        let remote = self.panels[self.active].remote().is_some();
        let last = form.rows.len() - 1;
        form.error = None;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => return self.last_find = Some(form),
            KeyCode::Enter => match form.query(self.exact, remote) {
                Ok(q) => {
                    let line = form.to_line(form.text(F_SAVED).trim());
                    self.note = self.remember("find-forms", &line);
                    self.last_find = Some(form);
                    return self.spawn_find(q);
                }
                Err(e) => form.error = Some(e),
            },
            KeyCode::Up | KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                let i = match key.code {
                    KeyCode::Up => form.hist.saturating_sub(1),
                    _ => (form.hist + 1).min(self.finds.len()),
                };
                let focus = form.focus;
                form = FindForm {
                    focus,
                    ..self.find_hist(i)
                };
            }
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => form.focus = form.focus.saturating_sub(1),
            KeyCode::F(2 | 3 | 8) => form = self.saved_search_key(key.code, form),
            code => form.key(
                code,
                key.modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
            ),
        }
        self.mode = Mode::FindForm(form);
    }

    /// F2 saves the form under the "Saved search" name, F3 loads the next saved search,
    /// F8 deletes the named one; the file is written at once.
    fn saved_search_key(&mut self, code: KeyCode, form: FindForm) -> FindForm {
        let name = form.text(F_SAVED).trim().to_owned();
        let line = form.to_line(&name);
        let (load, note) = preset_key(
            &mut self.searches,
            self.searches_file.as_deref(),
            code,
            &name,
            line,
            &SEARCH_KIND,
        );
        let mut form = match load {
            Some(l) => FindForm {
                focus: form.focus,
                ..FindForm::from_line(&l)
            },
            None => form,
        };
        form.error = Some(note);
        form
    }

    /// F2/F3/F8 in the multi-rename form: presets, as for the find form.
    fn rename_preset_key(&mut self, code: KeyCode, form: &mut RenameForm) {
        let name = form_text(&form.rows, R_PRESET).trim().to_owned();
        let line = form.to_line(&name);
        let (load, note) = preset_key(
            &mut self.rename_presets,
            self.rename_presets_file.as_deref(),
            code,
            &name,
            line,
            &RENAME_KIND,
        );
        if let Some(l) = load {
            form.load_line(&l);
        }
        form.error = Some(note);
    }

    /// Ctrl+Space: File Stats of the active panel's directory.
    fn spawn_stats(&mut self) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message("File Stats does not work in archives.".into());
            return;
        }
        self.scan_stats(panel.path().to_owned(), panel.remote().cloned());
    }

    /// Scan `dir` in a worker and show its File Stats.
    fn scan_stats(&mut self, dir: PathBuf, remote: Option<Arc<dyn Remote>>) {
        let progress = Arc::new(ops::Progress::default());
        let job = spawn_scan(dir.clone(), remote.clone(), progress.clone());
        self.mode = Mode::Stats(Stats {
            remote,
            deleting: None,
            marked: HashSet::new(),
            dir,
            root: Node::default(),
            path: Vec::new(),
            sel: 0,
            view: Vec::new(),
            filter: Filter::default(),
            by_files: false,
            order: stats::Order::default(),
            rev: false,
            splice: None,
            job: Some(job),
            progress,
            confirm: None,
        });
    }

    /// Keys in File Stats; only Esc/q/F10 (cancel) work while scanning.
    fn stats_mode_key(&mut self, mut st: Stats, key: KeyEvent) {
        if st.confirm.is_some() {
            return self.stats_confirm_key(st, key.code);
        }
        if !st.scanning() && st.filter.key(key) {
            st.rebuild();
            self.mode = Mode::Stats(st);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(10) => {
                st.progress.cancel.store(true, Ordering::Relaxed);
                // a cancelled rescan keeps the old tree; the first scan closes the screen
                if st.splice.take().is_some() {
                    st.job = None;
                    self.mode = Mode::Stats(st);
                }
                return;
            }
            _ if st.scanning() => {}
            KeyCode::F(3 | 4) => return self.stats_open(st, key),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => st.rescan(),
            KeyCode::F(8) | KeyCode::Char('d') => {
                st.delete_ask();
            }
            // Ctrl+Space opened the stats, it does not mark
            KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
            KeyCode::Char(':') => {
                self.stats = Some(st);
                self.mode = Mode::Cmd(LineEdit::default());
                return;
            }
            KeyCode::Char('i') => return self.stats_info(st),
            KeyCode::Char('p') => return self.stats_panelize(st),
            code => st.key(code, self.page),
        }
        self.mode = Mode::Stats(st);
    }

    /// `i`: message popup for the selected entry, the stats come back when it closes.
    fn stats_info(&mut self, st: Stats) {
        let Some(n) = st.selected() else {
            self.mode = Mode::Stats(st);
            return;
        };
        let path = st.path().join(&n.name);
        // a remote stat would block the UI thread
        let modified = match st.remote {
            Some(_) => None,
            None => fs::symlink_metadata(&path).and_then(|m| m.modified()).ok(),
        };
        let text = fmt::stats_info(&path, n, modified);
        self.stats = Some(st);
        self.mode = Mode::Message(text);
    }

    /// `p`: the marked entries, else all shown ones, become a virtual panel in the active panel.
    fn stats_panelize(&mut self, st: Stats) {
        if st.remote.is_some() {
            // the list's re-stat is local only
            self.stats = Some(st);
            self.mode = Mode::Message(REMOTE.into());
            return;
        }
        let dir = st.path();
        let marked = st.marked.iter().any(|m| st.shown().any(|n| &n.name == m));
        let entries = (st.shown())
            .filter(|n| !marked || st.is_marked(n))
            .map(|n| Entry {
                name: dir.join(&n.name).into_os_string(),
                is_dir: n.is_dir,
                is_symlink: false,
                size: n.size,
                modified: None,
                link: None,
            })
            .collect();
        let panel = &mut self.panels[self.active];
        panel.panelize(entries, HashMap::new());
        // dates come from a re-stat in a worker
        if let Err(e) = panel.reload() {
            self.mode = Mode::Message(e.to_string());
        } else {
            self.mode = Mode::Normal;
        }
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
        let dir = st.path();
        let names: Vec<OsString> = (st.targets().into_iter())
            .map(|i| st.current().children[i].name.clone())
            .collect();
        if let Some(r) = st.remote.clone() {
            let count = Arc::new(AtomicUsize::new(0));
            st.deleting = Some((names.clone(), count.clone()));
            self.stats = Some(st);
            let side = Side::Remote(r);
            return self.spawn_xfer((side.clone(), side.clone()), move |_, _| {
                names.iter().try_for_each(|n| {
                    side.delete(&dir.join(n))?;
                    count.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                })
            });
        }
        // in order, the first error stops the batch
        let mut done = Vec::new();
        let mut error = None;
        for name in names {
            let target = dir.join(&name);
            let result = if to_trash {
                ops::trash(&target)
            } else {
                ops::delete(&target)
            };
            if let Err(e) = result {
                error = Some(e.to_string());
                break;
            }
            done.push(name);
        }
        st.drop_names(&done);
        // `tick` brings the stats back once any error message is closed
        self.stats = Some(st);
        self.finish(error);
    }

    /// F3 view, F4 edit, Shift+F4 external editor on the selected file of File Stats.
    fn stats_open(&mut self, st: Stats, key: KeyEvent) {
        let Some(path) = st
            .selected()
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
            KeyCode::F(3) if cmdline::opens_externally(&path) => self.open_preview(path),
            KeyCode::F(4)
                if !key.modifiers.contains(KeyModifiers::SHIFT)
                    && cmdline::opens_externally(&path) =>
            {
                self.open_default(path)
            }
            KeyCode::F(3) => self.open_viewer(path),
            _ if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.external_edit(path, "code".into());
            }
            _ => self.edit_path(path),
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

    /// Back to the multi-rename form after the editor of its new names closed.
    fn restore_rename_back(&mut self) {
        if matches!(self.mode, Mode::Normal)
            && let Some((mut form, dir)) = self.rename_back.take()
        {
            let file =
                fs::File::open(dir.path().join(RENAME_NAMES_FILE)).map(std::io::BufReader::new);
            // bounded: one line more than items is already a count error
            match file.and_then(|r| r.lines().take(form.items.len() + 1).collect()) {
                Ok(lines) => form.set_names(lines),
                Err(e) => form.error = Some(format!("Cannot read the names: {e}")),
            }
            self.mode = Mode::MultiRename(form);
        }
    }

    /// F4 in the multi-rename form: the new names, one per line, in the internal editor.
    fn rename_edit_names(&mut self, mut form: RenameForm) {
        let written = TempDir::new().and_then(|dir| {
            let path = dir.path().join(RENAME_NAMES_FILE);
            let mut w = std::io::BufWriter::new(fs::File::create(&path)?);
            for (old, new) in &form.preview {
                writeln!(w, "{}", new.as_ref().unwrap_or(old))?;
            }
            w.flush()?;
            Ok((dir, path))
        });
        match written {
            Ok((dir, path)) => {
                self.rename_back = Some((form, dir));
                self.edit_path_at(path, None);
            }
            Err(e) => {
                form.error = Some(format!("Cannot write the names: {e}"));
                self.mode = Mode::MultiRename(form);
            }
        }
    }

    fn found_key(&mut self, key: KeyEvent, mut f: Found) {
        let page = self.page;
        let last = f.view.len().saturating_sub(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if f.filter.key(key) {
            f.rebuild();
            self.mode = Mode::Found(f);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') if f.running => {
                f.cancel.store(true, Ordering::Relaxed)
            }
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
            KeyCode::Char('r') if ctrl => {
                f.cancel.store(true, Ordering::Relaxed);
                return self.spawn_find(f.query);
            }
            KeyCode::F(n @ 3..=6) if ctrl => {
                f.set_sort(
                    [
                        FoundSort::Path,
                        FoundSort::Name,
                        FoundSort::Date,
                        FoundSort::Size,
                    ][usize::from(n) - 3],
                );
            }
            KeyCode::Enter | KeyCode::F(3 | 4) if f.remote => {
                self.found = Some(f);
                self.mode = Mode::Message("Remote results are only listed.".into());
                return;
            }
            KeyCode::Enter if f.view.is_empty() => {}
            KeyCode::Enter => return self.panelize_found(f),
            KeyCode::F(3 | 4) => {
                let Some(hit) = f.shown().nth(f.sel).filter(|h| !h.entry.is_dir).cloned() else {
                    self.mode = Mode::Found(f);
                    return;
                };
                if f.re.is_some() {
                    self.search.clone_from(&f.re);
                }
                self.found = Some(f);
                let line = hit.line.map(|l| l.0);
                return self.open_at(
                    PathBuf::from(hit.entry.name),
                    line,
                    key.code == KeyCode::F(4),
                );
            }
            KeyCode::Char('g') if ctrl && !f.remote => {
                f.cancel.store(true, Ordering::Relaxed);
                if let Some(h) = f.shown().nth(f.sel) {
                    let file = PathBuf::from(&h.entry.name);
                    if let Err(e) = self.panels[self.active].reveal(&file) {
                        self.mode = Mode::Message(e.to_string());
                    }
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

    /// Enter in the results: the found files (once each, in the shown order) become a virtual
    /// panel; their first matching line stays for F3/F4 there.
    fn panelize_found(&mut self, f: Found) {
        f.cancel.store(true, Ordering::Relaxed);
        if f.re.is_some() {
            self.search.clone_from(&f.re);
        }
        let (mut seen, mut lines, mut entries) = (HashSet::new(), HashMap::new(), Vec::new());
        for h in f.shown() {
            if !seen.insert(h.entry.name.clone()) {
                continue;
            }
            if let Some((n, _)) = &h.line {
                lines.insert(h.entry.name.clone(), *n);
            }
            entries.push(h.entry.clone());
        }
        self.panels[self.active].panelize(entries, lines);
    }

    /// Viewer (top line) or editor (cursor) at `line` when known; media files open the
    /// host's preview (F3) or default application (F4).
    fn open_at(&mut self, path: PathBuf, line: Option<u64>, edit: bool) {
        match (edit, cmdline::opens_externally(&path)) {
            (false, true) => self.open_preview(path),
            (true, true) => self.open_default(path),
            (true, false) => self.edit_path_at(path, line),
            (false, false) => {
                self.open_viewer(path);
                if let (Some(n), Mode::View { view, .. }) = (line, &mut self.mode)
                    && let Err(e) = view.goto_line(n)
                {
                    self.mode = Mode::Message(e.to_string());
                }
            }
        }
    }

    fn ask(&mut self, ask: Ask) {
        self.input = Some(Input {
            ask,
            text: LineEdit::default(),
            pattern: String::new(),
            each: false,
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
            Mode::Edit { editor, .. } => Ok(editor.find(re, forward, true)),
            Mode::View { view, .. } => view.search(re, forward),
            _ => return,
        };
        match found {
            Ok(true) => {}
            Ok(false) => self.note = "not found".into(),
            Err(e) => self.mode = Mode::Message(e.to_string()),
        }
    }

    /// One-by-one replace: optionally replaces the selected match, then selects the next one
    /// (to the end of the file, no wrap).
    fn replace_step(&mut self, replace: bool) {
        let (Some(re), Some(with), Mode::Edit { editor, .. }) =
            (&self.search, &self.replacing, &mut self.mode)
        else {
            self.replacing = None;
            return;
        };
        if replace {
            editor.replace_selection(re, with);
        }
        let more = editor.find(re, true, false);
        self.note = if more {
            "Replace?  [y] Yes   [n] Skip   [a] All   [Esc] Cancel".into()
        } else {
            self.replacing = None;
            "replace done".into()
        };
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
            KeyCode::Up if matches!(input.ask, Ask::Find | Ask::Replace) => {
                input.text.prev(&self.find_history);
            }
            KeyCode::Down if matches!(input.ask, Ask::Find | Ask::Replace) => {
                input.text.next(&self.find_history);
            }
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
        let exact = self.exact;
        let compile = |p: &str| search::compile_case(p, exact).map_err(|e| e.to_string());
        match input.ask {
            Ask::Goto => self.goto(&text)?,
            Ask::Select | Ask::Deselect => {
                let on = input.ask == Ask::Select;
                self.panels[self.active].select_mask(&text, on);
            }
            Ask::Find => {
                // empty answer repeats the last search
                if !text.is_empty() {
                    self.search = Some(compile(&text)?);
                    self.note = self.remember("find-history", &text);
                } else if self.search.is_none() {
                    return Err("empty pattern".into());
                }
                self.find(true);
            }
            Ask::Replace => {
                compile(&text)?;
                self.note = self.remember("find-history", &text);
                input.pattern = text;
                input.text.clear();
                input.ask = Ask::With;
                return Ok(false);
            }
            Ask::With => {
                let re = compile(&input.pattern)?;
                if input.each {
                    if re.is_match("") {
                        return Err("pattern matches empty text".into());
                    }
                    self.search = Some(re);
                    self.replacing = Some(text);
                    self.replace_step(false);
                } else {
                    if let Mode::Edit { editor, .. } = &mut self.mode {
                        self.note = format!("{} replaced", editor.replace_all(&re, &text));
                    }
                    self.search = Some(re);
                }
            }
        }
        Ok(true)
    }

    fn normal_key(&mut self, code: KeyCode) {
        let page = self.page as isize;
        let panel = &mut self.panels[self.active];
        match code {
            KeyCode::Char('q') | KeyCode::F(10) => self.mode = Mode::AskQuit,
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
        let question = delete_question(&names, entries.len(), None);
        // a symlink is removed as a link, not recursed into
        let recursive = entries.iter().any(|e| e.is_dir && !e.is_symlink);
        delete_text(
            question,
            recursive,
            self.panels[self.active].remote().is_some(),
        )
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
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
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

    /// Ctrl+M: multi-rename the selection (else the cursor entry), opened with the last settings.
    fn multi_rename(&mut self) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        if panel.is_virtual() {
            self.mode =
                Mode::Message("Multi-rename works in a directory, not in a found list.".into());
            return;
        }
        let items: Vec<_> = (panel.target_entries().into_iter())
            .map(|e| (e.name.clone(), e.modified))
            .collect();
        if items.is_empty() {
            return;
        }
        let mut form = RenameForm {
            rows: self.rename_hist(self.rename_history.len().saturating_sub(1)),
            focus: 0,
            dir: panel.path().to_owned(),
            items,
            preview: Vec::new(),
            scroll: 0,
            error: None,
            hist: self.rename_history.len(),
            names: None,
        };
        form.refresh();
        self.mode = Mode::MultiRename(form);
    }

    fn multi_rename_key(&mut self, key: KeyEvent, mut form: RenameForm) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let last = form.rows.len() - 1;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => return,
            KeyCode::Enter => return self.multi_rename_run(form),
            KeyCode::Up | KeyCode::Down if alt => {
                let len = self.rename_history.len();
                form.hist = match key.code {
                    KeyCode::Up => form.hist.saturating_sub(1),
                    _ => (form.hist + 1).min(len),
                };
                form.rows = self.rename_hist(form.hist);
                form.names = None;
                form.refresh();
            }
            KeyCode::F(2 | 3 | 8) => self.rename_preset_key(key.code, &mut form),
            KeyCode::F(4) => return self.rename_edit_names(form),
            KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.rename_undo_run(form);
            }
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => form.focus = form.focus.saturating_sub(1),
            KeyCode::PageUp => form.scroll = form.scroll.saturating_sub(fmt::RENAME_PREVIEW_ROWS),
            KeyCode::PageDown => {
                let end = form.preview.len().saturating_sub(fmt::RENAME_PREVIEW_ROWS);
                form.scroll = (form.scroll + fmt::RENAME_PREVIEW_ROWS).min(end);
            }
            code => {
                let ctrl = key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                form.rows[form.focus].key(code, ctrl);
                form.names = None;
                form.refresh();
            }
        }
        self.mode = Mode::MultiRename(form);
    }

    /// Enter: rename in a worker (remote panels too); the form stays open on a problem.
    fn multi_rename_run(&mut self, mut form: RenameForm) {
        let pairs = match form.pairs() {
            Ok(p) if p.is_empty() => Err("No name changes.".to_owned()),
            r => r,
        };
        let pairs = match pairs {
            Ok(p) => p,
            Err(e) => {
                form.error = Some(e);
                self.mode = Mode::MultiRename(form);
                return;
            }
        };
        if form.names.is_none() {
            self.note = self.remember("rename-history", &rows_to_line(&form.rows, R_PRESET));
        }
        let side = self.panels[self.active].side();
        let dir = form.dir;
        let sides = (side.clone(), side.clone());
        self.rename_running = Some(RenameBatch {
            side: side.clone(),
            dir: dir.clone(),
            pairs: pairs.clone(),
        });
        self.spawn_xfer(sides, move |p, _| rename::apply(&side, &dir, &pairs, p));
    }

    /// Ctrl+Z in the form: rename the last finished batch back in a worker.
    fn rename_undo_run(&mut self, mut form: RenameForm) {
        let Some(RenameBatch { side, dir, pairs }) = self.rename_undo.take() else {
            form.error = Some("Nothing to undo.".into());
            self.mode = Mode::MultiRename(form);
            return;
        };
        let sides = (side.clone(), side.clone());
        self.spawn_xfer(sides, move |p, _| rename::undo(&side, &dir, &pairs, p));
    }

    /// A multi-rename that renamed every pair becomes the one Ctrl+Z undoes (a cancel or an
    /// error leaves `files` short of the total).
    fn rename_done(&mut self, error: Option<&String>, progress: &ops::Progress) {
        if let Some(batch) = self.rename_running.take()
            && error.is_none()
            && progress.files.load(Ordering::Relaxed)
                == progress.files_total.load(Ordering::Relaxed)
        {
            self.rename_undo = Some(batch);
        }
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
            Mode::FindForm(f) => f.paste(&text),
            Mode::MultiRename(f) => f.paste(&text),
            Mode::SyncForm(f) => f.paste(&text),
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
        let sides = (src_side, dst_side);
        self.spawn_xfer(sides.clone(), move |c, ask| {
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
            copy_jobs(op, &jobs, &sides, c, ask)
        });
    }

    /// Ctrl+C / Ctrl+X: remember the selection (or cursor entry) of the active panel for Ctrl+V.
    fn clip_files(&mut self, op: Op) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message("Copy from an archive with F5 instead.".into());
            return;
        }
        let paths = panel.targets();
        let side = panel.side();
        if paths.is_empty() {
            return;
        }
        if matches!(side, Side::Local) {
            // also on the host clipboard, so Explorer, Finder or a file manager can paste them
            #[cfg(not(test))]
            {
                let host = paths.clone();
                thread::spawn(move || tc_core::hostclip::write_files(&host));
            }
            // our own list is not a new host paste at the next Ctrl+V
            self.last_host = paths.clone();
        }
        self.file_clip = Some(FileClip { op, paths, side });
    }

    /// True where keys go to the file panel (no prompt, dialog or help open).
    pub fn panel_keys(&self) -> bool {
        matches!(self.mode, Mode::Normal) && self.idle()
    }

    /// Ctrl+V: copy or move the files of the host clipboard (if they changed since the last
    /// Ctrl+V) or of Ctrl+C / Ctrl+X into the active panel's directory; conflicts ask.
    pub fn paste(&mut self) {
        // the host tools (PowerShell starts in about a second) run in a worker
        let dest = self.panels[self.active].path().to_owned();
        self.spawn("Reading clipboard", dest, 1, false, |_, cancel| {
            // tests never read the developer's real clipboard
            let files = if cfg!(test) {
                Vec::new()
            } else {
                tc_core::hostclip::read_files()
            };
            Done::HostFiles((!cancel.load(Ordering::Relaxed)).then_some(files))
        });
    }

    fn paste_with(&mut self, host: Vec<PathBuf>) {
        let fresh = !host.is_empty() && host != self.last_host;
        let host_clip = fresh.then(|| FileClip {
            op: Op::Copy,
            paths: host.clone(),
            side: Side::Local,
        });
        self.last_host = host;
        let Some(clip) = host_clip.or_else(|| self.file_clip.clone()) else {
            self.mode =
                Mode::Message("Nothing copied or cut yet: Ctrl+C or Ctrl+X on files first.".into());
            return;
        };
        let active = &self.panels[self.active];
        if active.archive().is_some() {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        let dest = active.path().to_owned();
        let sides = (clip.side.clone(), active.side());
        let jobs: Vec<(PathBuf, PathBuf)> = clip
            .paths
            .iter()
            .filter_map(|src| Some((src.clone(), dest.join(src.file_name()?))))
            .filter(|(src, target)| src != target)
            .collect();
        if jobs.is_empty() {
            self.mode = Mode::Message("Those files are already in this directory.".into());
            return;
        }
        let op = clip.op;
        self.cut_pending = op == Op::Move;
        self.spawn_xfer(sides.clone(), move |c, ask| {
            copy_jobs(op, &jobs, &sides, c, ask)
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
            // reap it when it exits, else it stays a zombie until tc quits
            Ok(mut child) => drop(thread::spawn(move || child.wait())),
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

    fn connections_key(&mut self, key: KeyEvent, sel: usize) {
        let mut sel = sel.min(self.connections.len().saturating_sub(1));
        if list_nav(&mut self.list_filter, &self.connections, &mut sel, key) {
            self.mode = Mode::Connections(sel);
            return;
        }
        let shown = list_rows(&self.connections, &self.list_filter).contains(&sel);
        let url = self
            .connections
            .get(sel)
            .filter(|_| shown)
            .map(|u| u.to_string_lossy().into_owned());
        match (key.code, url) {
            (KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q'), _) => return,
            (KeyCode::Enter, Some(url)) => return self.connect(url),
            (KeyCode::Insert | KeyCode::Char('a'), _) => {
                self.list_filter = Filter::default();
                self.mode = Mode::ConnEdit(None, LineEdit::from("sftp://".to_owned()));
                return;
            }
            (KeyCode::F(4) | KeyCode::Char('e'), Some(url)) => {
                self.list_filter = Filter::default();
                self.mode = Mode::ConnEdit(Some(sel), LineEdit::from(url));
                return;
            }
            (KeyCode::Delete, Some(_)) => {
                self.connections.remove(sel);
                self.connections_save();
                sel = list_rows(&self.connections, &self.list_filter)
                    .first()
                    .copied()
                    .unwrap_or(0);
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

    fn edit_path(&mut self, path: PathBuf) {
        self.edit_path_at(path, None);
    }

    /// Editor with the cursor on `line` (1-based) when given.
    fn edit_path_at(&mut self, path: PathBuf, line: Option<u64>) {
        let ask = |text: String, can_load| Mode::AskViewer {
            path: path.clone(),
            text,
            can_load,
        };
        match edit::check_size(&path) {
            Ok(SizeCheck::Ok) => self.spawn_load(path.clone(), line),
            Ok(SizeCheck::Large(n)) => {
                self.mode = ask(
                    format!(
                        "File is {}, open in viewer instead?\n\n[y] Viewer   [n] Edit   [Esc] Cancel",
                        edit::human(n)
                    ),
                    true,
                )
            }
            Err(e @ Error::OutOfMemory { .. }) => {
                self.mode = ask(
                    format!("{e}. Open in viewer?\n\n[y] Viewer   [Esc] Cancel"),
                    false,
                )
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

    fn spawn_load(&mut self, path: PathBuf, line: Option<u64>) {
        let total = std::fs::metadata(&path).map_or(1, |m| m.len());
        let p = path.clone();
        self.spawn("Loading", path, total, false, move |progress, cancel| {
            let mut loaded = Editor::open(&p, progress, cancel);
            if let (Ok(e), Some(n)) = (&mut loaded, line) {
                e.goto_line(usize::try_from(n).unwrap_or(usize::MAX));
            }
            Done::Loaded(loaded)
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
        if editor.changed_on_disk() {
            self.mode = Mode::AskSave {
                editor,
                to_utf8,
                exit,
                step: Step::Changed,
            };
        } else {
            self.save_prompts(editor, exit, to_utf8, from);
        }
    }

    fn save_prompts(&mut self, editor: Editor, exit: bool, to_utf8: bool, from: Step) {
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

    fn xfer_tick(&mut self) {
        let Mode::Xfer(x) = &mut self.mode else {
            return;
        };
        let done = x.progress.done.load(Ordering::Relaxed);
        x.eta.push(x.started.elapsed().as_secs_f64(), done as f64);
        match x.rx.try_recv() {
            Ok(XMsg::Ask(src, target)) => x.ask = Some((src, target)),
            Ok(XMsg::Done(error)) => {
                let progress = x.progress.clone();
                self.mode = Mode::Normal;
                self.rename_done(error.as_ref(), &progress);
                // a failed cut keeps its clipboard, so Ctrl+V can retry
                if mem::take(&mut self.cut_pending) && error.is_none() {
                    self.file_clip = None;
                }
                if let Some(st) = &mut self.stats
                    && let Some((names, count)) = st.deleting.take()
                {
                    // whatever the error: what was deleted leaves the tree
                    let n = count.load(Ordering::Relaxed).min(names.len());
                    st.drop_names(&names[..n]);
                }
                self.finish(error);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.rename_running = None;
                self.mode = Mode::Message("worker thread failed".into())
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
        if matches!(self.mode, Mode::Normal)
            && let Some(f) = self.found.take()
        {
            self.mode = Mode::Found(f);
        }
        self.restore_sync_back();
        self.restore_rename_back();
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
        if let Mode::Stats(st) = &mut self.mode
            && st
                .job
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
            && let Some(job) = st.job.take()
        {
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

/// Copy or move `jobs` (source, target) between the sides; stops at the first error.
fn copy_jobs(
    op: Op,
    jobs: &[(PathBuf, PathBuf)],
    sides: &(Side, Side),
    c: &ops::Progress,
    ask: ops::Ask,
) -> tc_core::Result<()> {
    let (src_side, dst_side) = sides;
    let srcs: Vec<PathBuf> = jobs.iter().map(|(s, _)| s.clone()).collect();
    vfs::scan(src_side, &srcs, c);
    let local = matches!((src_side, dst_side), (Side::Local, Side::Local));
    let mv = op == Op::Move;
    jobs.iter().try_for_each(|(src, target)| {
        if !local {
            vfs::transfer(src_side, src, dst_side, target, mv, ask, c)
        } else if mv {
            ops::move_(src, target, ask, c)
        } else {
            ops::copy(src, target, ask, c)
        }
    })
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
    fn multi_rename_counter_history_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        for _ in 0..3 {
            key(&mut app, KeyCode::Backspace); // "[N]"
        }
        for c in "x_[Q]".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.error.as_deref().is_some_and(|e| e.contains("[Q]")));
        key(&mut app, KeyCode::Enter); // refused, form stays
        assert!(matches!(app.mode, Mode::MultiRename(_)));
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Char('C'));
        key(&mut app, KeyCode::Char(']'));
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(f.preview[1].1.as_deref(), Ok("x_2.txt"));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        let read = |n: &str| std::fs::read_to_string(dir.path().join(n)).unwrap();
        assert_eq!(
            (read("x_1.txt"), read("x_2.txt")),
            ("a.txt".into(), "b.txt".into())
        );
        assert!(dir.path().join("c.txt").exists() && !dir.path().join("a.txt").exists());
        // reopens with the last settings; Alt+Down past the history gives the defaults
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(form_text(&f.rows, R_NAME), "x_[C]");
        with(&mut app, KeyCode::Down, KeyModifiers::ALT);
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(form_text(&f.rows, R_NAME), "[N]");
        assert_eq!(f.preview[0].1.as_deref(), Ok(f.preview[0].0.as_str()));
        key(&mut app, KeyCode::Enter); // nothing changes: refused
        assert!(matches!(&app.mode, Mode::MultiRename(f) if f.error.is_some()));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
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

    fn stats_app(dir: &Path) -> App {
        let mut app = two_panels(dir, dir);
        with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        app
    }

    #[test]
    fn stats_feeds_entries_to_panel() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "x".repeat(30)).unwrap();
        std::fs::write(tmp.path().join("b"), "x".repeat(20)).unwrap();
        std::fs::create_dir(tmp.path().join("c")).unwrap();
        let names = |app: &App| -> Vec<PathBuf> {
            let p = &app.panels[app.active];
            assert!(p.is_virtual());
            p.entries().iter().map(|e| PathBuf::from(&e.name)).collect()
        };
        let mut app = stats_app(tmp.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Insert); // b
        key(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.mode, Mode::Normal));
        settle(&mut app);
        assert_eq!(names(&app), [tmp.path().join("b")]);
        let mut app = stats_app(tmp.path());
        key(&mut app, KeyCode::Char('p'));
        settle(&mut app);
        let mut all = names(&app);
        all.sort();
        assert_eq!(all, ["a", "b", "c"].map(|n| tmp.path().join(n)));
    }

    #[test]
    fn stats_marks_and_deletes_all_marked() {
        let tmp = tempfile::tempdir().unwrap();
        for (n, size) in [("a", 30), ("b", 20), ("c", 10)] {
            std::fs::write(tmp.path().join(n), "x".repeat(size)).unwrap();
        }
        #[cfg(unix)]
        let odd = {
            use std::os::unix::ffi::OsStrExt;
            let odd = OsStr::from_bytes(b"f\xff");
            std::fs::write(tmp.path().join(odd), "x").unwrap();
            odd
        };
        let mut app = stats_app(tmp.path());
        key(&mut app, KeyCode::Char(' ')); // a, moves down
        key(&mut app, KeyCode::Insert); // b
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Insert); // b unmarked again, moves down
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Insert); // b marked
        key(&mut app, KeyCode::F(8));
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert!(st.confirm.as_ref().is_some_and(|t| t.contains("2 items (")));
        key(&mut app, KeyCode::Char('p'));
        app.tick();
        assert!(!tmp.path().join("a").exists() && !tmp.path().join("b").exists());
        assert!(tmp.path().join("c").exists());
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert_eq!(st.root.size, if cfg!(unix) { 11 } else { 10 });
        assert!(st.marked.is_empty());
        #[cfg(unix)]
        {
            assert!(tmp.path().join(odd).exists());
            assert_eq!(st.root.files, 2);
        }
    }

    #[cfg(unix)]
    #[test]
    fn stats_batch_stops_at_first_error() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "x".repeat(30)).unwrap();
        let ro = tmp.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::write(ro.join("f"), "x".repeat(20)).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        let restore = || std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755));
        if std::fs::write(ro.join("probe"), "").is_ok() {
            return restore().unwrap(); // root ignores permissions
        }
        let mut app = stats_app(tmp.path());
        key(&mut app, KeyCode::Insert); // a
        key(&mut app, KeyCode::Insert); // ro
        key(&mut app, KeyCode::F(8));
        key(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.mode, Mode::Message(_)));
        app.tick();
        key(&mut app, KeyCode::Esc);
        app.tick();
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert!(!tmp.path().join("a").exists() && ro.join("f").exists());
        let names: Vec<_> = st.root.children.iter().map(|n| n.name.clone()).collect();
        assert_eq!(names, ["ro"]);
        assert!(st.marked.contains(OsStr::new("ro")));
        restore().unwrap();
    }

    #[test]
    fn remote_stats_deletes_all_marked() {
        let left = tempfile::tempdir().unwrap();
        let mem = tc_core::vfs::mem::Mem::new();
        for (n, size) in [("/a", 3), ("/b", 2), ("/c", 1)] {
            mem.put(n, &b"x".repeat(size));
        }
        let mut app = two_panels(left.path(), left.path());
        app.active = 1;
        app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
        settle(&mut app);
        with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::F(8));
        key(&mut app, KeyCode::Char('p'));
        settle(&mut app);
        app.tick();
        assert!(mem.get("/a").is_none() && mem.get("/b").is_none() && mem.get("/c").is_some());
        assert!(
            matches!(&app.mode, Mode::Stats(st) if st.root.size == 1 && st.current().children.len() == 1)
        );
    }

    #[test]
    fn slash_filters_stats_and_acts_on_shown_rows_only() {
        let tmp = tempfile::tempdir().unwrap();
        for (n, size) in [("Alpha", 30), ("beta", 20), ("gamma", 10)] {
            std::fs::write(tmp.path().join(n), "x".repeat(size)).unwrap();
        }
        let mut app = two_panels(tmp.path(), tmp.path());
        with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        let shown = |app: &App| match &app.mode {
            Mode::Stats(st) => st
                .shown()
                .map(|n| n.name.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            _ => panic!("not in stats"),
        };
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, "AM");
        assert_eq!(shown(&app), ["gamma"]);
        key(&mut app, KeyCode::Enter);
        // delete acts on the shown row, not on the first child
        key(&mut app, KeyCode::F(8));
        key(&mut app, KeyCode::Char('p'));
        app.tick();
        assert!(!tmp.path().join("gamma").exists() && tmp.path().join("Alpha").exists());
        assert!(shown(&app).is_empty());
        key(&mut app, KeyCode::Esc);
        assert_eq!(shown(&app), ["Alpha", "beta"]);
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn slash_filters_bookmarks_and_enter_goes_to_the_shown_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("one"), tmp.path().join("two"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        app.hotlist = vec![a, b.clone()];
        with(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, "TW");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.panels[app.active].path(), b);
    }

    #[test]
    fn stats_info_popup_and_back() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("f"), "abc").unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        key(&mut app, KeyCode::Char('i'));
        let Mode::Message(text) = &app.mode else {
            panic!("no popup")
        };
        assert!(
            text.contains(&*tmp.path().join("f").to_string_lossy())
                && text.contains("Files:       1")
        );
        key(&mut app, KeyCode::Char('x'));
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
        key(&mut app, KeyCode::Tab); // only the shown value changes
        assert_eq!(top(&app), "big");
        key(&mut app, KeyCode::Char('s')); // active key again: smallest first
        assert_eq!(top(&app), "big"); // cursor stays on its entry
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert_eq!(st.current().children[0].name, "many");
        key(&mut app, KeyCode::Char('C'));
        assert_eq!(top(&app), "big");
        key(&mut app, KeyCode::Home);
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
    fn quit_asks_first() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(10));
        assert!(matches!(app.mode, Mode::AskQuit) && !app.quit);
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal) && !app.quit);
        key(&mut app, KeyCode::Char('q'));
        key(&mut app, KeyCode::Enter);
        assert!(app.quit);
    }

    #[test]
    fn footer_follows_screen_and_f10_closes_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        assert_eq!(app.footer_keys(), &FOOTER);
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::FindForm(_)));
        assert_eq!(app.footer_keys(), &FIND_FORM_FOOTER);
        key(&mut app, KeyCode::F(10));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn f1_help_opens_on_forms_and_lists() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        for (code, mods) in [
            (KeyCode::Char('f'), KeyModifiers::CONTROL),
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
            (KeyCode::Char('n'), KeyModifiers::CONTROL),
            (KeyCode::F(1), KeyModifiers::ALT),
        ] {
            app.on_key(KeyEvent::new(code, mods));
            let title = fmt::help(&app.mode).0;
            key(&mut app, KeyCode::F(1));
            assert!(app.help && title != "Help", "{title} {code:?}");
            key(&mut app, KeyCode::Esc);
            assert!(!app.help && !matches!(app.mode, Mode::Normal));
            key(&mut app, KeyCode::Esc);
            assert!(matches!(app.mode, Mode::Normal));
        }
    }

    #[test]
    fn footer_keys_are_documented_in_their_help() {
        let screens: [(&Footer, &[&str]); 9] = [
            (&FOOTER, &[fmt::HELP_MAIN]),
            (&VIEW_FOOTER, &[fmt::HELP_VIEW]),
            (&EDIT_FOOTER, &[fmt::HELP_EDIT]),
            (&FOUND_FOOTER, &[fmt::HELP_FOUND]),
            (&SYNC_FOOTER, &[fmt::HELP_SYNC]),
            (&FIND_FORM_FOOTER, &[fmt::HELP_FIND_FORM]),
            (&FORM_FOOTER, &[fmt::HELP_RENAME, fmt::HELP_SYNC_FORM]),
            (&STATS_FOOTER, &[fmt::HELP_STATS]),
            (&PROCS_FOOTER, &[fmt::HELP_PROCS]),
        ];
        for (footer, helps) in screens {
            for &(label, ..) in footer {
                let key = label
                    .split(' ')
                    .next()
                    .unwrap_or_default()
                    .replace("S-", "Shift+");
                // F1 opens the help itself; Tab and arrows are navigation
                if key == "F1" {
                    continue;
                }
                for help in helps {
                    let found = help.lines().any(|l| {
                        let col = l.get(..16).unwrap_or(l);
                        col.split(' ').chain(col.split('/')).any(|t| t == key)
                    });
                    assert!(found, "{label} missing in help");
                }
            }
        }
    }

    #[test]
    fn f9_menu_runs_the_chosen_action() {
        let tmp = tempfile::tempdir().unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        app.on_key(KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Menu(..)));
        let i = MENUS[0]
            .1
            .iter()
            .position(|m| m.1 == KeyCode::F(7) && m.2 == N)
            .unwrap();
        for _ in 0..i {
            app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Mkdir(_)));

        // click the "Go" title, then its first entry (home directory)
        app.mode = Mode::Normal;
        let click = |col, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(click(menu_x(1), 0));
        assert!(matches!(app.mode, Mode::Menu(1, _)));
        app.on_mouse(click(menu_x(1) + 2, 2));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn help_line_click_runs_action() {
        let tmp = tempfile::tempdir().unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        key(&mut app, KeyCode::F(1));
        let line = fmt::HELP_MAIN
            .lines()
            .position(|l| l.starts_with("Ctrl+B "))
            .unwrap();
        app.help_click(line);
        assert!(!app.help && matches!(app.mode, Mode::Hotlist(_)));
    }

    #[test]
    fn f1_help_esc_closes_typing_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(1));
        assert!(app.help);
        for c in "ncdu".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.help_entries().len(), 1);
        assert!(app.help_entries()[0].0.contains("FILE STATS"));
        key(&mut app, KeyCode::Esc);
        assert!(app.help && app.help_filter.is_empty());
        key(&mut app, KeyCode::Esc);
        assert!(!app.help && matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn help_down_scrolls_without_key_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(1));
        type_keys(&mut app, "== search");
        assert!(app.help_entries().iter().all(|e| e.1.is_none()));
        key(&mut app, KeyCode::Down);
        assert_eq!(app.help_scroll, 1);
    }

    #[test]
    fn help_filter_and_enter_runs_selected_key() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(1));
        // first key line is selected; Down moves to the next key line
        let first = app.help_sel;
        key(&mut app, KeyCode::Down);
        assert!(app.help_sel > first && app.help_action_at(app.help_sel).is_some());
        key(&mut app, KeyCode::Up);
        assert_eq!(app.help_sel, first);
        // filter to Ctrl+B bookmarks, Enter runs it
        for c in "bookmarks".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert!(app.help_action_at(app.help_sel).is_some());
        key(&mut app, KeyCode::Enter);
        assert!(!app.help && matches!(app.mode, Mode::Hotlist(_)));
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

    /// Fills the find form (name, then content regex, which ticks its checkbox) and searches.
    fn find_in_form(app: &mut App, mask: &str, content: Option<&str>) {
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        type_keys(app, mask);
        if let Some(re) = content {
            form_focus(app, F_CONTENT);
            type_keys(app, re);
        }
        key(app, KeyCode::Enter);
    }

    fn form_focus(app: &mut App, label: &str) {
        let Mode::FindForm(f) = &mut app.mode else {
            panic!("no find form");
        };
        f.focus = f.rows.iter().position(|r| r.label() == label).unwrap();
    }

    fn wait_found(app: &mut App) {
        while matches!(&app.mode, Mode::Found(f) if f.running) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn find_form_bad_field_keeps_form_open() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        find_in_form(&mut app, "*", None); // starts a search
        assert!(matches!(app.mode, Mode::Found(_)));
        key(&mut app, KeyCode::Esc); // stop
        while matches!(&app.mode, Mode::Found(f) if f.running) {
            app.tick();
        }
        key(&mut app, KeyCode::Esc); // close
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        type_keys(&mut app, "*");
        form_focus(&mut app, F_MIN);
        type_keys(&mut app, "10X");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(&app.mode, Mode::FindForm(f) if f.error.is_some()));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
        // Ctrl+F reopens the last form; a typed or pasted regex ticks content search
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        let Mode::FindForm(f) = &mut app.mode else {
            panic!("no find form");
        };
        assert_eq!(
            (f.text(F_NAME), f.text(F_MIN), f.error.as_deref()),
            ("**", "10X", None)
        );
        assert!(!f.checked(F_CONTENT_ON));
        form_focus(&mut app, F_CONTENT);
        app.on_paste("needle".into());
        assert!(
            matches!(&app.mode, Mode::FindForm(f) if f.checked(F_CONTENT_ON) && f.text(F_CONTENT) == "needle")
        );
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
        with(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(matches!(&app.mode, Mode::FindForm(_)));
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
    fn editor_shift_up_down_selects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        with(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        with(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(
            app.mode.editor_mut().unwrap().copy().as_deref(),
            Some("one\ntwo\n")
        );
        with(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(
            app.mode.editor_mut().unwrap().copy().as_deref(),
            Some("one\n")
        );
        key(&mut app, KeyCode::Down);
        assert_eq!(app.mode.editor_mut().unwrap().copy(), None);
    }

    #[test]
    fn editor_replace_each_and_reload_warning() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "ab ab\nab\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        with(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
        type_text(&mut app, "ab");
        type_text(&mut app, "X");
        key(&mut app, KeyCode::Char('y')); // first ab -> X
        key(&mut app, KeyCode::Char('n')); // second skipped
        key(&mut app, KeyCode::Char('a')); // rest
        assert!(app.replacing.is_none());
        let ed = app.mode.editor_mut().unwrap();
        assert_eq!(ed.visible(5, 9), ["X ab", "X", ""]);

        // someone else writes the file: saving asks first
        std::fs::write(&f, "other\n").unwrap();
        let t = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(t)
            .unwrap();
        key(&mut app, KeyCode::F(2));
        assert!(matches!(
            app.mode,
            Mode::AskSave {
                step: Step::Changed,
                ..
            }
        ));
        key(&mut app, KeyCode::Char('n'));
        assert!(matches!(app.mode, Mode::Edit { .. }));
        key(&mut app, KeyCode::F(2));
        key(&mut app, KeyCode::Char('y'));
        wait(&mut app);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "X ab\nX\n");
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
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (0, 0));
        key(&mut app, KeyCode::F(3));
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
    fn viewer_f4_edits_at_top_line_and_shared_keys_work() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a\nb\nFoo\nc\nfoo\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        with(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        type_text(&mut app, "foo");
        assert_eq!(app.mode_view_percent(), 28);
        with(&mut app, KeyCode::F(7), KeyModifiers::SHIFT);
        assert_eq!(app.mode_view_percent(), 71);
        with(&mut app, KeyCode::Char('c'), KeyModifiers::ALT);
        assert_eq!(app.note, "case sensitive");
        with(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        type_text(&mut app, "2");
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        assert_eq!(app.mode.editor_mut().unwrap().cursor.0, 1);
        // Ctrl+S saves like F2
        key(&mut app, KeyCode::Char('x'));
        with(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(std::fs::read_to_string(&file).unwrap().starts_with("a\nxb"));
    }

    #[test]
    fn wheel_moves_the_cursor_of_the_panel_under_the_pointer_by_3() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..10 {
            std::fs::write(dir.path().join(format!("f{n}")), "x").unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        app.areas = [Rect::new(0, 0, 10, 10), Rect::new(10, 0, 10, 10)];
        let wheel = |app: &mut App, kind, column| {
            app.on_mouse(MouseEvent {
                kind,
                column,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        };
        wheel(&mut app, MouseEventKind::ScrollDown, 12);
        assert_eq!((app.panels[0].cursor(), app.panels[1].cursor()), (0, 3));
        wheel(&mut app, MouseEventKind::ScrollUp, 12);
        assert_eq!(app.panels[1].cursor(), 0);
    }

    #[test]
    fn delete_text_lists_the_answers_and_no_trash_on_remote() {
        let local = delete_text("Delete a?".into(), true, false);
        assert!(
            local.contains("recursively")
                && local.ends_with("[y] Trash   [p] Delete permanently   [Esc] Cancel")
        );
        let remote = delete_text("Delete a?".into(), false, true);
        assert!(
            !remote.contains("[y]") && remote.ends_with("[p] Delete permanently   [Esc] Cancel")
        );
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
        find_in_form(&mut app, "*.txt", Some("hello"));
        wait_found(&mut app);
        assert!(matches!(&app.mode, Mode::Found(f) if f.list.len() == 1));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal) && app.panels[0].is_virtual());
        assert_eq!(app.panels[0].targets(), [dir.path().join("a.txt")]);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Esc); // back to the directory
        assert!(!app.panels[0].is_virtual());
        // Esc cancels a running search, a second Esc closes
        find_in_form(&mut app, "", None);
        key(&mut app, KeyCode::Esc);
        while matches!(&app.mode, Mode::Found(f) if f.running) {
            app.tick();
        }
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn found_sorts_by_size_name_and_reverses() {
        let dir = tempfile::tempdir().unwrap();
        for (n, len) in [("b.txt", 3), ("a.txt", 10), ("c.txt", 1)] {
            std::fs::write(dir.path().join(n), vec![b'x'; len]).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        find_in_form(&mut app, "*.txt", None);
        wait_found(&mut app);
        let shown = |app: &App| match &app.mode {
            Mode::Found(f) => f
                .shown()
                .map(|h| (h.entry.size, h.entry.name.clone()))
                .collect::<Vec<_>>(),
            _ => panic!("no results"),
        };
        let names = |app: &App| {
            shown(app)
                .iter()
                .map(|(_, n)| {
                    Path::new(n)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>()
        };
        with(&mut app, KeyCode::F(6), KeyModifiers::CONTROL);
        assert!(matches!(app.mode, Mode::Found(_)));
        assert_eq!(names(&app), ["c.txt", "b.txt", "a.txt"]);
        key(&mut app, KeyCode::Home); // on c.txt
        with(&mut app, KeyCode::F(6), KeyModifiers::CONTROL);
        assert_eq!(names(&app), ["a.txt", "b.txt", "c.txt"]);
        assert!(
            matches!(&app.mode, Mode::Found(f) if f.sel == 2 && fmt::found_title(f).contains("[size desc]"))
        );
        with(&mut app, KeyCode::F(4), KeyModifiers::CONTROL);
        assert_eq!(names(&app), ["a.txt", "b.txt", "c.txt"]);
        assert!(matches!(&app.mode, Mode::Found(f) if fmt::found_title(f).contains("[name]")));
    }

    #[test]
    fn found_sort_keeps_streamed_hits_sorted() {
        let (tx, rx) = mpsc::channel();
        let mut f = Found {
            list: Vec::new(),
            view: Vec::new(),
            filter: Filter::default(),
            sort: None,
            sel: 0,
            running: true,
            remote: false,
            error: Arc::default(),
            re: None,
            query: find::Query::default(),
            rx,
            cancel: Arc::default(),
        };
        f.set_sort(FoundSort::Size);
        let hit = |size| Hit {
            entry: Entry {
                size,
                name: OsString::from(format!("n{size}")),
                is_dir: false,
                is_symlink: false,
                modified: None,
                link: None,
            },
            line: None,
        };
        for size in [5, 1, 9] {
            tx.send(hit(size)).unwrap();
        }
        f.pump();
        for size in [3, 7, 0] {
            tx.send(hit(size)).unwrap();
        }
        f.pump();
        let sizes: Vec<u64> = f.shown().map(|h| h.entry.size).collect();
        assert_eq!(sizes, [0, 1, 3, 5, 7, 9]);
        assert_eq!(f.shown_len(), 6);
        // A filter keeps the cursor row, not an arbitrary hit of the unsorted view.
        f.sel = 0;
        f.filter.line = LineEdit::from("n".to_owned());
        f.rebuild();
        assert_eq!((f.sel, f.shown().next().map(|h| h.entry.size)), (0, Some(0)));
    }

    #[test]
    fn found_ctrl_r_searches_again_and_slash_filters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        std::fs::write(dir.path().join("B.txt"), "x").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        find_in_form(&mut app, "*.txt", None);
        wait_found(&mut app);
        std::fs::write(dir.path().join("c.txt"), "x").unwrap();
        with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        wait_found(&mut app);
        let n = |app: &App| match &app.mode {
            Mode::Found(f) => (f.list.len(), f.shown_len()),
            _ => panic!("no results"),
        };
        assert_eq!(n(&app), (3, 3));
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, "b.");
        assert_eq!(n(&app), (3, 1));
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter); // panelizes the shown result only
        assert_eq!(app.panels[0].entries().len(), 1);
    }

    #[test]
    fn found_enter_with_empty_filter_keeps_results() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        find_in_form(&mut app, "*.txt", None);
        wait_found(&mut app);
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, "zzz");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        assert!(matches!(&app.mode, Mode::Found(f) if f.shown_len() == 0));
    }

    #[test]
    fn q_and_f10_close_found_results_and_the_menu() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        find_in_form(&mut app, "*", None);
        wait_found(&mut app);
        key(&mut app, KeyCode::Char('q'));
        assert!(matches!(app.mode, Mode::Normal));
        key(&mut app, KeyCode::F(9));
        key(&mut app, KeyCode::F(10));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn stats_ctrl_r_rescans_subtree_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub/x"), "123").unwrap();
        std::fs::write(tmp.path().join("y"), "12345").unwrap();
        let mut app = stats_app(tmp.path());
        key(&mut app, KeyCode::Down); // sub (3 bytes) below y
        key(&mut app, KeyCode::Enter);
        std::fs::write(tmp.path().join("sub/z"), "x".repeat(10)).unwrap();
        // Esc cancels and keeps the old tree
        with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Esc);
        assert!(matches!(&app.mode, Mode::Stats(st) if !st.scanning() && st.root.size == 8));
        with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert_eq!((st.current().files, st.current().size), (2, 13));
        assert_eq!((st.root.files, st.root.size), (3, 18));
        key(&mut app, KeyCode::Backspace);
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert!(st.current().children.iter().any(|n| n.name == "y"));
    }

    #[test]
    fn stats_ctrl_r_after_shrink_keeps_cursor_valid() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "1234567").unwrap();
        std::fs::write(tmp.path().join("b"), "12").unwrap();
        let mut app = stats_app(tmp.path());
        key(&mut app, KeyCode::Down); // b, the last row
        std::fs::remove_file(tmp.path().join("a")).unwrap();
        with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert_eq!((st.current().children.len(), st.root.size), (1, 2));
        assert_eq!(st.selected().map(|n| n.name.clone()), Some("b".into()));
    }

    #[test]
    fn found_lines_open_at_match_and_feed_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/b.txt"), "x\nhello one\ny\nhello two\n").unwrap();
        let mut app = two_panels(d, d);
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        form_focus(&mut app, F_EVERY);
        key(&mut app, KeyCode::Char(' '));
        form_focus(&mut app, F_CONTENT);
        type_keys(&mut app, "hello");
        key(&mut app, KeyCode::Enter);
        wait_found(&mut app);
        let Mode::Found(f) = &app.mode else {
            panic!("no results");
        };
        let lines: Vec<_> = f.list.iter().map(|h| h.line.clone().unwrap().0).collect();
        assert_eq!(lines, [2, 4]);
        assert!(fmt::found_row(&f.list[1]).ends_with("b.txt:4: hello two"));
        // F3 opens the second hit at its line; closing the viewer brings the list back
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        let Mode::View { view, .. } = &mut app.mode else {
            panic!("no viewer");
        };
        assert_eq!(view.lines(1, 80).unwrap(), ["hello two"]);
        key(&mut app, KeyCode::Esc);
        app.tick();
        assert!(matches!(&app.mode, Mode::Found(f) if f.sel == 1));
        // Enter: one panel row per file, shown below the search root; F3 there at the first hit
        key(&mut app, KeyCode::Enter);
        let file = d.join("sub/b.txt");
        assert_eq!(app.panels[0].entries().len(), 1);
        assert_eq!(app.panels[0].hit_line(file.as_os_str()), Some(2));
        let today = jiff::ZonedDateTime::now().date();
        let shown = &fmt::entry_cols(&app, 0, &app.panels[0].entries()[0], today)[0];
        assert_eq!(
            shown.trim(),
            Path::new("sub").join("b.txt").to_string_lossy()
        );
        key(&mut app, KeyCode::F(3));
        let Mode::View { view, .. } = &mut app.mode else {
            panic!("no viewer");
        };
        assert_eq!(view.lines(1, 80).unwrap(), ["hello one"]);
        assert!(app.search.as_ref().is_some_and(|re| re.is_match("hello")));
        key(&mut app, KeyCode::Esc);
        app.tick();
        // F4 puts the editor cursor on the hit
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        assert!(matches!(&app.mode, Mode::Edit { editor, .. } if editor.cursor.0 == 1));
        key(&mut app, KeyCode::Esc);
        // Ctrl+G: the real directory with the cursor on the file
        with(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        settle(&mut app);
        let p = &app.panels[0];
        assert!(!p.is_virtual() && p.path() == d.join("sub"));
        assert_eq!(p.current().unwrap().name, "b.txt");
    }

    #[test]
    fn rename_presets_save_load_delete() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        let file = dir.path().join("presets");
        app.rename_presets_file = Some(file.clone());
        key(&mut app, KeyCode::Down); // skip ".."
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        for _ in 0..3 {
            key(&mut app, KeyCode::Backspace);
        }
        type_keys(&mut app, "x[N]");
        let preset_row = |app: &mut App| {
            let Mode::MultiRename(f) = &mut app.mode else {
                panic!("no form");
            };
            f.focus = f.rows.len() - 1;
        };
        preset_row(&mut app);
        key(&mut app, KeyCode::F(2)); // no name yet
        assert!(
            matches!(&app.mode, Mode::MultiRename(f) if f.error.as_deref().is_some_and(|e| e.contains("name")))
        );
        type_keys(&mut app, "p1");
        key(&mut app, KeyCode::F(2));
        assert!(std::fs::read_to_string(&file).unwrap().starts_with("p1\t"));
        let Mode::MultiRename(f) = &mut app.mode else {
            panic!("no form");
        };
        f.rows = rename_rows();
        key(&mut app, KeyCode::F(3));
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(
            (form_text(&f.rows, R_NAME), form_text(&f.rows, R_PRESET)),
            ("x[N]", "p1")
        );
        assert_eq!(f.preview[0].1.as_deref(), Ok("xa.txt"));
        key(&mut app, KeyCode::F(8));
        assert!(app.rename_presets.is_empty());
        assert!(std::fs::read_to_string(&file).unwrap().is_empty());
    }

    #[test]
    fn rename_pasted_names() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        app.on_paste("x\ny\n".into());
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(f.preview[1].1.as_deref(), Ok("y"));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(dir.path().join("x").exists() && dir.path().join("y").exists());
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        app.on_paste("z".into()); // one line: into the field
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.names.is_none() && form_text(&f.rows, R_NAME).ends_with('z'));
        app.on_paste("x\ny\nz".into());
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.names.is_none() && f.error.as_deref().is_some_and(|e| e.contains("names for")));
    }

    #[test]
    fn rename_f4_edits_names() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::F(4));
        let names_file = |app: &App| {
            let (_, tmp) = app.rename_back.as_ref().expect("no form stashed");
            tmp.path().join(RENAME_NAMES_FILE)
        };
        assert_eq!(std::fs::read_to_string(names_file(&app)).unwrap(), "a\nb\n");
        assert!(!matches!(app.mode, Mode::MultiRename(_)));
        for (text, ok) in [("p\nq\nr\n", false), ("p\nq\n", true)] {
            let file = names_file(&app);
            std::fs::write(&file, text).unwrap();
            app.mode = Mode::Normal; // the editor closed
            app.tick();
            let Mode::MultiRename(f) = &app.mode else {
                panic!("no form");
            };
            assert_eq!(f.names.is_some(), ok);
            if ok {
                assert_eq!(f.preview[1].1.as_deref(), Ok("q"));
            } else {
                assert!(f.error.as_deref().is_some_and(|e| e.contains("names for")));
                key(&mut app, KeyCode::F(4)); // again, for the good list
            }
        }
    }

    #[test]
    fn rename_ctrl_z_undoes() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let other = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), other.path());
        let open = |app: &mut App| {
            key(app, KeyCode::Down); // skip ".."
            key(app, KeyCode::Insert);
            key(app, KeyCode::Insert);
            with(app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        };
        open(&mut app);
        app.on_paste("x\ny\n".into());
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(dir.path().join("x").exists());
        // an unrelated transfer keeps the undo memory
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(other.path().join("x").exists() || other.path().join("y").exists());
        open(&mut app);
        with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(dir.path().join("a").exists() && dir.path().join("b").exists());
        assert!(!dir.path().join("x").exists());
        open(&mut app);
        with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(
            matches!(&app.mode, Mode::MultiRename(f) if f.error.as_deref() == Some("Nothing to undo."))
        );
    }

    #[test]
    fn saved_searches_save_load_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        let file = dir.path().join("searches");
        app.searches_file = Some(file.clone());
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        type_keys(&mut app, "*.rs");
        form_focus(&mut app, F_HIDDEN);
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::F(2)); // no name yet
        assert!(
            matches!(&app.mode, Mode::FindForm(f) if f.error.as_deref().is_some_and(|e| e.contains("name")))
        );
        form_focus(&mut app, F_SAVED);
        type_keys(&mut app, "rust");
        key(&mut app, KeyCode::F(2));
        assert_eq!(cmdline::load_history(&file).unwrap().len(), 1);
        key(&mut app, KeyCode::Esc);
        app.last_find = None; // a fresh form
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        key(&mut app, KeyCode::F(3));
        let Mode::FindForm(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!((f.text(F_NAME), f.text(F_SAVED)), ("*.rs", "rust"));
        assert!(f.checked(F_HIDDEN) && !f.checked(F_FOLLOW));
        key(&mut app, KeyCode::F(8));
        assert!(cmdline::load_history(&file).unwrap().is_empty());
    }

    #[test]
    fn ctrl_v_pastes_changed_host_files_before_app_clip() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("h.txt"), "host\n").unwrap();
        std::fs::write(src.path().join("a.txt"), "app\n").unwrap();
        let mut app = two_panels(src.path(), dst.path());
        key(&mut app, KeyCode::Tab); // paste into the destination panel
        // host files: pasted once, then the app clipboard is used again
        app.paste_with(vec![src.path().join("h.txt")]);
        wait(&mut app);
        assert!(dst.path().join("h.txt").exists());
        app.file_clip = Some(FileClip {
            op: Op::Copy,
            paths: vec![src.path().join("a.txt")],
            side: Side::Local,
        });
        app.paste_with(vec![src.path().join("h.txt")]);
        wait(&mut app);
        assert!(dst.path().join("a.txt").exists());
    }

    #[test]
    fn failed_cut_paste_keeps_the_clip() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let mut app = two_panels(src.path(), dst.path());
        app.file_clip = Some(FileClip {
            op: Op::Move,
            paths: vec![src.path().join("gone.txt")],
            side: Side::Local,
        });
        key(&mut app, KeyCode::Tab);
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(app.file_clip.is_some() && !dst.path().join("gone.txt").exists());
    }

    #[test]
    fn ctrl_c_v_copies_and_ctrl_x_v_moves_between_panels() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(src.path().join("b.txt"), "two\n").unwrap();
        let mut app = two_panels(src.path(), dst.path());
        key(&mut app, KeyCode::Down); // a.txt, the first entry after ".."
        with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Tab); // paste into the other panel
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(dst.path().join("a.txt").exists() && src.path().join("a.txt").exists());
        // cut b.txt from the left panel and paste it into the right one: it moves
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Down); // b.txt
        with(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Tab);
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(dst.path().join("b.txt").exists() && !src.path().join("b.txt").exists());
        assert!(app.file_clip.is_none());
        // nothing left to paste
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(matches!(&app.mode, Mode::Message(m) if m.starts_with("Nothing copied")));
    }

    fn procs(app: &App) -> &Procs {
        match &app.mode {
            Mode::Procs(p) => p,
            _ => panic!("not in the process monitor"),
        }
    }

    /// Tick until the next snapshot arrives.
    fn procs_wait(app: &mut App) {
        let before = procs(app).snap.clone();
        let t = std::time::Instant::now();
        while procs(app).snap == before && t.elapsed() < std::time::Duration::from_secs(5) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Ctrl+P: snapshot arrives, sort toggles, filter narrows and clears, Esc closes.
    #[test]
    fn process_monitor_sort_filter_close() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        while procs(&app).rows.is_empty() {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let all = procs(&app).rows.len();
        assert!(app.idle());
        assert_eq!(app.footer_keys(), &PROCS_FOOTER);
        key(&mut app, KeyCode::Char('p'));
        let p = procs(&app);
        assert_eq!((p.sort, p.rev), (ProcSort::Pid, false));
        let pids: Vec<u32> = p.rows.iter().map(|&i| p.snap.procs[i].pid).collect();
        assert!(pids.is_sorted());
        key(&mut app, KeyCode::Char('p'));
        assert!(procs(&app).rev);
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, &std::process::id().to_string());
        let p = procs(&app);
        assert!(p.filter.editing && p.rows.len() < all);
        assert_eq!(p.current().map(|c| c.pid), Some(std::process::id()));
        key(&mut app, KeyCode::Esc);
        assert_eq!(procs(&app).rows.len(), all);
        // the cursor keeps its row across refreshes
        key(&mut app, KeyCode::Down);
        procs_wait(&mut app);
        assert_eq!(procs(&app).sel, 1);
        key(&mut app, KeyCode::Char('s'));
        assert_eq!(procs(&app).every_edit.as_ref().unwrap().text, "2");
        with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        type_keys(&mut app, "0,5");
        key(&mut app, KeyCode::Enter);
        assert_eq!(procs(&app).every, std::time::Duration::from_millis(500));
        key(&mut app, KeyCode::Char('s'));
        with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        type_keys(&mut app, "0");
        key(&mut app, KeyCode::Enter);
        let p = procs(&app);
        assert_eq!(p.every, std::time::Duration::from_millis(500));
        assert!(p.status.contains("0.5 to 3600"));
        key(&mut app, KeyCode::F(8));
        assert!(procs(&app).confirm.is_some());
        key(&mut app, KeyCode::Char('n'));
        assert!(procs(&app).confirm.is_none());
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// A connection that cannot run commands (FTP) reports it instead of an empty list.
    #[test]
    fn process_monitor_needs_commands_on_remote() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        app.panels[0]
            .open_remote(tc_core::vfs::mem::Mem::new(), "/".into())
            .unwrap();
        settle(&mut app);
        with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        while matches!(app.mode, Mode::Procs(_)) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(matches!(&app.mode, Mode::Message(m) if m.contains("no commands")));
    }
}
