//! One file panel: a directory listing with cursor and selection.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::archive::Archive;
use crate::vfs::{Remote, Side, rpath};
use crate::{Result, io_err};

mod entries;
mod filter;
mod git;
mod load;
mod select;
pub use filter::{EntryFilter, ext_counts};

pub(crate) use entries::{entry_for, entry_with};
use entries::{read_entries, sort_entries};
use load::{Pending, Then};

const PARENT: &str = "..";
/// Directory history entries kept per panel.
const HISTORY_MAX: usize = 100;
/// How long a directory change waits for its listing before showing it later (slow mounts, huge dirs).
const LOAD_WAIT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: OsString,
    /// Directory, or symlink pointing to one.
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Target of a symlink (read once with the entry).
    pub link: Option<PathBuf>,
    /// Git-ignored or a `.git` dir; set for local listings only.
    pub ignored: bool,
}

/// What the list is ordered by; directories always come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Ext,
    Size,
    Date,
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
            link: None,
            ignored: false,
        }
    }
}

/// What Ctrl+Y selects. "Single" = present only on this side; the others need the name on both sides
/// (directories on both sides never differ).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareKind {
    Newer,
    Single,
    NewerSingle,
    Different,
    DifferentSingle,
}

impl CompareKind {
    pub const ALL: [Self; 5] = [
        Self::NewerSingle,
        Self::Newer,
        Self::Single,
        Self::DifferentSingle,
        Self::Different,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Newer => "Newer",
            Self::Single => "Single (only here)",
            Self::NewerSingle => "Newer + single",
            Self::Different => "Different (size or date)",
            Self::DifferentSingle => "Different + single",
        }
    }

    fn picks(self, e: &Entry, other: Option<&Entry>) -> bool {
        let Some(o) = other else {
            return !matches!(self, Self::Newer | Self::Different);
        };
        if e.is_dir {
            return false;
        }
        let newer = e.modified > o.modified;
        match self {
            Self::Newer | Self::NewerSingle => newer,
            Self::Different | Self::DifferentSingle => e.size != o.size || newer,
            Self::Single => false,
        }
    }
}

/// Names to select on each side for `kind`.
pub fn compare(
    a: &[Entry],
    b: &[Entry],
    kind: CompareKind,
) -> (HashSet<OsString>, HashSet<OsString>) {
    let pick = |x: &[Entry], y: &[Entry]| {
        let other: HashMap<&OsString, &Entry> = y.iter().map(|e| (&e.name, e)).collect();
        x.iter()
            .filter(|e| !e.is_parent())
            .filter(|e| kind.picks(e, other.get(&e.name).copied()))
            .map(|e| e.name.clone())
            .collect()
    };
    (pick(a, b), pick(b, a))
}

#[derive(Debug)]
pub struct Panel {
    path: PathBuf,
    entries: Vec<Entry>,
    cursor: usize,
    selected: HashSet<OsString>,
    /// Last dropped selection and its directory, for [`Panel::restore_selection`].
    prev_sel: Option<(PathBuf, HashSet<OsString>)>,
    /// Active filter (`/` popup); `all` keeps the unfiltered list while it is set.
    filter: EntryFilter,
    all: Option<Vec<Entry>>,
    /// Visited directories, oldest first, `pos` is the current one.
    history: Vec<PathBuf>,
    pos: usize,
    /// Local directory just shown by a visit, for [`Panel::take_visited`].
    visited: Option<PathBuf>,
    /// Panelized result list: entry names are absolute paths, `path` is where Backspace/Esc returns to.
    virt: bool,
    /// Line of the first content match per found entry (by name), for F3/F4 at the match.
    hits: HashMap<OsString, u64>,
    /// Open archive and the directory inside it (`""` = root); `path` is then the archive file joined with it.
    archive: Option<(Arc<Archive>, String)>,
    /// Connected remote; `path` is then the remote path and `local` the directory ".." at its root returns to.
    remote: Option<Arc<dyn Remote>>,
    local: PathBuf,
    /// Sort key and reverse flag; survives directory changes.
    sort: (SortKey, bool),
    /// Dot-names listed (default); only real directories are filtered.
    /// Cached (count, bytes) of the selection among visible entries; `None` = stale.
    sel_sum: Cell<Option<(usize, u64)>>,
    /// Directory sizes calculated on request (by entry name); dropped when the list is re-read.
    dir_sizes: HashMap<OsString, u64>,
    pending: Option<Pending>,
}

impl Panel {
    pub fn open(path: &Path) -> Result<Self> {
        let path = std::path::absolute(path).map_err(io_err(path))?;
        let entries = read_entries(&path, (SortKey::Name, false), &AtomicBool::new(false))?;
        Ok(Self {
            history: vec![path.clone()],
            pos: 0,
            visited: None,
            local: path.clone(),
            path,
            entries,
            cursor: 0,
            selected: HashSet::new(),
            prev_sel: None,
            filter: EntryFilter::default(),
            all: None,
            virt: false,
            hits: HashMap::new(),
            archive: None,
            remote: None,
            sort: (SortKey::Name, false),
            sel_sum: Cell::new(None),
            dir_sizes: HashMap::new(),
            pending: None,
        })
    }

    /// The local directory a user navigation just showed (once); none for history steps,
    /// remote, archive and found lists.
    pub fn take_visited(&mut self) -> Option<PathBuf> {
        self.visited.take()
    }

    pub fn sort(&self) -> (SortKey, bool) {
        self.sort
    }

    /// Same key reverses the order, another key sorts ascending. In memory, cursor stays on its entry.
    pub fn set_sort(&mut self, key: SortKey) {
        self.sort = (key, self.sort.0 == key && !self.sort.1);
        let keep = self.current().map(|e| e.name.clone());
        let sort = self.sort;
        for list in std::iter::once(&mut self.entries).chain(self.all.as_mut()) {
            let start = usize::from(list.first().is_some_and(Entry::is_parent));
            sort_entries(&mut list[start..], sort);
        }
        if let Some(name) = keep {
            self.focus(&name);
        }
    }

    /// Remember the calculated size of directory `name` (counts in the selection sum).
    pub fn set_dir_size(&mut self, name: OsString, bytes: u64) {
        self.dir_sizes.insert(name, bytes);
        self.sel_sum.set(None);
    }

    pub fn dir_size(&self, e: &Entry) -> Option<u64> {
        self.dir_sizes.get(&e.name).copied()
    }

    /// (count, bytes) of the selected visible entries; a directory adds its calculated size, else nothing. Cached until the selection, sizes or list change.
    pub fn selection_info(&self) -> (usize, u64) {
        if let Some(sum) = self.sel_sum.get() {
            return sum;
        }
        let sel = self.entries.iter().filter(|e| self.is_selected(e));
        let sum = sel.fold((0, 0), |(n, b), e| {
            let size = if e.is_dir {
                self.dir_size(e).unwrap_or(0)
            } else {
                e.size
            };
            (n + 1, b + size)
        });
        self.sel_sum.set(Some(sum));
        sum
    }

    pub fn archive(&self) -> Option<&Arc<Archive>> {
        self.archive.as_ref().map(|(a, _)| a)
    }

    /// Panel header: the path, inside an archive `file.zip:/sub`, remote `sftp://host/dir`.
    pub fn title(&self) -> String {
        match (&self.archive, &self.remote) {
            (Some((a, sub)), _) => format!("{}:/{sub}", a.file.display()),
            (None, Some(r)) => format!("{}{}", r.url(), rpath(&self.path)),
            (None, None) => self.path.display().to_string(),
        }
    }

    pub fn remote(&self) -> Option<&Arc<dyn Remote>> {
        self.remote.as_ref()
    }

    /// Where the panel's paths live.
    pub fn side(&self) -> Side {
        self.remote.clone().map_or(Side::Local, Side::Remote)
    }

    /// Show directory `dir` of `remote` (read in a worker like a local one); a failure leaves the panel unchanged.
    pub fn open_remote(&mut self, remote: Arc<dyn Remote>, dir: PathBuf) -> Result<()> {
        if self.remote.is_none() && self.archive.is_none() {
            self.local = self.path.clone();
        }
        let then = Then::Show {
            pos: None,
            visit: false,
            focus: None,
        };
        self.load_on(Some(remote), dir, then)
    }

    /// Back to the local directory the remote was opened from; drops this panel's hold on the session.
    pub fn disconnect(&mut self) -> Result<()> {
        let pending = self.pending.as_ref().is_some_and(|p| p.remote.is_some());
        if self.remote.is_none() && !pending {
            return Ok(());
        }
        let then = Then::Show {
            pos: None,
            visit: false,
            focus: None,
        };
        self.load_on(None, self.local.clone(), then)
    }

    /// New panel on the same directory and side (Ctrl+T).
    pub fn duplicate(&self) -> Result<Self> {
        let mut p = Self::open(&self.local)?;
        if let Some(r) = &self.remote {
            p.open_remote(r.clone(), self.path.clone())?;
        }
        Ok(p)
    }

    /// Show the root of `archive` (read-only); leaving it with ".." returns to the containing directory.
    pub fn open_archive(&mut self, archive: Archive) {
        self.archive = Some((Arc::new(archive), String::new()));
        self.show_archive();
    }

    /// Archive directory listing, sorted like a real one; always has "..".
    fn archive_entries(&self) -> Option<Vec<Entry>> {
        let (a, sub) = self.archive.as_ref()?;
        let mut e = a.list(sub);
        sort_entries(&mut e, self.sort);
        e.insert(0, Entry::parent());
        Some(e)
    }

    fn show_archive(&mut self) {
        self.cancel_load();
        let Some(entries) = self.archive_entries() else {
            return;
        };
        // remembered under the directory the names belong to
        self.unselect_all();
        let Some((a, sub)) = &self.archive else {
            return;
        };
        self.path = if sub.is_empty() {
            a.file.clone()
        } else {
            a.file.join(sub)
        };
        self.entries = entries;
        self.all = None;
        self.filter = EntryFilter::default();
        self.virt = false;
        self.cursor = 0;
    }

    pub fn is_virtual(&self) -> bool {
        self.virt
    }

    /// First content match line of a found entry; `None` outside a found list.
    pub fn hit_line(&self, name: &OsStr) -> Option<u64> {
        self.hits.get(name).copied().filter(|_| self.virt)
    }

    /// Show `entries` (names are absolute paths) as a flat list in search order; `hits` holds the
    /// first matching line per name. `leave_virtual` goes back.
    pub fn panelize(&mut self, entries: Vec<Entry>, hits: HashMap<OsString, u64>) {
        self.cancel_load();
        self.entries = entries;
        self.hits = hits;
        self.all = None;
        self.filter = EntryFilter::default();
        self.unselect_all();
        self.cursor = 0;
        self.virt = true;
    }

    /// Back to the directory the list was made from; false if the panel was not virtual.
    pub fn leave_virtual(&mut self) -> Result<bool> {
        if !self.virt {
            return Ok(false);
        }
        self.load(
            self.path.clone(),
            Then::Show {
                pos: None,
                visit: false,
                focus: None,
            },
        )?;
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
    /// A pending directory change is restarted instead (current sort).
    pub fn reload(&mut self) -> Result<()> {
        if let Some(p) = self.pending.take() {
            p.cancel.store(true, Ordering::Relaxed);
            if matches!(p.then, Then::Restat) {
                return self.restat();
            }
            return self.load_on(p.remote, p.path, p.then);
        }
        if let Some(e) = self.archive_entries() {
            self.reloaded(e);
            return Ok(());
        }
        if self.virt {
            return self.restat();
        }
        self.load(self.path.clone(), Then::Reload)
    }

    /// Stat the (unfiltered) virtual list in a worker: up to 100k files, maybe on a slow mount.
    fn restat(&mut self) -> Result<()> {
        let names: Vec<OsString> = (self.all.as_ref().unwrap_or(&self.entries).iter())
            .map(|e| e.name.clone())
            .collect();
        self.start(None, self.path.clone(), Then::Restat, move |cancel| {
            // entries that vanished (e.g. deleted from the list) drop out
            Ok(names
                .into_iter()
                .take_while(|_| !cancel.load(Ordering::Relaxed))
                .filter_map(|n| {
                    let path = PathBuf::from(&n);
                    entry_for(n, &path)
                })
                .collect())
        })
    }

    fn reloaded(&mut self, entries: Vec<Entry>) {
        self.dir_sizes.clear();
        let keep = self.current().map(|e| e.name.clone());
        self.entries = entries;
        self.all = None;
        if !self.filter.is_empty() {
            let f = std::mem::take(&mut self.filter);
            self.apply_filter(f);
        }
        let names: HashSet<&OsStr> = self.entries.iter().map(|e| e.name.as_os_str()).collect();
        self.selected.retain(|n| names.contains(n.as_os_str()));
        self.sel_sum.set(None);
        let old = self.cursor;
        self.cursor = 0;
        match keep {
            Some(name) if self.focus(&name) => {}
            _ => self.move_cursor(old as isize),
        }
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
        let (is_parent, name) = (entry.is_parent(), entry.name.clone());
        if is_parent {
            self.parent()?;
        } else if let Some((_, sub)) = &mut self.archive {
            if !sub.is_empty() {
                sub.push('/');
            }
            sub.push_str(&name.to_string_lossy());
            self.show_archive();
        } else {
            let path = self.path.join(&name);
            self.change_dir(path)?;
        }
        Ok(true)
    }

    /// Go to the parent directory, cursor on the directory we came from; at a remote root back to the local directory.
    pub fn parent(&mut self) -> Result<()> {
        if self.leave_virtual()? {
            return Ok(());
        }
        if self.remote.is_some() && self.archive.is_none() && self.path.parent().is_none() {
            let then = Then::Show {
                pos: None,
                visit: false,
                focus: None,
            };
            return self.load_on(None, self.local.clone(), then);
        }
        if let Some((_, sub)) = &mut self.archive
            && !sub.is_empty()
        {
            let came = match sub.rsplit_once('/') {
                Some((up, last)) => {
                    let last = last.to_owned();
                    sub.truncate(up.len());
                    last
                }
                None => std::mem::take(sub),
            };
            self.show_archive();
            self.focus(OsStr::new(&came));
            return Ok(());
        }
        let (Some(parent), Some(came_from)) = (self.path.parent(), self.path.file_name()) else {
            return Ok(());
        };
        let then = Then::Show {
            pos: None,
            visit: true,
            focus: Some(came_from.to_owned()),
        };
        self.load(parent.to_owned(), then)
    }

    /// Jump to local `path` (e.g. a bookmark), leaving a remote; a failure leaves the panel unchanged.
    pub fn go(&mut self, path: PathBuf) -> Result<()> {
        let then = Then::Show {
            pos: None,
            visit: true,
            focus: None,
        };
        self.load_on(None, path, then)
    }

    /// Jump to the local directory of `file` with the cursor on it (leaves a found list).
    pub fn reveal(&mut self, file: &Path) -> Result<()> {
        let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
            return Ok(());
        };
        let then = Then::Show {
            pos: None,
            visit: true,
            focus: Some(name.to_owned()),
        };
        self.load_on(None, dir.to_owned(), then)
    }

    /// Change to `path` on the panel's current side (local or remote).
    pub fn cd(&mut self, path: PathBuf) -> Result<()> {
        self.change_dir(path)
    }

    /// Directory history, oldest first, and the index of the shown entry.
    pub fn history(&self) -> (&[PathBuf], usize) {
        (&self.history, self.pos)
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
        let then = Then::Show {
            pos: Some(i),
            visit: false,
            focus: None,
        };
        self.load_on(None, self.history[i].clone(), then)
    }

    /// New directory: visiting it drops the forward history; repeats are not recorded.
    fn change_dir(&mut self, path: PathBuf) -> Result<()> {
        self.load(
            path,
            Then::Show {
                pos: None,
                visit: true,
                focus: None,
            },
        )
    }

    fn visit(&mut self, path: &Path) {
        if self.history.get(self.pos).map(PathBuf::as_path) != Some(path) {
            self.history.truncate(self.pos + 1);
            self.history.push(path.to_owned());
            if self.history.len() > HISTORY_MAX {
                self.history.remove(0);
            }
            self.pos = self.history.len() - 1;
        }
    }

    pub fn focus(&mut self, name: &OsStr) -> bool {
        match self.entries.iter().position(|e| e.name == name) {
            Some(i) => {
                self.cursor = i;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn compare_selects_only_differences() {
        let t = SystemTime::UNIX_EPOCH;
        let e = |n: &str, dir: bool, size: u64, s: u64| Entry {
            name: n.into(),
            is_dir: dir,
            is_symlink: false,
            size,
            modified: Some(t + std::time::Duration::from_secs(s)),
            link: None,
            ignored: false,
        };
        let a = [
            e("same", false, 1, 5),
            e("only_a", false, 1, 1),
            e("newer", false, 1, 9),
            e("older", false, 1, 1),
            e("size", false, 2, 5),
            e("d", true, 0, 9),
        ];
        let b = [
            e("same", false, 1, 5),
            e("only_b", true, 0, 1),
            e("newer", false, 1, 5),
            e("older", false, 1, 5),
            e("size", false, 3, 5),
            e("d", true, 0, 1),
        ];
        let n = |v: &[&str]| v.iter().map(OsString::from).collect::<HashSet<_>>();
        let run = |k| compare(&a, &b, k);
        use CompareKind::*;
        assert_eq!(run(Newer), (n(&["newer"]), n(&["older"])));
        assert_eq!(run(Single), (n(&["only_a"]), n(&["only_b"])));
        assert_eq!(
            run(NewerSingle),
            (n(&["only_a", "newer"]), n(&["only_b", "older"]))
        );
        assert_eq!(
            run(Different),
            (n(&["newer", "size"]), n(&["older", "size"]))
        );
        assert_eq!(
            run(DifferentSingle),
            (
                n(&["only_a", "newer", "size"]),
                n(&["only_b", "older", "size"])
            )
        );
    }

    fn settle(p: &mut Panel) {
        while p.loading().is_some() {
            if let Some(r) = p.wait(Duration::from_secs(5)) {
                r.unwrap();
            }
        }
    }

    #[test]
    fn remote_browse_and_leave_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        let m = crate::vfs::mem::Mem::new();
        m.dir("/d");
        m.put("/d/f", b"x");
        m.put("/.h", b"");
        let mut p = Panel::open(tmp.path()).unwrap();
        p.open_remote(m.clone(), "/".into()).unwrap();
        settle(&mut p);
        assert_eq!(p.title(), "mem:///");
        let names: Vec<_> = p.entries().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["..", "d", ".h"]);
        p.set_cursor(1);
        p.enter().unwrap();
        settle(&mut p);
        assert_eq!(p.title(), "mem:///d");
        p.set_cursor(1);
        assert_eq!(p.targets(), [PathBuf::from("/d/f")]);
        let dup = p.duplicate().unwrap();
        assert!(dup.remote().is_some());
        p.parent().unwrap();
        settle(&mut p);
        p.parent().unwrap();
        settle(&mut p);
        assert!(p.remote().is_none());
        assert_eq!(p.path(), tmp.path());
    }

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
        p.apply_filter(EntryFilter::by_name("AP"));
        assert_eq!(names(&p), ["..", "apple", "grape"]);
        assert_eq!(p.current().unwrap().name, "grape");
        fs::write(tmp.path().join("cap"), "").unwrap();
        p.reload().unwrap(); // filter survives reload
        assert_eq!(names(&p), ["..", "apple", "cap", "grape"]);
        p.apply_filter(EntryFilter::default());
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
        p.panelize(
            list.to_vec(),
            HashMap::from([(a.clone().into_os_string(), 3)]),
        );
        assert_eq!(p.hit_line(a.as_os_str()), Some(3));
        assert!(p.is_virtual());
        p.toggle_select();
        assert_eq!(p.targets(), std::slice::from_ref(&a));
        fs::remove_file(&b).unwrap();
        p.reload().unwrap();
        settle(&mut p);
        assert!(p.is_virtual());
        assert_eq!(p.targets(), std::slice::from_ref(&a)); // selection kept
        assert_eq!(p.entries().len(), 1);
        p.parent().unwrap();
        assert!(!p.is_virtual() && p.entries().len() == 2); // "..", a
    }

    #[test]
    fn archive_navigation() {
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, AtomicU64};
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("a.zip");
        let mut z = zip::ZipWriter::new(fs::File::create(&zip).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("d/e/f.txt", o).unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        let a = Archive::open(&zip, &AtomicU64::new(0), &AtomicBool::new(false)).unwrap();
        p.open_archive(a);
        assert_eq!(p.title(), format!("{}:/", zip.display()));
        assert_eq!(names(&p), ["..", "d"]);
        p.move_cursor(1);
        assert!(p.enter().unwrap()); // d
        p.move_cursor(1);
        assert!(p.enter().unwrap()); // d/e
        assert_eq!(p.title(), format!("{}:/d/e", zip.display()));
        assert_eq!(names(&p), ["..", "f.txt"]);
        assert_eq!(p.targets().len(), 0);
        p.parent().unwrap();
        assert_eq!(p.current().unwrap().name, "e");
        p.parent().unwrap();
        p.parent().unwrap(); // root of archive: back to the directory
        assert!(p.archive().is_none());
        assert_eq!(p.current().unwrap().name, "a.zip");
        assert_eq!(p.path(), std::path::absolute(tmp.path()).unwrap());
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
        let link = p.entries().iter().find(|e| e.name == "link").unwrap();
        assert_eq!(link.link, Some("missing".into()));
    }

    #[test]
    fn selection_sum_cached_and_invalidated() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a"), "123").unwrap();
        fs::write(tmp.path().join("b"), "12345").unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.select_mask("*", true);
        assert_eq!(p.selection_info(), (2, 8));
        p.set_cursor(1);
        p.toggle_select(); // deselect "a"
        assert_eq!(p.selection_info(), (1, 5));
        p.select_mask("*", true);
        p.apply_filter(EntryFilter::by_name("b"));
        assert_eq!(p.selection_info(), (1, 5));
    }

    #[test]
    fn sort_keys_reverse_and_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("b.txt"), "12345").unwrap();
        fs::write(tmp.path().join("a.rs"), "123456789").unwrap();
        fs::create_dir(tmp.path().join("z")).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.set_cursor(3); // b.txt
        p.set_sort(SortKey::Size);
        assert_eq!(names(&p), ["..", "z", "b.txt", "a.rs"]);
        assert_eq!(p.current().unwrap().name, "b.txt");
        p.set_sort(SortKey::Size);
        assert_eq!(names(&p), ["..", "z", "a.rs", "b.txt"]);
        p.set_sort(SortKey::Ext);
        assert_eq!(names(&p), ["..", "z", "a.rs", "b.txt"]);
        p.set_sort(SortKey::Ext); // reversed, dirs stay first
        assert_eq!(names(&p), ["..", "z", "b.txt", "a.rs"]);
        p.reload().unwrap(); // sort survives re-read
        assert_eq!(names(&p), ["..", "z", "b.txt", "a.rs"]);
    }

    #[test]
    fn dot_names_are_listed() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".a"), "").unwrap();
        fs::write(tmp.path().join("b"), "").unwrap();
        let p = Panel::open(tmp.path()).unwrap();
        assert_eq!(names(&p), ["..", ".a", "b"]);
    }

    #[test]
    fn selected_dir_adds_calculated_size() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("d")).unwrap();
        fs::write(tmp.path().join("f"), "123").unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.select_mask("*", true);
        p.set_cursor(1);
        p.toggle_select(); // "d"
        assert_eq!(p.selection_info(), (2, 3));
        p.set_dir_size("d".into(), 10);
        assert_eq!(p.selection_info(), (2, 13));
        p.reload().unwrap(); // stale after a re-read
        assert_eq!(p.selection_info(), (2, 3));
    }
}
