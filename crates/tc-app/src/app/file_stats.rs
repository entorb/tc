//! File Stats: directory size tree, marks, delete, panelize.

use super::{App, Filter, Mode, REMOTE, delete_question, delete_text};
use crate::fmt;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use tc_core::cmdline::{self, LineEdit};
use tc_core::ops;
use tc_core::panel::Entry;
use tc_core::stats::{self, Node};
use tc_core::vfs::{Remote, Side};

/// File Stats: the scanned tree and the directory being browsed.
pub struct Stats {
    pub dir: PathBuf,
    pub root: Node,
    /// Child indices from `root` down to the browsed directory.
    pub(super) path: Vec<usize>,
    /// Row in `view`.
    pub sel: usize,
    /// Indices into the browsed directory's children shown (all, or those matching `filter`).
    pub(super) view: Vec<usize>,
    pub filter: Filter,
    /// Show file count (value, share, bar) instead of size.
    pub by_files: bool,
    pub order: stats::Order,
    /// Reversed `order`.
    pub rev: bool,
    /// Path of the running rescan to swap into `root`; `None` for the first scan.
    pub(super) splice: Option<Vec<usize>>,
    /// Running scan; `root` is empty until it finishes.
    pub(super) job: Option<thread::JoinHandle<Node>>,
    pub progress: Arc<ops::Progress>,
    /// Delete question for the selected entry, shown while set.
    pub confirm: Option<String>,
    /// Connection of a remote panel the tree was scanned on.
    pub(super) remote: Option<Arc<dyn Remote>>,
    /// Names in the browsed directory a running remote delete removes, and how many it has
    /// deleted so far.
    pub(super) deleting: Option<(Vec<OsString>, Arc<AtomicUsize>)>,
    /// Marked names of the browsed directory.
    pub(super) marked: HashSet<OsString>,
}

impl Stats {
    pub fn scanning(&self) -> bool {
        self.job.is_some()
    }

    pub fn current(&self) -> &Node {
        self.path.iter().fold(&self.root, |n, &i| &n.children[i])
    }

    pub(super) fn current_mut(&mut self) -> &mut Node {
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
    pub(super) fn selected_index(&self) -> Option<usize> {
        self.view.get(self.sel).copied()
    }

    pub fn is_marked(&self, n: &Node) -> bool {
        self.marked.contains(&n.name)
    }

    /// Children indices a delete acts on: the shown marked entries, else the selected one.
    pub(super) fn targets(&self) -> Vec<usize> {
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
    pub(super) fn delete_ask(&mut self) {
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
    pub(super) fn drop_names(&mut self, names: &[OsString]) {
        for name in names {
            if let Some(i) = self.current().children.iter().position(|n| &n.name == name) {
                stats::remove(&mut self.root, &self.path, i);
            }
        }
        self.marked.retain(|m| !names.contains(m));
        self.rebuild();
    }

    /// Recompute the shown entries after a sort, a filter or a change of the tree.
    pub(super) fn rebuild(&mut self) {
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

    pub(super) fn key(&mut self, code: KeyCode, page: usize) {
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
    pub(super) fn rescan(&mut self) {
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
    pub(super) fn scanned(&mut self, node: Node) {
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
    pub(super) fn toggle_mark(&mut self, last: usize) {
        if let Some(name) = self.selected().map(|n| n.name.clone())
            && !self.marked.remove(&name)
        {
            self.marked.insert(name);
        }
        self.sel = (self.sel + 1).min(last);
    }

    /// Same order again reverses it; the cursor stays on its entry.
    pub(super) fn set_order(&mut self, order: stats::Order) {
        let name = self.selected().map(|n| n.name.clone());
        self.rev = self.order == order && !self.rev;
        self.order = order;
        self.resort();
        self.select_name(name);
    }

    /// Puts the cursor on the shown entry called `name`, if any.
    pub(super) fn select_name(&mut self, name: Option<OsString>) {
        if let Some(i) = name.and_then(|name| {
            self.view
                .iter()
                .position(|&i| self.current().children[i].name == name)
        }) {
            self.sel = i;
        }
    }

    /// Sorts the browsed directory and puts the cursor on its first shown entry.
    pub(super) fn resort(&mut self) {
        let (order, rev) = (self.order, self.rev);
        stats::sort(&mut self.current_mut().children, order, rev);
        self.sel = 0;
        self.rebuild();
    }
}

/// Scan `dir` (on `remote` if set) in a worker.
pub(super) fn spawn_scan(
    dir: PathBuf,
    remote: Option<Arc<dyn Remote>>,
    p: Arc<ops::Progress>,
) -> thread::JoinHandle<Node> {
    thread::spawn(move || match remote {
        Some(r) => stats::scan_remote(r.as_ref(), &dir, &p),
        None => stats::scan(&dir, &p),
    })
}

impl App {
    /// Ctrl+Space: File Stats of the active panel's directory.
    pub(super) fn spawn_stats(&mut self) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message("File Stats does not work in archives.".into());
            return;
        }
        self.scan_stats(panel.path().to_owned(), panel.remote().cloned());
    }

    /// Scan `dir` in a worker and show its File Stats.
    pub(super) fn scan_stats(&mut self, dir: PathBuf, remote: Option<Arc<dyn Remote>>) {
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
    pub(super) fn stats_mode_key(&mut self, mut st: Stats, key: KeyEvent) {
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
    pub(super) fn stats_info(&mut self, st: Stats) {
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
    pub(super) fn stats_panelize(&mut self, st: Stats) {
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
    pub(super) fn stats_confirm_key(&mut self, mut st: Stats, code: KeyCode) {
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
    pub(super) fn stats_open(&mut self, st: Stats, key: KeyEvent) {
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
}
