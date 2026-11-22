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
use tc_core::stats::treemap::{self, Tile};
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
    /// Treemap of the browsed directory and the size it was laid out for; cleared by `rebuild`.
    pub(super) map: Option<((u32, u32), Vec<Tile>)>,
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
    pub fn selected_index(&self) -> Option<usize> {
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
        self.map = None;
    }

    /// Treemap of the browsed directory for a `w`×`h` area; laid out again only after a change.
    pub fn treemap(&mut self, w: f32, h: f32, depth: u8, min_side: f32, inset: f32) -> &[Tile] {
        let key = (w.to_bits(), h.to_bits());
        if self.map.as_ref().is_none_or(|(k, _)| *k != key) {
            // count mode: dirs only, 2 levels
            let depth = if self.by_files { depth.min(1) } else { depth };
            let tiles =
                treemap::layout(self.current(), w, h, depth, min_side, inset, self.by_files);
            self.map = Some((key, tiles));
        }
        self.tiles()
    }

    /// Tiles of the last `treemap` call.
    pub fn tiles(&self) -> &[Tile] {
        self.map.as_ref().map_or(&[], |(_, t)| t)
    }

    /// Table row of the innermost tile at (`x`, `y`) of the last treemap, `None` outside or
    /// filtered out.
    pub fn map_row(&self, x: f32, y: f32) -> Option<usize> {
        let top = self.tiles()[self.map_hit(x, y)?].top;
        self.view.iter().position(|&i| i == top)
    }

    /// Node of the innermost tile at (`x`, `y`) of the last treemap, with its path below the
    /// browsed directory.
    pub fn map_node(&self, x: f32, y: f32) -> Option<(PathBuf, &Node)> {
        let tiles = self.tiles();
        let mut chain = Vec::new();
        let mut up = Some(self.map_hit(x, y)?);
        while let Some(k) = up {
            chain.push(tiles[k].child);
            up = tiles[k].parent;
        }
        let mut rel = PathBuf::new();
        let n = chain.iter().rev().fold(self.current(), |n, &i| {
            rel.push(&n.children[i].name);
            &n.children[i]
        });
        Some((rel, n))
    }

    /// Index of the innermost tile at (`x`, `y`) of the last treemap.
    fn map_hit(&self, x: f32, y: f32) -> Option<usize> {
        self.tiles()
            .iter()
            .rposition(|t| x >= t.x && x < t.x + t.w && y >= t.y && y < t.y + t.h)
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
            KeyCode::Tab => {
                self.by_files = !self.by_files;
                self.map = None;
            }
            KeyCode::Insert | KeyCode::Char(' ') => self.toggle_mark(last),
            KeyCode::Char('n') => self.set_order(stats::Order::Name),
            KeyCode::Char('s') => self.set_order(stats::Order::Size),
            KeyCode::Char('C') => self.set_order(stats::Order::Count),
            _ => {}
        }
    }

    /// F5: scan the browsed directory again in a worker and swap it into the tree.
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
    /// Left click on the File Stats treemap: cursor to the tile's entry, double click opens a
    /// directory. True if the click hit the chart.
    pub(super) fn stats_click(&mut self, col: u16, row: u16) -> bool {
        let area = self.stats_map;
        let Mode::Stats(st) = &mut self.mode else {
            return false;
        };
        if !area.contains((col, row).into()) {
            return false;
        }
        if st.confirm.is_some() || st.scanning() {
            return true;
        }
        let (x, y) = (f32::from(col - area.x) + 0.5, f32::from(row - area.y) + 0.5);
        let Some(r) = st.map_row(x, y) else {
            return true;
        };
        st.sel = r;
        if self.double_click(col, row) {
            self.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
        true
    }

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
            map: None,
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
            KeyCode::F(5) => st.rescan(),
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
                ignored: false,
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

#[cfg(test)]
mod tests {

    use super::super::{App, Mode};

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::super::tests::{key, settle, two_panels, type_keys, type_text, wait, with};

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
    fn stats_treemap_cache_and_hit_test() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "x".repeat(60)).unwrap();
        std::fs::write(tmp.path().join("b"), "x".repeat(30)).unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        std::fs::write(tmp.path().join("d").join("x"), "x".repeat(10)).unwrap();
        let mut app = stats_app(tmp.path());
        let Mode::Stats(st) = &mut app.mode else {
            panic!()
        };
        let n = st.treemap(100.0, 50.0, 1, 0.0, 0.0).len();
        assert_eq!(n, 4); // a, b, d, d/x
        let ptr = st.map.as_ref().map(|(_, t)| t.as_ptr());
        st.treemap(100.0, 50.0, 1, 0.0, 0.0);
        assert_eq!(st.map.as_ref().map(|(_, t)| t.as_ptr()), ptr);
        // every table row is hit somewhere; innermost tile wins
        let (_, tiles) = st.map.as_ref().unwrap();
        let t = tiles.iter().find(|t| t.depth == 0 && t.top == 0).unwrap();
        let row = st.map_row(t.x + t.w / 2.0, t.y + t.h / 2.0).unwrap();
        assert_eq!(st.shown().nth(row).unwrap().name, "a");
        assert_eq!(st.map_row(-1.0, 0.0), None);
        // the innermost tile's own node, with its path
        let x = tiles.iter().find(|t| t.depth == 1).unwrap();
        let (rel, n) = st.map_node(x.x + x.w / 2.0, x.y + x.h / 2.0).unwrap();
        assert_eq!((rel, n.size), (PathBuf::from("d").join("x"), 10));
        assert!(st.map_node(-1.0, 0.0).is_none());
        // filtered-out entry has no row
        st.filter.line.text = "b".into();
        let b = st
            .current()
            .children
            .iter()
            .position(|c| c.name == "b")
            .unwrap();
        st.treemap(100.0, 50.0, 1, 0.0, 0.0);
        st.rebuild();
        assert!(st.map.is_none());
        st.treemap(100.0, 50.0, 1, 0.0, 0.0);
        let (_, tiles) = st.map.as_ref().unwrap();
        let ta = tiles.iter().find(|t| t.depth == 0 && t.top != b).unwrap();
        assert_eq!(st.map_row(ta.x + 0.5, ta.y + 0.5), None);
        let tb = tiles.iter().find(|t| t.top == b).unwrap();
        assert_eq!(st.map_row(tb.x + 0.5, tb.y + 0.5), Some(0));
    }

    #[test]
    fn stats_treemap_follows_enter() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        std::fs::write(tmp.path().join("d").join("x"), "x").unwrap();
        std::fs::write(tmp.path().join("d").join("y"), "x").unwrap();
        let mut app = stats_app(tmp.path());
        let Mode::Stats(st) = &mut app.mode else {
            panic!()
        };
        assert_eq!(st.treemap(10.0, 10.0, 0, 0.0, 0.0).len(), 1);
        key(&mut app, KeyCode::Enter);
        let Mode::Stats(st) = &mut app.mode else {
            panic!()
        };
        assert!(st.map.is_none());
        assert_eq!(st.treemap(10.0, 10.0, 0, 0.0, 0.0).len(), 2);
    }

    #[test]
    fn stats_treemap_tab_counts_dirs_two_levels() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("d").join("e").join("f");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("x"), "x").unwrap();
        std::fs::write(tmp.path().join("loose"), "x").unwrap();
        let mut app = stats_app(tmp.path());
        let Mode::Stats(st) = &mut app.mode else {
            panic!()
        };
        assert_eq!(st.treemap(100.0, 100.0, u8::MAX, 0.0, 0.0).len(), 5);
        key(&mut app, KeyCode::Tab);
        let Mode::Stats(st) = &mut app.mode else {
            panic!()
        };
        assert!(st.map.is_none());
        let tiles = st.treemap(100.0, 100.0, u8::MAX, 0.0, 0.0);
        // d and d/e; no loose file, no d/e/f
        assert_eq!(tiles.len(), 2);
        assert!(
            tiles
                .iter()
                .all(|t| t.kind == tc_core::stats::treemap::Kind::Dir && t.depth <= 1)
        );
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
        #[cfg(target_os = "linux")] // macOS APFS rejects non-UTF-8 names (EILSEQ)
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
        assert_eq!(
            st.root.size,
            if cfg!(target_os = "linux") { 11 } else { 10 }
        );
        assert!(st.marked.is_empty());
        #[cfg(target_os = "linux")]
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
        with(&mut app, KeyCode::F(5), KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc);
        assert!(matches!(&app.mode, Mode::Stats(st) if !st.scanning() && st.root.size == 8));
        with(&mut app, KeyCode::F(5), KeyModifiers::NONE);
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
        with(&mut app, KeyCode::F(5), KeyModifiers::NONE);
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        let Mode::Stats(st) = &app.mode else { panic!() };
        assert_eq!((st.current().children.len(), st.root.size), (1, 2));
        assert_eq!(st.selected().map(|n| n.name.clone()), Some("b".into()));
    }
}
