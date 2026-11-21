//! Local file operations. Existing files are replaced only on an explicit [`Decision`].

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Error, Result, io_err};

/// Answer to a name conflict on a file or symlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Overwrite,
    Skip,
    /// Store under this single-component name next to the existing target.
    Rename(OsString),
    Cancel,
}

/// Asked once per conflicting file with `(src, target)`.
pub type Ask<'a> = &'a mut dyn FnMut(&Path, &Path) -> Decision;

/// True when `src` was modified after `target`.
pub fn is_newer(src: &Path, target: &Path) -> bool {
    let time = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    matches!((time(src), time(target)), (Some(s), Some(t)) if s > t)
}

/// Copy `src` (file, symlink or directory tree) to `target`. An existing directory is merged
/// into; an existing file is only replaced when `ask` says so, via temp copy plus rename, so a
/// failure keeps the old file. Newly created partial targets are removed on failure.
/// `cancel` is checked between files.
pub fn copy(src: &Path, target: &Path, ask: Ask, cancel: &AtomicBool) -> Result<()> {
    prepare(src, target)?;
    place(src, target, false, &mut Ctx { ask, cancel })
}

/// Move or rename `src` to `target`, with the conflict rules of [`copy`]. Source files are
/// deleted only after they are in place; across devices: copy, then delete source.
pub fn move_(src: &Path, target: &Path, ask: Ask, cancel: &AtomicBool) -> Result<()> {
    prepare(src, target)?;
    place(src, target, true, &mut Ctx { ask, cancel })
}

struct Ctx<'a> {
    ask: Ask<'a>,
    cancel: &'a AtomicBool,
}

fn place(src: &Path, target: &Path, mv: bool, ctx: &mut Ctx) -> Result<()> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let Ok(tmeta) = fs::symlink_metadata(target) else {
        return create(src, target, mv);
    };
    let smeta = fs::symlink_metadata(src).map_err(io_err(src))?;
    match (smeta.is_dir(), tmeta.is_dir()) {
        (true, true) => {
            for item in fs::read_dir(src).map_err(io_err(src))? {
                let item = item.map_err(io_err(src))?;
                place(&item.path(), &target.join(item.file_name()), mv, ctx)?;
            }
            // non-empty (skipped files) stays, never force-removed
            if mv {
                let _ = fs::remove_dir(src);
            }
            Ok(())
        }
        (false, false) => {
            if same_file(src, target) {
                return Err(Error::Exists(target.to_owned()));
            }
            match (ctx.ask)(src, target) {
                Decision::Skip => Ok(()),
                Decision::Cancel => Err(Error::Cancelled),
                Decision::Rename(name) => {
                    let parent = target.parent().unwrap_or(Path::new(""));
                    place(src, &parent.join(single_name(&name)?), mv, ctx)
                }
                Decision::Overwrite => replace(src, target, mv),
            }
        }
        _ => Err(Error::TypeMismatch(target.to_owned())),
    }
}

/// Target does not exist: plain move or copy.
fn create(src: &Path, target: &Path, mv: bool) -> Result<()> {
    if mv {
        return match fs::rename(src, target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                copy_tree(src, target).inspect_err(|_| remove_partial(target))?;
                delete(src)
            }
            Err(e) => Err(io_err(src)(e)),
        };
    }
    copy_tree(src, target).inspect_err(|_| remove_partial(target))
}

/// Replace file `target` atomically: the old one stays until the new one is complete.
fn replace(src: &Path, target: &Path, mv: bool) -> Result<()> {
    if mv && fs::rename(src, target).is_ok() {
        return Ok(());
    }
    let tmp = temp_name(target);
    let done = copy_tree(src, &tmp).and_then(|()| fs::rename(&tmp, target).map_err(io_err(target)));
    if let Err(e) = done {
        remove_partial(&tmp);
        return Err(e);
    }
    if mv { delete(src) } else { Ok(()) }
}

fn temp_name(target: &Path) -> PathBuf {
    let mut name = OsString::from(".tc-tmp-");
    name.push(target.file_name().unwrap_or_default());
    target.with_file_name(name)
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// `name` must be a single path component.
fn single_name(name: &OsStr) -> Result<&OsStr> {
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(n)), None) => Ok(n),
        _ => Err(Error::InvalidName(name.to_string_lossy().into_owned())),
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
    let path = parent.join(single_name(name)?);
    fs::create_dir(&path).map_err(io_err(&path))?;
    Ok(path)
}

/// Refuse copies of a directory into itself.
fn prepare(src: &Path, target: &Path) -> Result<()> {
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

/// Only called on targets this operation created.
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

    fn run(
        mv: bool,
        src: &Path,
        target: &Path,
        mut d: impl FnMut(&Path, &Path) -> Decision,
    ) -> Result<()> {
        let cancel = AtomicBool::new(false);
        if mv {
            super::move_(src, target, &mut d, &cancel)
        } else {
            super::copy(src, target, &mut d, &cancel)
        }
    }

    fn copy(src: &Path, target: &Path) -> Result<()> {
        run(false, src, target, |_, _| Decision::Cancel)
    }

    fn move_(src: &Path, target: &Path) -> Result<()> {
        run(true, src, target, |_, _| Decision::Cancel)
    }

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
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            move_(&src.join("a.txt"), &dst.join("a.txt")),
            Err(Error::Cancelled)
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

    /// src/a.txt = "a", dst/a.txt = "old", dst/a.txt older than src.
    fn clash() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("a.txt"), "old").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(dst.join("a.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        (tmp, src, dst)
    }

    fn read(p: PathBuf) -> String {
        fs::read_to_string(p).unwrap()
    }

    #[test]
    fn skip_and_overwrite() {
        let (_t, src, dst) = clash();
        run(false, &src.join("a.txt"), &dst.join("a.txt"), |_, _| {
            Decision::Skip
        })
        .unwrap();
        assert_eq!(read(dst.join("a.txt")), "old");
        run(false, &src.join("a.txt"), &dst.join("a.txt"), |_, _| {
            Decision::Overwrite
        })
        .unwrap();
        assert_eq!(read(dst.join("a.txt")), "a");
        assert!(src.join("a.txt").exists());
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 1); // no temp left
    }

    #[test]
    fn overwrite_if_newer() {
        let (_t, src, dst) = clash();
        assert!(is_newer(&src.join("a.txt"), &dst.join("a.txt")));
        assert!(!is_newer(&dst.join("a.txt"), &src.join("a.txt")));
        let newer = |s: &Path, t: &Path| {
            if is_newer(s, t) {
                Decision::Overwrite
            } else {
                Decision::Skip
            }
        };
        run(false, &src.join("a.txt"), &dst.join("a.txt"), newer).unwrap();
        assert_eq!(read(dst.join("a.txt")), "a");
    }

    #[test]
    fn rename_and_invalid_rename() {
        let (_t, src, dst) = clash();
        let ren = |n: &str| {
            let n = OsString::from(n);
            move |_: &Path, _: &Path| Decision::Rename(n.clone())
        };
        run(true, &src.join("a.txt"), &dst.join("a.txt"), ren("b.txt")).unwrap();
        assert_eq!(read(dst.join("b.txt")), "a");
        assert_eq!(read(dst.join("a.txt")), "old");
        assert!(!src.join("a.txt").exists());
        fs::write(src.join("a.txt"), "a").unwrap();
        let r = run(false, &src.join("a.txt"), &dst.join("a.txt"), ren("x/y"));
        assert!(matches!(r, Err(Error::InvalidName(_))));
    }

    #[test]
    fn move_overwrite_deletes_source_after() {
        let (_t, src, dst) = clash();
        run(true, &src.join("a.txt"), &dst.join("a.txt"), |_, _| {
            Decision::Overwrite
        })
        .unwrap();
        assert_eq!(read(dst.join("a.txt")), "a");
        assert!(!src.join("a.txt").exists());
    }

    #[test]
    fn merge_dirs_with_per_file_conflicts() {
        let (_t, src, dst) = clash();
        fs::create_dir(dst.join("src")).unwrap();
        fs::write(dst.join("src/a.txt"), "keep").unwrap();
        let mut asked = 0;
        run(true, &src, &dst.join("src"), |_, _| {
            asked += 1;
            Decision::Skip
        })
        .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(read(dst.join("src/a.txt")), "keep");
        assert_eq!(read(dst.join("src/sub/b.txt")), "b");
        assert!(src.join("a.txt").exists()); // skipped file keeps its dir alive
        assert!(!src.join("sub").exists());
    }

    #[test]
    fn dir_file_mismatch_is_error() {
        let (_t, src, dst) = clash();
        let r = run(false, &src.join("sub"), &dst.join("a.txt"), |_, _| {
            Decision::Overwrite
        });
        assert!(matches!(r, Err(Error::TypeMismatch(_))));
        fs::create_dir(dst.join("sub")).unwrap();
        let r = run(false, &src.join("a.txt"), &dst.join("sub"), |_, _| {
            Decision::Overwrite
        });
        assert!(matches!(r, Err(Error::TypeMismatch(_))));
        assert_eq!(read(dst.join("a.txt")), "old");
    }

    #[test]
    fn same_file_refused() {
        let (_t, src, _dst) = clash();
        let r = run(false, &src.join("a.txt"), &src.join("a.txt"), |_, _| {
            Decision::Overwrite
        });
        assert!(matches!(r, Err(Error::Exists(_))));
        assert_eq!(read(src.join("a.txt")), "a");
    }

    #[cfg(unix)]
    #[test]
    fn failed_overwrite_keeps_old_target() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, src, dst) = clash();
        let locked = src.join("a.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // running as root
        }
        let r = run(false, &locked, &dst.join("a.txt"), |_, _| {
            Decision::Overwrite
        });
        assert!(r.is_err());
        assert_eq!(read(dst.join("a.txt")), "old");
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 1); // temp removed
    }

    #[test]
    fn cancel_flag_stops() {
        let (_t, src, dst) = clash();
        let cancel = AtomicBool::new(true);
        let r = super::copy(&src, &dst.join("new"), &mut |_, _| Decision::Skip, &cancel);
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(!dst.join("new").exists());
    }
}
