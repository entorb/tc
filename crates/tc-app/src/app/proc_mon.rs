//! Process monitor (Ctrl+P): sampling, sort, filter, kill.

use super::widgets::form_text;
use super::{FormRow, edit_key};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tc_core::cmdline::LineEdit;
use tc_core::procs::{self, Criteria, Proc, ProcSort, Sampler, Snapshot};
use tc_core::vfs::Remote;

const F_TEXT: &str = "Find (id, command, name)";
const F_MEM: &str = "Min MEM (MB)";
const F_CPU: &str = "Min CPU (%)";
const F_RUN: &str = "Min runtime (min)";

/// Filter popup rows (`/`), empty = no limit.
fn filter_rows() -> Vec<FormRow> {
    let text = |label| FormRow::Text(label, LineEdit::default());
    vec![text(F_TEXT), text(F_MEM), text(F_CPU), text(F_RUN)]
}

/// Filter popup values as criteria; `Err` names the field that is not a number of 0 or more.
fn criteria_from(rows: &[FormRow]) -> Result<Criteria, String> {
    let num = |label: &str| -> Result<f64, String> {
        let t = form_text(rows, label).trim().replace(',', ".");
        if t.is_empty() {
            return Ok(0.0);
        }
        t.parse::<f64>()
            .ok()
            .filter(|v| *v >= 0.0)
            .ok_or_else(|| format!("{label}: a number, 0 or more"))
    };
    Ok(Criteria {
        text: form_text(rows, F_TEXT).trim().to_lowercase(),
        min_mem: (num(F_MEM)? * 1_048_576.0) as u64,
        min_cpu: num(F_CPU)? as f32,
        min_run: (num(F_RUN)? * 60.0) as u64,
    })
}

/// Filter popup being edited: rows, focused row, error of the last Enter.
pub struct Draft {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub error: Option<String>,
}

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
    /// Applied filter values (popup rows) and the criteria they give.
    pub filter_rows: Vec<FormRow>,
    pub crit: Criteria,
    /// Filter popup while open (`/`).
    pub draft: Option<Draft>,
    /// Pid of the process shown in the info popup (`i`), while open.
    pub info: Option<u32>,
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
            filter_rows: filter_rows(),
            crit: Criteria::default(),
            draft: None,
            info: None,
            confirm: None,
            kill: None,
            status: String::new(),
        }
    }

    /// No prompt or question open.
    pub fn idle(&self) -> bool {
        self.draft.is_none()
            && self.every_edit.is_none()
            && self.confirm.is_none()
            && self.info.is_none()
    }

    /// Process under the cursor.
    pub fn current(&self) -> Option<&Proc> {
        self.rows.get(self.sel).map(|&i| &self.snap.procs[i])
    }

    /// Re-sort and re-filter; the cursor keeps its row, so the list does not jump under it.
    pub(super) fn refresh(&mut self) {
        procs::sort(&mut self.snap.procs, self.sort);
        self.rows = (0..self.snap.procs.len())
            .filter(|&i| self.crit.matches(&self.snap.procs[i]))
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
        if self.draft.is_some() {
            self.draft_key(key);
            self.refresh();
            return true;
        }
        if self.info.is_some() {
            // info popup: i / Esc / F10 close it, other keys wait
            if matches!(key.code, KeyCode::Char('i') | KeyCode::Esc | KeyCode::F(10)) {
                self.info = None;
            }
            return true;
        }
        let last = self.rows.len().saturating_sub(1);
        match key.code {
            // sample now: the next snapshot comes right away
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sampler.set_interval(self.every);
            }
            KeyCode::F(5) => self.sampler.set_interval(self.every),
            KeyCode::Esc if self.crit.active() => {
                self.filter_rows = filter_rows();
                self.crit = Criteria::default();
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(10) => return false,
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(page),
            KeyCode::PageDown => self.sel = (self.sel + page).min(last),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = last,
            KeyCode::Tab => {
                self.sort = match self.sort {
                    ProcSort::Cpu => ProcSort::Mem,
                    ProcSort::Mem => ProcSort::Run,
                    ProcSort::Run => ProcSort::Cpu,
                };
            }
            KeyCode::Char('c') => self.sort = ProcSort::Cpu,
            KeyCode::Char('m') => self.sort = ProcSort::Mem,
            KeyCode::Char('r') => self.sort = ProcSort::Run,
            KeyCode::Char('/') | KeyCode::Char('f') => {
                self.draft = Some(Draft {
                    rows: self.filter_rows.clone(),
                    focus: 0,
                    error: None,
                });
            }
            KeyCode::Char('i') => self.info = self.current().map(|p| p.pid),
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

    /// Filter popup: Tab / Up / Down move between fields, Enter applies, Esc cancels.
    fn draft_key(&mut self, key: KeyEvent) {
        let Some(mut d) = self.draft.take() else {
            return;
        };
        let n = d.rows.len();
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => match criteria_from(&d.rows) {
                Ok(c) => {
                    self.crit = c;
                    self.filter_rows = d.rows;
                    return;
                }
                Err(e) => d.error = Some(e),
            },
            KeyCode::Tab | KeyCode::Down => d.focus = (d.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up => d.focus = (d.focus + n - 1) % n,
            code => d.rows[d.focus].key(code, key.modifiers.contains(KeyModifiers::CONTROL)),
        }
        self.draft = Some(d);
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

#[cfg(test)]
mod tests {

    use super::super::menu::PROCS_FOOTER;

    use super::Procs;

    use super::super::{App, Mode};
    use crate::fmt;

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use tc_core::procs::ProcSort;

    use super::super::tests::{key, settle, two_panels, type_keys, with};

    fn procs(app: &App) -> &Procs {
        match &app.mode {
            Mode::Procs(p) => p,
            _ => panic!("not in the process monitor"),
        }
    }

    /// Tick until the next snapshot arrives.
    fn procs_wait(app: &mut App) {
        let before = procs(app).snap.clone();
        let t = std::time::Instant::now();
        while procs(app).snap == before && t.elapsed() < std::time::Duration::from_secs(5) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Ctrl+P: snapshot arrives, sort toggles, filter narrows and clears, Esc closes.
    #[test]
    fn process_monitor_sort_filter_close() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        while procs(&app).rows.is_empty() {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let all = procs(&app).rows.len();
        assert!(app.idle());
        assert_eq!(app.footer_keys(), &PROCS_FOOTER);
        key(&mut app, KeyCode::Tab);
        let p = procs(&app);
        assert_eq!(p.sort, ProcSort::Mem);
        let mems: Vec<u64> = p.rows.iter().map(|&i| p.snap.procs[i].mem).collect();
        assert!(mems.is_sorted_by(|a, b| a >= b));
        key(&mut app, KeyCode::Tab);
        assert_eq!(procs(&app).sort, ProcSort::Run);
        key(&mut app, KeyCode::Tab);
        assert_eq!(procs(&app).sort, ProcSort::Cpu);
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, &std::process::id().to_string());
        let p = procs(&app);
        assert!(p.draft.is_some() && p.rows.len() == all);
        key(&mut app, KeyCode::Enter);
        let p = procs(&app);
        assert!(p.draft.is_none() && p.rows.len() < all);
        assert_eq!(p.current().map(|c| c.pid), Some(std::process::id()));
        key(&mut app, KeyCode::Esc);
        assert_eq!(procs(&app).rows.len(), all);
        // the cursor keeps its row across refreshes
        key(&mut app, KeyCode::Down);
        // a new interval wakes the worker at once, no 2 s wait for the next snapshot
        procs(&app)
            .sampler
            .set_interval(std::time::Duration::from_millis(50));
        procs_wait(&mut app);
        assert_eq!(procs(&app).sel, 1);
        key(&mut app, KeyCode::Char('s'));
        assert_eq!(procs(&app).every_edit.as_ref().unwrap().text, "2");
        with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        type_keys(&mut app, "0,5");
        key(&mut app, KeyCode::Enter);
        assert_eq!(procs(&app).every, std::time::Duration::from_millis(500));
        key(&mut app, KeyCode::Char('s'));
        with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        type_keys(&mut app, "0");
        key(&mut app, KeyCode::Enter);
        let p = procs(&app);
        assert_eq!(p.every, std::time::Duration::from_millis(500));
        assert!(p.status.contains("0.5 to 3600"));
        key(&mut app, KeyCode::F(8));
        assert!(procs(&app).confirm.is_some());
        key(&mut app, KeyCode::Char('n'));
        assert!(procs(&app).confirm.is_none());
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// `i`: info popup of the selected process, closed by i, Esc, F10; other keys wait.
    #[test]
    fn info_popup_shows_selected_and_closes() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        while procs(&app).rows.is_empty() {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let pid = procs(&app).current().unwrap().pid;
        key(&mut app, KeyCode::Char('i'));
        assert_eq!(procs(&app).info, Some(pid));
        let info = fmt::procs_info(procs(&app)).unwrap();
        assert_eq!(info.rows[0], ("PID", pid.to_string()));
        key(&mut app, KeyCode::Down);
        assert_eq!(procs(&app).info, Some(pid));
        key(&mut app, KeyCode::F(10));
        assert_eq!(procs(&app).info, None);
        assert!(matches!(app.mode, Mode::Procs(_)));
        key(&mut app, KeyCode::Char('i'));
        key(&mut app, KeyCode::Esc);
        assert_eq!(procs(&app).info, None);
    }

    /// `/` popup: a minimum that is not a number keeps the popup open with the error; Esc cancels.
    #[test]
    fn filter_popup_rejects_bad_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        type_keys(&mut app, "x");
        key(&mut app, KeyCode::Enter);
        let d = procs(&app).draft.as_ref().unwrap();
        assert!(d.error.as_deref().is_some_and(|e| e.starts_with("Min CPU")));
        key(&mut app, KeyCode::Esc);
        assert!(procs(&app).draft.is_none() && !procs(&app).crit.active());
    }

    /// A connection that cannot run commands (FTP) reports it instead of an empty list.
    #[test]
    fn process_monitor_needs_commands_on_remote() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        app.panels[0]
            .open_remote(tc_core::vfs::mem::Mem::new(), "/".into())
            .unwrap();
        settle(&mut app);
        with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        while matches!(app.mode, Mode::Procs(_)) {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(matches!(&app.mode, Mode::Message(m) if m.contains("no commands")));
    }
}
