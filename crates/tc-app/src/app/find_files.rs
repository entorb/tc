//! Find files (Alt+F7 / Ctrl+F): form, saved searches, streamed results, panelize.

use super::{
    App, Filter, FormRow, Mode, PresetKind, form_checked, form_text, paste_line, preset_key,
    rows_from_line, rows_to_line,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regex::Regex;
use std::cmp;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use tc_core::cmdline::LineEdit;
use tc_core::find::Hit;
use tc_core::panel::Entry;
use tc_core::vfs;
use tc_core::{find, search};

// Find form row labels; also the keys of saved searches.
pub(super) const F_NAME: &str = "Name (masks)";

pub(super) const F_NAME_RE: &str = "Name is a regex";

pub(super) const F_NAME_CASE: &str = "Name case-sensitive";

pub(super) const F_SKIP_FILES: &str = "Exclude files";

pub(super) const F_CONTENT_ON: &str = "Search file contents";

pub(super) const F_CONTENT: &str = "Content";

pub(super) const F_REGEX: &str = "Content is a regex";

pub(super) const F_CASE: &str = "Content case-sensitive";

pub(super) const F_WORD: &str = "Whole words";

pub(super) const F_NOT: &str = "Not containing";

pub(super) const F_EVERY: &str = "List every match";

pub(super) const F_SKIP_DIRS: &str = "Exclude dirs";

pub(super) const F_HIDDEN: &str = "Skip hidden";

pub(super) const F_GITIGNORE: &str = "Use .gitignore";

pub(super) const F_FOLLOW: &str = "Follow symlinks";

pub(super) const F_DEPTH: &str = "Max depth";

pub(super) const F_MIN: &str = "Min size";

pub(super) const F_MAX: &str = "Max size";

pub(super) const F_AFTER: &str = "Modified after";

pub(super) const F_BEFORE: &str = "Modified before";

pub(super) const F_AGE: &str = "Not older than";

pub(super) const F_EXTS: &str = "Extensions";

pub(super) const F_SAVED: &str = "Saved search";

pub(super) const SEARCH_KIND: PresetKind = PresetKind {
    none: "No saved searches yet: F2 saves one.",
    label: "Saved search",
    one: "saved search",
    all: "searches",
};

/// Find files form rows in focus order.
pub(super) fn find_rows() -> Vec<FormRow> {
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

/// Find files form: rows, the focused one, and the error (or note) of the last try.
#[derive(Clone)]
pub struct FindForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub error: Option<String>,
    /// Position in the find history for Alt+Up/Down (= its length: not from history).
    pub(super) hist: usize,
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

    pub(super) fn text(&self, label: &str) -> &str {
        form_text(&self.rows, label)
    }

    pub(super) fn checked(&self, label: &str) -> bool {
        form_checked(&self.rows, label)
    }

    /// Typing and space toggle on the focused row.
    pub(super) fn key(&mut self, code: KeyCode, ctrl: bool) {
        self.rows[self.focus].key(code, ctrl);
        self.tick_content();
    }

    /// Pasted text goes into the focused text field.
    pub(super) fn paste(&mut self, text: &str) {
        if let FormRow::Text(_, l) = &mut self.rows[self.focus] {
            paste_line(l, text);
        }
        self.tick_content();
    }

    /// Typed content text turns content search on, so it is not silently ignored.
    pub(super) fn tick_content(&mut self) {
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
    pub(super) fn to_line(&self, name: &str) -> String {
        format!(
            "{}\t{}",
            name.replace('\t', " "),
            rows_to_line(&self.rows, F_SAVED)
        )
    }

    /// The form a saved line describes; unknown labels are ignored, missing ones stay empty.
    pub(super) fn from_line(line: &str) -> Self {
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
    pub(super) fn query(&self, exact: bool, remote: bool) -> Result<find::Query, String> {
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
    pub(super) fn content(&self, exact: bool) -> Result<Option<find::Content>, String> {
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

/// What Ctrl+F3..F6 order the find results by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FoundSort {
    Path,
    Name,
    Date,
    Size,
}

/// Case-insensitive comparison of two names (no allocation for UTF-8 ones).
pub(super) fn cmp_lower(a: &OsStr, b: &OsStr) -> cmp::Ordering {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// Order of two hits by `key`, ties by path.
pub(super) fn cmp_hits(a: &Hit, b: &Hit, key: FoundSort) -> cmp::Ordering {
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
    pub(super) view: Vec<usize>,
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
    pub(super) re: Option<Regex>,
    /// What Ctrl+R searches again.
    pub(super) query: find::Query,
    pub(super) rx: Receiver<Hit>,
    pub(super) cancel: Arc<AtomicBool>,
}

impl Found {
    /// Shown results.
    pub fn shown(&self) -> impl Iterator<Item = &Hit> {
        self.view.iter().map(|&i| &self.list[i])
    }

    pub fn shown_len(&self) -> usize {
        self.view.len()
    }

    pub(super) fn matches(&self, h: &Hit) -> bool {
        self.filter.matches(&h.entry.name.to_string_lossy())
    }

    pub(super) fn rebuild(&mut self) {
        self.view = (0..self.list.len())
            .filter(|&i| self.matches(&self.list[i]))
            .collect();
        self.sort_view();
        self.sel = self.sel.min(self.view.len().saturating_sub(1));
    }

    /// Orders `view` by `sort`, the cursor stays on the same hit.
    pub(super) fn resort(&mut self) {
        let cur = self.view.get(self.sel).copied();
        self.sort_view();
        if let Some(i) = cur.and_then(|c| self.view.iter().position(|&v| v == c)) {
            self.sel = i;
        }
    }

    pub(super) fn sort_view(&mut self) {
        let Some((key, rev)) = self.sort else { return };
        let list = &self.list;
        self.view.sort_by(|&a, &b| {
            let o = cmp_hits(&list[a], &list[b], key);
            if rev { o.reverse() } else { o }
        });
    }

    /// Sorts by `key`; the same key again reverses.
    pub(super) fn set_sort(&mut self, key: FoundSort) {
        self.sort = Some((key, self.sort == Some((key, false))));
        self.resort();
    }

    pub(super) fn pump(&mut self) {
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

impl App {
    /// Search the active panel's directory in a worker thread, results arrive via `tick`.
    pub(super) fn spawn_find(&mut self, q: find::Query) {
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
    pub(super) fn find_form_key(&mut self, key: KeyEvent, mut form: FindForm) {
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
    pub(super) fn saved_search_key(&mut self, code: KeyCode, form: FindForm) -> FindForm {
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

    pub(super) fn found_key(&mut self, key: KeyEvent, mut f: Found) {
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
    pub(super) fn panelize_found(&mut self, f: Found) {
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
}
