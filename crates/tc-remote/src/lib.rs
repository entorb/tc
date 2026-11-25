//! Remote file system backends for tc: SFTP and FTP/FTPS, behind [`tc_core::vfs::Remote`].

use std::process::Command;
use std::sync::Arc;

use tc_core::vfs::Remote;
use tc_core::{Error, Result};

mod ftp;
mod sftp;
pub mod update;

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

/// The system `ssh` running `cmd` in `dir` on the host of `url`, on a terminal (`-t`). Prompts
/// for a password itself if keys or the agent do not log in.
// shortcut: assumes a POSIX shell on the server, upgrade with an exec channel on the russh session
pub fn ssh_command(url: &str, cmd: &str, dir: &str) -> Result<Command> {
    let url = Url::parse(url)?;
    if url.scheme != Scheme::Sftp {
        return Err(Error::Remote(
            "commands run only on SFTP connections".into(),
        ));
    }
    let quoted = tc_core::vfs::sh_quote(dir);
    let mut c = Command::new("ssh");
    c.args(["-t", "-p", &url.port.to_string()]);
    if !url.user.is_empty() {
        c.args(["-l", &url.user]);
    }
    // `--` so a host starting with `-` is no option
    c.args(["--", &url.host, &format!("cd {quoted} && {cmd}")]);
    Ok(c)
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
    use std::path::{Path, PathBuf};

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
    fn ssh_command_quotes_dir() {
        let c = ssh_command("sftp://me@h:2222", "du -sh *", "/it's").unwrap();
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-t",
                "-p",
                "2222",
                "-l",
                "me",
                "--",
                "h",
                r"cd '/it'\''s' && du -sh *"
            ]
        );
        assert!(ssh_command("ftp://h", "ls", "/").is_err());
    }

    #[test]
    fn path_helpers() {
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/x/", "a"), "/x/a");
        assert_eq!(split("/a"), ("/", "a"));
        assert_eq!(split("/x/y/a"), ("/x/y", "a"));
    }

    struct Live {
        var: &'static str,
        url: String,
        r: Arc<dyn Remote>,
        /// Fresh remote directory for this test.
        base: PathBuf,
    }

    /// Live servers: `TC_TEST_SFTP=sftp://user@host` (key auth) and `TC_TEST_FTP=ftp://user@host`
    /// with `TC_TEST_PASS`, both on the machine running the tests (some tests change the server's
    /// files directly). A missing variable skips locally but fails in CI.
    fn live(tag: &str) -> Vec<Live> {
        let pass = std::env::var("TC_TEST_PASS").ok();
        let mut out = Vec::new();
        for var in ["TC_TEST_SFTP", "TC_TEST_FTP"] {
            let Ok(url) = std::env::var(var) else {
                assert!(std::env::var_os("CI").is_none(), "{var} not set in CI");
                continue;
            };
            let mut ask = |p: Prompt| match p {
                Prompt::HostKey(_) => Some("y".into()),
                Prompt::Password(_) => pass.clone(),
            };
            let (r, home) = connect(&url, &mut ask).unwrap();
            let base = PathBuf::from(home).join(format!("tc-live-{tag}-{var}"));
            let path = tc_core::vfs::rpath(&base);
            if r.stat(&path).unwrap().is_none() {
                r.mkdir(&path).unwrap();
            }
            out.push(Live { var, url, r, base });
        }
        out
    }

    fn put(l: &Live, src: &Path, to: &Path, mv: bool) -> Result<()> {
        use tc_core::ops::{Decision, Progress};
        let ask = &mut |_: &Path, _: &Path| Decision::Overwrite;
        let side = tc_core::vfs::Side::Remote(l.r.clone());
        tc_core::vfs::transfer(
            &tc_core::vfs::Side::Local,
            src,
            &side,
            to,
            mv,
            ask,
            &Progress::default(),
        )
    }

    fn get(l: &Live, from: &Path, to: &Path, mv: bool) -> Result<()> {
        use tc_core::ops::{Decision, Progress};
        let ask = &mut |_: &Path, _: &Path| Decision::Overwrite;
        let side = tc_core::vfs::Side::Remote(l.r.clone());
        tc_core::vfs::transfer(
            &side,
            from,
            &tc_core::vfs::Side::Local,
            to,
            mv,
            ask,
            &Progress::default(),
        )
    }

    fn gone(l: &Live, p: &Path) -> bool {
        l.r.stat(&tc_core::vfs::rpath(p)).unwrap().is_none()
    }

    #[test]
    #[ignore = "needs a server"]
    fn live_roundtrip() {
        for l in live("roundtrip") {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("tc-live");
            std::fs::create_dir(&src).unwrap();
            std::fs::write(src.join("a"), "hello").unwrap();
            let remote = l.base.join("tc-live");
            put(&l, &src, &remote, false).unwrap();
            let p = tc_core::ops::Progress::default();
            let tree = tc_core::ncdu::scan_remote(l.r.as_ref(), &remote, &p);
            assert_eq!((tree.size, tree.files), (5, 1));
            if l.var == "TC_TEST_SFTP" {
                let out = ssh_command(&l.url, "cat a", &tc_core::vfs::rpath(&remote))
                    .unwrap()
                    .output()
                    .unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout), "hello", "{out:?}");
            }
            let back = tmp.path().join("back");
            get(&l, &remote, &back, true).unwrap();
            assert_eq!(std::fs::read_to_string(back.join("a")).unwrap(), "hello");
            assert!(gone(&l, &remote));
        }
    }

    /// Counted delete of a tree: bytes, files, dirs done match the scanned totals; nothing left.
    #[test]
    #[ignore = "needs a server"]
    fn live_delete_counts() {
        use std::sync::atomic::Ordering;
        for l in live("delete") {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("t");
            std::fs::create_dir_all(src.join("s")).unwrap();
            std::fs::write(src.join("a"), "hello").unwrap();
            std::fs::write(src.join("s/b"), "").unwrap();
            let remote = l.base.join("del");
            put(&l, &src, &remote, false).unwrap();
            let side = tc_core::vfs::Side::Remote(l.r.clone());
            let p = tc_core::ops::Progress::default();
            tc_core::vfs::delete_all(&side, std::slice::from_ref(&remote), false, &p, || {})
                .unwrap();
            assert!(gone(&l, &remote), "{}", l.var);
            let got = [
                &p.done,
                &p.files,
                &p.dirs_done,
                &p.total,
                &p.files_total,
                &p.dirs,
            ];
            let got = got.map(|c| c.load(Ordering::Relaxed));
            assert_eq!(got, [5, 2, 2, 5, 2, 2], "{}", l.var);
        }
    }

    /// Sync with delete extras: a target name that became a directory after the compare is
    /// refused, not deleted as a tree.
    #[test]
    #[ignore = "needs a server"]
    fn live_sync_delete_refuses_dir() {
        use tc_core::{ops::Progress, sync, vfs::Side};
        for l in live("sync") {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("s");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join("extra"), "x").unwrap();
            let remote = l.base.join("sync");
            put(&l, &src, &remote, false).unwrap();
            let left = tmp.path().join("l");
            std::fs::create_dir(&left).unwrap();
            let sides = [Side::Local, Side::Remote(l.r.clone())];
            let roots = [left, remote.clone()];
            let items = sync::compare(&sides, &roots, &Progress::default()).unwrap();
            let plan = sync::plan(&items, 0, true, true);
            let extra = tc_core::vfs::rpath(&remote.join("extra"));
            l.r.remove_file(&extra).unwrap();
            l.r.mkdir(&extra).unwrap();
            let keep = remote.join("extra/keep");
            put(&l, &src.join("extra"), &keep, false).unwrap();
            assert!(sync::run(&sides, &roots, 0, &plan, &Progress::default()).is_err());
            assert!(!gone(&l, &keep), "{}", l.var);
            Side::Remote(l.r.clone()).delete(&remote).unwrap();
        }
    }

    /// Remote find: hostile file names and query texts stay data in the server's shell
    /// (no injected command runs); FTP cannot run commands and says so.
    #[test]
    #[ignore = "needs a server"]
    fn live_find_remote_quoting() {
        use std::sync::atomic::AtomicBool;
        use tc_core::find::{Content, Query, find_remote};
        for l in live("find") {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("s");
            std::fs::create_dir_all(src.join("sk'ip")).unwrap();
            for n in [
                "a'b.txt",
                "$(touch pwned).txt",
                "`touch pwned`.log",
                "sk'ip/in.txt",
            ] {
                std::fs::write(src.join(n), "needle it's\n").unwrap();
            }
            let remote = l.base.join("f'ind $x");
            put(&l, &src, &remote, false).unwrap();
            let q = Query {
                mask: "*'*, $(touch*".into(),
                exclude: vec!["sk'ip".into()],
                content: Some(Content::new("it's", true, false, false).unwrap()),
                ..Query::default()
            };
            let root = tc_core::vfs::rpath(&remote);
            let mut names = Vec::new();
            let res = find_remote(l.r.as_ref(), &root, &q, &AtomicBool::new(false), |e| {
                names.push(e.name.to_string_lossy().into_owned());
            });
            if l.var == "TC_TEST_FTP" {
                assert!(res.is_err());
            } else {
                res.unwrap();
                names.sort();
                let want = ["$(touch pwned).txt", "a'b.txt"].map(|n| format!("{root}/{n}"));
                assert_eq!(names, want);
                assert!(gone(&l, &remote.join("pwned")));
            }
            tc_core::vfs::Side::Remote(l.r.clone())
                .delete(&remote)
                .unwrap();
        }
    }

    /// Remote multi-rename: a swap goes via temp names, an existing target is refused before
    /// anything moves, undo restores; no temp file is left behind.
    #[test]
    #[ignore = "needs a server"]
    fn live_rename_swap_refuse_undo() {
        use tc_core::{ops::Progress, rename, vfs::Side};
        for l in live("rename") {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("s");
            std::fs::create_dir(&src).unwrap();
            for n in ["a", "b", "c"] {
                std::fs::write(src.join(n), n).unwrap();
            }
            let remote = l.base.join("ren");
            put(&l, &src, &remote, false).unwrap();
            let side = Side::Remote(l.r.clone());
            let pair = |o: &str, n: &str| (o.to_owned(), n.to_owned());
            let p = Progress::default();
            let swap = [pair("a", "b"), pair("b", "a")];
            rename::apply(&side, &remote, &swap, &p).unwrap();
            let refused = rename::apply(&side, &remote, &[pair("c", "a")], &p);
            assert!(refused.is_err(), "{}", l.var);
            rename::undo(&side, &remote, &swap, &p).unwrap();
            let back = tmp.path().join("back");
            get(&l, &remote, &back, true).unwrap();
            let mut names: Vec<_> = std::fs::read_dir(&back)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            assert_eq!(names, ["a", "b", "c"], "{}", l.var);
            for n in ["a", "b", "c"] {
                assert_eq!(std::fs::read_to_string(back.join(n)).unwrap(), n);
            }
        }
    }

    /// Remote process monitor over SSH: a snapshot lists processes and kill ends one (the
    /// server is this machine); FTP cannot run commands and reports an error.
    // `sleep` is a POSIX tool
    #[cfg(unix)]
    #[test]
    #[ignore = "needs a server"]
    fn live_top_sample_and_kill() {
        use tc_core::top;
        for l in live("top") {
            let sampler = top::sample(Some(l.r.clone()), top::INTERVAL);
            let snap = sampler.rx.recv().unwrap();
            if l.var == "TC_TEST_FTP" {
                assert!(snap.is_err());
                continue;
            }
            assert!(!snap.unwrap().procs.is_empty());
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            top::kill(Some(l.r.as_ref()), child.id(), "sleep", false).unwrap();
            assert!(!child.wait().unwrap().success());
            assert!(top::kill(Some(l.r.as_ref()), u32::MAX - 1, "x", false).is_err());
        }
    }

    /// Spaces, quotes, `$`, backticks, unicode and a leading dash survive upload, listing,
    /// the server-side `find` scan (shell quoting) and download.
    #[test]
    #[ignore = "needs a server"]
    fn live_names() {
        const NAMES: [&str; 5] = ["sp ace", "q'uote", "ünï çødé 日本", "-dash", "$x;`y`"];
        for l in live("names") {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("src");
            std::fs::create_dir(&src).unwrap();
            for n in NAMES {
                std::fs::write(src.join(n), n).unwrap();
            }
            let remote = l.base.join("d'ir ü $x");
            put(&l, &src, &remote, false).unwrap();
            let mut got: Vec<_> =
                l.r.list(&tc_core::vfs::rpath(&remote))
                    .unwrap()
                    .into_iter()
                    .map(|e| e.name.to_string_lossy().into_owned())
                    .collect();
            got.sort();
            let mut want = NAMES.map(String::from).to_vec();
            want.sort();
            assert_eq!(got, want, "{}", l.var);
            let p = tc_core::ops::Progress::default();
            let tree = tc_core::ncdu::scan_remote(l.r.as_ref(), &remote, &p);
            let bytes = NAMES.iter().map(|n| n.len() as u64).sum::<u64>();
            assert_eq!((tree.size, tree.files), (bytes, 5), "{}", l.var);
            let back = tmp.path().join("back");
            get(&l, &remote, &back, true).unwrap();
            for n in NAMES {
                assert_eq!(std::fs::read_to_string(back.join(n)).unwrap(), n);
            }
        }
    }

    /// A failing transfer leaves the source intact and no partial target.
    #[test]
    #[ignore = "needs a server"]
    #[cfg(unix)]
    fn live_failure_cleanup() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for l in live("cleanup") {
            let tmp = tempfile::tempdir().unwrap();
            // a dangling link cannot be read: fails after (or before) `a` was copied
            let src = tmp.path().join("src");
            std::fs::create_dir(&src).unwrap();
            std::fs::write(src.join("a"), "keep").unwrap();
            symlink("nowhere", src.join("bad")).unwrap();
            let t = l.base.join("t");
            assert!(put(&l, &src, &t, true).is_err(), "{}", l.var);
            assert!(gone(&l, &t), "{}: partial target left", l.var);
            assert_eq!(std::fs::read_to_string(src.join("a")).unwrap(), "keep");
            assert!(src.join("bad").symlink_metadata().is_ok());

            // read-only target directory (the server runs as this user)
            let ro = l.base.join("ro");
            l.r.mkdir(&tc_core::vfs::rpath(&ro)).unwrap();
            if !ro.is_dir() {
                continue; // server on another machine
            }
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
            let probe = ro.join(".probe");
            if std::fs::File::create(&probe).is_ok() {
                let _ = std::fs::remove_file(&probe); // root ignores modes
            } else {
                let f = tmp.path().join("f");
                std::fs::write(&f, "keep").unwrap();
                let res = put(&l, &f, &ro.join("f"), true);
                assert!(res.is_err(), "{}", l.var);
                assert!(gone(&l, &ro.join("f")), "{}", l.var);
                assert_eq!(std::fs::read_to_string(&f).unwrap(), "keep");
            }
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Directory links (even a loop) are skipped, never followed; scan and download terminate.
    #[test]
    #[ignore = "needs a server"]
    #[cfg(unix)]
    fn live_symlinks() {
        use std::os::unix::fs::symlink;
        for l in live("links") {
            let tree = l.base.join("tree");
            let dir = tc_core::vfs::rpath(&tree);
            l.r.mkdir(&dir).unwrap();
            if !tree.is_dir() {
                continue; // server on another machine
            }
            std::fs::create_dir(tree.join("real")).unwrap();
            std::fs::write(tree.join("real/f"), "x").unwrap();
            symlink(".", tree.join("loop")).unwrap();
            symlink("real", tree.join("lnk")).unwrap();
            let p = tc_core::ops::Progress::default();
            let node = tc_core::ncdu::scan_remote(l.r.as_ref(), &tree, &p);
            assert!(node.files >= 1, "{}", l.var);

            let tmp = tempfile::tempdir().unwrap();
            let back = tmp.path().join("back");
            let res = get(&l, &tree, &back, false);
            if l.var == "TC_TEST_FTP" {
                // vsftpd lists a link to a directory as a plain file: RETR fails, the download rolls back
                assert!(res.is_err() && !back.exists(), "{res:?}");
            } else {
                let err = res.unwrap_err().to_string();
                assert!(err.contains("symlinked directories"), "{}: {err}", l.var);
                assert_eq!(std::fs::read_to_string(back.join("real/f")).unwrap(), "x");
                assert!(!back.join("loop").exists() && !back.join("lnk").exists());
            }

            // a dangling link cannot be read: the download fails and leaves nothing behind
            symlink("nowhere", tree.join("dangling")).unwrap();
            let back2 = tmp.path().join("back2");
            assert!(get(&l, &tree, &back2, false).is_err(), "{}", l.var);
            assert!(!back2.exists(), "{}: partial download left", l.var);
        }
    }

    /// Kills the server side of an open session: the next call must fail, not hang.
    /// Needs passwordless `sudo`; run it alone (it ends every session of the user).
    #[test]
    #[ignore = "needs a server and sudo, kills sessions"]
    fn live_drop() {
        for l in live("drop") {
            let user = Url::parse(&l.url).unwrap().user;
            let home = tc_core::vfs::rpath(&l.base);
            l.r.list(&home).unwrap();
            let pattern = if l.var == "TC_TEST_SFTP" {
                // anchored: the pattern must not match this `sudo pkill` command line
                vec!["-f".to_string(), format!("^sshd(-session)?: {user}")]
            } else {
                vec!["-x".to_string(), "vsftpd".to_string()]
            };
            let st = Command::new("sudo")
                .args(["-n", "pkill"])
                .args(&pattern)
                .status()
                .unwrap();
            assert!(st.success(), "pkill {pattern:?}: {st}");
            std::thread::sleep(std::time::Duration::from_secs(1));
            let (tx, rx) = std::sync::mpsc::channel();
            let r = l.r.clone();
            std::thread::spawn(move || tx.send(r.list(&home).is_err()));
            let failed = rx.recv_timeout(std::time::Duration::from_secs(60));
            assert_eq!(
                failed,
                Ok(true),
                "{}: list after drop hung or succeeded",
                l.var
            );
        }
    }
}
