//! Disk usage tree for the File Stats screen (ncdu-style).

use std::ffi::OsString;
use std::fs;
use std::num::NonZero;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::ops::Progress;
use crate::vfs::Remote;

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

/// Scan `dir` recursively (symlinks not followed, unreadable parts count 0), children sorted
/// by size. Subdirectories go to worker threads while one is free. `p.files`, `p.dirs` and
/// `p.dirs_done` count live; stops early on `p.cancel`.
// shortcut: thread count is automatic, spinning disks may seek more; upgrade with a thread option
pub fn scan(dir: &Path, p: &Progress) -> Node {
    let threads = thread::available_parallelism().map_or(1, NonZero::get);
    let free = AtomicUsize::new(threads.min(MAX_THREADS) - 1);
    p.dirs.fetch_add(1, Ordering::Relaxed);
    let mut node = scan_dir(dir, p, &free);
    node.name = dir.file_name().unwrap_or(dir.as_os_str()).to_owned();
    node
}

/// Take a worker slot if one is free.
fn take_slot(free: &AtomicUsize) -> bool {
    free.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
        .is_ok()
}

fn scan_dir(dir: &Path, p: &Progress, free: &AtomicUsize) -> Node {
    let mut node = Node {
        is_dir: true,
        ..Node::default()
    };
    thread::scope(|s| {
        let mut workers = Vec::new();
        for item in fs::read_dir(dir).into_iter().flatten().flatten() {
            if p.cancel.load(Ordering::Relaxed) {
                break;
            }
            if item.file_type().is_ok_and(|t| t.is_dir()) {
                p.dirs.fetch_add(1, Ordering::Relaxed);
                let path = item.path();
                if take_slot(free) {
                    let worker = s.spawn(move || {
                        let c = scan_dir(&path, p, free);
                        free.fetch_add(1, Ordering::AcqRel);
                        c
                    });
                    workers.push((item.file_name(), worker));
                    continue;
                }
                let mut c = scan_dir(&path, p, free);
                c.name = item.file_name();
                node.children.push(c);
            } else {
                p.files.fetch_add(1, Ordering::Relaxed);
                node.children.push(Node {
                    name: item.file_name(),
                    size: item.metadata().map_or(0, |m| m.len()),
                    files: 1,
                    ..Node::default()
                });
            }
        }
        for (name, worker) in workers {
            // a panicked worker counts as an empty directory
            let mut c = worker.join().unwrap_or_else(|_| Node {
                is_dir: true,
                ..Node::default()
            });
            c.name = name;
            node.children.push(c);
        }
    });
    node.size = node.children.iter().map(|c| c.size).sum();
    node.files = node.children.iter().map(|c| c.files).sum();
    sort(&mut node.children, false);
    p.dirs_done.fetch_add(1, Ordering::Relaxed);
    node
}

/// Like [`scan`] on a remote `dir`, single threaded since a connection serializes requests.
/// Symlinks are counted as files, never descended into.
pub fn scan_remote(r: &dyn Remote, dir: &str, p: &Progress) -> Node {
    p.dirs.fetch_add(1, Ordering::Relaxed);
    let mut node = scan_remote_dir(r, dir, p);
    node.name = dir
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(dir)
        .into();
    node
}

fn scan_remote_dir(r: &dyn Remote, dir: &str, p: &Progress) -> Node {
    let mut node = Node {
        is_dir: true,
        ..Node::default()
    };
    for e in r.list(dir).into_iter().flatten() {
        if p.cancel.load(Ordering::Relaxed) {
            break;
        }
        if e.is_dir && !e.is_symlink {
            p.dirs.fetch_add(1, Ordering::Relaxed);
            let path = format!("{}/{}", dir.trim_end_matches('/'), e.name.to_string_lossy());
            let mut c = scan_remote_dir(r, &path, p);
            c.name = e.name;
            node.children.push(c);
        } else {
            p.files.fetch_add(1, Ordering::Relaxed);
            node.children.push(Node {
                name: e.name,
                size: e.size,
                files: 1,
                ..Node::default()
            });
        }
    }
    node.size = node.children.iter().map(|c| c.size).sum();
    node.files = node.children.iter().map(|c| c.files).sum();
    sort(&mut node.children, false);
    p.dirs_done.fetch_add(1, Ordering::Relaxed);
    node
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

/// Largest first by size or file count, ties by name.
pub fn sort(children: &mut [Node], by_files: bool) {
    let key = |n: &Node| if by_files { n.files } else { n.size };
    children.sort_by(|a, b| key(b).cmp(&key(a)).then_with(|| a.name.cmp(&b.name)));
}

#[cfg(test)]
mod tests {
    use super::*;

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
        sort(&mut root.children, true);
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
}
