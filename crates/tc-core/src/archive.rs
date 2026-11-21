//! Read-only archive browsing: zip, tar (plain, gz, xz) and single `.xz` files.
//!
//! The entry list is read once; file data is only streamed out on demand. Entry names are
//! sanitized when listed (absolute paths and `..` are dropped), only regular files and
//! directories are ever written, so extraction cannot leave its target (zip-slip).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use tempfile::TempDir;

use crate::panel::Entry;
use crate::{Error, Result, io_err};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    None,
    Gz,
    Xz,
}

/// Supported archive kinds; a new backend (SSH, FTP) would add its own listing and extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar(Codec),
    /// One compressed file.
    Xz,
}

impl Format {
    /// By file name extension.
    pub fn detect(name: &Path) -> Option<Self> {
        let n = name.file_name()?.to_string_lossy().to_lowercase();
        let ends = |s: &[&str]| s.iter().any(|s| n.ends_with(s));
        Some(if ends(&[".zip"]) {
            Self::Zip
        } else if ends(&[".tar.gz", ".tgz"]) {
            Self::Tar(Codec::Gz)
        } else if ends(&[".tar.xz", ".txz"]) {
            Self::Tar(Codec::Xz)
        } else if ends(&[".tar"]) {
            Self::Tar(Codec::None)
        } else if ends(&[".xz"]) {
            Self::Xz
        } else {
            return None;
        })
    }
}

#[derive(Debug)]
pub struct Item {
    /// Sanitized, `/` separated, no leading or trailing slash.
    pub path: String,
    /// Position in the archive, used to find the entry again when extracting.
    index: usize,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub is_dir: bool,
    /// Symlink or hard link: listed, never extracted.
    pub link: bool,
}

#[derive(Debug)]
pub struct Archive {
    pub file: PathBuf,
    format: Format,
    pub items: Vec<Item>,
}

fn err(e: impl ToString) -> Error {
    Error::Archive(e.to_string())
}

/// Counts compressed bytes read, as listing progress.
struct Counting<'a, R>(R, &'a AtomicU64);

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.0.read(buf)?;
        self.1.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// `None` for names that are empty, absolute or climb out with `..`.
fn sanitize(raw: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(raw);
    if s.starts_with('/') || s.contains('\0') {
        return None;
    }
    let mut parts = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(c),
        }
    }
    let p = parts.join("/");
    let ok = !p.is_empty()
        && Path::new(&p)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    ok.then_some(p)
}

/// Zip stores local time without zone; shown as if UTC.
// shortcut: zone ignored, upgrade if mtimes off by hours matter
fn dos_time(d: zip::DateTime) -> Option<SystemTime> {
    let (y, m, day) = (
        i64::from(d.year()),
        i64::from(d.month()),
        i64::from(d.day()),
    );
    let y = if m <= 2 { y - 1 } else { y };
    let (era, yoe) = (y.div_euclid(400), y.rem_euclid(400));
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let secs = days * 86_400
        + i64::from(d.hour()) * 3600
        + i64::from(d.minute()) * 60
        + i64::from(d.second());
    UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(secs).ok()?))
}

fn unix_time(secs: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

fn copy_capped(
    r: &mut impl Read,
    w: &mut impl Write,
    limit: u64,
    progress: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut buf = vec![0; 64 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = r.read(&mut buf).map_err(err)?;
        if n == 0 {
            return Ok(());
        }
        total += n as u64;
        if total > limit {
            return Err(err("entry too large"));
        }
        w.write_all(&buf[..n]).map_err(err)?;
        progress.fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// Stream `r` into a new file at `target` (never overwrites); a failure removes the partial file.
fn write_file(
    mut r: impl Read,
    target: &Path,
    modified: Option<SystemTime>,
    limit: u64,
    progress: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let mut f = File::create_new(target).map_err(io_err(target))?;
    let res = copy_capped(&mut r, &mut f, limit, progress, cancel);
    if res.is_err() {
        drop(f);
        let _ = fs::remove_file(target);
    } else if let Some(t) = modified {
        let _ = f.set_modified(t);
    }
    res
}

impl Archive {
    fn stream<'a>(&self, codec: Codec, p: &'a AtomicU64) -> Result<Box<dyn Read + 'a>> {
        let f = Counting(File::open(&self.file).map_err(io_err(&self.file))?, p);
        let f = BufReader::new(f);
        Ok(match codec {
            Codec::None => Box::new(f),
            Codec::Gz => Box::new(flate2::bufread::MultiGzDecoder::new(f)),
            Codec::Xz => Box::new(lzma_rust2::XzReader::new(f, true)),
        })
    }

    /// Read the entry list; `progress` counts compressed bytes read (tar, xz).
    pub fn open(file: &Path, progress: &AtomicU64, cancel: &AtomicBool) -> Result<Self> {
        let format = Format::detect(file).ok_or_else(|| err("unknown archive type"))?;
        let mut a = Self {
            file: file.to_owned(),
            format,
            items: Vec::new(),
        };
        a.items = match format {
            Format::Zip => a.list_zip()?,
            Format::Tar(c) => a.list_tar(c, progress, cancel)?,
            Format::Xz => {
                let name = file.file_stem().unwrap_or_default().to_string_lossy();
                let path = sanitize(name.as_bytes()).ok_or_else(|| err("bad file name"))?;
                let modified = fs::metadata(file).and_then(|m| m.modified()).ok();
                // size inside is unknown without decompressing everything
                vec![Item {
                    path,
                    index: 0,
                    size: 0,
                    modified,
                    is_dir: false,
                    link: false,
                }]
            }
        };
        Ok(a)
    }

    fn list_zip(&self) -> Result<Vec<Item>> {
        let f = File::open(&self.file).map_err(io_err(&self.file))?;
        let mut z = zip::ZipArchive::new(BufReader::new(f)).map_err(err)?;
        let mut items = Vec::with_capacity(z.len());
        for index in 0..z.len() {
            let e = z.by_index_raw(index).map_err(err)?;
            if let Some(path) = sanitize(e.name_raw()) {
                items.push(Item {
                    path,
                    index,
                    size: e.size(),
                    modified: e.last_modified().and_then(dos_time),
                    is_dir: e.is_dir(),
                    link: e.is_symlink(),
                });
            }
        }
        Ok(items)
    }

    // shortcut: tar.gz and tar.xz must decompress the whole stream to list, upgrade with a cache if reopening is slow
    fn list_tar(&self, c: Codec, progress: &AtomicU64, cancel: &AtomicBool) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        let mut tar = tar::Archive::new(self.stream(c, progress)?);
        for (index, e) in tar.entries().map_err(err)?.enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let e = e.map_err(err)?;
            let (h, t) = (e.header(), e.header().entry_type());
            let link = t.is_symlink() || t.is_hard_link();
            // pax headers, devices etc. are not shown
            if !(t.is_dir() || t.is_file() || link) {
                continue;
            }
            if let Some(path) = sanitize(&e.path_bytes()) {
                items.push(Item {
                    path,
                    index,
                    size: h.size().unwrap_or(0),
                    modified: h.mtime().ok().and_then(unix_time),
                    is_dir: t.is_dir(),
                    link,
                });
            }
        }
        Ok(items)
    }

    /// Directory `sub` (`""` = root) as panel entries, unsorted, without "..".
    /// Directories missing as entries of their own are made up from the paths below them.
    pub fn list(&self, sub: &str) -> Vec<Entry> {
        let mut map: HashMap<&str, Entry> = HashMap::new();
        for it in &self.items {
            let rest = match sub {
                "" => it.path.as_str(),
                _ => match it.path.strip_prefix(sub).and_then(|r| r.strip_prefix('/')) {
                    Some(r) => r,
                    None => continue,
                },
            };
            let (name, nested) = rest
                .split_once('/')
                .map_or((rest, false), |(n, _)| (n, true));
            let dir = nested || it.is_dir;
            let e = map.entry(name).or_insert_with(|| Entry {
                name: name.into(),
                is_dir: dir,
                is_symlink: !dir && it.link,
                size: if dir { 0 } else { it.size },
                modified: it.modified,
                link: None,
            });
            if dir && !nested {
                e.modified = it.modified;
            }
        }
        map.into_values().collect()
    }

    /// In-archive path of a panel path below the archive file, e.g. `x.zip/a/b` gives `a/b`.
    pub fn inner_of(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.file).ok()?;
        let parts: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect();
        Some(parts.join("/"))
    }

    /// Write the entry `inner` (file, or directory with everything below) to `dest`, which must
    /// not exist yet. Files bigger than `limit` fail; `progress` counts bytes written.
    pub fn extract(
        &self,
        inner: &str,
        dest: &Path,
        limit: u64,
        progress: &AtomicU64,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let mut files: HashMap<usize, (PathBuf, &Item)> = HashMap::new();
        let mut any = false;
        for it in &self.items {
            let target = if it.path == inner {
                dest.to_owned()
            } else if let Some(r) = it
                .path
                .strip_prefix(inner)
                .and_then(|r| r.strip_prefix('/'))
            {
                dest.join(r)
            } else {
                continue;
            };
            any = true;
            if it.is_dir {
                fs::create_dir_all(&target).map_err(io_err(&target))?;
            } else if !it.link {
                files.insert(it.index, (target, it));
            } else if it.path == inner {
                return Err(err("links are not extracted"));
            }
        }
        if !any {
            return Err(err(format!("{inner}: not in archive")));
        }
        let put = |r: &mut dyn Read, (target, it): &(PathBuf, &Item)| {
            write_file(r, target, it.modified, limit, progress, cancel)
        };
        let sink = AtomicU64::new(0);
        match self.format {
            Format::Zip => {
                let f = File::open(&self.file).map_err(io_err(&self.file))?;
                let mut z = zip::ZipArchive::new(BufReader::new(f)).map_err(err)?;
                let mut order: Vec<_> = files.keys().copied().collect();
                order.sort_unstable();
                for i in order {
                    put(&mut z.by_index(i).map_err(err)?, &files[&i])?;
                }
            }
            Format::Tar(c) => {
                let mut left = files.len();
                let mut tar = tar::Archive::new(self.stream(c, &sink)?);
                for (i, e) in tar.entries().map_err(err)?.enumerate() {
                    if left == 0 {
                        break;
                    }
                    let mut e = e.map_err(err)?;
                    if let Some(f) = files.get(&i) {
                        put(&mut e, f)?;
                        left -= 1;
                    }
                }
            }
            Format::Xz => {
                let mut r = self.stream(Codec::Xz, &sink)?;
                if let Some(f) = files.get(&0) {
                    put(&mut r, f)?;
                }
            }
        }
        Ok(())
    }

    /// Extract one entry into a fresh temp dir; returns the dir (delete on drop) and the file in it.
    pub fn extract_temp(
        &self,
        inner: &str,
        limit: u64,
        progress: &AtomicU64,
        cancel: &AtomicBool,
    ) -> Result<(TempDir, PathBuf)> {
        let tmp = tempfile::tempdir().map_err(err)?;
        let name = inner.rsplit('/').next().unwrap_or(inner);
        let file = tmp.path().join(name);
        self.extract(inner, &file, limit, progress, cancel)?;
        Ok((tmp, file))
    }

    /// Extract the panel paths `srcs` into one temp dir, each keeping its name; returns the
    /// extracted paths in the same order.
    pub fn extract_all(
        &self,
        srcs: &[PathBuf],
        cancel: &AtomicBool,
    ) -> Result<(TempDir, Vec<PathBuf>)> {
        let tmp = tempfile::tempdir().map_err(err)?;
        let progress = AtomicU64::new(0);
        let mut out = Vec::new();
        for (n, src) in srcs.iter().enumerate() {
            let inner = self.inner_of(src).ok_or_else(|| err("not in archive"))?;
            let name = src.file_name().ok_or_else(|| err("no name"))?;
            let dest = tmp.path().join(n.to_string()).join(name);
            self.extract(&inner, &dest, u64::MAX, &progress, cancel)?;
            out.push(dest);
        }
        Ok((tmp, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no() -> (AtomicU64, AtomicBool) {
        (AtomicU64::new(0), AtomicBool::new(false))
    }

    fn open(p: &Path) -> Archive {
        let (a, b) = no();
        Archive::open(p, &a, &b).unwrap()
    }

    fn names(a: &Archive, sub: &str) -> Vec<String> {
        let mut v: Vec<_> = a
            .list(sub)
            .iter()
            .map(|e| {
                format!(
                    "{}{}",
                    e.name.to_string_lossy(),
                    if e.is_dir { "/" } else { "" }
                )
            })
            .collect();
        v.sort();
        v
    }

    /// Raw tar bytes with a normal tree, a zip-slip name, an absolute name and a non-UTF-8 name.
    fn tar_bytes() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut add = |name: &[u8], data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mtime(1_700_000_000);
            h.set_mode(0o644);
            // raw name bytes: set_path would refuse `..`
            h.as_old_mut().name[..name.len()].copy_from_slice(name);
            h.set_cksum();
            b.append(&h, data).unwrap();
        };
        add(b"top.txt", b"top");
        add(b"d/e/deep.txt", b"deep");
        add(b"../evil", b"x");
        add(b"/abs", b"x");
        add(b"bad\xff.txt", b"raw");
        b.into_inner().unwrap()
    }

    fn check(a: &Archive) {
        assert_eq!(names(a, ""), ["bad\u{fffd}.txt", "d/", "top.txt"]);
        assert_eq!(names(a, "d"), ["e/"]); // d itself is implied
        assert_eq!(names(a, "d/e"), ["deep.txt"]);
        let top = a
            .list("")
            .into_iter()
            .find(|e| e.name == "top.txt")
            .unwrap();
        assert_eq!((top.size, top.modified), (3, unix_time(1_700_000_000)));
        let tmp = tempfile::tempdir().unwrap();
        let (p, c) = no();
        let out = tmp.path().join("out");
        a.extract("d", &out, u64::MAX, &p, &c).unwrap();
        assert_eq!(fs::read(out.join("e/deep.txt")).unwrap(), b"deep");
        let (_t, f) = a.extract_temp("top.txt", 10, &p, &c).unwrap();
        assert_eq!(fs::read(f).unwrap(), b"top");
        assert!(a.extract_temp("top.txt", 2, &p, &c).is_err()); // over the cap
        assert!(a.extract("../evil", &out, u64::MAX, &p, &c).is_err());
        assert!(!tmp.path().join("evil").exists());
    }

    #[test]
    fn tar_plain_gz_and_xz() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = tar_bytes();
        let plain = tmp.path().join("a.tar");
        fs::write(&plain, &raw).unwrap();
        check(&open(&plain));

        let gz = tmp.path().join("a.tgz");
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(&raw).unwrap();
        fs::write(&gz, e.finish().unwrap()).unwrap();
        check(&open(&gz));

        let xz = tmp.path().join("a.tar.xz");
        let mut w =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        w.write_all(&raw).unwrap();
        fs::write(&xz, w.finish().unwrap()).unwrap();
        check(&open(&xz));
    }

    #[test]
    fn plain_xz() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("log.txt.xz");
        let mut w =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        w.write_all(b"hello").unwrap();
        fs::write(&p, w.finish().unwrap()).unwrap();
        let a = open(&p);
        assert_eq!(names(&a, ""), ["log.txt"]);
        let (p, c) = no();
        let (_t, f) = a.extract_temp("log.txt", u64::MAX, &p, &c).unwrap();
        assert_eq!(fs::read(f).unwrap(), b"hello");
    }

    #[test]
    fn zip_with_slip_names() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.zip");
        let mut z = zip::ZipWriter::new(File::create(&p).unwrap());
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(zip::DateTime::from_date_and_time(2024, 1, 1, 0, 0, 0).unwrap());
        for (n, d) in [
            ("top.txt", "top"),
            ("d/e/deep.txt", "deep"),
            ("../evil", "x"),
            ("/abs", "x"),
        ] {
            z.start_file(n, o).unwrap();
            z.write_all(d.as_bytes()).unwrap();
        }
        z.add_directory("empty/", o).unwrap();
        z.finish().unwrap();
        let a = open(&p);
        assert_eq!(names(&a, ""), ["d/", "empty/", "top.txt"]);
        assert_eq!(names(&a, "d/e"), ["deep.txt"]);
        let top = a
            .list("")
            .into_iter()
            .find(|e| e.name == "top.txt")
            .unwrap();
        assert_eq!(top.modified, unix_time(1_704_067_200));
        let (pr, c) = no();
        let (_t, f) = a.extract_temp("d/e/deep.txt", u64::MAX, &pr, &c).unwrap();
        assert_eq!(fs::read(f).unwrap(), b"deep");
        let out = tmp.path().join("out");
        a.extract("empty", &out, u64::MAX, &pr, &c).unwrap();
        assert!(out.is_dir());
        assert_eq!(a.inner_of(&p.join("d/e")).as_deref(), Some("d/e"));
    }

    #[test]
    fn sanitize_rules() {
        assert_eq!(sanitize(b"./a//b/").as_deref(), Some("a/b"));
        for bad in [&b"../x"[..], b"a/../x", b"/etc/x", b"", b"./", b"a\0b"] {
            assert_eq!(sanitize(bad), None);
        }
    }

    #[test]
    fn detect() {
        let d = |n: &str| Format::detect(Path::new(n));
        assert_eq!(d("A.TAR.GZ"), Some(Format::Tar(Codec::Gz)));
        assert_eq!(d("x.txz"), Some(Format::Tar(Codec::Xz)));
        assert_eq!(d("x.xz"), Some(Format::Xz));
        assert_eq!(d("x.txt"), None);
    }
}
