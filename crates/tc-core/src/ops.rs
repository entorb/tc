//! Local file operations. Existing files are replaced only on an explicit [`Decision`].

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{Error, Result, io_err};

mod copy;
use copy::{copy_file, copy_tree};

/// Copy chunk: bounds memory and the delay until a cancel is noticed.
const CHUNK: usize = 1 << 20;

/// Shared between the worker (writes) and the UI (reads); also carries the cancel flag.
#[derive(Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub files: AtomicU64,
    pub files_total: AtomicU64,
    /// `total` and `files_total` are final.
    pub scanned: AtomicBool,
    /// Directories in total / done (copy, move, delete); File Stats scan: found / listing read.
    pub dirs: AtomicU64,
    pub dirs_done: AtomicU64,
    /// File being copied.
    pub current: Mutex<PathBuf>,
    /// Asked when a file fails; `None` stops at the first error.
    pub on_error: Option<OnError>,
    /// Files left out after an error ([`Recover::Skip`]).
    pub skipped: AtomicU64,
}

/// What to do with a file that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recover {
    Retry,
    Skip,
    Cancel,
}

/// Error handler with the failed path; called from worker threads, one at a time is up to it.
pub type OnError = Box<dyn Fn(&Path, &Error) -> Recover + Send + Sync>;

impl Progress {
    /// Run `f` until it works; on an error ask [`Progress::on_error`]: `Ok(false)` when the
    /// file was skipped, `Err` on cancel (the error was shown) or without a handler.
    pub fn retry(&self, path: &Path, mut f: impl FnMut() -> Result<()>) -> Result<bool> {
        loop {
            let e = match f() {
                Ok(()) => return Ok(true),
                Err(e @ Error::Cancelled) => return Err(e),
                Err(e) => e,
            };
            match self.on_error.as_ref().map(|h| h(path, &e)) {
                Some(Recover::Retry) => {}
                Some(Recover::Skip) => {
                    self.skipped.fetch_add(1, Ordering::Relaxed);
                    return Ok(false);
                }
                Some(Recover::Cancel) => return Err(Error::Cancelled),
                None => return Err(e),
            }
        }
    }

    fn skips(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    pub(crate) fn file_done(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
        self.files.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn current_is(&self, path: &Path) {
        if let Ok(mut cur) = self.current.lock() {
            path.clone_into(&mut cur);
        }
    }

    pub(crate) fn dir_done(&self) {
        self.dirs_done.fetch_add(1, Ordering::Relaxed);
    }

    /// Add `c` to the done counters.
    pub(crate) fn add(&self, c: Counts) {
        self.done.fetch_add(c.bytes, Ordering::Relaxed);
        self.files.fetch_add(c.files, Ordering::Relaxed);
        self.dirs_done.fetch_add(c.dirs, Ordering::Relaxed);
    }

    /// Store `c` as the final totals.
    pub(crate) fn set_total(&self, c: Counts) {
        self.total.store(c.bytes, Ordering::Relaxed);
        self.files_total.store(c.files, Ordering::Relaxed);
        self.dirs.store(c.dirs, Ordering::Relaxed);
        self.scanned.store(true, Ordering::Release);
    }
}

/// Bytes, files (also symlinks) and directories below a path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Self) {
        self.bytes += o.bytes;
        self.files += o.files;
        self.dirs += o.dirs;
    }
}

/// Count bytes, files and directories below `paths` (symlinks not followed, unreadable parts
/// skipped; the copy reports those errors) so progress has a total. Stops early on cancel.
pub fn scan(paths: &[PathBuf], p: &Progress) {
    let mut sum = Counts::default();
    for path in paths {
        sum += count(path, &p.cancel);
    }
    if !p.cancel.load(Ordering::Relaxed) {
        p.set_total(sum);
    }
}

/// [`Counts`] below `path`; symlinks count as files and are not followed.
pub fn count(path: &Path, cancel: &AtomicBool) -> Counts {
    let mut c = Counts::default();
    let mut stack = vec![path.to_owned()];
    while let Some(path) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() {
            c.files += 1;
            c.bytes += meta.len();
            continue;
        }
        c.dirs += 1;
        // entry metadata comes with the listing on Windows: no open per file
        for item in fs::read_dir(&path).into_iter().flatten().flatten() {
            match item.metadata() {
                Ok(m) if !m.is_dir() => {
                    c.files += 1;
                    c.bytes += m.len();
                }
                _ => stack.push(item.path()),
            }
        }
    }
    c
}

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
/// `p.cancel` is checked per file and per 1 MiB chunk; a cancel removes the partial target.
pub fn copy(src: &Path, target: &Path, ask: Ask, p: &Progress) -> Result<()> {
    prepare(src, target)?;
    place(src, target, false, &mut Ctx { ask, p })
}

/// Move or rename `src` to `target`, with the conflict rules of [`copy`]. Source files are
/// deleted only after they are in place; across devices: copy, then delete source.
pub fn move_(src: &Path, target: &Path, ask: Ask, p: &Progress) -> Result<()> {
    prepare(src, target)?;
    place(src, target, true, &mut Ctx { ask, p })
}

struct Ctx<'a> {
    ask: Ask<'a>,
    p: &'a Progress,
}

fn place(src: &Path, target: &Path, mv: bool, ctx: &mut Ctx) -> Result<()> {
    if ctx.p.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let Ok(tmeta) = fs::symlink_metadata(target) else {
        return create(src, target, mv, ctx.p);
    };
    let smeta = fs::symlink_metadata(src).map_err(io_err(src))?;
    match (smeta.is_dir(), tmeta.is_dir()) {
        (true, true) => {
            for item in fs::read_dir(src).map_err(io_err(src))? {
                let item = item.map_err(io_err(src))?;
                place(&item.path(), &target.join(item.file_name()), mv, ctx)?;
            }
            ctx.p.dir_done();
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
                Decision::Skip => {
                    ctx.p.file_done(smeta.len());
                    Ok(())
                }
                Decision::Cancel => Err(Error::Cancelled),
                Decision::Rename(name) => {
                    let parent = target.parent().unwrap_or(Path::new(""));
                    place(src, &parent.join(single_name(&name)?), mv, ctx)
                }
                Decision::Overwrite => ctx
                    .p
                    .retry(src, || replace(src, target, mv, ctx.p))
                    .map(drop),
            }
        }
        _ => Err(Error::TypeMismatch(target.to_owned())),
    }
}

/// Open a file for reading; sockets, pipes and devices are refused (opening a FIFO blocks forever).
pub(crate) fn open_regular(path: &Path) -> io::Result<fs::File> {
    if fs::metadata(path).is_ok_and(|m| copy::is_special(&m)) {
        let msg = "socket, pipe or device: no file content to read";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
    }
    fs::File::open(path)
}

/// Target does not exist: plain move or copy.
fn create(src: &Path, target: &Path, mv: bool, p: &Progress) -> Result<()> {
    if mv {
        return match fs::rename(src, target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let skips = p.skips();
                copy_tree(src, target, p, false).inspect_err(|_| remove_partial(target))?;
                // a skipped file exists only in the source: keep it all
                if p.skips() == skips {
                    delete(src)
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(io_err(src)(e)),
        };
    }
    copy_tree(src, target, p, true).inspect_err(|_| remove_partial(target))
}

/// Replace file `target` atomically: the old one stays until the new one is complete.
fn replace(src: &Path, target: &Path, mv: bool, p: &Progress) -> Result<()> {
    if mv && fs::rename(src, target).is_ok() {
        return Ok(());
    }
    // a fresh dir next to the target: same file system for the rename, never a user's file,
    // removed with whatever is left in it
    let parent = target.parent().unwrap_or(Path::new("."));
    let dir = tempfile::Builder::new()
        .prefix(".tc-tmp-")
        .tempdir_in(parent)
        .map_err(io_err(parent))?;
    let tmp = dir.path().join(target.file_name().unwrap_or_default());
    let meta = fs::symlink_metadata(src).map_err(io_err(src))?;
    if copy::is_special(&meta) {
        // a FIFO would block the open forever; the existing target stays
        return Err(Error::Special(src.to_owned()));
    }
    if meta.is_symlink() {
        copy_tree(src, &tmp, p, true)?;
    } else {
        copy_file(src, &tmp, &meta, p, true)?;
    }
    rename_over(&tmp, target).map_err(io_err(target))?;
    if mv { delete(src) } else { Ok(()) }
}

/// Rename onto an existing `target`. Windows refuses to replace a read-only file: its flag is
/// lifted for the rename (the user confirmed the overwrite) and restored on failure.
pub(crate) fn rename_over(from: &Path, target: &Path) -> io::Result<()> {
    let renamed = fs::rename(from, target);
    #[cfg(windows)]
    if renamed
        .as_ref()
        .is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied)
        && let Ok(meta) = fs::symlink_metadata(target)
        && meta.permissions().readonly()
    {
        let mut writable = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)] // Windows: only the attribute
        writable.set_readonly(false); // NOSONAR rust:S2612: Windows read-only attribute only, user confirmed the overwrite
        fs::set_permissions(target, writable)?;
        return fs::rename(from, target).inspect_err(|_| {
            let _ = fs::set_permissions(target, meta.permissions());
        });
    }
    renamed
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// `name` must be a single path component.
pub(crate) fn single_name(name: &OsStr) -> Result<&OsStr> {
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

/// Like [`delete`], counting removed bytes, files and directories into `p`; stops on cancel.
/// Unix walks by directory handle (`openat` with `O_NOFOLLOW`, `unlinkat`), so a directory
/// swapped for a symlink mid-delete is never followed.
#[cfg(unix)]
pub fn remove(path: &Path, p: &Progress) -> Result<()> {
    use rustix::fs::{CWD, Mode, OFlags, openat};
    let meta = fs::symlink_metadata(path).map_err(io_err(path))?;
    let (Some(name), true) = (path.file_name(), meta.is_dir()) else {
        // files, symlinks and odd roots (`/`, `..`): one unlink, nothing to descend into
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        p.current_is(path);
        delete(path)?;
        p.file_done(meta.len());
        return Ok(());
    };
    let parent = path.parent().filter(|d| !d.as_os_str().is_empty());
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let dir = openat(CWD, parent.unwrap_or(Path::new(".")), flags, Mode::empty())
        .map_err(|e| io_err(path)(e.into()))?;
    unix_remove::at(&dir, name, path, p)
}

#[cfg(unix)]
mod unix_remove {
    use super::{Error, Ordering, Progress, Result, io_err};
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat, unlinkat};
    use std::ffi::{OsStr, OsString};
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// Remove `name` inside the open directory `dir`; `path` is only for messages.
    pub(super) fn at(dir: impl AsFd, name: &OsStr, path: &Path, p: &Progress) -> Result<()> {
        let err = |e: rustix::io::Errno| io_err(path)(e.into());
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let st = statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(err)?;
        if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
            p.current_is(path);
            // a directory swapped in meanwhile makes this fail (EISDIR), never recurse
            if p.retry(path, || unlinkat(&dir, name, AtFlags::empty()).map_err(err))? {
                p.file_done(u64::try_from(st.st_size).unwrap_or(0));
            }
            return Ok(());
        }
        let sub = open_dir(&dir, name).map_err(err)?;
        // names first: removing while reading can skip entries on some file systems
        let names: Vec<OsString> = Dir::read_from(&sub)
            .map_err(err)?
            .map(|e| e.map(|e| OsStr::from_bytes(e.file_name().to_bytes()).to_owned()))
            .filter(|n| n.as_ref().map_or(true, |n| n != "." && n != ".."))
            .collect::<rustix::io::Result<_>>()
            .map_err(err)?;
        let skips = p.skipped.load(Ordering::Relaxed);
        for n in names {
            at(&sub, &n, &path.join(&n), p)?;
        }
        if p.skipped.load(Ordering::Relaxed) != skips {
            return Ok(()); // still holds what the user skipped
        }
        unlinkat(&dir, name, AtFlags::REMOVEDIR).map_err(err)?;
        p.dir_done();
        Ok(())
    }

    /// Fails (ELOOP / ENOTDIR) when `name` is a symlink, even one to a directory.
    pub(super) fn open_dir(dir: impl AsFd, name: &OsStr) -> rustix::io::Result<OwnedFd> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        openat(dir, name, flags, Mode::empty())
    }
}

/// Like [`delete`], counting into `p`: std's `remove_dir_all` guards against symlink swaps, so
/// progress moves once per path.
#[cfg(not(unix))]
pub fn remove(path: &Path, p: &Progress) -> Result<()> {
    if p.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let c = count(path, &p.cancel);
    p.current_is(path);
    delete(path)?;
    p.add(c);
    Ok(())
}

/// Create directory `name` inside `parent`. `name` must be a single path component.
pub fn mkdir(parent: &Path, name: &OsStr) -> Result<PathBuf> {
    let path = parent.join(single_name(name)?);
    fs::create_dir(&path).map_err(io_err(&path))?;
    Ok(path)
}

/// Create empty file `name` inside `parent`; never truncates an existing file.
pub fn touch(parent: &Path, name: &OsStr) -> Result<PathBuf> {
    let path = parent.join(single_name(name)?);
    fs::File::create_new(&path).map_err(io_err(&path))?;
    Ok(path)
}

/// Refuse to touch an existing `link` path, dangling symlinks included.
fn free_link(link: &Path) -> Result<()> {
    match fs::symlink_metadata(link) {
        Ok(_) => Err(io_err(link)(io::Error::from(io::ErrorKind::AlreadyExists))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(link)(e)),
    }
}

/// Create symlink `link` pointing to `target`, stored as given (may be relative, may dangle).
/// Never replaces an existing path.
pub fn symlink(target: &Path, link: &Path) -> Result<()> {
    free_link(link)?;
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = {
        // a relative target is relative to the link's directory
        let abs = link.parent().unwrap_or(link).join(target);
        if abs.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    };
    made.map_err(io_err(link))
}

/// Create hard link `link` to the file `target`. Directories are refused. Never replaces an
/// existing path.
pub fn hard_link(target: &Path, link: &Path) -> Result<()> {
    free_link(link)?;
    let meta = fs::symlink_metadata(target).map_err(io_err(target))?;
    if meta.is_dir() {
        let e = io::Error::other("hard links to directories are not possible");
        return Err(io_err(target)(e));
    }
    fs::hard_link(target, link).map_err(io_err(link))
}

/// Move to the system trash.
pub fn trash(path: &Path) -> Result<()> {
    ::trash::delete(path).map_err(|e| io_err(path)(io::Error::other(e.to_string())))
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

    /// `io::copy` of a `Take<&File>` into a `File` runs in the kernel (`copy_file_range`:
    /// reflinks, server-side NFS/SMB copies), still one chunk at a time for progress and cancel.
    #[cfg(target_os = "linux")]
    pub fn copy_chunk(
        from: &std::fs::File,
        to: &mut std::fs::File,
        _buf: &mut Vec<u8>,
    ) -> io::Result<u64> {
        use std::io::Read;
        io::copy(&mut from.take(super::CHUNK as u64), to)
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) use super::copy::copy_chunk_buffered as copy_chunk;
}

#[cfg(windows)]
mod platform {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::{fs, io, path::Path};

    pub(super) use super::copy::copy_chunk_buffered as copy_chunk;

    /// ERROR_PRIVILEGE_NOT_HELD: symlinks need admin rights or developer mode.
    const NO_PRIVILEGE: i32 = 1314;

    /// Copy of the link `src` (pointing to `link`) at `dst`. Without the symlink privilege a
    /// directory link becomes a junction (pnpm's `node_modules`), a file link a copy of its target.
    pub fn symlink(link: &Path, dst: &Path, src: &Path) -> io::Result<()> {
        let is_dir = fs::metadata(src).is_ok_and(|m| m.is_dir());
        let made = if is_dir {
            std::os::windows::fs::symlink_dir(link, dst)
        } else {
            std::os::windows::fs::symlink_file(link, dst)
        };
        match made {
            Err(e) if e.raw_os_error() == Some(NO_PRIVILEGE) && is_dir => {
                let base = src.parent().unwrap_or(Path::new("."));
                junction(&base.join(link), dst).map_err(|_| e)
            }
            Err(e) if e.raw_os_error() == Some(NO_PRIVILEGE) => {
                let mut from = fs::File::open(src)?;
                let mut to = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dst)?;
                io::copy(&mut from, &mut to).map(drop)
            }
            r => r,
        }
    }

    /// `mklink /J dst target`: junctions need no privilege; the target must be absolute.
    /// Never replaces `dst` (mklink fails on an existing path).
    fn junction(target: &Path, dst: &Path) -> io::Result<()> {
        let target = std::path::absolute(target)?;
        let target = target.to_string_lossy();
        let target = target.strip_prefix(r"\\?\").unwrap_or(&target);
        let dst = std::path::absolute(dst)?;
        let dst = dst.to_string_lossy();
        // cmd expands %VAR% even inside quotes
        if target.contains('%') || dst.contains('%') {
            return Err(io::Error::other("% in path"));
        }
        let status = Command::new("cmd")
            .raw_arg(format!("/C mklink /J \"{dst}\" \"{target}\""))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("mklink /J: {status}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_points_to_target_and_never_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        if !dir_link(tmp.path(), &tmp.path().join("probe")) {
            return;
        }
        let (t, l) = (tmp.path().join("t"), tmp.path().join("l"));
        fs::write(&t, "x").unwrap();
        symlink(&t, &l).unwrap();
        assert_eq!(fs::read_link(&l).unwrap(), t);
        assert!(symlink(&t, &l).is_err());
        assert_eq!(fs::read_link(&l).unwrap(), t);
        // dangling link counts as existing
        let d = tmp.path().join("d");
        symlink(Path::new("nowhere"), &d).unwrap();
        assert!(symlink(&t, &d).is_err());
        assert_eq!(fs::read_link(&d).unwrap(), Path::new("nowhere"));
        // existing file untouched
        assert!(symlink(&l, &t).is_err());
        assert_eq!(fs::read(&t).unwrap(), b"x");
    }

    #[test]
    fn hard_link_shares_content_and_refuses_dirs_and_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let (t, l) = (tmp.path().join("t"), tmp.path().join("l"));
        fs::write(&t, "x").unwrap();
        hard_link(&t, &l).unwrap();
        fs::write(&t, "y").unwrap();
        assert_eq!(fs::read(&l).unwrap(), b"y");
        assert!(hard_link(&t, &l).is_err());
        let dir = tmp.path().join("dir");
        fs::create_dir(&dir).unwrap();
        assert!(hard_link(&dir, &tmp.path().join("dl")).is_err());
        assert!(!tmp.path().join("dl").exists());
    }

    #[test]
    fn touch_never_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let f = touch(tmp.path(), OsStr::new("a")).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"");
        fs::write(&f, "keep").unwrap();
        assert!(touch(tmp.path(), OsStr::new("a")).is_err());
        assert_eq!(fs::read(&f).unwrap(), b"keep");
        assert!(touch(tmp.path(), OsStr::new("a/b")).is_err());
    }

    fn run(
        mv: bool,
        src: &Path,
        target: &Path,
        mut d: impl FnMut(&Path, &Path) -> Decision,
    ) -> Result<()> {
        let p = Progress::default();
        if mv {
            super::move_(src, target, &mut d, &p)
        } else {
            super::copy(src, target, &mut d, &p)
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

    /// Progress with an error handler giving `answers` in turn (then Cancel).
    fn asking(answers: &[Recover]) -> Progress {
        let left = Mutex::new(answers.to_vec());
        Progress {
            on_error: Some(Box::new(move |_, _| {
                let mut l = left.lock().unwrap();
                if l.is_empty() {
                    Recover::Cancel
                } else {
                    l.remove(0)
                }
            })),
            ..Default::default()
        }
    }

    #[test]
    fn retry_skip_cancel() {
        let fail = || Err(Error::InvalidName("x".into()));
        let mut tries = 0;
        let p = asking(&[Recover::Retry, Recover::Retry]);
        let ok = p.retry(Path::new("f"), || {
            tries += 1;
            if tries < 3 { fail() } else { Ok(()) }
        });
        assert_eq!((ok.unwrap(), tries), (true, 3));
        let p = asking(&[Recover::Skip]);
        assert!(!p.retry(Path::new("f"), fail).unwrap());
        assert_eq!(p.skipped.load(Ordering::Relaxed), 1);
        assert!(matches!(
            asking(&[]).retry(Path::new("f"), fail),
            Err(Error::Cancelled)
        ));
        // no handler: the error itself, as before
        let none = Progress::default().retry(Path::new("f"), fail);
        assert!(matches!(none, Err(Error::InvalidName(_))));
    }

    /// An unreadable file is skipped and the rest copied; a delete keeps the directory that
    /// holds a skipped file.
    #[cfg(unix)] // file modes are Unix-only
    #[test]
    fn skipped_file_keeps_its_source() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let locked = src.join("sub/b.txt");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&locked).is_ok() {
            return; // root reads anything
        }
        let p = asking(&[Recover::Skip]);
        let dst = tmp.path().join("dst");
        super::copy(&src, &dst, &mut |_, _| Decision::Cancel, &p).unwrap();
        assert_eq!(read(dst.join("a.txt")), "a");
        assert!(!dst.join("sub/b.txt").exists()); // no partial file
        assert_eq!(p.skipped.load(Ordering::Relaxed), 1);
        // delete: a locked dir keeps its file, the user skips, the dir stays, the rest goes
        let sub = dst.join("sub");
        fs::write(sub.join("c.txt"), "c").unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).unwrap();
        let p = asking(&[Recover::Skip]);
        remove(&dst, &p).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(sub.join("c.txt").exists() && !dst.join("a.txt").exists());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(unix)] // sockets and FIFOs are Unix-only
    #[test]
    fn copy_skips_sockets_and_fifos() {
        use rustix::fs::{CWD, FileType, Mode, mknodat};
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let _sock = std::os::unix::net::UnixListener::bind(src.join("d.sock")).unwrap();
        mknodat(
            CWD,
            src.join("sub/pipe"),
            FileType::Fifo,
            Mode::RUSR | Mode::WUSR,
            0,
        )
        .unwrap();
        copy(&src, &tmp.path().join("dst")).unwrap(); // a FIFO open would hang here
        let dst = tmp.path().join("dst");
        assert_eq!(fs::read_to_string(dst.join("sub/b.txt")).unwrap(), "b");
        assert!(!dst.join("d.sock").exists() && !dst.join("sub/pipe").exists());
        // overwriting a file with a FIFO fails and keeps the file
        let ask = |_: &Path, _: &Path| Decision::Overwrite;
        let r = run(false, &src.join("sub/pipe"), &dst.join("a.txt"), ask);
        assert!(matches!(r, Err(Error::Special(_))));
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "a");
        // viewer, editor, find and remote uploads refuse it instead of blocking
        assert!(super::open_regular(&src.join("sub/pipe")).is_err());
    }

    /// Directory symlink; false where the user may not create symlinks (Windows without
    /// developer mode or admin rights).
    fn dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, link);
        match made {
            Ok(()) => true,
            Err(e) if cfg!(windows) && e.raw_os_error() == Some(1314) => false,
            Err(e) => panic!("{e}"),
        }
    }

    /// Make `path` unreadable for this process while the guard lives; `None` as root.
    #[cfg(unix)]
    fn lock(path: &Path) -> Option<Box<dyn std::any::Any>> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        fs::read(path)
            .is_err()
            .then(|| Box::new(()) as Box<dyn std::any::Any>)
    }

    /// Make `path` unreadable for this process while the guard (an exclusive handle) lives.
    #[cfg(windows)]
    fn lock(path: &Path) -> Option<Box<dyn std::any::Any>> {
        use std::os::windows::fs::OpenOptionsExt;
        let f = fs::File::options().read(true).share_mode(0).open(path);
        Some(Box::new(f.unwrap()))
    }

    fn set_readonly(path: &Path, on: bool) {
        let mut perm = fs::metadata(path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(on);
        fs::set_permissions(path, perm).unwrap();
    }

    /// Paths beyond Windows' MAX_PATH (260) take the stream copy.
    #[test]
    fn copy_and_delete_long_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let mut deep = tmp.path().join("src");
        for i in 0..6 {
            deep.push(format!("{i}{}", "d".repeat(50)));
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("f.txt"), "deep").unwrap();
        let dst = tmp.path().join("dst");
        copy(&tmp.path().join("src"), &dst).unwrap();
        let rel = deep.strip_prefix(tmp.path().join("src")).unwrap();
        assert_eq!(read(dst.join(rel).join("f.txt")), "deep");
        let p = Progress::default();
        remove(&dst, &p).unwrap();
        assert!(!dst.exists());
    }

    /// Read-only files (git objects) keep the flag on copy and can be replaced and deleted.
    #[test]
    fn read_only_files_copy_replace_and_delete() {
        let (_t, src, dst) = clash();
        set_readonly(&src.join("a.txt"), true);
        set_readonly(&dst.join("a.txt"), true);
        copy(&src.join("a.txt"), &dst.join("b.txt")).unwrap();
        assert!(
            fs::metadata(dst.join("b.txt"))
                .unwrap()
                .permissions()
                .readonly()
        );
        run(false, &src.join("a.txt"), &dst.join("a.txt"), |_, _| {
            Decision::Overwrite
        })
        .unwrap();
        assert_eq!(read(dst.join("a.txt")), "a");
        delete(&dst.join("b.txt")).unwrap();
        assert!(!dst.join("b.txt").exists());
        remove(&src, &Progress::default()).unwrap();
        assert!(!src.exists());
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

    /// Many files in nested and read-only directories go through the parallel workers.
    #[test]
    fn copy_many_files_into_read_only_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        for d in 0..5 {
            fs::create_dir_all(src.join(format!("d{d}/e"))).unwrap();
            for f in 0..40 {
                fs::write(src.join(format!("d{d}/e/{f}")), format!("{d}-{f}")).unwrap();
            }
            let ro = src.join(format!("d{d}"));
            let mut perm = fs::metadata(&ro).unwrap().permissions();
            perm.set_readonly(true);
            fs::set_permissions(&ro, perm).unwrap();
        }
        let p = Progress::default();
        super::copy(
            &src,
            &tmp.path().join("dst"),
            &mut |_, _| Decision::Cancel,
            &p,
        )
        .unwrap();
        assert_eq!(p.files.load(Ordering::Relaxed), 200);
        let got = fs::read_to_string(tmp.path().join("dst/d4/e/39")).unwrap();
        assert_eq!(got, "4-39");
        assert!(
            fs::metadata(tmp.path().join("dst/d0"))
                .unwrap()
                .permissions()
                .readonly()
        );
        for d in 0..5 {
            for root in ["src", "dst"] {
                let dir = tmp.path().join(format!("{root}/d{d}"));
                let mut perm = fs::metadata(&dir).unwrap().permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                fs::set_permissions(&dir, perm).unwrap();
            }
        }
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
    fn overwrite_keeps_mtime_and_spares_foreign_temp_names() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("a.txt"), "old").unwrap();
        fs::write(dst.join(".tc-tmp-a.txt"), "mine").unwrap();
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::options()
            .write(true)
            .open(src.join("a.txt"))
            .unwrap()
            .set_modified(t)
            .unwrap();
        run(false, &src.join("a.txt"), &dst.join("a.txt"), |_, _| {
            Decision::Overwrite
        })
        .unwrap();
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "a");
        assert_eq!(
            fs::read_to_string(dst.join(".tc-tmp-a.txt")).unwrap(),
            "mine"
        );
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 2); // no temp dir left
        let mtime = |p: &Path| fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(mtime(&dst.join("a.txt")), t);
        copy(&src.join("a.txt"), &dst.join("b.txt")).unwrap();
        assert_eq!(mtime(&dst.join("b.txt")), t);
    }

    #[test]
    fn copies_big_file_in_chunks_with_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("big");
        let data: Vec<u8> = (0..3 * CHUNK + 7).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &data).unwrap();
        let p = Progress::default();
        super::copy(
            &src,
            &tmp.path().join("out"),
            &mut |_, _| Decision::Cancel,
            &p,
        )
        .unwrap();
        assert!(fs::read(tmp.path().join("out")).unwrap() == data);
        assert_eq!(p.done.load(Ordering::Relaxed), data.len() as u64);
        assert_eq!(p.files.load(Ordering::Relaxed), 1);
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

    #[test]
    fn symlinks_copied_and_deleted_as_links() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "").unwrap();
        if !dir_link(&outside, &src.join("link")) {
            return;
        }
        dir_link(&tmp.path().join("gone"), &src.join("dangling"));
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        copy(&src, &dst.join("src")).unwrap();
        assert!(
            fs::symlink_metadata(dst.join("src/link"))
                .unwrap()
                .is_symlink()
        );
        let dangling = fs::symlink_metadata(dst.join("src/dangling")).unwrap();
        assert!(dangling.is_symlink());
        delete(&src).unwrap();
        assert!(!src.exists());
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn failed_copy_cleans_partial_target() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let Some(_lock) = lock(&src.join("sub/b.txt")) else {
            return;
        };
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

    #[test]
    fn failed_overwrite_keeps_old_target() {
        let (_t, src, dst) = clash();
        let locked = src.join("a.txt");
        let Some(_lock) = lock(&locked) else {
            return;
        };
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
        let p = Progress::default();
        p.cancel.store(true, Ordering::Relaxed);
        let r = super::copy(&src, &dst.join("new"), &mut |_, _| Decision::Skip, &p);
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(!dst.join("new").exists());
    }

    /// Cancel after the first chunk of a 2 GiB sparse file (never fully read).
    #[test]
    #[cfg_attr(target_os = "macos", ignore = "APFS clones the file at once")]
    fn cancel_mid_file_leaves_no_partial() {
        let (_t, src, dst) = clash();
        let big = src.join("big");
        fs::File::create(&big).unwrap().set_len(2 << 30).unwrap();
        fs::File::create(dst.join("big"))
            .unwrap()
            .set_len(7)
            .unwrap();
        for (name, d) in [("new", Decision::Skip), ("big", Decision::Overwrite)] {
            let p = Progress::default();
            let r = std::thread::scope(|s| {
                s.spawn(|| {
                    while p.done.load(Ordering::Relaxed) == 0 {
                        std::thread::yield_now();
                    }
                    p.cancel.store(true, Ordering::Relaxed);
                });
                super::copy(&big, &dst.join(name), &mut |_, _| d.clone(), &p)
            });
            assert!(matches!(r, Err(Error::Cancelled)));
            assert!(p.done.load(Ordering::Relaxed) < 2 << 30);
        }
        assert!(!dst.join("new").exists());
        assert_eq!(fs::metadata(dst.join("big")).unwrap().len(), 7);
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 2); // a.txt, big: no temp
        assert_eq!(fs::metadata(&big).unwrap().len(), 2 << 30);
    }

    #[test]
    fn scan_counts_and_progress_advances() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let p = Progress::default();
        scan(std::slice::from_ref(&src), &p);
        assert_eq!(p.total.load(Ordering::Relaxed), 2);
        assert_eq!(p.files_total.load(Ordering::Relaxed), 2);
        super::copy(&src, &tmp.path().join("c"), &mut |_, _| Decision::Skip, &p).unwrap();
        assert_eq!(p.done.load(Ordering::Relaxed), 2);
        assert_eq!(p.files.load(Ordering::Relaxed), 2);
        assert_eq!(p.dirs.load(Ordering::Relaxed), 2);
        assert_eq!(p.dirs_done.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn remove_counts_and_keeps_symlink_target() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "k").unwrap();
        if !dir_link(&outside, &src.join("sub/link")) {
            return;
        }
        let p = Progress::default();
        scan(std::slice::from_ref(&src), &p);
        remove(&src, &p).unwrap();
        assert!(!src.exists());
        assert!(outside.join("keep").exists());
        for (done, total) in [
            (&p.done, &p.total),
            (&p.files, &p.files_total),
            (&p.dirs_done, &p.dirs),
        ] {
            assert_eq!(done.load(Ordering::Relaxed), total.load(Ordering::Relaxed));
        }
        assert_eq!(p.files.load(Ordering::Relaxed), 3);
        assert_eq!(p.dirs_done.load(Ordering::Relaxed), 2);
    }

    /// The guard against a directory swapped for a symlink mid-delete.
    #[cfg(unix)]
    #[test]
    fn remove_never_opens_a_symlink_as_directory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("real")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("swapped")).unwrap();
        let dir = fs::File::open(tmp.path()).unwrap();
        assert!(super::unix_remove::open_dir(&dir, OsStr::new("real")).is_ok());
        assert!(super::unix_remove::open_dir(&dir, OsStr::new("swapped")).is_err());
    }

    #[test]
    fn remove_stops_on_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tree(tmp.path());
        let p = Progress::default();
        p.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(remove(&src, &p), Err(Error::Cancelled)));
        assert!(src.join("a.txt").exists());
    }
}
