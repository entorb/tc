//! Synchronize directories (like Total Commander): compare two trees, then copy and delete
//! the differences the user kept.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use crate::ops::{self, Decision, Progress};
use crate::panel::{Entry, entry_with};
use crate::search::Masks;
use crate::vfs::{self, Side, rpath};
use crate::{Error, Result, io_err};

/// Content compare chunk per side.
const CHUNK: usize = 1 << 16;

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// File name masks (`*`, `?`, comma or space separated); empty = all files.
    pub mask: String,
    pub subdirs: bool,
    /// Files of the same size are equal only with the same content.
    pub by_content: bool,
    /// Same size = equal; a difference gets no direction.
    pub ignore_date: bool,
    /// Make the right side a copy of the left: right-only entries are deleted.
    pub asymmetric: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Equal,
    /// Differs, no direction (same date, date ignored, or file against directory).
    NotEqual,
    ToRight,
    ToLeft,
    DeleteRight,
    /// The user dropped the action.
    Skip,
}

impl Action {
    pub fn symbol(self) -> &'static str {
        match self {
            Action::Equal => "=",
            Action::NotEqual => "!=",
            Action::ToRight => "->",
            Action::ToLeft => "<-",
            Action::DeleteRight => "del",
            Action::Skip => "--",
        }
    }
}

/// One compared name: a file, or a directory that exists on one side only (copied or deleted whole).
#[derive(Debug, Clone)]
pub struct Item {
    /// Path below both roots.
    pub rel: PathBuf,
    pub left: Option<Entry>,
    pub right: Option<Entry>,
    /// Result of the compare; show filters group by it.
    pub default: Action,
    /// What a sync does, changed by the user.
    pub action: Action,
}

impl Item {
    pub fn is_dir(&self) -> bool {
        self.left
            .as_ref()
            .or(self.right.as_ref())
            .is_some_and(|e| e.is_dir)
    }
}

/// Compare `roots[0]` on `sides[0]` (left) with `roots[1]` on `sides[1]` (right); items sorted
/// by path. Directory symlinks are not descended. `p.files` counts compared names; `p.cancel` stops.
pub fn compare(
    sides: &[Side; 2],
    roots: &[PathBuf; 2],
    o: &Options,
    p: &Progress,
) -> Result<Vec<Item>> {
    let same_remote = sides[0].remote().is_some() && sides[0].same(&sides[1]);
    if o.by_content && same_remote {
        return Err(Error::Remote(
            "compare by content needs two different connections or a local side".into(),
        ));
    }
    let masks = Masks::new(&o.mask);
    let mut items = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let mut names: BTreeMap<OsString, [Option<Entry>; 2]> = BTreeMap::new();
        for (i, side) in sides.iter().enumerate() {
            for e in list(side, &roots[i].join(&rel))? {
                let slot = &mut names.entry(e.name.clone()).or_default()[i];
                *slot = Some(e);
            }
        }
        p.dirs_done.fetch_add(1, Ordering::Relaxed);
        for (name, [l, r]) in names {
            if p.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            p.files.fetch_add(1, Ordering::Relaxed);
            let rel = rel.join(&name);
            let paths = [roots[0].join(&rel), roots[1].join(&rel)];
            let Some(action) = classify(sides, &paths, (l.as_ref(), r.as_ref()), o, &masks, p)?
            else {
                if is_plain_dir(l.as_ref()) && is_plain_dir(r.as_ref()) && o.subdirs {
                    stack.push(rel);
                }
                continue;
            };
            items.push(Item {
                rel,
                left: l,
                right: r,
                default: action,
                action,
            });
        }
    }
    items.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(items)
}

fn is_plain_dir(e: Option<&Entry>) -> bool {
    e.is_some_and(|e| e.is_dir && !e.is_symlink)
}

/// Action for one name, `None` if it is not listed (filtered out, or a directory on both sides).
fn classify(
    sides: &[Side; 2],
    paths: &[PathBuf; 2],
    pair: (Option<&Entry>, Option<&Entry>),
    o: &Options,
    masks: &Masks,
    p: &Progress,
) -> Result<Option<Action>> {
    let any_dir = [pair.0, pair.1].into_iter().flatten().any(|e| e.is_dir);
    if any_dir && !o.subdirs {
        return Ok(None);
    }
    let name = paths[0].file_name().unwrap_or_default().to_string_lossy();
    if !any_dir && !masks.is_empty() && !masks.matches(&name) {
        return Ok(None);
    }
    Ok(match pair {
        (Some(_), None) => Some(Action::ToRight),
        (None, Some(_)) if o.asymmetric => Some(Action::DeleteRight),
        (None, Some(_)) => Some(Action::ToLeft),
        (Some(a), Some(b)) if a.is_dir && b.is_dir => None,
        (Some(a), Some(b)) if a.is_dir || b.is_dir => Some(Action::NotEqual),
        (Some(a), Some(b)) => Some(file_action(sides, paths, a, b, o, p)?),
        (None, None) => None,
    })
}

/// Two files: equal by size and date (or content), else the newer one wins.
fn file_action(
    sides: &[Side; 2],
    paths: &[PathBuf; 2],
    a: &Entry,
    b: &Entry,
    o: &Options,
    p: &Progress,
) -> Result<Action> {
    // whole seconds: file systems and servers store times at different precision
    let secs = |e: &Entry| {
        e.modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    };
    let same_time = secs(a) == secs(b);
    let equal = a.size == b.size
        && if o.by_content {
            same_content(sides, paths, p)?
        } else {
            o.ignore_date || same_time
        };
    Ok(if equal {
        Action::Equal
    } else if o.ignore_date || same_time {
        Action::NotEqual
    } else if secs(a) > secs(b) {
        Action::ToRight
    } else {
        Action::ToLeft
    })
}

/// Streams both files chunk by chunk; stops at the first difference.
fn same_content(sides: &[Side; 2], paths: &[PathBuf; 2], p: &Progress) -> Result<bool> {
    let mut same = false;
    sides[0].read(&paths[0], &mut |a| {
        sides[1].read(&paths[1], &mut |b| {
            same = equal_streams(a, b, paths, p)?;
            Ok(())
        })
    })?;
    Ok(same)
}

fn equal_streams(
    a: &mut dyn Read,
    b: &mut dyn Read,
    paths: &[PathBuf; 2],
    p: &Progress,
) -> Result<bool> {
    let (mut x, mut y) = (vec![0; CHUNK], vec![0; CHUNK]);
    loop {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = fill(a, &mut x, &paths[0])?;
        let m = fill(b, &mut y, &paths[1])?;
        if x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Read until `buf` is full or the stream ends (network reads return short chunks).
fn fill(r: &mut dyn Read, buf: &mut [u8], path: &Path) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]).map_err(io_err(path))? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
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

/// Run the actions of `items`: copies (overwriting the older file, the user confirmed them)
/// first, then deletes on the right (local: trash, remote: permanently). Stops at the first error.
pub fn run(sides: &[Side; 2], roots: &[PathBuf; 2], items: &[Item], p: &Progress) -> Result<()> {
    let srcs = |a: Action, i: usize| -> Vec<PathBuf> {
        let it = items.iter().filter(|it| it.action == a);
        it.map(|it| roots[i].join(&it.rel)).collect()
    };
    let jobs = [(Action::ToRight, 0, 1), (Action::ToLeft, 1, 0)];
    let (mut bytes, mut files) = (0, 0);
    for &(a, from, _) in &jobs {
        vfs::scan(&sides[from], &srcs(a, from), p);
        bytes += p.total.load(Ordering::Relaxed);
        files += p.files_total.load(Ordering::Relaxed);
    }
    p.total.store(bytes, Ordering::Relaxed);
    p.files_total.store(files, Ordering::Relaxed);
    let mut overwrite = |_: &Path, _: &Path| Decision::Overwrite;
    for (a, from, to) in jobs {
        let local = matches!((&sides[from], &sides[to]), (Side::Local, Side::Local));
        for it in items.iter().filter(|it| it.action == a) {
            let (s, t) = (roots[from].join(&it.rel), roots[to].join(&it.rel));
            if local {
                ops::copy(&s, &t, &mut overwrite, p)?;
            } else {
                vfs::transfer(&sides[from], &s, &sides[to], &t, false, &mut overwrite, p)?;
            }
        }
    }
    for it in items.iter().filter(|it| it.action == Action::DeleteRight) {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let t = roots[1].join(&it.rel);
        match &sides[1] {
            Side::Local => ops::trash(&t)?,
            side => side.delete(&t)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Action, Item, Options, Progress, Side, compare, run};
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

    fn cmp(l: &Path, r: &Path, o: &Options) -> Vec<(String, Action)> {
        let roots = [l.to_owned(), r.to_owned()];
        let items = compare(&[Side::Local, Side::Local], &roots, o, &Progress::default()).unwrap();
        items
            .into_iter()
            .map(|i| (i.rel.to_string_lossy().replace('\\', "/"), i.action))
            .collect()
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
        put(&l.join("left.txt"), "l", 100);
        put(&r.join("right.log"), "r", 100);
        put(&l.join("sub/deep.txt"), "d", 100);
        put(&r.join("sub/deep.txt"), "DD", 100);
        put(&l.join("only/x.txt"), "x", 100);
        (t, l, r)
    }

    #[test]
    fn compare_directions_subdirs_mask_and_asymmetric() {
        let (_t, l, r) = pair();
        let subdirs = Options {
            subdirs: true,
            ..Options::default()
        };
        assert_eq!(
            cmp(&l, &r, &subdirs),
            [
                ("left.txt".into(), Action::ToRight),
                ("newer.txt".into(), Action::ToRight),
                ("older.txt".into(), Action::ToLeft),
                ("only".into(), Action::ToRight),
                ("right.log".into(), Action::ToLeft),
                ("same.txt".into(), Action::Equal),
                ("sub/deep.txt".into(), Action::NotEqual),
            ]
        );
        let flat = Options {
            mask: "*.log".into(),
            asymmetric: true,
            ..Options::default()
        };
        assert_eq!(
            cmp(&l, &r, &flat),
            [("right.log".into(), Action::DeleteRight)]
        );
        let ignore = Options {
            ignore_date: true,
            mask: "newer.txt".into(),
            ..Options::default()
        };
        assert_eq!(cmp(&l, &r, &ignore), [("newer.txt".into(), Action::Equal)]);
    }

    #[test]
    fn by_content_finds_equal_and_different_files() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        put(&l.join("eq"), "same", 10);
        put(&r.join("eq"), "same", 100);
        put(&l.join("ne"), "abcd", 100);
        put(&r.join("ne"), "abce", 100);
        // bigger than one chunk, differs at the end
        let big = "x".repeat(super::CHUNK + 5);
        put(&l.join("big"), &big, 100);
        put(
            &r.join("big"),
            &(big[..big.len() - 1].to_owned() + "y"),
            100,
        );
        let o = Options {
            by_content: true,
            ..Options::default()
        };
        assert_eq!(
            cmp(&l, &r, &o),
            [
                ("big".into(), Action::NotEqual),
                ("eq".into(), Action::Equal),
                ("ne".into(), Action::NotEqual)
            ]
        );
    }

    #[test]
    fn run_copies_both_ways_overwrites_and_skips() {
        let (_t, l, r) = pair();
        let roots = [l.clone(), r.clone()];
        let sides = [Side::Local, Side::Local];
        let o = Options {
            subdirs: true,
            ..Options::default()
        };
        let mut items: Vec<Item> = compare(&sides, &roots, &o, &Progress::default()).unwrap();
        for it in &mut items {
            if it.rel == Path::new("right.log") {
                it.action = Action::Skip;
            }
        }
        run(&sides, &roots, &items, &Progress::default()).unwrap();
        let read = |p: PathBuf| fs::read_to_string(p).unwrap();
        assert_eq!(read(r.join("newer.txt")), "new");
        assert_eq!(read(l.join("older.txt")), "new!");
        assert_eq!(read(r.join("only/x.txt")), "x");
        assert_eq!(read(r.join("left.txt")), "l");
        assert!(!l.join("right.log").exists());
        // not-equal files keep both versions
        assert_eq!(
            (read(l.join("sub/deep.txt")), read(r.join("sub/deep.txt"))),
            ("d".into(), "DD".into())
        );
        let left: Vec<_> = cmp(&l, &r, &o)
            .into_iter()
            .filter(|(_, a)| *a != Action::Equal)
            .collect();
        assert_eq!(
            left,
            [
                ("right.log".into(), Action::ToLeft),
                ("sub/deep.txt".into(), Action::NotEqual)
            ]
        );
    }
}
