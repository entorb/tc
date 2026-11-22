//! Process monitor: periodic snapshots of the local or an SSH server's processes, and kill.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use sysinfo::{
    Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, ThreadKind, UpdateKind, Users,
};

use crate::vfs::Remote;
use crate::{Error, Result};

/// Default time between snapshots; CPU % is the average over it.
pub const INTERVAL: Duration = Duration::from_secs(2);
/// Cap of one remote sample's output (bounded memory against a misbehaving server).
const MAX_OUT: u64 = 16 << 20;
/// Linux `/proc` summary (each part optional), then `ps`; missing tools stay silent.
const SCRIPT: &str = "head -1 /proc/stat 2>/dev/null; cat /proc/loadavg /proc/uptime 2>/dev/null; \
grep -E '^(MemTotal|MemAvailable|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null; \
nproc 2>/dev/null; echo ---; cat /proc/[0-9]*/stat 2>/dev/null; echo ---; \
ps -eo pid=,user=,pcpu=,rss=,args= 2>/dev/null";

/// One process; strings are display-only, `pid` is the identity.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    pub cmd: String,
    pub user: String,
    /// Percent of one core (may exceed 100 for multi-threaded processes).
    pub cpu: f32,
    /// Resident memory in bytes.
    pub mem: u64,
}

/// System summary and process list; `0` / `None` = unknown (e.g. a non-Linux server).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Total CPU usage in percent of all cores.
    pub cpu: Option<f32>,
    pub cores: usize,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load: Option<[f64; 3]>,
    pub uptime: Option<u64>,
    pub procs: Vec<Proc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcSort {
    Pid,
    Name,
    Cpu,
    Mem,
}

/// Sort `procs` by `key`: pid and name ascending, CPU and memory largest first; `rev` flips.
pub fn sort(procs: &mut [Proc], key: ProcSort, rev: bool) {
    procs.sort_by(|a, b| {
        let o = match key {
            ProcSort::Pid => a.pid.cmp(&b.pid),
            ProcSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            ProcSort::Cpu => b.cpu.total_cmp(&a.cpu),
            ProcSort::Mem => b.mem.cmp(&a.mem),
        };
        let o = o.then(a.pid.cmp(&b.pid));
        if rev { o.reverse() } else { o }
    });
}

/// Case-insensitive substring of name, command line, user or pid; `filter` must be lowercase.
pub fn matches(p: &Proc, filter: &str) -> bool {
    filter.is_empty()
        || p.name.to_lowercase().contains(filter)
        || p.cmd.to_lowercase().contains(filter)
        || p.user.to_lowercase().contains(filter)
        || p.pid.to_string().contains(filter)
}

/// Snapshots from a worker thread; dropping it ends the worker.
pub struct Sampler {
    pub rx: Receiver<Result<Snapshot>>,
    ctl: Sender<Duration>,
}

impl Sampler {
    /// Time between snapshots; the next one is taken right away.
    pub fn set_interval(&self, every: Duration) {
        let _ = self.ctl.send(every);
    }
}

/// Snapshots every `every` (CPU % averaged over it): local via sysinfo, `remote` via one shell
/// command per sample. The worker ends when the [`Sampler`] is dropped or after sending an error.
pub fn sample(remote: Option<Arc<dyn Remote>>, every: Duration) -> Sampler {
    let (tx, rx) = mpsc::channel();
    let (ctl, ctl_rx) = mpsc::channel();
    thread::spawn(move || match remote {
        Some(r) => remote_loop(r.as_ref(), &tx, &ctl_rx, every),
        None => local_loop(&tx, &ctl_rx, every),
    });
    Sampler { rx, ctl }
}

/// Wait `every` or until a new interval arrives; `false` once the sampler is dropped.
fn wait(ctl: &Receiver<Duration>, every: &mut Duration) -> bool {
    match ctl.recv_timeout(*every) {
        Ok(d) => {
            *every = d;
            true
        }
        Err(RecvTimeoutError::Timeout) => true,
        Err(RecvTimeoutError::Disconnected) => false,
    }
}

fn local_loop(tx: &Sender<Result<Snapshot>>, ctl: &Receiver<Duration>, mut every: Duration) {
    let (Ok(mut sys), Ok(users)) = (System::new(), Users::new_with_refreshed_list()) else {
        let _ = tx.send(Err(Error::Process("cannot read system information".into())));
        return;
    };
    let kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet);
    // CPU usage is the difference between two refreshes
    sys.refresh_cpu_usage();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    loop {
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        if tx.send(Ok(local_snapshot(&sys, &users))).is_err() || !wait(ctl, &mut every) {
            return;
        }
    }
}

fn local_snapshot(sys: &System, users: &Users) -> Snapshot {
    let procs = sys
        .processes()
        .values()
        // Linux lists the threads of a process as entries too
        .filter(|p| p.thread_kind() != Some(ThreadKind::Userland))
        .map(|p| Proc {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            cmd: p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            user: p
                .user_id()
                .and_then(|u| users.get_user_by_id(u))
                .map_or_else(String::new, |u| u.name().to_owned()),
            cpu: p.cpu_usage(),
            mem: p.memory(),
        })
        .collect();
    Snapshot {
        cpu: Some(sys.global_cpu_usage()),
        cores: sys.cpus().len(),
        mem_used: sys.used_memory(),
        mem_total: sys.total_memory(),
        swap_used: sys.used_swap(),
        swap_total: sys.total_swap(),
        load: System::load_average()
            .ok()
            .map(|l| [l.one, l.five, l.fifteen]),
        uptime: System::uptime().ok(),
        procs,
    }
}

fn remote_loop(
    r: &dyn Remote,
    tx: &Sender<Result<Snapshot>>,
    ctl: &Receiver<Duration>,
    mut every: Duration,
) {
    let mut prev = Prev::default();
    loop {
        let snap = run(r, SCRIPT).and_then(|out| {
            let snap = parse_remote(&out, &mut prev);
            if snap.procs.is_empty() {
                return Err(Error::Process(format!(
                    "{}: `ps -eo pid,user,pcpu,rss,args` gave no processes",
                    r.url()
                )));
            }
            Ok(snap)
        });
        let failed = snap.is_err();
        if tx.send(snap).is_err() || failed || !wait(ctl, &mut every) {
            return;
        }
    }
}

/// Stdout of `cmd` on the server, capped at [`MAX_OUT`].
fn run(r: &dyn Remote, cmd: &str) -> Result<String> {
    let mut buf = Vec::new();
    r.exec(cmd, &mut |out| {
        out.take(MAX_OUT)
            .read_to_end(&mut buf)
            .map_err(|e| Error::Remote(e.to_string()))?;
        Ok(())
    })?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Busy and total jiffies of the `cpu` line of `/proc/stat`.
fn cpu_stat(line: &str) -> Option<(u64, u64)> {
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    let total: u64 = v.iter().sum();
    // idle + iowait
    let idle = v.get(3)? + v.get(4).unwrap_or(&0);
    Some((total - idle, total))
}

/// CPU counters of the previous remote sample, for averages over the interval.
#[derive(Debug, Default)]
struct Prev {
    /// Busy and total jiffies of all cores.
    stat: Option<(u64, u64)>,
    /// Used jiffies (user + system) per pid.
    procs: HashMap<u32, u64>,
}

/// Pid and used jiffies (utime + stime) of a `/proc/<pid>/stat` line.
fn proc_stat(line: &str) -> Option<(u32, u64)> {
    let (pid, rest) = line.split_once(' ')?;
    // the name in parentheses may hold spaces; utime and stime are fields 14 and 15
    let mut f = rest.rsplit_once(')')?.1.split_whitespace().skip(11);
    let used = f.next()?.parse::<u64>().ok()? + f.next()?.parse::<u64>().ok()?;
    Some((pid.parse().ok()?, used))
}

/// Parse the output of [`SCRIPT`]; CPU % of the system and of each process is the average since
/// `prev`, which is updated. Without `/proc` (or on the first sample) processes keep `ps` CPU %.
fn parse_remote(out: &str, prev: &mut Prev) -> Snapshot {
    let mut parts = out.splitn(3, "---\n");
    let (head, stats, list) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let (mut s, stat) = parse_head(head);
    let used: HashMap<u32, u64> = stats.lines().filter_map(proc_stat).collect();
    s.procs = list.lines().filter_map(parse_ps).collect();
    if let (Some((b0, t0)), Some((b1, t1))) = (prev.stat, stat)
        && t1 > t0
    {
        let span = (t1 - t0) as f32;
        s.cpu = Some(b1.saturating_sub(b0) as f32 * 100.0 / span);
        // per core, like ps and top
        let per_core = 100.0 * s.cores.max(1) as f32 / span;
        for p in &mut s.procs {
            if let (Some(u0), Some(u1)) = (prev.procs.get(&p.pid), used.get(&p.pid)) {
                p.cpu = u1.saturating_sub(*u0) as f32 * per_core;
            }
        }
    }
    *prev = Prev { stat, procs: used };
    s
}

/// Summary part of [`SCRIPT`] and the `/proc/stat` CPU counters.
fn parse_head(head: &str) -> (Snapshot, Option<(u64, u64)>) {
    let mut s = Snapshot::default();
    let mut stat = None;
    let (mut avail, mut swap_free) = (0, 0);
    for line in head.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let kib = || f.get(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
        match f.as_slice() {
            ["cpu", ..] => stat = cpu_stat(line),
            ["MemTotal:", ..] => s.mem_total = kib(),
            ["MemAvailable:", ..] => avail = kib(),
            ["SwapTotal:", ..] => s.swap_total = kib(),
            ["SwapFree:", ..] => swap_free = kib(),
            [a, b, c, tasks, _] if tasks.contains('/') => {
                s.load = a
                    .parse()
                    .ok()
                    .zip(b.parse().ok())
                    .zip(c.parse().ok())
                    .map(|((a, b), c)| [a, b, c]);
            }
            [up, _] => s.uptime = up.parse::<f64>().ok().map(|u| u as u64),
            [n] => s.cores = n.parse().unwrap_or(0),
            _ => {}
        }
    }
    s.mem_used = s.mem_total.saturating_sub(avail);
    s.swap_used = s.swap_total.saturating_sub(swap_free);
    (s, stat)
}

/// One `ps -eo pid=,user=,pcpu=,rss=,args=` line.
fn parse_ps(line: &str) -> Option<Proc> {
    let mut it = line.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    let user = it.next()?.to_owned();
    let cpu = it.next()?.parse().ok()?;
    let mem = it.next()?.parse::<u64>().ok()? * 1024;
    let cmd = it.collect::<Vec<_>>().join(" ");
    let first = cmd.split(' ').next().unwrap_or_default();
    let name = first.rsplit('/').next().unwrap_or(first).to_owned();
    Some(Proc {
        pid,
        name,
        cmd,
        user,
        cpu,
        mem,
    })
}

/// Send SIGTERM (`force`: SIGKILL) to `pid`, locally or on the `remote` server.
pub fn kill(remote: Option<&dyn Remote>, pid: u32, force: bool) -> Result<()> {
    let Some(r) = remote else {
        return kill_local(pid, force);
    };
    let sig = if force { "KILL" } else { "TERM" };
    // exec reports no exit status, so success is echoed
    let out = run(r, &format!("kill -{sig} {pid} 2>&1 && echo ok"))?;
    match out.trim().strip_suffix("ok") {
        Some("") => Ok(()),
        _ => Err(Error::Process(format!("kill {pid}: {}", out.trim()))),
    }
}

fn kill_local(pid: u32, force: bool) -> Result<()> {
    let id = Pid::from_u32(pid);
    let mut sys = System::new().map_err(|e| Error::Process(e.to_string()))?;
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[id]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let p = sys
        .process(id)
        .ok_or_else(|| Error::Process(format!("process {pid} is gone")))?;
    let sig = if force { Signal::Kill } else { Signal::Term };
    // no SIGTERM on Windows: kill() terminates there
    if p.kill_with(sig).unwrap_or_else(|| p.kill()) {
        Ok(())
    } else {
        Err(Error::Process(format!(
            "cannot signal process {pid} (permission?)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        INTERVAL, Prev, ProcSort, Snapshot, kill, matches, parse_ps, parse_remote, sample, sort,
    };
    use std::time::{Duration, Instant};

    fn p(pid: u32, name: &str, cpu: f32, mem: u64) -> super::Proc {
        super::Proc {
            pid,
            name: name.into(),
            cmd: format!("/bin/{name} --x"),
            user: "root".into(),
            cpu,
            mem,
        }
    }

    #[test]
    fn sorts_and_reverses() {
        let mut v = vec![p(3, "b", 1.0, 10), p(1, "C", 5.0, 30), p(2, "a", 3.0, 20)];
        let pids = |v: &[super::Proc]| v.iter().map(|p| p.pid).collect::<Vec<_>>();
        sort(&mut v, ProcSort::Cpu, false);
        assert_eq!(pids(&v), [1, 2, 3]);
        sort(&mut v, ProcSort::Mem, true);
        assert_eq!(pids(&v), [3, 2, 1]);
        sort(&mut v, ProcSort::Name, false);
        assert_eq!(pids(&v), [2, 3, 1]);
        sort(&mut v, ProcSort::Pid, true);
        assert_eq!(pids(&v), [3, 2, 1]);
    }

    #[test]
    fn filter_matches_fields() {
        let x = p(42, "Firefox", 0.0, 0);
        assert!(matches(&x, ""));
        assert!(matches(&x, "fire"));
        assert!(matches(&x, "--x"));
        assert!(matches(&x, "roo"));
        assert!(matches(&x, "42"));
        assert!(!matches(&x, "chrome"));
    }

    #[test]
    fn parses_linux_server() {
        let first = "cpu  100 0 100 800 0 0 0 0\n0.50 0.25 0.10 2/300 999\n1000.5 3000.1\n\
MemTotal: 1000 kB\nMemAvailable: 400 kB\nSwapTotal: 100 kB\nSwapFree: 100 kB\n4\n---\n\
77 (my prog) S 1 77 77 0 -1 4194304 100 0 0 0 30 10 0 0 20 0 1 0\n---\n\
    1 root      0.0  1200 /sbin/init splash\n  77 alice    12.5   800 /usr/bin/my prog -v\n";
        let mut prev = Prev::default();
        let s = parse_remote(first, &mut prev);
        assert_eq!(s.cpu, None);
        assert_eq!(prev.stat, Some((200, 1000)));
        assert_eq!(prev.procs.get(&77), Some(&40));
        assert_eq!(s.load, Some([0.5, 0.25, 0.1]));
        assert_eq!(s.uptime, Some(1000));
        assert_eq!((s.mem_used, s.mem_total), (600 * 1024, 1000 * 1024));
        assert_eq!((s.swap_used, s.swap_total), (0, 100 * 1024));
        assert_eq!(s.cores, 4);
        assert_eq!(s.procs.len(), 2);
        assert_eq!(s.procs[1].name, "my");
        assert_eq!(s.procs[1].cmd, "/usr/bin/my prog -v");
        assert_eq!((s.procs[1].cpu, s.procs[1].mem), (12.5, 800 * 1024));
        // 100 of 200 more jiffies busy; pid 77 used 20 of 200 / 4 cores per core
        let next = first
            .replace("cpu  100 0 100 800", "cpu  200 0 100 900")
            .replace(" 30 10 ", " 45 15 ");
        let s = parse_remote(&next, &mut prev);
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!((s.procs[0].cpu, s.procs[1].cpu), (0.0, 40.0));
    }

    #[test]
    fn parses_server_without_proc() {
        let mut prev = Prev::default();
        let s = parse_remote("---\n---\n  5 bob 1.0 10 [kthread]\nbad line\n", &mut prev);
        assert_eq!(prev.stat, None);
        assert_eq!(
            Snapshot {
                procs: Vec::new(),
                ..s.clone()
            },
            Snapshot::default()
        );
        assert_eq!(s.procs, vec![parse_ps("5 bob 1.0 10 [kthread]").unwrap()]);
    }

    #[test]
    fn samples_own_process() {
        let s = sample(None, INTERVAL);
        let snap = s.rx.recv().unwrap().unwrap();
        assert!(snap.procs.iter().any(|p| p.pid == std::process::id()));
        assert!(snap.mem_total > 0 && snap.cores > 0);
        // a new interval wakes the worker at once
        let t = Instant::now();
        s.set_interval(Duration::from_millis(50));
        s.rx.recv().unwrap().unwrap();
        s.rx.recv().unwrap().unwrap();
        assert!(t.elapsed() < INTERVAL);
    }

    #[cfg(unix)]
    #[test]
    fn kills_local_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        kill(None, child.id(), false).unwrap();
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn kill_unknown_pid_fails() {
        assert!(kill(None, u32::MAX - 1, true).is_err());
    }
}
