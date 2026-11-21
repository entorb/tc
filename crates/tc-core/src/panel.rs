//! One file panel: a directory listing with cursor and selection.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{Result, io_err};

const PARENT: &str = "..";
/// Directory history entries kept per panel.
const HISTORY_MAX: usize = 100;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: OsString,
    /// Directory, or symlink pointing to one.
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl Entry {
    pub fn is_parent(&self) -> bool {
        self.name == PARENT
    }

    fn parent() -> Self {
        Self {
            name: PARENT.into(),
            is_dir: true,
            is_symlink: false,
            size: 0,
            modified: None,
        }
    }
}

#[derive(Debug)]
pub struct Panel {
    path: PathBuf,
    entries: Vec<Entry>,
    cursor: usize,
    selected: HashSet<OsString>,
    /// Lowercased name filter; `all` keeps the unfiltered list while it is set.
    filter: String,
    all: Option<Vec<Entry>>,
    /// Visited directories, oldest first, `pos` is the current one.
    history: Vec<PathBuf>,
    pos: usize,
    /// Panelized result list: entry names are absolute paths, `path` is where Backspace/Esc returns to.
    virt: bool,
}

impl Panel {
    pub fn open(path: &Path) -> Result<Self> {
        let path = std::path::absolute(path).map_err(io_err(path))?;
        let entries = read_entries(&path)?;
        Ok(Self {
            history: vec![path.clone()],
            pos: 0,
            path,
            entries,
            cursor: 0,
            selected: HashSet::new(),
            filter: String::new(),
            all: None,
            virt: false,
        })
    }

    pub fn is_virtual(&self) -> bool {
        self.virt
    }

    /// Show `entries` (names are absolute paths) as a flat list; `leave_virtual` goes back.
    pub fn panelize(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.all = None;
        self.filter.clear();
        self.selected.clear();
        self.cursor = 0;
        self.virt = true;
    }

    /// Back to the directory the list was made from; false if the panel was not virtual.
    pub fn leave_virtual(&mut self) -> Result<bool> {
        if !self.virt {
            return Ok(false);
        }
        self.load(self.path.clone())?;
        Ok(true)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    pub fn is_selected(&self, entry: &Entry) -> bool {
        self.selected.contains(&entry.name)
    }

    /// Re-read the directory, keeping cursor name and still existing selections.
    pub fn reload(&mut self) -> Result<()> {
        let keep = self.current().map(|e| e.name.clone());
        self.entries = if self.virt {
            // entries that vanished (e.g. deleted from the list) drop out
            let all = self.all.take().unwrap_or(std::mem::take(&mut self.entries));
            all.into_iter()
                .filter_map(|e| entry_for(e.name.clone(), Path::new(&e.name)))
                .collect()
        } else {
            read_entries(&self.path)?
        };
        self.all = None;
        if !self.filter.is_empty() {
            let f = std::mem::take(&mut self.filter);
            self.apply_filter(&f);
        }
        let names: HashSet<&OsStr> = self.entries.iter().map(|e| e.name.as_os_str()).collect();
        self.selected.retain(|n| names.contains(n.as_os_str()));
        let old = self.cursor;
        self.cursor = 0;
        match keep {
            Some(name) if self.focus(&name) => {}
            _ => self.move_cursor(old as isize),
        }
        Ok(())
    }

    /// Move cursor by `delta`, clamped to the list.
    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.entries.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// Enter the directory under the cursor. Returns false if it is not a directory.
    pub fn enter(&mut self) -> Result<bool> {
        let Some(entry) = self.current() else {
            return Ok(false);
        };
        if !entry.is_dir {
            return Ok(false);
        }
        if entry.is_parent() {
            self.parent()?;
        } else {
            let path = self.path.join(&entry.name);
            self.change_dir(path)?;
        }
        Ok(true)
    }

    /// Go to the parent directory, cursor on the directory we came from.
    pub fn parent(&mut self) -> Result<()> {
        if self.leave_virtual()? {
            return Ok(());
        }
        let (Some(parent), Some(came_from)) = (self.path.parent(), self.path.file_name()) else {
            return Ok(());
        };
        let came_from = came_from.to_owned();
        self.change_dir(parent.to_owned())?;
        self.focus(&came_from);
        Ok(())
    }

    /// Select entries `a..=b` (any order), not "..", and put the cursor on `b`.
    pub fn select_range(&mut self, a: usize, b: usize) {
        let last = self.entries.len().saturating_sub(1);
        let (lo, hi) = (a.min(b).min(last), a.max(b).min(last));
        let names = self.entries.get(lo..=hi).unwrap_or_default();
        let names: Vec<_> = names
            .iter()
            .filter(|e| !e.is_parent())
            .map(|e| e.name.clone())
            .collect();
        self.selected.extend(names);
        self.cursor = b.min(last);
    }

    /// Active name filter (lowercased), empty if none.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Show only ".." and entries whose name contains `text` (case-insensitive); in memory, no re-read.
    pub fn apply_filter(&mut self, text: &str) {
        let keep = self.current().map(|e| e.name.clone());
        let all = self
            .all
            .take()
            .unwrap_or_else(|| std::mem::take(&mut self.entries));
        self.filter = text.to_lowercase();
        self.entries = if self.filter.is_empty() {
            all.clone()
        } else {
            let f = &self.filter;
            let hit =
                |e: &&Entry| e.is_parent() || e.name.to_string_lossy().to_lowercase().contains(f);
            all.iter().filter(hit).cloned().collect()
        };
        if !self.filter.is_empty() {
            self.all = Some(all);
        }
        self.cursor = 0;
        if let Some(name) = keep {
            self.focus(&name);
        }
    }

    /// Quick search: move the cursor to the next entry (from `cursor + skip`, wrapping) whose name contains `text`.
    pub fn search(&mut self, text: &str, skip: usize) -> bool {
        let text = text.to_lowercase();
        let n = self.entries.len();
        let hit = (0..n).map(|k| (self.cursor + skip + k) % n).find(|&i| {
            self.entries[i]
                .name
                .to_string_lossy()
                .to_lowercase()
                .contains(&text)
        });
        hit.inspect(|&i| self.cursor = i).is_some()
    }

    /// Put the cursor on entry `i`, clamped to the list.
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.entries.len().saturating_sub(1));
    }

    /// Toggle selection of the cursor entry and move down.
    pub fn toggle_select(&mut self) {
        if let Some(entry) = self.current().filter(|e| !e.is_parent()) {
            let name = entry.name.clone();
            if !self.selected.remove(&name) {
                self.selected.insert(name);
            }
        }
        self.move_cursor(1);
    }

    /// Select (`on`) or deselect visible files matching `masks`; directories are left alone.
    pub fn select_mask(&mut self, masks: &str, on: bool) {
        for e in self
            .entries
            .iter()
            .filter(|e| !e.is_dir && crate::search::mask_match(masks, &e.name.to_string_lossy()))
        {
            if on {
                self.selected.insert(e.name.clone());
            } else {
                self.selected.remove(&e.name);
            }
        }
    }

    /// Invert the selection of visible files; directories are left alone.
    pub fn invert_selection(&mut self) {
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            if !self.selected.remove(&e.name) {
                self.selected.insert(e.name.clone());
            }
        }
    }

    /// Paths an operation acts on: the selection, else the cursor entry.
    pub fn targets(&self) -> Vec<PathBuf> {
        let names: Vec<&OsString> = if self.selected.is_empty() {
            self.current()
                .filter(|e| !e.is_parent())
                .map(|e| &e.name)
                .into_iter()
                .collect()
        } else {
            self.entries
                .iter()
                .filter(|e| self.is_selected(e))
                .map(|e| &e.name)
                .collect()
        };
        names.into_iter().map(|n| self.path.join(n)).collect()
    }

    /// Jump to `path` (e.g. a bookmark); a failure leaves the panel unchanged.
    pub fn go(&mut self, path: PathBuf) -> Result<()> {
        self.change_dir(path)
    }

    /// Step back (`-1`) or forward (`1`) in the directory history.
    pub fn history_step(&mut self, delta: isize) -> Result<()> {
        let Some(i) = self
            .pos
            .checked_add_signed(delta)
            .filter(|&i| i < self.history.len())
        else {
            return Ok(());
        };
        self.load(self.history[i].clone())?;
        self.pos = i;
        Ok(())
    }

    /// New directory: visiting it drops the forward history; repeats are not recorded.
    fn change_dir(&mut self, path: PathBuf) -> Result<()> {
        self.load(path.clone())?;
        if self.history.get(self.pos) != Some(&path) {
            self.history.truncate(self.pos + 1);
            self.history.push(path);
            if self.history.len() > HISTORY_MAX {
                self.history.remove(0);
            }
            self.pos = self.history.len() - 1;
        }
        Ok(())
    }

    /// Read the new directory first so a failure leaves the panel unchanged.
    fn load(&mut self, path: PathBuf) -> Result<()> {
        self.entries = read_entries(&path)?;
        self.all = None;
        self.filter.clear();
        self.virt = false;
        self.path = path;
        self.cursor = 0;
        self.selected.clear();
        Ok(())
    }

    fn focus(&mut self, name: &OsStr) -> bool {
        match self.entries.iter().position(|e| e.name == name) {
            Some(i) => {
                self.cursor = i;
                true
            }
            None => false,
        }
    }
}

/// Entry named `name` for `path`; None if it cannot be stat'ed.
pub(crate) fn entry_for(name: OsString, path: &Path) -> Option<Entry> {
    let link_meta = fs::symlink_metadata(path).ok()?;
    let is_symlink = link_meta.file_type().is_symlink();
    // dangling symlink: fall back to link metadata
    let meta = if is_symlink {
        fs::metadata(path).unwrap_or(link_meta)
    } else {
        link_meta
    };
    Some(Entry {
        name,
        is_dir: meta.is_dir(),
        is_symlink,
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

fn read_entries(dir: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir).map_err(io_err(dir))? {
        let item = item.map_err(io_err(dir))?;
        // None: vanished between read_dir and stat
        entries.extend(entry_for(item.file_name(), &item.path()));
    }
    entries.sort_by_cached_key(|e| {
        (
            !e.is_dir,
            e.name.to_string_lossy().to_lowercase(),
            e.name.clone(),
        )
    });
    if dir.parent().is_some() {
        entries.insert(0, Entry::parent());
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_back_forward_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = a.join("b");
        fs::create_dir_all(&b).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.go(a.clone()).unwrap();
        p.go(a.clone()).unwrap(); // consecutive duplicate
        p.go(b.clone()).unwrap();
        assert_eq!(p.history.len(), 3);
        p.history_step(-1).unwrap();
        assert_eq!(p.path(), a);
        p.history_step(-1).unwrap();
        p.history_step(-1).unwrap(); // at start: no-op
        assert_eq!(p.path(), std::path::absolute(tmp.path()).unwrap());
        p.history_step(1).unwrap();
        assert_eq!(p.path(), a);
        p.go(tmp.path().to_owned()).unwrap(); // drops forward entries
        assert_eq!(p.history.len(), 3);
        p.history_step(1).unwrap();
        assert_eq!(p.path(), tmp.path());
        for i in 0..HISTORY_MAX + 5 {
            p.go(if i % 2 == 0 {
                a.clone()
            } else {
                tmp.path().to_owned()
            })
            .unwrap();
        }
        assert_eq!(p.history.len(), HISTORY_MAX);
    }

    fn names(p: &Panel) -> Vec<String> {
        p.entries()
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn sorts_dirs_first_then_name() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("b.txt"), "").unwrap();
        fs::write(tmp.path().join("A.txt"), "").unwrap();
        fs::create_dir(tmp.path().join("z")).unwrap();
        let p = Panel::open(tmp.path()).unwrap();
        assert_eq!(names(&p), ["..", "z", "A.txt", "b.txt"]);
    }

    #[test]
    fn enter_and_parent_restore_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("a")).unwrap();
        fs::create_dir(tmp.path().join("b")).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.move_cursor(2);
        assert!(p.enter().unwrap());
        assert!(p.path().ends_with("b"));
        assert!(p.enter().unwrap()); // ".."
        assert_eq!(p.current().unwrap().name, "b");
    }

    #[test]
    fn failed_enter_keeps_panel() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("gone")).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.move_cursor(1);
        fs::remove_dir(tmp.path().join("gone")).unwrap();
        assert!(p.enter().is_err());
        assert_eq!(p.path(), std::path::absolute(tmp.path()).unwrap());
    }

    #[test]
    fn selection_drives_targets() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a"), "").unwrap();
        fs::write(tmp.path().join("b"), "").unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        assert!(p.targets().is_empty()); // cursor on ".."
        p.toggle_select(); // ".." not selectable, moves to "a"
        assert_eq!(p.targets(), [p.path().join("a")]);
        p.toggle_select(); // select "a"
        p.toggle_select(); // select "b"
        assert_eq!(p.targets(), [p.path().join("a"), p.path().join("b")]);
        fs::remove_file(tmp.path().join("a")).unwrap();
        p.reload().unwrap();
        assert_eq!(p.targets(), [p.path().join("b")]);
    }

    #[test]
    fn mask_and_invert() {
        let tmp = tempfile::tempdir().unwrap();
        for n in ["a.rs", "b.RS", "c.txt"] {
            fs::write(tmp.path().join(n), "").unwrap();
        }
        fs::create_dir(tmp.path().join("d.rs")).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.select_mask("*.rs", true);
        assert_eq!(p.targets(), [p.path().join("a.rs"), p.path().join("b.RS")]);
        p.invert_selection();
        assert_eq!(p.targets(), [p.path().join("c.txt")]);
        p.select_mask("", true);
        p.select_mask("c.*", false);
        assert_eq!(p.targets().len(), 2);
    }

    #[test]
    fn filter_and_search() {
        let tmp = tempfile::tempdir().unwrap();
        for n in ["apple", "Banana", "grape"] {
            fs::write(tmp.path().join(n), "").unwrap();
        }
        let mut p = Panel::open(tmp.path()).unwrap();
        assert!(p.search("AN", 0));
        assert_eq!(p.current().unwrap().name, "Banana");
        assert!(!p.search("zzz", 0));
        assert!(p.search("ap", 1)); // wraps past the end
        p.apply_filter("AP");
        assert_eq!(names(&p), ["..", "apple", "grape"]);
        assert_eq!(p.current().unwrap().name, "grape");
        fs::write(tmp.path().join("cap"), "").unwrap();
        p.reload().unwrap(); // filter survives reload
        assert_eq!(names(&p), ["..", "apple", "cap", "grape"]);
        p.apply_filter("");
        assert_eq!(p.entries().len(), 5);
    }

    #[test]
    fn panelize_and_leave() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, "").unwrap();
        fs::write(&b, "").unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        let list = [&a, &b].map(|f| entry_for(f.clone().into_os_string(), f).unwrap());
        p.panelize(list.to_vec());
        assert!(p.is_virtual());
        p.toggle_select();
        assert_eq!(p.targets(), std::slice::from_ref(&a));
        fs::remove_file(&b).unwrap();
        p.reload().unwrap();
        assert_eq!(p.entries().len(), 1);
        p.parent().unwrap();
        assert!(!p.is_virtual() && p.entries().len() == 2); // "..", a
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_and_dangling_symlink() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let bad = OsStr::from_bytes(b"x\xff");
        // APFS rejects non-UTF-8 names; Linux filesystems accept them
        let bad_ok = fs::write(tmp.path().join(bad), "").is_ok();
        std::os::unix::fs::symlink("missing", tmp.path().join("link")).unwrap();
        let p = Panel::open(tmp.path()).unwrap();
        assert_eq!(p.entries().iter().any(|e| e.name == bad), bad_ok);
        assert!(
            p.entries()
                .iter()
                .any(|e| e.name == "link" && e.is_symlink && !e.is_dir)
        );
    }
}
