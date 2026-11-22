use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::{CHUNK, Side, rpath};
use crate::ops::{self, Ask, Decision, Progress};
use crate::{Error, Result, io_err};

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
        let tmp = self.dst.free_temp(t)?;
        let moved = self.mv && self.src.same(self.dst);
        let staged = if moved {
            self.dst.rename(s, &tmp)
        } else {
            self.copy_file(s, &tmp).inspect_err(|_| {
                let _ = self.dst.delete(&tmp);
            })
        };
        staged?;
        if let Err(e) = self.dst.rename(&tmp, t) {
            // remote renames do not replace: drop the old file only once the new one is complete
            if !self.dst.exists(t).unwrap_or(false) {
                return Err(self.unstage(&tmp, moved, e));
            }
            if let Err(e) = self.dst.remove_file(t) {
                return Err(self.unstage(&tmp, moved, e));
            }
            // the old file is gone: `tmp` holds the only copy of the new one
            self.dst
                .rename(&tmp, t)
                .map_err(|e| self.unstage(&tmp, true, e))?;
        }
        if self.mv && !self.src.same(self.dst) {
            self.src.delete(s)?;
        }
        Ok(())
    }

    /// After a failed swap: a copied `tmp` is dropped while the old target is intact, else it
    /// is kept and named in the error.
    fn unstage(&self, tmp: &Path, keep: bool, e: Error) -> Error {
        if !keep && self.dst.delete(tmp).is_ok() {
            return e;
        }
        Error::Remote(format!("{e}; the new content is kept as {}", tmp.display()))
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
    /// Something (also a dangling symlink) is at `p`.
    pub(crate) fn exists(&self, p: &Path) -> Result<bool> {
        match self {
            Side::Remote(r) => Ok(r.lstat(&rpath(p))?.is_some()),
            Side::Local => match fs::symlink_metadata(p) {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(io_err(p)(e)),
            },
        }
    }

    /// Unused `.tc-tmp-[<n>-]<name>` next to `t`, so a failed transfer never deletes a user's file.
    // shortcut: a file created there between this check and the exclusive create would be
    // removed on failure, upgrade if remote backends report AlreadyExists distinctly
    pub(crate) fn free_temp(&self, t: &Path) -> Result<PathBuf> {
        let name = t.file_name().unwrap_or_default();
        for n in 0..100 {
            let mut tmp = OsString::from(".tc-tmp-");
            if n > 0 {
                tmp.push(format!("{n}-"));
            }
            tmp.push(name);
            let tmp = t.with_file_name(tmp);
            if !self.exists(&tmp)? {
                return Ok(tmp);
            }
        }
        Err(io_err(t)(io::ErrorKind::AlreadyExists.into()))
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::mem::Mem;

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
    fn overwrite_spares_foreign_temp_names_and_keeps_new_content_on_failed_swap() {
        let m = Mem::new();
        let r = Side::Remote(m.clone());
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a");
        fs::write(&src, "new").unwrap();
        let s = src.to_str().unwrap();
        m.put("/a", b"old");
        m.put("/.tc-tmp-a", b"mine");
        go(&Side::Local, s, &r, "/a", false, Decision::Overwrite).unwrap();
        assert_eq!(m.get("/a").unwrap(), b"new");
        assert_eq!(m.paths(), ["/", "/.tc-tmp-a", "/a"]);
        assert_eq!(m.get("/.tc-tmp-a").unwrap(), b"mine");
        // the swap fails after the old file is removed: the new content survives in the temp
        m.fail_renames_to("/a");
        let e = go(&Side::Local, s, &r, "/a", false, Decision::Overwrite)
            .unwrap_err()
            .to_string();
        assert!(e.contains("kept as /.tc-tmp-1-a"), "{e}");
        assert_eq!(m.get("/.tc-tmp-1-a").unwrap(), b"new");
        assert_eq!(m.get("/.tc-tmp-a").unwrap(), b"mine");
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
                matches!(&res, Err(Error::Remote(m)) if m.contains("self")),
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

    #[test]
    fn stats_scan_remote_tree_skips_links() {
        let m = Mem::new();
        m.dir("/d");
        m.dir("/d/sub");
        m.put("/d/sub/a", b"abc");
        m.put("/d/b", b"de");
        m.link("/d/loop", "/d");
        let root = crate::stats::scan_remote(m.as_ref(), Path::new("/d"), &Progress::default());
        assert_eq!(
            (root.name.to_str(), root.size, root.files),
            (Some("d"), 5, 3)
        );
        assert_eq!(root.children[0].name, "sub");
        assert_eq!((root.children[0].size, root.children[0].files), (3, 1));
    }
}
