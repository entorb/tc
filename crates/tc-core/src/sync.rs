//! Sync Dirs one way: compare the files of two directories (top level only,
//! by size and date), then make the target match the source.

use std::cmp;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use crate::ops::{self, Decision, Progress};
use crate::panel::{Entry, entry_with};
use crate::vfs::{self, Side, rpath};
use crate::{Error, Result, io_err};

/// How the two files of a name compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Equal,
    LeftOnly,
    RightOnly,
    /// Size or date differ; [`Item::order`] says how.
    Differs,
}

impl Status {
    /// Status column: an arrow points from the side that has the file, `≠` differs.
    pub fn icon(self) -> &'static str {
        match self {
            Status::Equal => "=",
            Status::LeftOnly => "➡",
            Status::RightOnly => "⬅",
            Status::Differs => "≠",
        }
    }
}

/// One file name found in either directory.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: OsString,
    pub left: Option<Entry>,
    pub right: Option<Entry>,
}

/// Whole seconds: file systems and servers store times at different precision.
fn secs(e: &Entry) -> Option<u64> {
    e.modified
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

impl Item {
    /// Equal by size and date (`ignore_date`: by size alone).
    pub fn status(&self, ignore_date: bool) -> Status {
        match (&self.left, &self.right) {
            (Some(l), Some(r)) if l.size == r.size && (ignore_date || secs(l) == secs(r)) => {
                Status::Equal
            }
            (Some(_), Some(_)) => Status::Differs,
            (Some(_), None) => Status::LeftOnly,
            _ => Status::RightOnly,
        }
    }

    /// Left against right file: (date, size) order, `None` when one side is missing;
    /// `ignore_date` makes the dates compare equal.
    pub fn order(&self, ignore_date: bool) -> Option<(cmp::Ordering, cmp::Ordering)> {
        let (l, r) = (self.left.as_ref()?, self.right.as_ref()?);
        let date = if ignore_date {
            cmp::Ordering::Equal
        } else {
            secs(l).cmp(&secs(r))
        };
        Some((date, l.size.cmp(&r.size)))
    }

    /// The entry on side `i` (0 left, 1 right).
    pub fn side(&self, i: usize) -> Option<&Entry> {
        if i == 0 {
            self.left.as_ref()
        } else {
            self.right.as_ref()
        }
    }
}

/// List the files of `roots[0]` on `sides[0]` (left) and `roots[1]` on `sides[1]` (right),
/// sorted by name. Subdirectories are not entered; a name that is a directory on either side
/// is left out. `p.files` counts listed names; `p.cancel` stops.
pub fn compare(sides: &[Side; 2], roots: &[PathBuf; 2], p: &Progress) -> Result<Vec<Item>> {
    let mut names: BTreeMap<OsString, [Option<Entry>; 2]> = BTreeMap::new();
    for (i, side) in sides.iter().enumerate() {
        for e in list(side, &roots[i])? {
            if p.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            p.files.fetch_add(1, Ordering::Relaxed);
            let slot = &mut names.entry(e.name.clone()).or_default()[i];
            *slot = Some(e);
        }
    }
    let files = names
        .into_iter()
        .filter(|(_, pair)| !pair.iter().flatten().any(|e| e.is_dir));
    Ok(files
        .map(|(name, [left, right])| Item { name, left, right })
        .collect())
}

fn list(side: &Side, dir: &Path) -> Result<Vec<Entry>> {
    if let Some(r) = side.remote() {
        return r.list(&rpath(dir));
    }
    let mut out = Vec::new();
    for item in fs::read_dir(dir).map_err(io_err(dir))? {
        let item = item.map_err(io_err(dir))?;
        // Err: vanished between read_dir and stat
        if let Ok(meta) = item.metadata() {
            out.push(entry_with(item.file_name(), &item.path(), meta));
        }
    }
    Ok(out)
}

/// What a one-way sync from side `from` does.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Names copied from the source, overwriting the target's file.
    pub copy: Vec<OsString>,
    /// Names only on the target, deleted.
    pub delete: Vec<OsString>,
}

/// Every file of `from` that is missing or different on the other side is copied;
/// with `delete_extras`, the target's files missing on `from` are deleted.
pub fn plan(items: &[Item], from: usize, ignore_date: bool, delete_extras: bool) -> Plan {
    let mut plan = Plan::default();
    for it in items
        .iter()
        .filter(|it| it.status(ignore_date) != Status::Equal)
    {
        if it.side(from).is_some() {
            plan.copy.push(it.name.clone());
        } else if delete_extras {
            plan.delete.push(it.name.clone());
        }
    }
    plan
}

/// Run `plan` from side `from` to the other: copies first (overwriting, the user confirmed),
/// then deletes (local: trash, remote: permanently). Stops at the first error.
pub fn run(
    sides: &[Side; 2],
    roots: &[PathBuf; 2],
    from: usize,
    plan: &Plan,
    p: &Progress,
) -> Result<()> {
    let to = 1 - from;
    let srcs: Vec<PathBuf> = plan.copy.iter().map(|n| roots[from].join(n)).collect();
    vfs::scan(&sides[from], &srcs, p);
    let local = matches!((&sides[from], &sides[to]), (Side::Local, Side::Local));
    let mut overwrite = |_: &Path, _: &Path| Decision::Overwrite;
    for (s, name) in srcs.iter().zip(&plan.copy) {
        let t = roots[to].join(name);
        if local {
            ops::copy(s, &t, &mut overwrite, p)?;
        } else {
            vfs::transfer(&sides[from], s, &sides[to], &t, false, &mut overwrite, p)?;
        }
    }
    for name in &plan.delete {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let t = roots[to].join(name);
        match &sides[to] {
            Side::Local => ops::trash(&t)?,
            side => side.delete(&t)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Plan, Progress, Side, Status, compare, plan, run};
    use crate::vfs::mem::Mem;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    fn put(p: &Path, text: &str, age: u64) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age);
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn pair() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        put(&l.join("same.txt"), "a", 100);
        put(&r.join("same.txt"), "a", 100);
        put(&l.join("newer.txt"), "new", 10);
        put(&r.join("newer.txt"), "old", 100);
        put(&l.join("older.txt"), "old", 100);
        put(&r.join("older.txt"), "new!", 10);
        put(&l.join("size.txt"), "ab", 100);
        put(&r.join("size.txt"), "abc", 100);
        put(&l.join("left.txt"), "l", 100);
        put(&r.join("right.log"), "r", 100);
        put(&l.join("sub/deep.txt"), "d", 100);
        put(&r.join("sub/deep.txt"), "DD", 100);
        put(&l.join("mixed/x.txt"), "x", 100);
        put(&r.join("mixed"), "a file", 100);
        (t, l, r)
    }

    fn statuses(l: &Path, r: &Path, ignore_date: bool) -> Vec<(String, Status)> {
        let roots = [l.to_owned(), r.to_owned()];
        let items = compare(&[Side::Local, Side::Local], &roots, &Progress::default()).unwrap();
        (items.iter())
            .map(|i| (i.name.to_string_lossy().into(), i.status(ignore_date)))
            .collect()
    }

    fn names(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn compare_top_level_files_by_size_and_date() {
        let (_t, l, r) = pair();
        let want = |newer: Status| -> Vec<(String, Status)> {
            [
                ("left.txt", Status::LeftOnly),
                ("newer.txt", newer),
                ("older.txt", Status::Differs),
                ("right.log", Status::RightOnly),
                ("same.txt", Status::Equal),
                ("size.txt", Status::Differs),
            ]
            .map(|(n, s)| (n.to_owned(), s))
            .into()
        };
        assert_eq!(statuses(&l, &r, false), want(Status::Differs));
        // same size: equal
        assert_eq!(statuses(&l, &r, true), want(Status::Equal));
        let roots = [l, r];
        let items = compare(&[Side::Local, Side::Local], &roots, &Progress::default()).unwrap();
        use std::cmp::Ordering::{Equal, Greater, Less};
        let order = |nodate| -> Vec<_> { items.iter().map(|i| i.order(nodate)).collect() };
        let o = order(false);
        assert_eq!(
            [o[0], o[1], o[2], o[3], o[5]],
            [
                None,
                Some((Greater, Equal)),
                Some((Less, Less)),
                None,
                Some((Equal, Less))
            ]
        );
        assert_eq!(order(true)[2], Some((Equal, Less)));
    }

    #[test]
    fn plan_copies_differences_one_way_and_deletes_extras_on_request() {
        let (_t, l, r) = pair();
        let roots = [l, r];
        let items = compare(&[Side::Local, Side::Local], &roots, &Progress::default()).unwrap();
        let to_right = ["left.txt", "newer.txt", "older.txt", "size.txt"];
        assert_eq!(
            plan(&items, 0, false, false),
            Plan {
                copy: names(&to_right),
                delete: vec![]
            }
        );
        assert_eq!(plan(&items, 0, false, true).delete, names(&["right.log"]));
        let to_left = plan(&items, 1, true, true);
        assert_eq!(to_left.copy, names(&["older.txt", "right.log", "size.txt"]));
        assert_eq!(to_left.delete, names(&["left.txt"]));
    }

    #[test]
    fn run_overwrites_newer_target_and_leaves_extras() {
        let (_t, l, r) = pair();
        let roots = [l.clone(), r.clone()];
        let sides = [Side::Local, Side::Local];
        let items = compare(&sides, &roots, &Progress::default()).unwrap();
        run(
            &sides,
            &roots,
            0,
            &plan(&items, 0, false, false),
            &Progress::default(),
        )
        .unwrap();
        let read = |p: PathBuf| fs::read_to_string(p).unwrap();
        assert_eq!(read(r.join("newer.txt")), "new");
        assert_eq!(read(r.join("older.txt")), "old");
        assert_eq!(read(r.join("left.txt")), "l");
        assert_eq!(read(r.join("right.log")), "r");
        // subdirectories are not touched
        assert_eq!(read(r.join("sub/deep.txt")), "DD");
        let left: Vec<_> = statuses(&l, &r, false)
            .into_iter()
            .filter(|(_, s)| *s != Status::Equal)
            .collect();
        assert_eq!(left, [("right.log".to_owned(), Status::RightOnly)]);
    }

    #[test]
    fn run_deletes_extras_on_the_target_only() {
        let (ml, mr) = (Mem::new(), Mem::new());
        let sides = [Side::Remote(ml.clone()), Side::Remote(mr.clone())];
        ml.put("/a", b"x");
        mr.put("/a", b"x");
        mr.put("/extra", b"x");
        let roots = [PathBuf::from("/"), PathBuf::from("/")];
        let items = compare(&sides, &roots, &Progress::default()).unwrap();
        let p = plan(&items, 0, true, true);
        assert_eq!(p.delete, names(&["extra"]));
        run(&sides, &roots, 0, &p, &Progress::default()).unwrap();
        assert_eq!(ml.paths(), ["/", "/a"]);
        assert_eq!(mr.paths(), ["/", "/a"]);
    }
}
