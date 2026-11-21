//! Disk usage tree for the File Stats screen (ncdu-style).

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;

use crate::ops::Progress;

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

/// Scan `dir` recursively (symlinks not followed, unreadable parts count 0), children sorted
/// by size. `p.files` counts live; stops early on `p.cancel`.
pub fn scan(dir: &Path, p: &Progress) -> Node {
    let mut node = Node {
        name: dir.file_name().unwrap_or(dir.as_os_str()).to_owned(),
        is_dir: true,
        ..Node::default()
    };
    for item in fs::read_dir(dir).into_iter().flatten().flatten() {
        if p.cancel.load(Ordering::Relaxed) {
            break;
        }
        let child = if item.file_type().is_ok_and(|t| t.is_dir()) {
            let mut c = scan(&item.path(), p);
            c.name = item.file_name();
            c
        } else {
            p.files.fetch_add(1, Ordering::Relaxed);
            Node {
                name: item.file_name(),
                size: item.metadata().map_or(0, |m| m.len()),
                files: 1,
                ..Node::default()
            }
        };
        node.size += child.size;
        node.files += child.files;
        node.children.push(child);
    }
    sort(&mut node.children, false);
    node
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
}
