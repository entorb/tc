//! Local file operations. None of them overwrite an existing target.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result, io_err};

/// Copy `src` (file, symlink or directory tree) to the new path `target`.
/// On failure the partial target is removed.
pub fn copy(src: &Path, target: &Path) -> Result<()> {
    prepare(src, target)?;
    copy_tree(src, target).inspect_err(|_| remove_partial(target))
}

/// Move or rename `src` to the new path `target`. Across devices: copy, then delete source.
pub fn move_(src: &Path, target: &Path) -> Result<()> {
    prepare(src, target)?;
    match fs::rename(src, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            copy_tree(src, target).inspect_err(|_| remove_partial(target))?;
            delete(src)
        }
        Err(e) => Err(io_err(src)(e)),
    }
}

/// Delete a file, symlink (not its target) or directory tree.
pub fn delete(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(io_err(path))?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(io_err(path))
}

/// Create directory `name` inside `parent`. `name` must be a single path component.
pub fn mkdir(parent: &Path, name: &OsStr) -> Result<PathBuf> {
    let mut parts = Path::new(name).components();
    if !matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    ) {
        return Err(Error::InvalidName(name.to_string_lossy().into_owned()));
    }
    let path = parent.join(name);
    fs::create_dir(&path).map_err(io_err(&path))?;
    Ok(path)
}

/// Target path for `src` in `dst_dir`, refusing existing targets and copies into itself.
fn prepare(src: &Path, target: &Path) -> Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(Error::Exists(target.to_owned()));
    }
    let src_meta = fs::symlink_metadata(src).map_err(io_err(src))?;
    if src_meta.is_dir() {
        let dst_dir = target.parent().unwrap_or(target);
        // canonicalize so symlinked paths cannot hide the nesting
        let src_real = fs::canonicalize(src).map_err(io_err(src))?;
        let dst_real = fs::canonicalize(dst_dir).map_err(io_err(dst_dir))?;
        if dst_real.starts_with(&src_real) {
            return Err(Error::IntoItself(src.to_owned()));
        }
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src).map_err(io_err(src))?;
    if meta.is_symlink() {
        let link = fs::read_link(src).map_err(io_err(src))?;
        platform::symlink(&link, dst, src).map_err(io_err(dst))
    } else if meta.is_dir() {
        fs::create_dir(dst).map_err(io_err(dst))?;
        for item in fs::read_dir(src).map_err(io_err(src))? {
            let item = item.map_err(io_err(src))?;
            copy_tree(&item.path(), &dst.join(item.file_name()))?;
        }
        // after the contents, so a read-only source dir does not block them
        fs::set_permissions(dst, meta.permissions()).map_err(io_err(dst))
    } else {
        fs::copy(src, dst).map(drop).map_err(io_err(src))
    }
}

/// Only called on targets this operation created (`prepare` ensured they did not exist).
fn remove_partial(target: &Path) {
    let _ = delete(target);
}

#[cfg(unix)]
mod platform {
    use std::{io, path::Path};

    pub fn symlink(link: &Path, dst: &Path, _src: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(link, dst)
    }
}

#[cfg(windows)]
mod platform {
    use std::{fs, io, path::Path};

    pub fn symlink(link: &Path, dst: &Path, src: &Path) -> io::Result<()> {
        if fs::metadata(src).is_ok_and(|m| m.is_dir()) {
            std::os::windows::fs::symlink_dir(link, dst)
        } else {
            std::os::windows::fs::symlink_file(link, dst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) -> PathBuf {
        let src = root.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("sub/b.txt"), "b").unwrap();
        src
    }

    #[test]
    fn copy_dir_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        copy(&src, &dst.join("src")).unwrap();
        assert_eq!(fs::read_to_string(dst.join("src/sub/b.txt")).unwrap(), "b");
        assert!(src.join("a.txt").exists());
    }

    #[test]
    fn never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("a.txt"), "keep").unwrap();
        assert!(matches!(
            copy(&src.join("a.txt"), &dst.join("a.txt")),
            Err(Error::Exists(_))
        ));
        assert!(matches!(
            move_(&src.join("a.txt"), &dst.join("a.txt")),
            Err(Error::Exists(_))
        ));
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "keep");
        assert!(src.join("a.txt").exists());
    }

    #[test]
    fn refuses_into_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        assert!(matches!(
            copy(&src, &src.join("sub/src")),
            Err(Error::IntoItself(_))
        ));
        assert!(matches!(
            move_(&src, &src.join("sub/src")),
            Err(Error::IntoItself(_))
        ));
    }

    #[test]
    fn copy_and_move_with_new_name() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        copy(&src.join("a.txt"), &src.join("c.txt")).unwrap();
        move_(&src.join("c.txt"), &src.join("d.txt")).unwrap();
        assert!(!src.join("c.txt").exists());
        assert_eq!(fs::read_to_string(src.join("d.txt")).unwrap(), "a");
        assert!(src.join("a.txt").exists());
    }

    #[test]
    fn move_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        move_(&src.join("a.txt"), &src.join("sub/a.txt")).unwrap();
        assert!(!src.join("a.txt").exists());
        assert!(src.join("sub/a.txt").exists());
    }

    #[test]
    fn mkdir_validates_name() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in ["", ".", "..", "a/b"] {
            assert!(
                matches!(
                    mkdir(tmp.path(), OsStr::new(bad)),
                    Err(Error::InvalidName(_))
                ),
                "{bad}"
            );
        }
        assert!(mkdir(tmp.path(), OsStr::new("new")).unwrap().is_dir());
        assert!(mkdir(tmp.path(), OsStr::new("new")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_copied_and_deleted_as_links() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("link")).unwrap();
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        copy(&src, &dst.join("src")).unwrap();
        assert!(
            fs::symlink_metadata(dst.join("src/link"))
                .unwrap()
                .is_symlink()
        );
        delete(&src).unwrap();
        assert!(!src.exists());
        assert!(outside.join("keep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_copy_cleans_partial_target() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let locked = src.join("sub/b.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // running as root, permission cannot be tested
        }
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        assert!(copy(&src, &dst.join("src")).is_err());
        assert!(!dst.join("src").exists());
        assert!(src.join("a.txt").exists());
    }
}
