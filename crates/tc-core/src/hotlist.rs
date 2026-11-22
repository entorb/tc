//! Bookmarked directories, stored as a plain-text file with one path per line (bookmarks:
//! `alias<TAB>path` or `path`).

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Result, io_err};

/// `$XDG_CONFIG_HOME` or `~/.config` (`%APPDATA%` on Windows) + `tc/hotlist`.
pub fn default_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| Path::new(&h).join(".config")))
    };
    base.map(|b| b.join("tc").join("hotlist"))
}

/// Mount points (drive roots on Windows), sorted, no duplicates.
pub fn mounts() -> Vec<PathBuf> {
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else {
        return Vec::new();
    };
    let mut list: Vec<PathBuf> = disks.iter().map(|d| d.mount_point().to_owned()).collect();
    list.sort();
    list.dedup();
    list
}

/// Missing file means empty list. Paths containing a newline cannot be stored.
pub fn load(file: &Path) -> Result<Vec<PathBuf>> {
    let bytes = match fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(file)(e)),
    };
    Ok(bytes
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| to_path(l.strip_suffix(b"\r").unwrap_or(l)))
        .collect())
}

/// Atomic replace (a crash keeps the old file); new files are private (0600 on Unix): the
/// command history can hold secrets. A symlinked file is replaced at its target.
pub fn save(file: &Path, list: &[PathBuf]) -> Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(io_err(dir))?;
    let file = fs::canonicalize(file).unwrap_or_else(|_| file.to_owned());
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for p in list.iter().filter(|p| !p.as_os_str().is_empty()) {
        let b = to_bytes(p);
        if !b.contains(&b'\n') {
            out.extend_from_slice(&b);
            out.push(b'\n');
        }
    }
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_err(dir))?;
    tmp.write_all(&out).map_err(io_err(tmp.path()))?;
    tmp.as_file().sync_all().map_err(io_err(tmp.path()))?;
    tmp.persist(&file).map_err(|e| io_err(&file)(e.error))?;
    Ok(())
}

/// A bookmarked directory with an optional alias (empty: none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub alias: String,
    pub path: PathBuf,
}

impl Bookmark {
    pub fn new(path: PathBuf) -> Self {
        Self {
            alias: String::new(),
            path,
        }
    }
}

/// Lines are `alias<TAB>path` or `path`, split at the first tab (shortcut: an old file with a
/// tab inside a path and no alias misparses).
pub fn load_bookmarks(file: &Path) -> Result<Vec<Bookmark>> {
    Ok(load(file)?
        .iter()
        .filter_map(|l| {
            let b = to_bytes(l);
            let (alias, path) = match b.iter().position(|&c| c == b'\t') {
                Some(i) => (String::from_utf8_lossy(&b[..i]).into_owned(), &b[i + 1..]),
                None => (String::new(), &b[..]),
            };
            (!path.is_empty()).then(|| Bookmark {
                alias,
                path: to_path(path),
            })
        })
        .collect())
}

/// Like [`save`]; a path with a tab and no alias gets an empty alias so the tab stays in the path.
pub fn save_bookmarks(file: &Path, list: &[Bookmark]) -> Result<()> {
    let lines: Vec<PathBuf> = list
        .iter()
        .map(|b| {
            let path = to_bytes(&b.path);
            let mut line = b.alias.replace(['\t', '\n'], "").into_bytes();
            if !line.is_empty() || path.contains(&b'\t') {
                line.push(b'\t');
            }
            line.extend_from_slice(&path);
            to_path(&line)
        })
        .collect();
    save(file, &lines)
}

#[cfg(unix)]
pub(crate) fn to_bytes(p: &Path) -> Vec<u8> {
    std::os::unix::ffi::OsStrExt::as_bytes(p.as_os_str()).to_vec()
}

#[cfg(unix)]
pub(crate) fn to_path(b: &[u8]) -> PathBuf {
    PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b))
}

// shortcut: non-UTF-8 paths are lossy off Unix, upgrade with WTF-8 if it matters
#[cfg(not(unix))]
pub(crate) fn to_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

#[cfg(not(unix))]
pub(crate) fn to_path(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("sub").join("hotlist");
        assert!(load(&file).unwrap().is_empty());
        let list = vec![PathBuf::from("/a b"), PathBuf::from("/c")];
        save(&file, &list).unwrap();
        assert_eq!(load(&file).unwrap(), list);
    }

    #[cfg(unix)]
    #[test]
    fn save_is_private_and_follows_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (real, link) = (tmp.path().join("real"), tmp.path().join("link"));
        save(&real, &[PathBuf::from("/a")]).unwrap();
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::os::unix::fs::symlink(&real, &link).unwrap();
        save(&link, &[PathBuf::from("/b")]).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(load(&real).unwrap(), [PathBuf::from("/b")]);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 2); // no temp left
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("h");
        let p = to_path(b"/x/\xff\xfe");
        save(&file, std::slice::from_ref(&p)).unwrap();
        assert_eq!(load(&file).unwrap(), [p]);
    }

    #[test]
    fn bookmarks_old_format_and_alias_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("h");
        fs::write(&file, "/a\n/b\n").unwrap();
        let mut list = load_bookmarks(&file).unwrap();
        assert_eq!(
            list,
            [Bookmark::new("/a".into()), Bookmark::new("/b".into())]
        );
        list[0].alias = "Proj".into();
        save_bookmarks(&file, &list).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"Proj\t/a\n/b\n");
        assert_eq!(load_bookmarks(&file).unwrap(), list);
    }

    #[test]
    fn bookmark_path_with_tab_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("h");
        let list = [Bookmark::new("/x\ty".into())];
        save_bookmarks(&file, &list).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"\t/x\ty\n");
        assert_eq!(load_bookmarks(&file).unwrap(), list);
    }

    #[cfg(unix)]
    #[test]
    fn bookmark_non_utf8_path_with_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("h");
        let list = [Bookmark {
            alias: "Ä".into(),
            path: to_path(b"/x/\xff"),
        }];
        save_bookmarks(&file, &list).unwrap();
        assert_eq!(load_bookmarks(&file).unwrap(), list);
    }
}
