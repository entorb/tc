//! Docker, Podman and Kubernetes containers through their CLI: every operation is one
//! `<cli> exec <target> sh -c <script>`, so the container needs a POSIX `sh`, `find` and `stat`
//! (GNU or busybox).

use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use tc_core::panel::Entry;
use tc_core::vfs::{Remote, sh_quote};
use tc_core::{Error, Result};

use crate::{Scheme, Url};

#[derive(Debug)]
pub struct Container {
    base: String,
    home: String,
    prog: OsString,
    /// Arguments between `exec [-i]` and the command: user / namespace / container flags and name.
    target: Vec<String>,
}

impl Container {
    /// Connect: one `pwd` checks that the CLI, the container and its shell work.
    pub fn connect(url: &Url) -> Result<Self> {
        let mut c = Self::new(url);
        c.home = String::from_utf8_lossy(&c.run("pwd")?).trim().to_owned();
        if c.home.is_empty() {
            c.home = "/".into();
        }
        Ok(c)
    }

    pub(crate) fn new(url: &Url) -> Self {
        let mut target = Vec::new();
        let prog = match url.scheme {
            Scheme::Kube => {
                let (ns, pod) = url.host.split_once('/').unwrap_or(("default", &url.host));
                target.extend(["-n".into(), ns.into(), pod.into()]);
                if !url.user.is_empty() {
                    target.extend(["-c".into(), url.user.clone()]);
                }
                target.push("--".into());
                "kubectl"
            }
            s => {
                if !url.user.is_empty() {
                    target.extend(["-u".into(), url.user.clone()]);
                }
                target.push(url.host.clone());
                if s == Scheme::Podman {
                    "podman"
                } else {
                    "docker"
                }
            }
        };
        Self {
            base: url.base(),
            home: "/".into(),
            prog: prog.into(),
            target,
        }
    }

    /// `<cli> exec [-i] <target> sh -c <script>`; `-i` passes stdin through.
    pub(crate) fn command(&self, script: &str, stdin: bool) -> Command {
        let mut c = Command::new(&self.prog);
        c.arg("exec");
        if stdin {
            c.arg("-i");
        }
        c.args(&self.target).args(["sh", "-c", script]);
        no_window(&mut c);
        c
    }

    fn spawn(&self, c: &mut Command) -> Result<Child> {
        c.spawn().map_err(|e| self.spawn_err(&e))
    }

    fn spawn_err(&self, e: &std::io::Error) -> Error {
        spawn_err(&self.prog, e)
    }

    fn failed(&self, out: &Output) -> Result<()> {
        failed(&self.base, out)
    }

    /// Run `script`, return its stdout; a non-zero exit is an error with the stderr text.
    fn run(&self, script: &str) -> Result<Vec<u8>> {
        let out = self
            .command(script, false)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| self.spawn_err(&e))?;
        self.failed(&out)?;
        Ok(out.stdout)
    }

    /// One `stat` line of `path` (`-L` follows a symlink), `None` if nothing is there.
    fn stat_line(&self, path: &str, follow: bool) -> Result<Option<Entry>> {
        let p = sh_quote(path);
        let (test, flag) = if follow {
            (format!("[ -e {p} ]"), "-L ")
        } else {
            (format!("[ -e {p} ] || [ -L {p} ]"), "")
        };
        let out = self.run(&format!("if {test}; then stat {flag}-c {STAT} -- {p}; fi"))?;
        let name = crate::split(path).1;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .find_map(parse_stat)
            .map(|mut e| {
                e.name = name.into();
                e
            }))
    }
}

fn spawn_err(prog: &OsStr, e: &std::io::Error) -> Error {
    let prog = prog.to_string_lossy();
    if e.kind() == ErrorKind::NotFound {
        Error::Remote(format!("{prog} not found in PATH"))
    } else {
        Error::Remote(format!("{prog}: {e}"))
    }
}

/// A non-zero exit as an error with the stderr text, prefixed by `what`.
fn failed(what: &str, out: &Output) -> Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stderr);
    let msg = msg.trim();
    let msg = if msg.is_empty() {
        out.status.to_string()
    } else {
        msg.to_owned()
    };
    Err(Error::Remote(format!("{what}: {msg}")))
}

/// A running container the connection form offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// Host part of the URL: container name, or `namespace/pod`.
    pub host: String,
    /// User part: the container of a pod with several, else empty.
    pub user: String,
    /// List row: name, image and status, or pod and container.
    pub label: String,
}

/// Rows shown at most; a bigger cluster is filtered by typing a name instead.
const PICK_MAX: usize = 10_000;

/// Running containers of `scheme` (Docker, Podman), or containers of running pods in all
/// namespaces of the current `kubectl` context.
pub fn running(scheme: Scheme) -> Result<Vec<Pick>> {
    const PS: [&str; 3] = ["ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Status}}"];
    const PODS: [&str; 7] = [
        "get",
        "pods",
        "-A",
        "--field-selector=status.phase=Running",
        "--no-headers",
        "-o",
        "custom-columns=NS:.metadata.namespace,POD:.metadata.name,CT:.spec.containers[*].name",
    ];
    let (prog, args): (&str, &[&str]) = match scheme {
        Scheme::Kube => ("kubectl", &PODS),
        Scheme::Podman => ("podman", &PS),
        _ => ("docker", &PS),
    };
    let mut c = Command::new(prog);
    c.args(args).stdin(Stdio::null());
    no_window(&mut c);
    let out = c.output().map_err(|e| spawn_err(prog.as_ref(), &e))?;
    failed(prog, &out)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut picks = if scheme == Scheme::Kube {
        parse_pods(&text)
    } else {
        parse_ps(&text)
    };
    picks.truncate(PICK_MAX);
    Ok(picks)
}

/// `ps --format` lines: name, image, status separated by tabs.
fn parse_ps(out: &str) -> Vec<Pick> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let name = it.next().filter(|n| !n.is_empty())?;
            let rest: Vec<&str> = it.collect();
            Some(Pick {
                host: name.into(),
                user: String::new(),
                label: format!("{name}   {}", rest.join("   ")).trim_end().into(),
            })
        })
        .collect()
}

/// `kubectl get pods` custom columns: namespace, pod, comma separated containers; a pod
/// with several containers gets one row each.
fn parse_pods(out: &str) -> Vec<Pick> {
    let mut picks = Vec::new();
    for l in out.lines() {
        let mut it = l.split_whitespace();
        let (Some(ns), Some(pod)) = (it.next(), it.next()) else {
            continue;
        };
        let host = format!("{ns}/{pod}");
        let cts: Vec<&str> = it.next().unwrap_or("").split(',').collect();
        for ct in &cts {
            let several = cts.len() > 1;
            picks.push(Pick {
                host: host.clone(),
                user: if several { (*ct).into() } else { String::new() },
                label: if several {
                    format!("{host}   {ct}")
                } else {
                    host.clone()
                },
            });
        }
    }
    picks
}

/// `stat -c` format: hex mode, size, mtime, name (last: it may hold spaces).
const STAT: &str = "'%f %s %Y %n'";

/// One `stat -c STAT` line; the name has a leading `./` stripped.
fn parse_stat(line: &str) -> Option<Entry> {
    let mut it = line.splitn(4, ' ');
    let mode = u32::from_str_radix(it.next()?, 16).ok()?;
    let size = it.next()?.parse().ok()?;
    let secs: u64 = it.next()?.parse().ok()?;
    let name = it.next()?;
    let kind = mode & 0o170_000;
    Some(Entry {
        name: name.strip_prefix("./").unwrap_or(name).into(),
        is_dir: kind == 0o040_000,
        is_symlink: kind == 0o120_000,
        size,
        modified: SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)),
        link: None,
        ignored: false,
    })
}

/// `list` output: `stat` lines, then `D <name>` for each symlink pointing to a directory.
// shortcut: names with line breaks are dropped or split, upgrade to NUL-separated output if needed
fn parse_list(out: &str) -> Vec<Entry> {
    let link_dirs: std::collections::HashSet<&str> =
        out.lines().filter_map(|l| l.strip_prefix("D ./")).collect();
    out.lines()
        .filter(|l| !l.starts_with("D "))
        .filter_map(parse_stat)
        .map(|mut e| {
            if e.is_symlink && e.name.to_str().is_some_and(|n| link_dirs.contains(n)) {
                e.is_dir = true;
            }
            e
        })
        .collect()
}

#[cfg(windows)]
fn no_window(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    // tc is a GUI app: without this every exec flashes a console window
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_: &mut Command) {}

impl Remote for Container {
    fn url(&self) -> &str {
        &self.base
    }

    fn home(&self) -> String {
        self.home.clone()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let d = sh_quote(dir);
        let script = format!(
            r"cd {d} && find . -mindepth 1 -maxdepth 1 -exec stat -c {STAT} {{}} + && find . -mindepth 1 -maxdepth 1 -type l -exec test -d {{}} \; -exec printf 'D %s\n' {{}} \;"
        );
        Ok(parse_list(&String::from_utf8_lossy(&self.run(&script)?)))
    }

    fn stat(&self, path: &str) -> Result<Option<Entry>> {
        self.stat_line(path, true)
    }

    fn lstat(&self, path: &str) -> Result<Option<Entry>> {
        self.stat_line(path, false)
    }

    fn read(&self, path: &str, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
        let mut c = self.command(&format!("cat -- {}", sh_quote(path)), false);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.spawn(&mut c)?;
        let r = match child.stdout.take() {
            Some(mut out) => f(&mut out),
            None => Ok(()),
        };
        if r.is_err() {
            let _ = child.kill();
        }
        let out = child.wait_with_output().map_err(|e| self.spawn_err(&e))?;
        r.and_then(|()| self.failed(&out))
    }

    fn write_new(&self, path: &str, f: &mut dyn FnMut(&mut dyn Write) -> Result<()>) -> Result<()> {
        let p = sh_quote(path);
        // noclobber: an exclusive create, so an existing file is never touched
        self.run(&format!("set -C; : > {p}"))?;
        let mut c = self.command(&format!("cat >> {p}"), true);
        c.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = self.spawn(&mut c)?;
        let r = match child.stdin.take() {
            Some(mut w) => f(&mut w)
                .and_then(|()| w.flush().map_err(|e| Error::Remote(format!("{path}: {e}")))),
            None => Ok(()),
        };
        if r.is_err() {
            let _ = child.kill();
        }
        let out = child.wait_with_output().map_err(|e| self.spawn_err(&e))?;
        r.and_then(|()| self.failed(&out))
    }

    fn mkdir(&self, path: &str) -> Result<()> {
        self.run(&format!("mkdir -- {}", sh_quote(path))).map(drop)
    }

    fn remove_file(&self, path: &str) -> Result<()> {
        self.run(&format!("rm -- {}", sh_quote(path))).map(drop)
    }

    fn remove_dir(&self, path: &str) -> Result<()> {
        self.run(&format!("rmdir -- {}", sh_quote(path))).map(drop)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (f, t) = (sh_quote(from), sh_quote(to));
        self.run(&format!(
            "if [ -e {t} ] || [ -L {t} ]; then echo {t}: exists >&2; exit 1; fi; mv -- {f} {t}"
        ))
        .map(drop)
    }

    fn exec(&self, cmd: &str, f: &mut dyn FnMut(&mut dyn Read) -> Result<()>) -> Result<()> {
        let mut c = self.command(cmd, false);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = self.spawn(&mut c)?;
        let r = match child.stdout.take() {
            Some(mut out) => f(&mut out),
            None => Ok(()),
        };
        if r.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_lines() {
        let e = parse_stat("41ed 4096 1700000000 ./my dir").unwrap();
        assert!(e.is_dir && !e.is_symlink);
        assert_eq!(e.name, "my dir");
        assert_eq!(
            e.modified,
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
        let e = parse_stat("81a4 5 0 /etc/a b").unwrap();
        assert_eq!((e.size, e.name.to_str()), (5, Some("/etc/a b")));
        assert!(parse_stat("zz 1 2 x").is_none());
        assert!(parse_stat("81a4 5").is_none());
        let l = parse_list("a1ff 3 0 ./ln\na1ff 3 0 ./fl\n81a4 1 0 ./f\nD ./ln\n");
        let dirs: Vec<_> = l
            .iter()
            .map(|e| (e.name.to_str().unwrap(), e.is_dir))
            .collect();
        assert_eq!(dirs, [("ln", true), ("fl", false), ("f", false)]);
    }

    /// A fake `docker` that drops `exec [-i] <name>` and runs the rest on this machine.
    // Linux only: the scripts need GNU or busybox `stat -c` (macOS has BSD stat, Windows no sh)
    #[cfg(target_os = "linux")]
    #[test]
    fn ops_through_fake_cli() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("fake-docker");
        let script = "#!/bin/sh\nshift\n[ \"$1\" = -i ] && shift\nshift\nexec \"$@\"\n";
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let c = Container {
            base: "docker://c".into(),
            home: "/".into(),
            prog: fake.into(),
            target: vec!["c".into()],
        };
        ops(&c, &root.to_string_lossy());
    }

    /// Real container: `TC_TEST_CONTAINER=docker://name` (or `podman://`, `kube://ns/pod`); the
    /// test works in a fresh `/tmp/tc-live-*` directory there.
    #[test]
    #[ignore = "needs a container"]
    fn live_container() {
        let Ok(url) = std::env::var("TC_TEST_CONTAINER") else {
            return;
        };
        let c = Container::connect(&Url::parse(&url).unwrap()).unwrap();
        let root = format!("/tmp/tc-live-{}", std::process::id());
        c.mkdir(&root).unwrap();
        ops(&c, &root);
        c.run(&format!("rm -rf -- {}", sh_quote(&root))).unwrap();
    }

    /// Every `Remote` operation inside the empty directory `root`.
    fn ops(c: &Container, root: &str) {
        let at = |n: &str| format!("{root}/{n}");
        let put = |p: &str, data: &[u8]| {
            c.write_new(&at(p), &mut |w| w.write_all(data).map_err(crate::err))
        };
        let get = |p: &str| {
            let mut v = Vec::new();
            c.read(&at(p), &mut |r| {
                r.read_to_end(&mut v).map(drop).map_err(crate::err)
            })
            .map(|()| v)
        };

        c.mkdir(&at("d")).unwrap();
        put("it's a b", b"hello").unwrap();
        assert!(
            put("it's a b", b"other").is_err(),
            "write_new must not overwrite"
        );
        assert_eq!(get("it's a b").unwrap(), b"hello");
        put("empty", b"").unwrap();
        assert_eq!(get("empty").unwrap(), b"");
        let big: Vec<u8> = (0..3_000_000u32).map(|i| i as u8).collect();
        put("big", &big).unwrap();
        assert!(get("big").unwrap() == big);
        assert!(get("missing").is_err());
        let ln = format!(
            "cd {} && ln -s d ld && ln -s nowhere dangling",
            sh_quote(root)
        );
        c.run(&ln).unwrap();

        let mut l: Vec<_> = (c.list(root).unwrap().into_iter())
            .map(|e| {
                (
                    e.name.into_string().unwrap(),
                    e.is_dir,
                    e.is_symlink,
                    e.size,
                )
            })
            .collect();
        l.sort();
        let kinds: Vec<_> = l.iter().map(|(n, d, s, _)| (n.as_str(), *d, *s)).collect();
        assert_eq!(
            kinds,
            [
                ("big", false, false),
                ("d", true, false),
                ("dangling", false, true),
                ("empty", false, false),
                ("it's a b", false, false),
                ("ld", true, true),
            ]
        );
        assert_eq!((l[0].3, l[3].3, l[4].3), (3_000_000, 0, 5));
        assert!(
            c.stat(&at("ld"))
                .unwrap()
                .is_some_and(|e| e.is_dir && !e.is_symlink)
        );
        assert!(c.lstat(&at("ld")).unwrap().is_some_and(|e| e.is_symlink));
        assert!(c.stat(&at("dangling")).unwrap().is_none());
        assert!(c.lstat(&at("dangling")).unwrap().is_some());
        assert!(c.stat(&at("nope")).unwrap().is_none());
        assert!(c.list(&at("nope")).is_err());

        assert!(
            c.rename(&at("empty"), &at("big")).is_err(),
            "rename must not replace"
        );
        assert_eq!(get("big").unwrap().len(), big.len());
        c.rename(&at("empty"), &at("d/e")).unwrap();
        assert!(c.remove_dir(&at("d")).is_err(), "not empty");
        c.remove_file(&at("d/e")).unwrap();
        c.remove_dir(&at("d")).unwrap();
        let mut out = String::new();
        c.exec(&format!("ls {}", sh_quote(root)), &mut |r| {
            r.read_to_string(&mut out).map(drop).map_err(crate::err)
        })
        .unwrap();
        assert!(out.contains("big") && !out.contains("empty"), "{out}");
    }

    #[test]
    fn missing_cli_is_named() {
        let mut c = Container::new(&Url::parse("docker://x").unwrap());
        c.prog = "tc-no-such-cli".into();
        let e = c.list("/").unwrap_err().to_string();
        assert!(e.contains("tc-no-such-cli not found"), "{e}");
    }

    #[test]
    fn running_lists() {
        let ps = parse_ps("web\tnginx:1\tUp 2 hours\n\ndb\tpostgres\tUp 1 minute\n");
        assert_eq!(ps.len(), 2);
        assert_eq!(
            (ps[0].host.as_str(), ps[0].label.as_str()),
            ("web", "web   nginx:1   Up 2 hours")
        );
        let pods = parse_pods("prod   api-1   app,sidecar\nkube-system   dns   coredns\n");
        let rows: Vec<_> = pods
            .iter()
            .map(|p| (p.host.as_str(), p.user.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("prod/api-1", "app"),
                ("prod/api-1", "sidecar"),
                ("kube-system/dns", "")
            ]
        );
        assert_eq!(pods[1].label, "prod/api-1   sidecar");
    }

    #[test]
    fn exec_targets() {
        let args = |u: &str| {
            let c = Container::new(&Url::parse(u).unwrap()).command("ls", true);
            let a: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into()).collect();
            (c.get_program().to_string_lossy().into_owned(), a.join(" "))
        };
        assert_eq!(
            args("docker://web/x"),
            ("docker".into(), "exec -i web sh -c ls".into())
        );
        assert_eq!(
            args("podman://root@db"),
            ("podman".into(), "exec -i -u root db sh -c ls".into())
        );
        assert_eq!(
            args("kube://app@prod/web-1/srv"),
            (
                "kubectl".into(),
                "exec -i -n prod web-1 -c app -- sh -c ls".into()
            )
        );
    }
}
