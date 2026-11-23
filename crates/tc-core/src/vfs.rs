//! Remote file systems (SFTP, FTP, SMB) behind one trait, and copy, move and delete between
//! them and the local disk with the conflict rules of [`crate::ops`].

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::ops::{self};
use crate::panel::Entry;
use crate::{Error, Result, io_err};

#[cfg(any(test, feature = "mem"))]
pub mod mem;
mod xfer;

pub use xfer::{delete_all, is_newer, scan, transfer};

const CHUNK: usize = 1 << 20;

/// A connected remote file system. Paths are absolute and `/` separated. Implementations
/// serialize access internally, so one connection can be shared by listing and copy workers.
pub trait Remote: Send + Sync + fmt::Debug {
    /// `scheme://user@host:port`, shown in front of the path.
    fn url(&self) -> &str;
    /// Directory entries without `.` and `..`.
    fn list(&self, dir: &str) -> Result<Vec<Entry>>;
    /// `None` if nothing is at `path`. Follows symlinks.
    fn stat(&self, path: &str) -> Result<Option<Entry>>;
    /// Like `stat`, but a symlink is reported as the link itself (`is_symlink`), never followed.
    fn lstat(&self, path: &str) -> Result<Option<Entry>> {
        self.stat(path)
    }
    /// Stream the file at `path` into `f`.
    fn read(&self, path: &str, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()>;
    /// Create a new file at `path` (error if it exists) and let `f` write its content.
    fn write_new(&self, path: &str, f: &mut dyn FnMut(&mut dyn Write) -> Result<()>) -> Result<()>;
    fn mkdir(&self, path: &str) -> Result<()>;
    fn remove_file(&self, path: &str) -> Result<()>;
    /// Remove an empty directory.
    fn remove_dir(&self, path: &str) -> Result<()>;
    /// Target must not exist.
    fn rename(&self, from: &str, to: &str) -> Result<()>;
    /// Run `cmd` in the server's shell and stream its stdout into `f` (stderr dropped).
    fn exec(&self, _cmd: &str, _f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
        Err(crate::Error::Remote(format!("{}: no commands", self.url())))
    }
    /// Directory a new connection starts in.
    fn home(&self) -> String {
        "/".into()
    }
}

/// Remote path from a panel path (`\` from `Path::join` on Windows becomes `/`).
pub fn rpath(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Where a path lives.
#[derive(Clone, Debug)]
pub enum Side {
    Local,
    Remote(Arc<dyn Remote>),
}

impl Side {
    pub fn remote(&self) -> Option<&Arc<dyn Remote>> {
        match self {
            Side::Local => None,
            Side::Remote(r) => Some(r),
        }
    }

    pub(crate) fn same(&self, other: &Side) -> bool {
        match (self, other) {
            (Side::Local, Side::Local) => true,
            (Side::Remote(a), Side::Remote(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Symlink pointing to a directory: never descended into below the selected paths, so a
    /// link loop cannot recurse forever.
    fn is_dir_link(&self, p: &Path) -> Result<bool> {
        let link = match self {
            Side::Remote(r) => r.lstat(&rpath(p))?.is_some_and(|e| e.is_symlink),
            Side::Local => fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink()),
        };
        Ok(link && self.stat(p)?.is_some_and(|e| e.is_dir))
    }

    /// Local sources follow symlinks (a dangling one reads as the link itself).
    pub fn stat(&self, p: &Path) -> Result<Option<Entry>> {
        match self {
            Side::Remote(r) => r.stat(&rpath(p)),
            Side::Local => match fs::symlink_metadata(p) {
                Ok(_) => {
                    let name = p.file_name().unwrap_or_default().to_owned();
                    Ok(crate::panel::entry_for(name, p))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io_err(p)(e)),
            },
        }
    }

    fn names(&self, dir: &Path) -> Result<Vec<OsString>> {
        match self {
            Side::Remote(r) => Ok(r.list(&rpath(dir))?.into_iter().map(|e| e.name).collect()),
            Side::Local => fs::read_dir(dir)
                .map_err(io_err(dir))?
                .map(|i| i.map(|i| i.file_name()).map_err(io_err(dir)))
                .collect(),
        }
    }

    pub(crate) fn read(
        &self,
        p: &Path,
        f: &mut dyn FnMut(&mut dyn Read) -> Result<()>,
    ) -> Result<()> {
        match self {
            Side::Remote(r) => r.read(&rpath(p), f),
            Side::Local => f(&mut fs::File::open(p).map_err(io_err(p))?),
        }
    }

    fn write_new(&self, p: &Path, f: &mut dyn FnMut(&mut dyn Write) -> Result<()>) -> Result<()> {
        match self {
            Side::Remote(r) => r.write_new(&rpath(p), f),
            Side::Local => f(&mut fs::File::create_new(p).map_err(io_err(p))?),
        }
    }

    /// Create directory `name` (a single path component) inside `parent`.
    pub fn mkdir_in(&self, parent: &Path, name: &std::ffi::OsStr) -> Result<PathBuf> {
        let path = parent.join(ops::single_name(name)?);
        self.mkdir(&path)?;
        Ok(path)
    }

    pub fn mkdir(&self, p: &Path) -> Result<()> {
        match self {
            Side::Remote(r) => r.mkdir(&rpath(p)),
            Side::Local => fs::create_dir(p).map_err(io_err(p)),
        }
    }

    fn remove_file(&self, p: &Path) -> Result<()> {
        match self {
            Side::Remote(r) => r.remove_file(&rpath(p)),
            Side::Local => fs::remove_file(p).map_err(io_err(p)),
        }
    }

    pub(crate) fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        match self {
            Side::Remote(r) => r.rename(&rpath(from), &rpath(to)),
            Side::Local => fs::rename(from, to).map_err(io_err(from)),
        }
    }

    /// Delete a file or directory tree; symlinks are removed as links, their targets are never touched.
    pub fn delete(&self, p: &Path) -> Result<()> {
        match self {
            Side::Remote(_) => self.delete_with(p, &ops::Progress::default()),
            Side::Local => ops::delete(p),
        }
    }

    /// Like [`Side::delete`], counting into `pr`; stops on cancel.
    pub fn delete_with(&self, p: &Path, pr: &ops::Progress) -> Result<()> {
        let Side::Remote(r) = self else {
            return ops::remove(p, pr);
        };
        if pr.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(crate::Error::Cancelled);
        }
        let path = rpath(p);
        let Some(e) = r.lstat(&path)? else {
            return Err(io_err(p)(io::ErrorKind::NotFound.into()));
        };
        if e.is_dir && !e.is_symlink {
            for name in self.names(p)? {
                self.delete_with(&p.join(name), pr)?;
            }
            r.remove_dir(&path)?;
            pr.dir_done();
        } else {
            if let Ok(mut cur) = pr.current.lock() {
                p.clone_into(&mut cur);
            }
            r.remove_file(&path)?;
            pr.file_done(e.size);
        }
        Ok(())
    }
}

/// Download a remote file into a fresh temp dir (for the viewer and editor); at most `limit` bytes.
pub fn download_temp(
    r: &Arc<dyn Remote>,
    path: &Path,
    limit: u64,
    progress: &std::sync::atomic::AtomicU64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir().map_err(io_err(std::env::temp_dir()))?;
    let file = dir.path().join(path.file_name().unwrap_or("file".as_ref()));
    let side = Side::Remote(r.clone());
    side.read(path, &mut |rd| {
        let mut out = fs::File::create_new(&file).map_err(io_err(&file))?;
        let mut rd = rd.take(limit + 1);
        let mut buf = vec![0; CHUNK];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = rd.read(&mut buf).map_err(io_err(path))?;
            if n == 0 {
                return Ok(());
            }
            if progress.fetch_add(n as u64, Ordering::Relaxed) + n as u64 > limit {
                return Err(Error::Remote(
                    "file too large to open, copy it with F5".into(),
                ));
            }
            out.write_all(&buf[..n]).map_err(io_err(&file))?;
        }
    })?;
    Ok((dir, file))
}
