//! Find files (Alt+F7 / Ctrl+F): form, streamed results, panelize.

use crate::app::widgets::{list_step, step_focus, step_key, tab_key, tab_of, tab_range};
use crate::app::{
    App, Filter, FormRow, Mode, form_checked, form_text, paste_line, rows_from_line, rows_to_line,
};
use crate::{KeyCode, KeyEvent, KeyModifiers};
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

// Find form row labels; also the keys of the find history lines.
pub(in crate::app) const F_NAME: &str = "Name (masks)";

pub(in crate::app) const F_NAME_RE: &str = "Name is a regex";

pub(in crate::app) const F_NAME_CASE: &str = "Name case-sensitive";

pub(in crate::app) const F_SKIP_FILES: &str = "Exclude files";

pub(in crate::app) const F_CONTENT_ON: &str = "Search file contents";

pub(in crate::app) const F_CONTENT: &str = "Content";

pub(in crate::app) const F_REGEX: &str = "Content is a regex";

pub(in crate::app) const F_CASE: &str = "Content case-sensitive";

pub(in crate::app) const F_WORD: &str = "Whole words";

pub(in crate::app) const F_NOT: &str = "Not containing";

pub(in crate::app) const F_EVERY: &str = "List every match";

pub(in crate::app) const F_SKIP_DIRS: &str = "Exclude dirs";

pub(in crate::app) const F_HIDDEN: &str = "Skip hidden";

pub(in crate::app) const F_GITIGNORE: &str = "Use .gitignore";

pub(in crate::app) const F_FOLLOW: &str = "Follow symlinks";

pub(in crate::app) const F_DEPTH: &str = "Max depth";

pub(in crate::app) const F_MIN: &str = "Min size";

pub(in crate::app) const F_MAX: &str = "Max size";

pub(in crate::app) const F_AFTER: &str = "Modified after";

pub(in crate::app) const F_BEFORE: &str = "Modified before";

pub(in crate::app) const F_AGE: &str = "Not older than";

pub(in crate::app) const F_EXTS: &str = "Extensions";

/// Rows per tab (titles in `fmt::FIND_TABS`), in row order.
pub(in crate::app) const FIND_TAB_ROWS: [usize; 4] = [5, 7, 5, 5];

const _: () = assert!(FIND_TAB_ROWS.len() == crate::fmt::FIND_TABS.len());

/// Find files form rows in focus order, tab by tab; all start empty or off.
pub(in crate::app) fn find_rows() -> Vec<FormRow> {
    let text = |label| FormRow::Text(label, LineEdit::default());
    let check = |label| FormRow::Check(label, false);
    vec![
        text(F_NAME),
        check(F_NAME_RE),
        check(F_NAME_CASE),
        text(F_EXTS),
        text(F_SKIP_FILES),
        text(F_CONTENT),
        check(F_CONTENT_ON),
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
    ]
}

/// Find files form: rows, the focused one, and the error (or note) of the last try.
#[derive(Clone)]
pub struct FindForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub error: Option<String>,
    /// Position in the find history for Alt+Up/Down (= its length: not from history).
    pub(in crate::app) hist: usize,
    /// Tab shown (`FIND_TAB_ROWS`).
    pub tab: usize,
}

impl FindForm {
    pub fn new() -> Self {
        FindForm {
            rows: find_rows(),
            focus: 0,
            error: None,
            hist: 0,
            tab: 0,
        }
    }

    /// Flat row indexes of the shown tab.
    pub fn shown_rows(&self) -> std::ops::Range<usize> {
        tab_range(&FIND_TAB_ROWS, self.tab)
    }

    /// Tab `t` has a field filled or a box ticked (its title gets a dot).
    pub fn tab_changed(&self, t: usize) -> bool {
        self.rows[tab_range(&FIND_TAB_ROWS, t)]
            .iter()
            .any(|r| match r {
                FormRow::Text(_, l) => !l.text.trim().is_empty(),
                FormRow::Check(_, on) => *on,
                FormRow::Choice(_, _, k) => *k != 0,
            })
    }

    /// Show tab `t` and focus its first row.
    pub(in crate::app) fn goto_tab(&mut self, t: usize) {
        self.tab = t.min(FIND_TAB_ROWS.len() - 1);
        self.focus = self.shown_rows().start;
    }

    /// Focus moved (Tab, click): the tab follows it.
    pub(in crate::app) fn follow_focus(&mut self) {
        if let Some(t) = tab_of(&FIND_TAB_ROWS, self.focus) {
            self.tab = t;
        }
    }

    pub(in crate::app) fn text(&self, label: &str) -> &str {
        form_text(&self.rows, label)
    }

    pub(in crate::app) fn checked(&self, label: &str) -> bool {
        form_checked(&self.rows, label)
    }

    /// Typing and space toggle on the focused row.
    pub(in crate::app) fn key(&mut self, key: KeyEvent) {
        self.rows[self.focus].key(key);
        self.tick_content();
    }

    /// Pasted text goes into the focused text field.
    pub(in crate::app) fn paste(&mut self, text: &str) {
        if let FormRow::Text(_, l) = &mut self.rows[self.focus] {
            paste_line(l, text);
        }
        self.tick_content();
    }

    /// Typed content text turns content search on, so it is not silently ignored.
    pub(in crate::app) fn tick_content(&mut self) {
        if self.text(F_CONTENT).is_empty() {
            return;
        }
        for row in &mut self.rows {
            if let FormRow::Check(F_CONTENT_ON, on) = row {
                *on = true;
            }
        }
    }

    /// One line of the find history: the [`rows_to_line`] of the form.
    pub(in crate::app) fn to_line(&self) -> String {
        rows_to_line(&self.rows, "")
    }

    /// The form a history line describes; unknown labels (a saved-search name in old lines)
    /// are ignored, missing ones stay empty.
    pub(in crate::app) fn from_line(line: &str) -> Self {
        let mut form = FindForm::new();
        rows_from_line(&mut form.rows, line);
        form
    }

    /// The search the fields describe; the first field that does not parse is the error.
    pub(in crate::app) fn query(&self, exact: bool, remote: bool) -> Result<find::Query, String> {
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
    pub(in crate::app) fn content(&self, exact: bool) -> Result<Option<find::Content>, String> {
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
pub(in crate::app) fn cmp_lower(a: &OsStr, b: &OsStr) -> cmp::Ordering {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// Order of two hits by `key`, ties by path.
pub(in crate::app) fn cmp_hits(a: &Hit, b: &Hit, key: FoundSort) -> cmp::Ordering {
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
    pub(in crate::app) view: Vec<usize>,
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
    pub(in crate::app) re: Option<Regex>,
    /// What Ctrl+R searches again.
    pub(in crate::app) query: find::Query,
    pub(in crate::app) rx: Receiver<Hit>,
    pub(in crate::app) cancel: Arc<AtomicBool>,
}

impl Found {
    /// Shown results.
    pub fn shown(&self) -> impl Iterator<Item = &Hit> {
        self.view.iter().map(|&i| &self.list[i])
    }

    pub fn shown_len(&self) -> usize {
        self.view.len()
    }

    pub(in crate::app) fn matches(&self, h: &Hit) -> bool {
        self.filter.matches(&h.entry.name.to_string_lossy())
    }

    pub(in crate::app) fn rebuild(&mut self) {
        self.view = (0..self.list.len())
            .filter(|&i| self.matches(&self.list[i]))
            .collect();
        self.sort_view();
        self.sel = self.sel.min(self.view.len().saturating_sub(1));
    }

    /// Orders `view` by `sort`, the cursor stays on the same hit.
    pub(in crate::app) fn resort(&mut self) {
        let cur = self.view.get(self.sel).copied();
        self.sort_view();
        if let Some(i) = cur.and_then(|c| self.view.iter().position(|&v| v == c)) {
            self.sel = i;
        }
    }

    pub(in crate::app) fn sort_view(&mut self) {
        let Some((key, rev)) = self.sort else { return };
        let list = &self.list;
        self.view.sort_by(|&a, &b| {
            let o = cmp_hits(&list[a], &list[b], key);
            if rev { o.reverse() } else { o }
        });
    }

    /// Sorts by `key`; the same key again reverses.
    pub(in crate::app) fn set_sort(&mut self, key: FoundSort) {
        self.sort = Some((key, self.sort == Some((key, false))));
        self.resort();
    }

    pub(in crate::app) fn pump(&mut self) {
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
    pub(in crate::app) fn spawn_find(&mut self, q: find::Query) {
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

    /// Find files form: Enter / F10 searches, F2-F5 show a tab, Tab/Up/Down move, Space
    /// toggles, Esc / F9 cancel.
    pub(in crate::app) fn find_form_key(&mut self, key: KeyEvent, mut form: FindForm) {
        let remote = self.panels[self.active].remote().is_some();
        form.error = None;
        if let Some(t) = tab_key(key, form.tab, FIND_TAB_ROWS.len()) {
            form.goto_tab(t);
            self.mode = Mode::FindForm(form);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::F(9) => return self.last_find = Some(form),
            KeyCode::Enter | KeyCode::F(10) => match form.query(self.exact, remote) {
                Ok(q) => {
                    let line = form.to_line();
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
                let (focus, tab) = (form.focus, form.tab);
                form = FindForm {
                    focus,
                    tab,
                    ..self.find_hist(i)
                };
            }
            KeyCode::Tab | KeyCode::BackTab => {
                form.focus = step_key(key.code, form.focus, form.rows.len());
                form.follow_focus();
            }
            KeyCode::Down => form.focus = step_focus(form.shown_rows(), form.focus, 1, |_| false),
            KeyCode::Up => form.focus = step_focus(form.shown_rows(), form.focus, -1, |_| false),
            _ => form.key(key),
        }
        self.mode = Mode::FindForm(form);
    }

    pub(in crate::app) fn found_key(&mut self, key: KeyEvent, mut f: Found) {
        let page = self.page;
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
            code => f.sel = list_step(code, f.sel, f.view.len(), page).unwrap_or(f.sel),
        }
        self.mode = Mode::Found(f);
    }

    /// Enter in the results: the found files (once each, in the shown order) become a virtual
    /// panel; their first matching line stays for F3/F4 there.
    pub(in crate::app) fn panelize_found(&mut self, f: Found) {
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

#[cfg(test)]
mod tests {
    use crate::app::tools::find::{
        F_CONTENT, F_CONTENT_ON, F_EVERY, F_MIN, F_NAME, F_SKIP_DIRS, Found, FoundSort,
    };

    use crate::app::{App, Filter, Mode};
    use crate::fmt;
    use crate::{KeyCode, KeyEvent, KeyModifiers};

    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::mpsc;
    use tc_core::cmdline::LineEdit;
    use tc_core::find;
    use tc_core::find::Hit;
    use tc_core::panel::{Entry, Panel};

    use crate::app::tests::{key, settle, two_panels, type_keys, wait, with};

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
                ignored: false,
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
        assert_eq!(
            (f.sel, f.shown().next().map(|h| h.entry.size)),
            (0, Some(0))
        );
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
        assert!(fmt::found_cols(&f.list[1])[1].ends_with("b.txt:4: hello two"));
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
        let file = d.join("sub").join("b.txt");
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
    fn find_form_tabs_follow_keys_and_focus() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        let form = |app: &App| match &app.mode {
            Mode::FindForm(f) => (f.tab, f.rows[f.focus].label(), f.tab_changed(0)),
            _ => panic!("no form"),
        };
        key(&mut app, KeyCode::F(3));
        assert_eq!(form(&app), (1, F_CONTENT, false));
        // Up / Down stay in the tab, Tab walks on and the tab follows
        key(&mut app, KeyCode::Up);
        assert_eq!(form(&app).1, F_CONTENT);
        for _ in 0..7 {
            key(&mut app, KeyCode::Tab);
        }
        assert_eq!(form(&app), (2, F_SKIP_DIRS, false));
        with(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
        type_keys(&mut app, "*.rs");
        assert_eq!(form(&app), (0, F_NAME, true));
        // F10 searches, F9 cancels
        key(&mut app, KeyCode::F(10));
        assert!(matches!(app.mode, Mode::Found(_)));
    }
}
