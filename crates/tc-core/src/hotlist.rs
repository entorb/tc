//! Bookmarked directories, stored as a plain-text file with one path per line.

use std::env;
use std::fs;
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

pub fn save(file: &Path, list: &[PathBuf]) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let mut out = Vec::new();
    for p in list.iter().filter(|p| !p.as_os_str().is_empty()) {
        let b = to_bytes(p);
        if !b.contains(&b'\n') {
            out.extend_from_slice(&b);
            out.push(b'\n');
        }
    }
    fs::write(file, out).map_err(io_err(file))
}

#[cfg(unix)]
fn to_bytes(p: &Path) -> Vec<u8> {
    std::os::unix::ffi::OsStrExt::as_bytes(p.as_os_str()).to_vec()
}

#[cfg(unix)]
fn to_path(b: &[u8]) -> PathBuf {
    PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b))
}

// shortcut: non-UTF-8 paths are lossy off Unix, upgrade with WTF-8 if it matters
#[cfg(not(unix))]
fn to_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

#[cfg(not(unix))]
fn to_path(b: &[u8]) -> PathBuf {
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
    fn non_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("h");
        let p = to_path(b"/x/\xff\xfe");
        save(&file, std::slice::from_ref(&p)).unwrap();
        assert_eq!(load(&file).unwrap(), [p]);
    }
}
