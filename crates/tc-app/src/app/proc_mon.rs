//! Process monitor (Ctrl+P): sampling, sort, filter, kill.

use super::{Filter, edit_key};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tc_core::cmdline::LineEdit;
use tc_core::procs::{self, Proc, ProcSort, Sampler, Snapshot};
use tc_core::vfs::Remote;

/// Process monitor (Ctrl+P): latest snapshot, sorted and filtered, refreshed by a worker.
pub struct Procs {
    /// Connection of the remote panel it was opened on; `None`: this computer.
    pub remote: Option<Arc<dyn Remote>>,
    pub(super) sampler: Sampler,
    /// Time between snapshots, CPU % is averaged over it.
    pub every: Duration,
    /// Interval being typed (`s`), in seconds.
    pub every_edit: Option<LineEdit>,
    pub snap: Snapshot,
    /// Indices into `snap.procs` matching the filter, in sort order.
    pub rows: Vec<usize>,
    pub sel: usize,
    pub sort: ProcSort,
    pub rev: bool,
    pub filter: Filter,
    /// Kill question for (pid, name), shown while set.
    pub confirm: Option<(u32, String)>,
    pub(super) kill: Option<thread::JoinHandle<tc_core::Result<()>>>,
    /// Result of the last kill, shown in the footer until the next key.
    pub status: String,
}

impl Procs {
    pub(super) fn new(remote: Option<Arc<dyn Remote>>) -> Self {
        Procs {
            sampler: procs::sample(remote.clone(), procs::INTERVAL),
            every: procs::INTERVAL,
            every_edit: None,
            remote,
            snap: Snapshot::default(),
            rows: Vec::new(),
            sel: 0,
            sort: ProcSort::Cpu,
            rev: false,
            filter: Filter::default(),
            confirm: None,
            kill: None,
            status: String::new(),
        }
    }

    /// No prompt or question open.
    pub fn idle(&self) -> bool {
        !self.filter.editing && self.every_edit.is_none() && self.confirm.is_none()
    }

    /// Process under the cursor.
    pub fn current(&self) -> Option<&Proc> {
        self.rows.get(self.sel).map(|&i| &self.snap.procs[i])
    }

    /// Re-sort and re-filter; the cursor keeps its row, so the list does not jump under it.
    pub(super) fn refresh(&mut self) {
        procs::sort(&mut self.snap.procs, self.sort, self.rev);
        let filter = self.filter.line.text.to_lowercase();
        self.rows = (0..self.snap.procs.len())
            .filter(|&i| procs::matches(&self.snap.procs[i], &filter))
            .collect();
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    /// Takes the newest snapshot and a finished kill; `Some(error)` when sampling failed.
    pub(super) fn tick(&mut self) -> Option<String> {
        let mut new = None;
        while let Ok(r) = self.sampler.rx.try_recv() {
            match r {
                Ok(s) => new = Some(s),
                Err(e) => return Some(e.to_string()),
            }
        }
        if let Some(s) = new {
            self.snap = s;
            self.refresh();
        }
        if self
            .kill
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
            && let Some(job) = self.kill.take()
        {
            self.status = match job.join() {
                Ok(Ok(())) => "signal sent".into(),
                Ok(Err(e)) => e.to_string(),
                Err(_) => "kill failed".into(),
            };
        }
        None
    }

    /// `false` closes the monitor.
    pub(super) fn key(&mut self, key: KeyEvent, page: usize) -> bool {
        self.status.clear();
        if self.confirm.is_some() {
            self.confirm_key(key.code);
            return true;
        }
        if let Some(l) = self.every_edit.take() {
            self.every_key(l, key);
            return true;
        }
        if self.filter.key(key) {
            self.refresh();
            return true;
        }
        let last = self.rows.len().saturating_sub(1);
        let sort = |p: &mut Self, key| {
            p.rev = p.sort == key && !p.rev;
            p.sort = key;
        };
        match key.code {
            // sample now: the next snapshot comes right away
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sampler.set_interval(self.every);
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(10) => return false,
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(page),
            KeyCode::PageDown => self.sel = (self.sel + page).min(last),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = last,
            KeyCode::Char('c') => sort(self, ProcSort::Cpu),
            KeyCode::Char('m') => sort(self, ProcSort::Mem),
            KeyCode::Char('p') => sort(self, ProcSort::Pid),
            KeyCode::Char('n') => sort(self, ProcSort::Name),
            KeyCode::Char('f') => self.filter.editing = true,
            KeyCode::Char('s') => {
                self.every_edit = Some(LineEdit::from(self.every.as_secs_f64().to_string()));
            }
            KeyCode::F(8) | KeyCode::Char('k') => {
                self.confirm = self.current().map(|p| (p.pid, p.name.clone()));
            }
            _ => {}
        }
        self.refresh();
        true
    }

    /// Typing the update interval: Enter sets it (0.5 to 3600 s), Esc keeps the old one.
    pub(super) fn every_key(&mut self, mut l: LineEdit, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => match l.text.trim().replace(',', ".").parse::<f64>() {
                Ok(secs) if (0.5..=3600.0).contains(&secs) => {
                    self.every = Duration::from_secs_f64(secs);
                    self.sampler.set_interval(self.every);
                }
                _ => self.status = "update interval: 0.5 to 3600 seconds".into(),
            },
            _ => {
                edit_key(&mut l, key);
                self.every_edit = Some(l);
            }
        }
    }

    /// Answer to the kill question: `t` SIGTERM, `k` SIGKILL, sent in a worker (slow links).
    pub(super) fn confirm_key(&mut self, code: KeyCode) {
        let force = match code {
            KeyCode::Char('t' | 'T') | KeyCode::Enter => false,
            KeyCode::Char('k' | 'K') => true,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.confirm = None;
                return;
            }
            _ => return,
        };
        let Some((pid, _)) = self.confirm.take() else {
            return;
        };
        let r = self.remote.clone();
        self.kill = Some(thread::spawn(move || procs::kill(r.as_deref(), pid, force)));
        self.status = format!(
            "sending SIG{} to {pid}",
            if force { "KILL" } else { "TERM" }
        );
    }
}
