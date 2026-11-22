//! In-memory [`Remote`] for tests.
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use super::Remote;
use crate::panel::Entry;
use crate::{Error, Result, io_err};

#[derive(Debug, Clone)]
enum Node {
    Dir,
    File(Vec<u8>, SystemTime),
    /// Symlink to this absolute path.
    Link(String),
}

/// Tree, and a path every rename to fails (fault injection).
#[derive(Debug, Default)]
pub struct Mem(Mutex<BTreeMap<String, Node>>, Mutex<Option<String>>);

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

    /// Every later rename to `p` fails.
    pub fn fail_renames_to(&self, p: &str) {
        *self.1.lock().unwrap() = Some(p.into());
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

    fn write_new(&self, path: &str, f: &mut dyn FnMut(&mut dyn Write) -> Result<()>) -> Result<()> {
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
        if self.1.lock().unwrap().as_deref() == Some(to) {
            return Err(Error::Remote("injected rename failure".into()));
        }
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
