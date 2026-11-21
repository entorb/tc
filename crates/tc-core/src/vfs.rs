//! Remote file systems (SFTP, FTP, SMB) behind one trait, and copy, move and delete between
//! them and the local disk with the conflict rules of [`crate::ops`].

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::ops::{self, Ask, Decision, Progress};
use crate::panel::Entry;
use crate::{Error, Result, io_err};

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

    fn same(&self, other: &Side) -> bool {
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

    fn read(&self, p: &Path, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
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

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        match self {
            Side::Remote(r) => r.rename(&rpath(from), &rpath(to)),
            Side::Local => fs::rename(from, to).map_err(io_err(from)),
        }
    }

    /// Delete a file or directory tree; symlinks are removed as links, their targets are never touched.
    pub fn delete(&self, p: &Path) -> Result<()> {
        let Side::Remote(r) = self else {
            return ops::delete(p);
        };
        let path = rpath(p);
        let Some(e) = r.lstat(&path)? else {
            return Err(io_err(p)(io::ErrorKind::NotFound.into()));
        };
        if e.is_dir && !e.is_symlink {
            for name in self.names(p)? {
                self.delete(&p.join(name))?;
            }
            r.remove_dir(&path)
        } else {
            r.remove_file(&path)
        }
    }
}

/// Like [`ops::scan`] for any side.
pub fn scan(side: &Side, paths: &[PathBuf], p: &Progress) {
    if matches!(side, Side::Local) {
        return ops::scan(paths, p);
    }
    let (mut bytes, mut files) = (0, 0);
    // (path, selected): selected paths are followed, symlinked directories below them are not (as in transfer)
    let mut stack: Vec<(PathBuf, bool)> = paths.iter().map(|p| (p.clone(), true)).collect();
    while let Some((path, selected)) = stack.pop() {
        if p.cancel.load(Ordering::Relaxed) {
            return;
        }
        if !selected && side.is_dir_link(&path).unwrap_or(true) {
            continue;
        }
        match side.stat(&path) {
            Ok(Some(e)) if e.is_dir => stack.extend(
                side.names(&path)
                    .into_iter()
                    .flatten()
                    .map(|n| (path.join(n), false)),
            ),
            Ok(Some(e)) => {
                files += 1;
                bytes += e.size;
            }
            _ => {}
        }
    }
    p.total.store(bytes, Ordering::Relaxed);
    p.files_total.store(files, Ordering::Relaxed);
    p.scanned.store(true, Ordering::Release);
}

/// True when `s` was modified after `t`.
pub fn is_newer(src: &Side, s: &Path, dst: &Side, t: &Path) -> bool {
    let time = |side: &Side, p| side.stat(p).ok().flatten().and_then(|e| e.modified);
    matches!((time(src, s), time(dst, t)), (Some(a), Some(b)) if a > b)
}

/// Copy (`mv` false) or move `s` on `src` to `t` on `dst`, rules as [`ops::copy`]: directories
/// merge, an existing file is replaced only when `ask` says so (via `.tc-tmp-<name>` and a
/// rename), new partial targets are removed on failure or cancel, a moved source is deleted
/// only after its copy is complete. Directories into themselves are refused on the same side.
pub fn transfer(
    src: &Side,
    s: &Path,
    dst: &Side,
    t: &Path,
    mv: bool,
    ask: Ask,
    p: &Progress,
) -> Result<()> {
    if src.same(dst) && t.starts_with(s) && s != t {
        return Err(Error::IntoItself(s.to_owned()));
    }
    let x = Xfer {
        src,
        dst,
        mv,
        p,
        links: Default::default(),
    };
    x.place(s, t, ask)?;
    let links = x.links.into_inner();
    if links.is_empty() {
        return Ok(());
    }
    let names: Vec<_> = links
        .iter()
        .take(3)
        .map(|l| l.display().to_string())
        .collect();
    let more = if links.len() > 3 { ", …" } else { "" };
    let kept = if mv {
        "; their source directories were kept"
    } else {
        ""
    };
    Err(Error::Remote(format!(
        "{} symlinked directories not copied (symlinks are not created on remote targets){kept}: {}{more}",
        links.len(),
        names.join(", ")
    )))
}

struct Xfer<'a> {
    src: &'a Side,
    dst: &'a Side,
    mv: bool,
    p: &'a Progress,
    /// Symlinked directories below the selection, skipped instead of followed.
    links: std::cell::RefCell<Vec<PathBuf>>,
}

impl Xfer<'_> {
    fn place(&self, s: &Path, t: &Path, ask: Ask) -> Result<()> {
        if self.p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let se = self
            .src
            .stat(s)?
            .ok_or_else(|| io_err(s)(io::ErrorKind::NotFound.into()))?;
        let Some(te) = self.dst.stat(t)? else {
            return self.create(s, t, se.is_dir);
        };
        match (se.is_dir, te.is_dir) {
            (true, true) => {
                for name in self.src.names(s)? {
                    let child = s.join(&name);
                    if !self.skip_link(&child)? {
                        self.place(&child, &t.join(&name), ask)?;
                    }
                }
                // non-empty (skipped files) stays
                if self.mv {
                    let _ = self.src.delete_empty_dir(s);
                }
                Ok(())
            }
            (false, false) => {
                if self.src.same(self.dst) && s == t {
                    return Err(Error::Exists(t.to_owned()));
                }
                match ask(s, t) {
                    Decision::Skip => {
                        self.p.done.fetch_add(se.size, Ordering::Relaxed);
                        self.p.files.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                    Decision::Cancel => Err(Error::Cancelled),
                    Decision::Rename(name) => {
                        let parent = t.parent().unwrap_or(Path::new(""));
                        self.place(s, &parent.join(ops::single_name(&name)?), ask)
                    }
                    Decision::Overwrite => self.replace(s, t),
                }
            }
            _ => Err(Error::TypeMismatch(t.to_owned())),
        }
    }

    fn create(&self, s: &Path, t: &Path, is_dir: bool) -> Result<()> {
        if self.mv && self.src.same(self.dst) {
            return self.dst.rename(s, t);
        }
        let skipped = self.links.borrow().len();
        self.copy_tree(s, t, is_dir).inspect_err(|_| {
            let _ = self.dst.delete(t);
        })?;
        // a skipped link inside would be lost with its source tree
        if self.mv && self.links.borrow().len() == skipped {
            self.src.delete(s)
        } else {
            Ok(())
        }
    }

    /// Record and skip `p` if it is a symlinked directory.
    fn skip_link(&self, p: &Path) -> Result<bool> {
        let link = self.src.is_dir_link(p)?;
        if link {
            self.links.borrow_mut().push(p.to_owned());
        }
        Ok(link)
    }

    /// The old file stays until the new one is complete.
    fn replace(&self, s: &Path, t: &Path) -> Result<()> {
        let tmp = ops::temp_name(t);
        let staged = if self.mv && self.src.same(self.dst) {
            self.dst.rename(s, &tmp)
        } else {
            self.copy_file(s, &tmp).inspect_err(|_| {
                let _ = self.dst.delete(&tmp);
            })
        };
        staged?;
        // remote renames do not replace: drop the old file only once the new one is complete
        if self.dst.rename(&tmp, t).is_err() {
            self.dst.remove_file(t)?;
            self.dst.rename(&tmp, t)?;
        }
        if self.mv && !self.src.same(self.dst) {
            self.src.delete(s)?;
        }
        Ok(())
    }

    fn copy_tree(&self, s: &Path, t: &Path, is_dir: bool) -> Result<()> {
        if !is_dir {
            return self.copy_file(s, t);
        }
        self.dst.mkdir(t)?;
        for name in self.src.names(s)? {
            let (s, t) = (s.join(&name), t.join(&name));
            if self.p.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if self.skip_link(&s)? {
                continue;
            }
            let is_dir = self.src.stat(&s)?.is_some_and(|e| e.is_dir);
            self.copy_tree(&s, &t, is_dir)?;
        }
        Ok(())
    }

    /// Chunked, cancellable. Leaves a partial `t` on error; callers remove it.
    fn copy_file(&self, s: &Path, t: &Path) -> Result<()> {
        if let Ok(mut cur) = self.p.current.lock() {
            cur.clear();
            cur.push(s);
        }
        let p = self.p;
        if self.src.same(self.dst) && self.src.remote().is_some() {
            // one connection cannot stream to itself: stage in a local temp file
            let mut tmp = tempfile::tempfile().map_err(io_err(s))?;
            self.src
                .read(s, &mut |r| pump(r, &mut tmp, s, t, &Progress::default()))?;
            tmp.rewind().map_err(io_err(s))?;
            self.dst.write_new(t, &mut |w| pump(&mut tmp, w, s, t, p))?;
        } else {
            self.src.read(s, &mut |r| {
                self.dst.write_new(t, &mut |w| pump(r, w, s, t, p))
            })?;
        }
        p.files.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl Side {
    fn delete_empty_dir(&self, p: &Path) -> Result<()> {
        match self {
            Side::Remote(r) => r.remove_dir(&rpath(p)),
            Side::Local => fs::remove_dir(p).map_err(io_err(p)),
        }
    }
}

fn pump(r: &mut dyn Read, w: &mut dyn Write, s: &Path, t: &Path, p: &Progress) -> Result<()> {
    let mut buf = vec![0; CHUNK];
    loop {
        if p.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = r.read(&mut buf).map_err(io_err(s))?;
        if n == 0 {
            return w.flush().map_err(io_err(t));
        }
        w.write_all(&buf[..n]).map_err(io_err(t))?;
        p.done.fetch_add(n as u64, Ordering::Relaxed);
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

#[cfg(any(test, feature = "mem"))]
pub mod mem {
    //! In-memory [`Remote`] for tests.
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::SystemTime;

    #[derive(Debug, Clone)]
    enum Node {
        Dir,
        File(Vec<u8>, SystemTime),
        /// Symlink to this absolute path.
        Link(String),
    }

    #[derive(Debug, Default)]
    pub struct Mem(Mutex<BTreeMap<String, Node>>);

    fn missing(p: &str) -> Error {
        io_err(p)(io::ErrorKind::NotFound.into())
    }

    fn parent(p: &str) -> &str {
        match p.rsplit_once('/') {
            Some(("", _)) | None => "/",
            Some((d, _)) => d,
        }
    }

    impl Mem {
        pub fn new() -> Arc<Self> {
            let m = Self::default();
            m.0.lock().unwrap().insert("/".into(), Node::Dir);
            Arc::new(m)
        }

        pub fn put(&self, p: &str, data: &[u8]) {
            let t = SystemTime::now();
            self.0
                .lock()
                .unwrap()
                .insert(p.into(), Node::File(data.into(), t));
        }

        pub fn dir(&self, p: &str) {
            self.0.lock().unwrap().insert(p.into(), Node::Dir);
        }

        pub fn link(&self, p: &str, target: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(p.into(), Node::Link(target.into()));
        }

        /// File content, through one symlink.
        pub fn get(&self, p: &str) -> Option<Vec<u8>> {
            let m = self.0.lock().unwrap();
            let node = match m.get(p)? {
                Node::Link(t) => m.get(t)?,
                n => n,
            };
            match node {
                Node::File(d, _) => Some(d.clone()),
                _ => None,
            }
        }

        pub fn paths(&self) -> Vec<String> {
            self.0.lock().unwrap().keys().cloned().collect()
        }

        /// `follow`: a link reads as its target (dangling: as the link).
        fn entry(m: &BTreeMap<String, Node>, name: &str, n: &Node, follow: bool) -> Entry {
            let target = match n {
                Node::Link(t) if follow => m.get(t).filter(|t| !matches!(t, Node::Link(_))),
                _ => None,
            };
            let shown = target.unwrap_or(n);
            Entry {
                name: name.into(),
                is_dir: matches!(shown, Node::Dir),
                is_symlink: matches!(n, Node::Link(_)) && target.is_none(),
                size: match shown {
                    Node::File(d, _) => d.len() as u64,
                    _ => 0,
                },
                modified: match shown {
                    Node::File(_, t) => Some(*t),
                    _ => None,
                },
                link: None,
            }
        }
    }

    impl Remote for Mem {
        fn url(&self) -> &str {
            "mem://"
        }

        fn list(&self, dir: &str) -> Result<Vec<Entry>> {
            let m = self.0.lock().unwrap();
            if !matches!(m.get(dir), Some(Node::Dir)) {
                return Err(missing(dir));
            }
            Ok(m.iter()
                .filter(|(k, _)| k.as_str() != "/" && parent(k) == dir)
                .map(|(k, n)| Self::entry(&m, k.rsplit('/').next().unwrap_or(k), n, false))
                .collect())
        }

        fn stat(&self, path: &str) -> Result<Option<Entry>> {
            let m = self.0.lock().unwrap();
            let name = path.rsplit('/').next().unwrap_or(path);
            Ok(m.get(path).map(|n| Self::entry(&m, name, n, true)))
        }

        fn lstat(&self, path: &str) -> Result<Option<Entry>> {
            let m = self.0.lock().unwrap();
            let name = path.rsplit('/').next().unwrap_or(path);
            Ok(m.get(path).map(|n| Self::entry(&m, name, n, false)))
        }

        fn read(&self, path: &str, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
            let data = self.get(path).ok_or_else(|| missing(path))?;
            // lock held during the stream, like FTP/SMB
            let _guard = self.0.lock().unwrap();
            f(&mut data.as_slice())
        }

        fn write_new(
            &self,
            path: &str,
            f: &mut dyn FnMut(&mut dyn Write) -> Result<()>,
        ) -> Result<()> {
            if self.stat(path)?.is_some() {
                return Err(io_err(path)(io::ErrorKind::AlreadyExists.into()));
            }
            self.put(path, b"");
            let mut buf = Vec::new();
            let r = f(&mut buf);
            self.put(path, &buf);
            r
        }

        fn mkdir(&self, path: &str) -> Result<()> {
            if self.stat(path)?.is_some() {
                return Err(io_err(path)(io::ErrorKind::AlreadyExists.into()));
            }
            self.dir(path);
            Ok(())
        }

        fn remove_file(&self, path: &str) -> Result<()> {
            let mut m = self.0.lock().unwrap();
            match m.get(path) {
                Some(Node::File(..) | Node::Link(_)) => {
                    m.remove(path).map(drop).ok_or_else(|| missing(path))
                }
                _ => Err(missing(path)),
            }
        }

        fn remove_dir(&self, path: &str) -> Result<()> {
            let mut m = self.0.lock().unwrap();
            if m.keys().any(|k| parent(k) == path && k != "/") {
                return Err(Error::Remote("directory not empty".into()));
            }
            m.remove(path).map(drop).ok_or_else(|| missing(path))
        }

        fn rename(&self, from: &str, to: &str) -> Result<()> {
            let mut m = self.0.lock().unwrap();
            if m.contains_key(to) {
                return Err(io_err(to)(io::ErrorKind::AlreadyExists.into()));
            }
            let moved: Vec<String> = m
                .keys()
                .filter(|k| *k == from || k.starts_with(&format!("{from}/")))
                .cloned()
                .collect();
            if moved.is_empty() {
                return Err(missing(from));
            }
            for k in moved {
                let n = m.remove(&k).unwrap();
                m.insert(format!("{to}{}", &k[from.len()..]), n);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mem::Mem;
    use super::*;

    fn go(src: &Side, s: &str, dst: &Side, t: &str, mv: bool, d: Decision) -> Result<()> {
        let p = Progress::default();
        transfer(
            src,
            Path::new(s),
            dst,
            Path::new(t),
            mv,
            &mut |_, _| d.clone(),
            &p,
        )
    }

    #[test]
    fn upload_download_tree_and_move() {
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path();
        fs::create_dir(local.join("d")).unwrap();
        fs::write(local.join("d/a"), "a").unwrap();
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        let l = local.to_str().unwrap();
        go(
            &Side::Local,
            &format!("{l}/d"),
            &r,
            "/d",
            false,
            Decision::Cancel,
        )
        .unwrap();
        assert_eq!(m.get("/d/a").unwrap(), b"a");
        go(
            &r,
            "/d",
            &Side::Local,
            &format!("{l}/e"),
            true,
            Decision::Cancel,
        )
        .unwrap();
        assert_eq!(fs::read(local.join("e/a")).unwrap(), b"a");
        assert_eq!(m.paths(), ["/"]);
    }

    #[test]
    fn conflicts_skip_overwrite_rename_mismatch() {
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a");
        fs::write(&src, "new").unwrap();
        let s = src.to_str().unwrap();
        m.put("/a", b"old");
        go(&Side::Local, s, &r, "/a", false, Decision::Skip).unwrap();
        assert_eq!(m.get("/a").unwrap(), b"old");
        assert!(matches!(
            go(&Side::Local, s, &r, "/a", false, Decision::Cancel),
            Err(Error::Cancelled)
        ));
        go(&Side::Local, s, &r, "/a", false, Decision::Overwrite).unwrap();
        assert_eq!(m.get("/a").unwrap(), b"new");
        go(
            &Side::Local,
            s,
            &r,
            "/a",
            false,
            Decision::Rename("b".into()),
        )
        .unwrap();
        assert_eq!(m.get("/b").unwrap(), b"new");
        assert_eq!(m.paths(), ["/", "/a", "/b"]); // no temp left
        m.dir("/d");
        assert!(matches!(
            go(&Side::Local, s, &r, "/d", false, Decision::Overwrite),
            Err(Error::TypeMismatch(_))
        ));
    }

    #[test]
    fn same_remote_move_renames_and_copy_stages() {
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        m.dir("/d");
        m.put("/d/a", b"a");
        go(&r, "/d", &r, "/e", false, Decision::Cancel).unwrap();
        assert_eq!(m.get("/e/a").unwrap(), b"a");
        go(&r, "/e", &r, "/f", true, Decision::Cancel).unwrap();
        assert_eq!(m.paths(), ["/", "/d", "/d/a", "/f", "/f/a"]);
        assert!(matches!(
            go(&r, "/d", &r, "/d/x", false, Decision::Cancel),
            Err(Error::IntoItself(_))
        ));
        m.put("/f/a", b"old");
        go(&r, "/d/a", &r, "/f/a", true, Decision::Overwrite).unwrap();
        assert_eq!(m.get("/f/a").unwrap(), b"a");
        assert!(m.get("/d/a").is_none());
    }

    /// Cancel during a 2 GiB sparse upload: never read whole, no partial remote file.
    #[test]
    fn cancel_mid_upload_leaves_no_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big");
        fs::File::create(&big).unwrap().set_len(2 << 30).unwrap();
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        let p = Progress::default();
        let res = std::thread::scope(|sc| {
            sc.spawn(|| {
                while p.done.load(Ordering::Relaxed) == 0 {
                    std::thread::yield_now();
                }
                p.cancel.store(true, Ordering::Relaxed);
            });
            transfer(
                &Side::Local,
                &big,
                &r,
                Path::new("/big"),
                false,
                &mut |_, _| Decision::Skip,
                &p,
            )
        });
        assert!(matches!(res, Err(Error::Cancelled)));
        assert!(p.done.load(Ordering::Relaxed) < 2 << 30);
        assert_eq!(m.paths(), ["/"]);
    }

    /// Deleting a symlink removes only the link; a link loop inside a tree does not recurse.
    #[test]
    fn delete_symlink_keeps_target() {
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        m.dir("/t");
        m.put("/t/f", b"keep");
        m.link("/t/self", "/t");
        m.link("/l", "/t");
        r.delete(Path::new("/l")).unwrap();
        assert_eq!(m.get("/t/f").unwrap(), b"keep");
        r.delete(Path::new("/t")).unwrap();
        assert_eq!(m.paths(), ["/"]);
    }

    /// A link loop inside a copied tree is skipped and reported, not followed forever;
    /// a move then keeps the source. File links are copied as their content.
    #[test]
    fn copy_skips_symlinked_dirs() {
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        m.dir("/t");
        m.put("/t/f", b"x");
        m.link("/t/self", "/t");
        m.link("/t/fl", "/t/f");
        let p = Progress::default();
        scan(&r, &[PathBuf::from("/t")], &p);
        assert_eq!(p.files_total.load(Ordering::Relaxed), 2);
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("t");
        for mv in [false, true] {
            let _ = fs::remove_dir_all(&dst);
            let res = go(
                &r,
                "/t",
                &Side::Local,
                dst.to_str().unwrap(),
                mv,
                Decision::Cancel,
            );
            assert!(
                matches!(&res, Err(Error::Remote(m)) if m.contains("/t/self")),
                "{res:?}"
            );
            assert_eq!(fs::read(dst.join("f")).unwrap(), b"x");
            assert_eq!(fs::read(dst.join("fl")).unwrap(), b"x");
            assert!(!dst.join("self").exists());
            assert_eq!(m.get("/t/f").unwrap(), b"x"); // move kept its source
        }
    }

    #[cfg(unix)]
    #[test]
    fn upload_skips_local_link_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("d");
        fs::create_dir(&d).unwrap();
        fs::write(d.join("f"), "x").unwrap();
        std::os::unix::fs::symlink(&d, d.join("loop")).unwrap();
        let m = Mem::new();
        let res = go(
            &Side::Local,
            d.to_str().unwrap(),
            &Side::Remote(m.clone()),
            "/d",
            false,
            Decision::Cancel,
        );
        assert!(matches!(res, Err(Error::Remote(_))));
        assert_eq!(m.paths(), ["/", "/d", "/d/f"]);
    }

    #[test]
    fn scan_and_delete_remote_tree() {
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        m.dir("/d");
        m.put("/d/a", b"abc");
        m.put("/d/b", b"de");
        let p = Progress::default();
        scan(&r, &[PathBuf::from("/d")], &p);
        assert_eq!(p.total.load(Ordering::Relaxed), 5);
        assert_eq!(p.files_total.load(Ordering::Relaxed), 2);
        r.delete(Path::new("/d")).unwrap();
        assert_eq!(m.paths(), ["/"]);
    }
}
