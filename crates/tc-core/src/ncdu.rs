//! Disk usage tree for the Dir Size screen (ncdu-style).

pub mod treemap;

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::num::NonZero;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::ops::Progress;
use crate::vfs::{self, Remote};

/// One entry of the tree; a directory sums its children.
#[derive(Debug, Default)]
pub struct Node {
    pub name: OsString,
    pub is_dir: bool,
    /// Bytes (apparent size).
    pub size: u64,
    /// Files below (1 for a file), directories not counted.
    pub files: u64,
    pub children: Vec<Node>,
}

/// Most worker threads of a scan; more rarely helps, `read_dir` latency is the bottleneck.
const MAX_THREADS: usize = 8;

/// A directory entry as a scan needs it.
struct Item {
    name: OsString,
    /// Directory, not a symlink to one.
    dir: bool,
    size: u64,
}

type List<'a> = dyn Fn(&Path) -> Vec<Item> + Sync + 'a;

/// Scan `dir` recursively (symlinks not followed, unreadable parts count 0), children sorted
/// by size. Subdirectories go to worker threads while one is free. `p.files`, `p.dirs` and
/// `p.dirs_done` count live; stops early on `p.cancel`.
// shortcut: thread count is automatic, spinning disks may seek more; upgrade with a thread option
pub fn scan(dir: &Path, p: &Progress) -> Node {
    let threads = thread::available_parallelism().map_or(1, NonZero::get);
    walk(dir, p, threads.min(MAX_THREADS), &|d| {
        let items = fs::read_dir(d).into_iter().flatten().flatten();
        items
            .map(|i| Item {
                dir: i.file_type().is_ok_and(|t| t.is_dir()),
                size: i.metadata().map_or(0, |m| m.len()),
                name: i.file_name(),
            })
            .collect()
    })
}

/// Like [`scan`] on a remote `dir`: one `find` on the server if it can run commands (GNU find),
/// else a walk with parallel listings, which hides the network latency of each request.
pub fn scan_remote(r: &dyn Remote, dir: &Path, p: &Progress) -> Node {
    let quoted = vfs::sh_quote(&vfs::rpath(dir));
    let cmd = format!(r"find {quoted} -mindepth 1 -printf '%y %s %P\0'");
    let mut found = None;
    // no output at all (no GNU find, empty dir): the walk decides
    let _ = r.exec(&cmd, &mut |out| {
        found = parse_find(out, p)?;
        Ok(())
    });
    let mut node = match found {
        Some(node) => node,
        None if p.cancel.load(Ordering::Relaxed) => Node::default(),
        None => {
            return walk(dir, p, MAX_THREADS, &|d| {
                let entries = r.list(&vfs::rpath(d)).unwrap_or_default();
                entries
                    .into_iter()
                    .map(|e| Item {
                        dir: e.is_dir && !e.is_symlink,
                        size: e.size,
                        name: e.name,
                    })
                    .collect()
            });
        }
    };
    node.name = dir.file_name().unwrap_or(dir.as_os_str()).to_owned();
    node
}

fn walk(dir: &Path, p: &Progress, threads: usize, list: &List) -> Node {
    let free = AtomicUsize::new(threads - 1);
    p.dirs.fetch_add(1, Ordering::Relaxed);
    let mut node = scan_dir(dir, p, &free, list);
    node.name = dir.file_name().unwrap_or(dir.as_os_str()).to_owned();
    node
}

/// Take a worker slot if one is free.
fn take_slot(free: &AtomicUsize) -> bool {
    free.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
        .is_ok()
}

fn scan_dir(dir: &Path, p: &Progress, free: &AtomicUsize, list: &List) -> Node {
    let mut node = Node {
        is_dir: true,
        ..Node::default()
    };
    thread::scope(|s| {
        let mut workers = Vec::new();
        for item in list(dir) {
            if p.cancel.load(Ordering::Relaxed) {
                break;
            }
            if item.dir {
                p.dirs.fetch_add(1, Ordering::Relaxed);
                let path = dir.join(&item.name);
                if take_slot(free) {
                    let worker = s.spawn(move || {
                        let c = scan_dir(&path, p, free, list);
                        free.fetch_add(1, Ordering::AcqRel);
                        c
                    });
                    workers.push((item.name, worker));
                    continue;
                }
                let mut c = scan_dir(&path, p, free, list);
                c.name = item.name;
                node.children.push(c);
            } else {
                p.files.fetch_add(1, Ordering::Relaxed);
                node.children.push(Node {
                    name: item.name,
                    size: item.size,
                    files: 1,
                    ..Node::default()
                });
            }
        }
        for (name, worker) in workers {
            let mut c = worker.join().unwrap_or_else(|_| Node {
                is_dir: true,
                ..Node::default()
            });
            c.name = name;
            node.children.push(c);
        }
    });
    finish(&mut node);
    p.dirs_done.fetch_add(1, Ordering::Relaxed);
    node
}

/// Sum the children and sort them by size.
fn finish(node: &mut Node) {
    node.size = node.children.iter().map(|c| c.size).sum();
    node.files = node.children.iter().map(|c| c.files).sum();
    sort(&mut node.children, Order::Size);
}

/// Most bytes of one `find` record; a longer one ends the parse as garbage.
const MAX_RECORD: usize = 64 << 10;

/// Tree from `find -printf '%y %s %P\0'` output (pre-order: a directory before its contents).
/// `None` without a single record; `Err(Cancelled)` on `p.cancel`.
fn parse_find(out: &mut dyn Read, p: &Progress) -> crate::Result<Option<Node>> {
    let mut out = BufReader::new(out);
    // open directories from the root down, with their relative paths
    let mut stack: Vec<(String, Node)> = vec![(
        String::new(),
        Node {
            is_dir: true,
            ..Node::default()
        },
    )];
    let mut rec = Vec::new();
    let mut any = false;
    loop {
        rec.clear();
        let n = (&mut out)
            .take(MAX_RECORD as u64)
            .read_until(0, &mut rec)
            .map_err(|e| crate::Error::Remote(e.to_string()))?;
        if p.cancel.load(Ordering::Relaxed) {
            return Err(crate::Error::Cancelled);
        }
        if rec.pop() != Some(0) {
            // a record cut off or too long: a broken stream, not a tree to trust
            if n > 0 {
                return Ok(None);
            }
            break;
        }
        let text = String::from_utf8_lossy(&rec);
        let mut parts = text.splitn(3, ' ');
        let (Some(kind), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        any = true;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        // close finished directories; an unknown parent lands in the root
        while stack.len() > 1 && stack.last().is_some_and(|(d, _)| d != parent) {
            pop(&mut stack, p);
        }
        let mut node = Node {
            name: name.into(),
            ..Node::default()
        };
        if kind == "d" {
            p.dirs.fetch_add(1, Ordering::Relaxed);
            node.is_dir = true;
            stack.push((path.to_owned(), node));
        } else {
            p.files.fetch_add(1, Ordering::Relaxed);
            node.size = size.parse().unwrap_or(0);
            node.files = 1;
            if let Some((_, dir)) = stack.last_mut() {
                dir.children.push(node);
            }
        }
    }
    while stack.len() > 1 {
        pop(&mut stack, p);
    }
    let Some((_, mut root)) = stack.pop().filter(|_| any) else {
        return Ok(None);
    };
    finish(&mut root);
    p.dirs.fetch_add(1, Ordering::Relaxed);
    p.dirs_done.fetch_add(1, Ordering::Relaxed);
    Ok(Some(root))
}

/// Finish the innermost open directory and add it to its parent.
fn pop(stack: &mut Vec<(String, Node)>, p: &Progress) {
    if let Some((_, mut dir)) = stack.pop() {
        finish(&mut dir);
        p.dirs_done.fetch_add(1, Ordering::Relaxed);
        if let Some((_, parent)) = stack.last_mut() {
            parent.children.push(dir);
        }
    }
}

/// Drop child `idx` of the directory at `path` below `root`; every ancestor loses its size
/// and file count.
pub fn remove(root: &mut Node, path: &[usize], idx: usize) {
    let dir = path.iter().fold(&*root, |n, &i| &n.children[i]);
    let Some(gone) = dir.children.get(idx) else {
        return;
    };
    let (size, files) = (gone.size, gone.files);
    let mut n = root;
    for &i in path {
        n.size -= size;
        n.files -= files;
        n = &mut n.children[i];
    }
    n.size -= size;
    n.files -= files;
    n.children.remove(idx);
}

/// Swap in a rescan `new` of the directory at `path` below `root` (keeping its name); every
/// ancestor's size and file count change by the difference.
pub fn replace(root: &mut Node, path: &[usize], new: Node) {
    let old = path.iter().fold(&*root, |n, &i| &n.children[i]);
    let (size, files) = (old.size, old.files);
    let mut n = root;
    for &i in path {
        // the old subtree is part of this ancestor, so subtracting first cannot underflow
        n.size = n.size - size + new.size;
        n.files = n.files - files + new.files;
        n = &mut n.children[i];
    }
    (n.size, n.files, n.children) = (new.size, new.files, new.children);
}

/// Directories below `n`, `n` itself not counted.
pub fn dirs(n: &Node) -> u64 {
    n.children
        .iter()
        .map(|c| u64::from(c.is_dir) + dirs(c))
        .sum()
}

/// Sort key of a directory listing: size or file count, as the browser shows them.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Order {
    #[default]
    Size,
    Count,
}

/// Largest first by size or file count (ties by name).
pub fn sort(children: &mut [Node], order: Order) {
    children.sort_by(|a, b| match order {
        Order::Size => b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)),
        Order::Count => b.files.cmp(&a.files).then_with(|| a.name.cmp(&b.name)),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(out: &[u8]) -> Option<Node> {
        parse_find(&mut &out[..], &Progress::default()).unwrap()
    }

    #[test]
    fn parse_find_builds_tree_in_preorder() {
        let out = b"d 4096 a\0f 3 a/x y\0d 4096 a/b\0f 5 a/b/z\0f 1 a/w\0l 7 link\0d 0 empty\0";
        let root = find(out).unwrap();
        assert_eq!((root.size, root.files), (16, 4));
        let names: Vec<_> = root
            .children
            .iter()
            .map(|c| c.name.to_str().unwrap())
            .collect();
        assert_eq!(names, ["a", "link", "empty"]);
        let a = &root.children[0];
        assert_eq!(
            (a.size, a.files, a.children[0].name.to_str()),
            (9, 3, Some("b"))
        );
        assert!(root.children[2].is_dir && root.children[2].children.is_empty());
    }

    #[test]
    fn parse_find_rejects_empty_cut_and_out_of_order() {
        assert!(find(b"").is_none());
        assert!(find(b"f 1 a\0f 2 b").is_none());
        // a parent never listed: the entry still counts, in the root
        let root = find(b"f 2 gone/x\0").unwrap();
        assert_eq!((root.size, root.children[0].name.to_str()), (2, Some("x")));
        let p = Progress::default();
        p.cancel.store(true, Ordering::Relaxed);
        assert!(parse_find(&mut &b"f 1 a\0"[..], &p).is_err());
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "NTFS is not sparse by default, set_len reserves 1 TiB"
    )]
    fn sums_tree_and_sorts_by_size_or_files() {
        let tmp = tempfile::tempdir().unwrap();
        let many = tmp.path().join("many");
        fs::create_dir(&many).unwrap();
        for n in ["a", "b", "c"] {
            fs::write(many.join(n), "xy").unwrap();
        }
        // 1 TiB sparse: only its metadata is read
        fs::File::create(tmp.path().join("big"))
            .unwrap()
            .set_len(1 << 40)
            .unwrap();
        let mut root = scan(tmp.path(), &Progress::default());
        assert_eq!((root.size, root.files), ((1 << 40) + 6, 4));
        assert_eq!(root.children[0].name, "big");
        assert_eq!((root.children[1].size, root.children[1].files), (6, 3));
        sort(&mut root.children, Order::Count);
        assert_eq!(root.children[0].name, "many");
    }

    #[test]
    fn deep_tree_counts_dirs_and_remove_updates_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        // 20 dirs with a nested dir each: enough to hand some to worker threads
        for i in 0..20 {
            let sub = tmp.path().join(format!("d{i}")).join("in");
            fs::create_dir_all(&sub).unwrap();
            fs::write(sub.join("f"), "abc").unwrap();
        }
        let p = Progress::default();
        let mut root = scan(tmp.path(), &p);
        assert_eq!((root.size, root.files), (60, 20));
        let dirs = p.dirs.load(Ordering::Relaxed);
        assert_eq!((dirs, p.dirs_done.load(Ordering::Relaxed)), (41, 41));
        assert!(
            root.children
                .iter()
                .all(|c| c.name.to_string_lossy().starts_with('d'))
        );
        // remove d?/in: root and d? lose 3 bytes and 1 file
        remove(&mut root, &[0], 0);
        assert_eq!((root.size, root.files), (57, 19));
        assert_eq!(
            (root.children[0].size, root.children[0].children.len()),
            (0, 0)
        );
        remove(&mut root, &[], 0);
        assert_eq!((root.size, root.children.len()), (57, 19));
    }

    #[test]
    fn sort_by_size_or_count() {
        let n = |name: &str, size, files| Node {
            name: name.into(),
            size,
            files,
            ..Node::default()
        };
        let mut v = vec![n("b", 5, 1), n("a", 5, 9), n("c", 7, 2)];
        let names = |v: &[Node]| {
            v.iter()
                .map(|n| n.name.to_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        sort(&mut v, Order::Size);
        assert_eq!(names(&v), ["c", "a", "b"]);
        sort(&mut v, Order::Count);
        assert_eq!(names(&v), ["a", "c", "b"]);
    }

    #[test]
    fn dirs_counts_subtree() {
        let leaf = |is_dir| Node {
            is_dir,
            ..Node::default()
        };
        let sub = Node {
            is_dir: true,
            children: vec![leaf(true), leaf(false)],
            ..Node::default()
        };
        let root = Node {
            is_dir: true,
            children: vec![sub, leaf(false)],
            ..Node::default()
        };
        assert_eq!(
            (
                dirs(&root),
                dirs(&root.children[0]),
                dirs(&root.children[1])
            ),
            (2, 1, 0)
        );
    }

    #[test]
    fn replace_updates_ancestors() {
        let file = |name: &str, size| Node {
            name: name.into(),
            size,
            files: 1,
            ..Node::default()
        };
        let dir = |name: &str, children: Vec<Node>| Node {
            name: name.into(),
            is_dir: true,
            size: children.iter().map(|c| c.size).sum(),
            files: children.iter().map(|c| c.files).sum(),
            children,
        };
        let mut root = dir("r", vec![dir("sub", vec![file("x", 3)]), file("y", 5)]);
        replace(
            &mut root,
            &[0],
            dir("other", vec![file("x", 3), file("z", 10)]),
        );
        assert_eq!((root.size, root.files), (18, 3));
        assert_eq!(
            (root.children[0].size, root.children[0].name.to_str()),
            (13, Some("sub"))
        );
        replace(&mut root, &[0], dir("sub", vec![]));
        assert_eq!((root.size, root.files), (5, 1));
        replace(&mut root, &[], dir("new", vec![file("a", 1)]));
        assert_eq!(
            (root.size, root.files, root.name.to_str()),
            (1, 1, Some("r"))
        );
    }
}
