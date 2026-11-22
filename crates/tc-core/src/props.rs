//! File properties (F9 File → Properties): Unix mode, owner and group; elsewhere the read-only flag.

use crate::{Error, Result, io_err};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

/// Permission bits (setuid, setgid, sticky included) from 1 to 4 octal digits: `644`, `0755`, `4755`.
pub fn parse_mode(s: &str) -> Option<u32> {
    let s = s.trim();
    let octal = !s.is_empty() && s.len() <= 4 && s.bytes().all(|b| (b'0'..=b'7').contains(&b));
    octal.then(|| u32::from_str_radix(s, 8).ok()).flatten()
}

/// `ls -l` style permission string: `rwxr-xr-x`, setuid/setgid `s`/`S`, sticky `t`/`T`.
pub fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for (shift, special, mark) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = mode >> shift;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(match (mode & special != 0, bits & 1 != 0) {
            (true, true) => mark,
            (true, false) => mark.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// Metadata of one entry, symlinks not followed. Mode, uid and gid are 0 off Unix.
pub struct Info {
    pub kind: &'static str,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub link: Option<PathBuf>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub readonly: bool,
}

pub fn info(path: &Path) -> Result<Info> {
    let meta = fs::symlink_metadata(path).map_err(io_err(path))?;
    let ft = meta.file_type();
    let link = ft.is_symlink().then(|| fs::read_link(path).ok()).flatten();
    #[cfg(unix)]
    let (kind, mode, uid, gid) = {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let kind = match () {
            () if ft.is_fifo() => "fifo",
            () if ft.is_socket() => "socket",
            () if ft.is_block_device() => "block device",
            () if ft.is_char_device() => "character device",
            () => kind_of(ft),
        };
        (kind, meta.mode() & 0o7777, meta.uid(), meta.gid())
    };
    #[cfg(not(unix))]
    let (kind, mode, uid, gid) = (kind_of(ft), 0, 0, 0);
    Ok(Info {
        kind,
        size: meta.len(),
        modified: meta.modified().ok(),
        link,
        mode,
        uid,
        gid,
        readonly: meta.permissions().readonly(),
    })
}

fn kind_of(ft: fs::FileType) -> &'static str {
    if ft.is_symlink() {
        "symlink"
    } else if ft.is_dir() {
        "directory"
    } else {
        "file"
    }
}

/// Name of user `uid`, else the number.
#[cfg(unix)]
pub fn user_name(uid: u32) -> String {
    let users = sysinfo::Users::new_with_refreshed_list().ok();
    let user = users
        .as_ref()
        .and_then(|u| u.iter().find(|u| **u.id() == uid));
    user.map_or_else(|| uid.to_string(), |u| u.name().to_owned())
}

/// Name of group `gid`, else the number.
#[cfg(unix)]
pub fn group_name(gid: u32) -> String {
    let groups = sysinfo::Groups::new_with_refreshed_list().ok();
    let group = groups
        .as_ref()
        .and_then(|g| g.iter().find(|g| **g.id() == gid));
    group.map_or_else(|| gid.to_string(), |g| g.name().to_owned())
}

/// Uid of a user name or number.
#[cfg(unix)]
pub fn resolve_user(s: &str) -> Result<u32> {
    let s = s.trim();
    if let Ok(n) = s.parse() {
        return Ok(n);
    }
    let users = sysinfo::Users::new_with_refreshed_list().ok();
    let user = users
        .as_ref()
        .and_then(|u| u.iter().find(|u| u.name() == s));
    user.map(|u| **u.id())
        .ok_or_else(|| Error::InvalidName(format!("unknown user {s}")))
}

/// Gid of a group name or number.
#[cfg(unix)]
pub fn resolve_group(s: &str) -> Result<u32> {
    let s = s.trim();
    if let Ok(n) = s.parse() {
        return Ok(n);
    }
    let groups = sysinfo::Groups::new_with_refreshed_list().ok();
    let group = groups
        .as_ref()
        .and_then(|g| g.iter().find(|g| g.name() == s));
    group
        .map(|g| **g.id())
        .ok_or_else(|| Error::InvalidName(format!("unknown group {s}")))
}

/// What to set; `None` leaves that attribute alone.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Change {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Non-Unix only.
    pub readonly: Option<bool>,
    /// Also everything below directories (symlinked directories are not entered).
    pub recursive: bool,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.mode.is_none() && self.uid.is_none() && self.gid.is_none() && self.readonly.is_none()
    }
}

/// Apply `change` to `paths` (and below, if recursive); `done` counts entries changed.
/// Stops at the first error or when `cancel` is set; what was changed stays changed.
pub fn apply(
    paths: &[PathBuf],
    change: &Change,
    done: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut one = |path: &Path| {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        set(path, change)?;
        done.fetch_add(1, Ordering::Relaxed);
        Ok(())
    };
    for p in paths {
        if change.recursive {
            walk(p, &mut one)?;
        } else {
            one(p)?;
        }
    }
    Ok(())
}

/// `f` on `root`, then on everything below it if it is a real directory; depth-first,
/// one open directory per level.
fn walk(root: &Path, f: &mut impl FnMut(&Path) -> Result<()>) -> Result<()> {
    f(root)?;
    let meta = fs::symlink_metadata(root).map_err(io_err(root))?;
    if !meta.is_dir() {
        return Ok(());
    }
    let mut stack = vec![(root.to_owned(), fs::read_dir(root).map_err(io_err(root))?)];
    while let Some((dir, it)) = stack.last_mut() {
        let Some(entry) = it.next() else {
            stack.pop();
            continue;
        };
        let entry = entry.map_err(io_err(dir.as_path()))?;
        let path = entry.path();
        f(&path)?;
        // `DirEntry::file_type` does not follow symlinks
        if entry.file_type().map_err(io_err(&path))?.is_dir() {
            let it = fs::read_dir(&path).map_err(io_err(&path))?;
            stack.push((path, it));
        }
    }
    Ok(())
}

/// One entry, symlinks not followed: owner and group of the link itself, no mode change
/// (Unix chmod on a link would change its target).
fn set(path: &Path, c: &Change) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(io_err(path))?;
    let link = meta.file_type().is_symlink();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // chown first: it may clear setuid / setgid bits
        if c.uid.is_some() || c.gid.is_some() {
            std::os::unix::fs::lchown(path, c.uid, c.gid).map_err(io_err(path))?;
        }
        if let Some(m) = c.mode.filter(|_| !link) {
            fs::set_permissions(path, fs::Permissions::from_mode(m)).map_err(io_err(path))?;
        }
    }
    #[cfg(not(unix))]
    if let Some(ro) = c.readonly.filter(|_| !link) {
        let mut perm = meta.permissions();
        perm.set_readonly(ro);
        fs::set_permissions(path, perm).map_err(io_err(path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{mode_string, parse_mode};

    #[test]
    fn mode_parse_and_string() {
        assert_eq!(parse_mode("644"), Some(0o644));
        assert_eq!(parse_mode(" 0755 "), Some(0o755));
        assert_eq!(parse_mode("4755"), Some(0o4755));
        assert_eq!(parse_mode("7"), Some(0o7));
        for bad in ["", "9", "12345", "+644", "-1", "rwx", "64 4"] {
            assert_eq!(parse_mode(bad), None, "{bad}");
        }
        assert_eq!(mode_string(0o644), "rw-r--r--");
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o4755), "rwsr-xr-x");
        assert_eq!(mode_string(0o2644), "rw-r-Sr--");
        assert_eq!(mode_string(0o1777), "rwxrwxrwt");
        assert_eq!(mode_string(0o1666), "rw-rw-rwT");
        assert_eq!(mode_string(0), "---------");
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::{Change, Error, apply, info, resolve_group, resolve_user};
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::sync::atomic::{AtomicBool, AtomicU64};

    fn mode(p: &std::path::Path) -> u32 {
        fs::symlink_metadata(p).unwrap().mode() & 0o7777
    }

    #[test]
    fn recursive_chmod_skips_symlinks_and_their_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let (d, outside) = (tmp.path().join("d"), tmp.path().join("outside"));
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(d.join("sub/f"), "x").unwrap();
        fs::write(outside.join("o"), "x").unwrap();
        fs::set_permissions(outside.join("o"), fs::Permissions::from_mode(0o600)).unwrap();
        symlink(outside.join("o"), d.join("lf")).unwrap();
        symlink(&outside, d.join("ld")).unwrap();
        let change = Change {
            mode: Some(0o750),
            recursive: true,
            ..Change::default()
        };
        let done = AtomicU64::new(0);
        apply(
            std::slice::from_ref(&d),
            &change,
            &done,
            &AtomicBool::new(false),
        )
        .unwrap();
        // d, sub, f, lf, ld visited; links left alone
        assert_eq!(done.into_inner(), 5);
        for p in [&d, &d.join("sub"), &d.join("sub/f")] {
            assert_eq!(mode(p), 0o750, "{}", p.display());
        }
        assert_eq!(mode(&outside.join("o")), 0o600);
        assert_ne!(mode(&outside), 0o750);
        assert_eq!(info(&d.join("lf")).unwrap().link, Some(outside.join("o")));
    }

    #[test]
    fn stops_at_first_error_and_on_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, gone, b) = (
            tmp.path().join("a"),
            tmp.path().join("gone"),
            tmp.path().join("b"),
        );
        fs::write(&a, "").unwrap();
        fs::write(&b, "").unwrap();
        let change = Change {
            mode: Some(0o600),
            ..Change::default()
        };
        let done = AtomicU64::new(0);
        let paths = [a.clone(), gone.clone(), b.clone()];
        let err = apply(&paths, &change, &done, &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(&err, Error::Io { path, .. } if *path == gone));
        assert_eq!((mode(&a), done.into_inner()), (0o600, 1));
        assert_ne!(mode(&b), 0o600);
        let cancelled = apply(
            std::slice::from_ref(&b),
            &change,
            &AtomicU64::new(0),
            &AtomicBool::new(true),
        );
        assert!(matches!(cancelled, Err(Error::Cancelled)));
        assert_ne!(mode(&b), 0o600);
    }

    #[test]
    fn chown_to_own_ids_by_number_and_name() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        fs::write(&f, "").unwrap();
        let i = info(&f).unwrap();
        assert_eq!(resolve_user(&i.uid.to_string()).unwrap(), i.uid);
        assert_eq!(resolve_user(&super::user_name(i.uid)).unwrap(), i.uid);
        assert_eq!(resolve_group(&super::group_name(i.gid)).unwrap(), i.gid);
        assert!(resolve_user("no-such-user-tc").is_err());
        let change = Change {
            uid: Some(i.uid),
            gid: Some(i.gid),
            ..Change::default()
        };
        apply(&[f], &change, &AtomicU64::new(0), &AtomicBool::new(false)).unwrap();
    }
}
