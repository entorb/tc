//! Remote file system backends for tc: SFTP and FTP/FTPS, behind [`tc_core::vfs::Remote`].

use std::sync::Arc;

use tc_core::vfs::Remote;
use tc_core::{Error, Result};

mod ftp;
mod sftp;

/// Question a connection needs answered by the user; `None` answer cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Unknown SFTP host key: the text shows host and fingerprint, answer `"y"` to trust and save it.
    HostKey(String),
    /// Password for the text shown.
    Password(String),
}

pub type Ask<'a> = &'a mut dyn FnMut(Prompt) -> Option<String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Sftp,
    Ftp,
    Ftps,
}

/// `scheme://[user@]host[:port][/path]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: Scheme,
    pub user: String,
    pub host: String,
    pub port: u16,
    /// Start directory; empty = the server's home.
    pub path: String,
}

impl Url {
    // shortcut: no IPv6 literals ([::1]) and no %-escapes, upgrade when someone needs them
    pub fn parse(s: &str) -> Result<Self> {
        let bad = || Error::Remote(format!("not a remote URL: {s} (sftp://, ftp://, ftps://)"));
        let (scheme, rest) = s.trim().split_once("://").ok_or_else(bad)?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "sftp" | "ssh" => Scheme::Sftp,
            "ftp" => Scheme::Ftp,
            "ftps" => Scheme::Ftps,
            _ => return Err(bad()),
        };
        let (auth, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let (user, hostport) = match auth.rsplit_once('@') {
            Some((u, h)) => (u, h),
            None => ("", auth),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().map_err(|_| bad())?),
            None => (hostport, if scheme == Scheme::Sftp { 22 } else { 21 }),
        };
        if host.is_empty() {
            return Err(bad());
        }
        Ok(Self {
            scheme,
            user: user.into(),
            host: host.into(),
            port,
            path: path.trim_end_matches('/').into(),
        })
    }

    /// Shown before remote paths: `sftp://user@host:22`.
    pub fn base(&self) -> String {
        let scheme = match self.scheme {
            Scheme::Sftp => "sftp",
            Scheme::Ftp => "ftp",
            Scheme::Ftps => "ftps",
        };
        let at = if self.user.is_empty() { "" } else { "@" };
        format!("{scheme}://{}{at}{}:{}", self.user, self.host, self.port)
    }
}

/// Connect and log in; returns the connection and the directory to show first.
pub fn connect(url: &str, ask: Ask) -> Result<(Arc<dyn Remote>, String)> {
    let url = Url::parse(url)?;
    let remote: Arc<dyn Remote> = match url.scheme {
        Scheme::Sftp => Arc::new(sftp::Sftp::connect(&url, ask)?),
        Scheme::Ftp | Scheme::Ftps => ftp::connect(&url, ask)?,
    };
    let dir = if url.path.is_empty() {
        remote.home()
    } else {
        url.path.clone()
    };
    Ok((remote, dir))
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Remote(e.to_string())
}

/// Join a directory and a name with `/`.
fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

/// Parent directory and name of an absolute `/` path.
fn split(path: &str) -> (&str, &str) {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((dir, name)) => (dir, name),
        None => ("/", path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        let u = Url::parse("sftp://me@host.example:2222/srv/data/").unwrap();
        assert_eq!(
            u,
            Url {
                scheme: Scheme::Sftp,
                user: "me".into(),
                host: "host.example".into(),
                port: 2222,
                path: "/srv/data".into(),
            }
        );
        assert_eq!(u.base(), "sftp://me@host.example:2222");
        let u = Url::parse("FTP://ftp.example").unwrap();
        assert_eq!(
            (u.scheme, u.port, u.user.as_str(), u.path.as_str()),
            (Scheme::Ftp, 21, "", "")
        );
        assert_eq!(Url::parse("ftps://a@b@h/").unwrap().user, "a@b");
        for bad in ["host", "http://x", "sftp://", "sftp://h:port"] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn path_helpers() {
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/x/", "a"), "/x/a");
        assert_eq!(split("/a"), ("/", "a"));
        assert_eq!(split("/x/y/a"), ("/x/y", "a"));
    }

    /// Live servers: `TC_TEST_SFTP=sftp://user@host/tmp/dir` (key auth) or `TC_TEST_FTP=ftp://user@host/dir` with `TC_TEST_PASS`.
    #[test]
    #[ignore = "needs a server"]
    fn live_roundtrip() {
        use std::path::Path;
        use tc_core::ops::{Decision, Progress};
        use tc_core::vfs::{Side, transfer};
        for var in ["TC_TEST_SFTP", "TC_TEST_FTP"] {
            let Ok(url) = std::env::var(var) else {
                continue;
            };
            let pass = std::env::var("TC_TEST_PASS").ok();
            let mut ask = |p: Prompt| match p {
                Prompt::HostKey(_) => Some("y".into()),
                Prompt::Password(_) => pass.clone(),
            };
            let (r, dir) = connect(&url, &mut ask).unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("tc-live");
            std::fs::create_dir(&src).unwrap();
            std::fs::write(src.join("a"), "hello").unwrap();
            let side = Side::Remote(r.clone());
            let remote = Path::new(&dir).join("tc-live");
            let p = Progress::default();
            let mut d = |_: &Path, _: &Path| Decision::Overwrite;
            transfer(&Side::Local, &src, &side, &remote, false, &mut d, &p).unwrap();
            let back = tmp.path().join("back");
            transfer(&side, &remote, &Side::Local, &back, true, &mut d, &p).unwrap();
            assert_eq!(std::fs::read_to_string(back.join("a")).unwrap(), "hello");
            assert!(r.stat(&tc_core::vfs::rpath(&remote)).unwrap().is_none());
        }
    }
}
