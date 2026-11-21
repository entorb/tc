//! FTP and explicit FTPS (AUTH TLS, rustls with the webpki roots).

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use suppaftp::list::{File, ListParser};
use suppaftp::types::FileType;
use suppaftp::{FtpStream, ImplFtpStream, RustlsConnector, RustlsFtpStream, TlsStream};
use tc_core::panel::Entry;
use tc_core::vfs::Remote;
use tc_core::{Error, Result};

use crate::{Ask, Prompt, Scheme, Url, err, split};

const TIMEOUT: Duration = Duration::from_secs(15);

pub fn connect(url: &Url, ask: Ask) -> Result<Arc<dyn Remote>> {
    use std::net::ToSocketAddrs;
    let addr = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(err)?
        .next()
        .ok_or_else(|| Error::Remote(format!("{}: host not found", url.host)))?;
    let (user, pass) = if url.user.is_empty() {
        ("anonymous".to_owned(), "anonymous@".to_owned())
    } else {
        let label = format!("Password for {}", url.base());
        let pass = ask(Prompt::Password(label)).ok_or(Error::Cancelled)?;
        (url.user.clone(), pass)
    };
    if url.scheme == Scheme::Ftps {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let ftp = RustlsFtpStream::connect_timeout(addr, TIMEOUT)
            .map_err(err)?
            .into_secure(RustlsConnector::from(Arc::new(config)), &url.host)
            .map_err(err)?;
        Ok(Arc::new(Ftp::login(url, ftp, &user, &pass)?))
    } else {
        let ftp = FtpStream::connect_timeout(addr, TIMEOUT).map_err(err)?;
        Ok(Arc::new(Ftp::login(url, ftp, &user, &pass)?))
    }
}

/// One control connection, used by one operation at a time.
pub struct Ftp<T: TlsStream> {
    base: String,
    home: String,
    conn: Mutex<ImplFtpStream<T>>,
}

impl<T: TlsStream> std::fmt::Debug for Ftp<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.base)
    }
}

impl<T: TlsStream> Ftp<T> {
    fn login(url: &Url, mut ftp: ImplFtpStream<T>, user: &str, pass: &str) -> Result<Self> {
        ftp.login(user, pass).map_err(err)?;
        ftp.transfer_type(FileType::Binary).map_err(err)?;
        let home = ftp.pwd().unwrap_or_else(|_| "/".into());
        Ok(Self {
            base: url.base(),
            home,
            conn: Mutex::new(ftp),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, ImplFtpStream<T>>> {
        self.conn
            .lock()
            .map_err(|_| Error::Remote("connection lost".into()))
    }
}

fn entry(f: &File) -> Entry {
    Entry {
        name: f.name().into(),
        is_dir: f.is_directory(),
        is_symlink: f.is_symlink(),
        size: f.size() as u64,
        modified: Some(f.modified()),
        link: f.symlink().map(Into::into),
    }
}

impl<T: TlsStream + Send + 'static> Remote for Ftp<T> {
    fn url(&self) -> &str {
        &self.base
    }

    fn home(&self) -> String {
        self.home.clone()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let mut c = self.conn()?;
        // MLSD has exact sizes and times; old servers only know LIST
        let (lines, mlsd) = match c.mlsd(Some(dir)) {
            Ok(l) => (l, true),
            Err(_) => (c.list(Some(dir)).map_err(err)?, false),
        };
        Ok(lines
            .iter()
            .filter_map(|l| {
                if mlsd {
                    ListParser::parse_mlsd(l).ok()
                } else {
                    l.parse::<File>().ok()
                }
            })
            .filter(|f| !matches!(f.name(), "." | ".."))
            .map(|f| entry(&f))
            .collect())
    }

    // shortcut: lists the parent directory per call (slow for many small files), upgrade to MLST when supported
    fn stat(&self, path: &str) -> Result<Option<Entry>> {
        let (dir, name) = split(path);
        if name.is_empty() || path == "/" {
            return Ok(Some(Entry {
                name: "/".into(),
                is_dir: true,
                is_symlink: false,
                size: 0,
                modified: None,
                link: None,
            }));
        }
        match self.list(dir) {
            Ok(list) => Ok(list.into_iter().find(|e| e.name == name)),
            Err(_) => Ok(None),
        }
    }

    fn read(&self, path: &str, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
        let mut c = self.conn()?;
        let mut s = c.retr_as_stream(path).map_err(err)?;
        f(&mut s)?;
        s.finish().map_err(err)
    }

    fn write_new(&self, path: &str, f: &mut dyn FnMut(&mut dyn Write) -> Result<()>) -> Result<()> {
        // FTP has no exclusive create: check first (a racing writer could slip in between)
        if self.stat(path)?.is_some() {
            return Err(Error::Io {
                path: path.into(),
                source: io::ErrorKind::AlreadyExists.into(),
            });
        }
        let mut c = self.conn()?;
        let mut s = c.put_with_stream(path).map_err(err)?;
        f(&mut s)?;
        s.finish().map_err(err)
    }

    fn mkdir(&self, path: &str) -> Result<()> {
        self.conn()?.mkdir(path).map_err(err)
    }

    fn remove_file(&self, path: &str) -> Result<()> {
        self.conn()?.rm(path).map_err(err)
    }

    fn remove_dir(&self, path: &str) -> Result<()> {
        self.conn()?.rmdir(path).map_err(err)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        if self.stat(to)?.is_some() {
            return Err(Error::Io {
                path: to.into(),
                source: io::ErrorKind::AlreadyExists.into(),
            });
        }
        self.conn()?.rename(from, to).map_err(err)
    }
}
