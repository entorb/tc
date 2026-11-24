//! Copy, move, delete and Ctrl+C / Ctrl+X / Ctrl+V of files: workers, conflicts, progress.

use super::{App, Done, Mode, Op, READ_ONLY, edit_key};
use crate::{KeyCode, KeyEvent};
use std::borrow::Cow;
use std::ffi::OsString;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tc_core::cmdline::LineEdit;
use tc_core::edit;
use tc_core::eta::Estimator;
use tc_core::vfs::{self, Side};
use tc_core::{Error, ops};

/// Files copied (Ctrl+C) or cut (Ctrl+X) from a panel, pasted with Ctrl+V into the active one.
#[derive(Clone)]
pub struct FileClip {
    pub op: Op,
    pub paths: Vec<PathBuf>,
    pub side: Side,
}

pub(super) enum XMsg {
    /// Conflict: (source, existing target); the worker waits for an `Answer`.
    Ask(PathBuf, PathBuf),
    /// A file failed (error text with its path); the worker waits for retry, skip or cancel.
    Error(String),
    /// Finished with an error text, if any.
    Done(Option<String>),
}

#[derive(Clone, Copy)]
pub(super) enum Policy {
    Overwrite,
    Skip,
    Newer,
}

pub(super) enum Answer {
    One(Policy),
    /// Same policy for all further conflicts.
    All(Policy),
    Rename(String),
    Retry,
    Cancel,
}

/// Worker end of the popup, shared by the conflict question and the error handler (copy
/// threads): one question at a time.
struct Line {
    tx: mpsc::Sender<XMsg>,
    answers: Receiver<Answer>,
    /// Answer to every further error: Skip all, or Cancel once cancelled.
    sticky: Option<ops::Recover>,
}

impl Line {
    fn ask(&self, msg: XMsg) -> Option<Answer> {
        self.tx.send(msg).ok()?;
        self.answers.recv().ok()
    }
}

/// `Progress::on_error` putting each failed file to the user: retry, skip (all) or cancel.
fn error_handler(line: Arc<Mutex<Line>>) -> ops::OnError {
    use ops::Recover;
    Box::new(move |_, e| {
        let Ok(mut l) = line.lock() else {
            return Recover::Cancel;
        };
        if let Some(r) = l.sticky {
            return r;
        }
        let r = match l.ask(XMsg::Error(e.to_string())) {
            Some(Answer::Retry) => return Recover::Retry,
            Some(Answer::One(Policy::Skip)) => return Recover::Skip,
            Some(Answer::All(Policy::Skip)) => Recover::Skip,
            _ => Recover::Cancel,
        };
        l.sticky = Some(r);
        r
    })
}

pub struct Xfer {
    /// Popup title: Copy, Move, Delete, ...
    pub label: &'static str,
    pub ask: Option<(PathBuf, PathBuf)>,
    /// A file failed (error text), waiting for retry / skip / cancel.
    pub error: Option<String>,
    /// Name being typed after `r`.
    pub rename: Option<LineEdit>,
    pub(super) rx: Receiver<XMsg>,
    pub(super) reply: mpsc::Sender<Answer>,
    pub progress: Arc<ops::Progress>,
    /// Bytes, files, directories over time, fed from `App::tick`.
    pub(super) rates: [Estimator; 3],
    /// Rates of `rates` and the ETA in seconds, refreshed once a second.
    pub shown: Shown,
    pub(super) started: Instant,
}

#[derive(Default)]
pub struct Shown {
    pub at: Option<Instant>,
    pub rates: [Option<f64>; 3],
    pub eta: Option<f64>,
}

impl Xfer {
    /// Quick copies finish before a popup would only flicker.
    pub fn visible(&self) -> bool {
        self.ask.is_some()
            || self.error.is_some()
            || self.started.elapsed() > Duration::from_secs(1)
    }

    /// Feed the regressions; the shown rates and ETA change at most once a second.
    fn sample(&mut self, now: Instant) {
        let p = &self.progress;
        let done = [&p.done, &p.files, &p.dirs_done].map(|c| c.load(Ordering::Relaxed) as f64);
        let secs = now.duration_since(self.started).as_secs_f64();
        for (e, d) in self.rates.iter_mut().zip(done) {
            e.push(secs, d);
        }
        if self
            .shown
            .at
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        let remaining = p.total.load(Ordering::Relaxed) as f64 - done[0];
        self.shown = Shown {
            at: Some(now),
            rates: self.rates.each_ref().map(Estimator::rate),
            eta: (p.scanned.load(Ordering::Acquire))
                .then(|| self.rates[0].eta(remaining))
                .flatten(),
        };
    }

    pub(super) fn answer(&mut self, a: Answer) {
        self.ask = None;
        self.error = None;
        self.rename = None;
        let _ = self.reply.send(a);
    }

    pub(super) fn key(&mut self, key: KeyEvent) {
        let code = key.code;
        if let Some(text) = &mut self.rename {
            match code {
                KeyCode::Enter => {
                    let name = std::mem::take(&mut text.text);
                    self.answer(Answer::Rename(name));
                }
                KeyCode::Esc => self.rename = None,
                _ => {
                    edit_key(text, key);
                }
            }
        } else if self.error.is_some() {
            match code {
                KeyCode::Char('r') | KeyCode::Enter => self.answer(Answer::Retry),
                KeyCode::Char('s') => self.answer(Answer::One(Policy::Skip)),
                KeyCode::Char('S') => self.answer(Answer::All(Policy::Skip)),
                KeyCode::Esc | KeyCode::Char('c') => self.answer(Answer::Cancel),
                _ => {}
            }
        } else if let Some((_, target)) = &self.ask {
            use Policy::*;
            match code {
                KeyCode::Char('o') | KeyCode::Enter => self.answer(Answer::One(Overwrite)),
                KeyCode::Char('s') => self.answer(Answer::One(Skip)),
                KeyCode::Char('n') => self.answer(Answer::One(Newer)),
                KeyCode::Char('O') => self.answer(Answer::All(Overwrite)),
                KeyCode::Char('S') => self.answer(Answer::All(Skip)),
                KeyCode::Char('N') => self.answer(Answer::All(Newer)),
                KeyCode::Char('r') => {
                    let name = target.file_name().unwrap_or_default();
                    self.rename = Some(LineEdit::from(name.to_string_lossy().into_owned()));
                }
                KeyCode::Esc | KeyCode::Char('c') => self.answer(Answer::Cancel),
                _ => {}
            }
        } else if code == KeyCode::Esc {
            self.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub struct Job {
    pub label: &'static str,
    pub(super) path: PathBuf,
    pub(super) rx: Receiver<Done>,
    pub(super) progress: Arc<AtomicU64>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) total: u64,
    pub(super) exit: bool,
    pub(super) started: Instant,
}

impl Job {
    /// Saves are usually instant, so their popup waits for a slow one.
    pub fn visible(&self) -> bool {
        self.label != "Saving" || self.started.elapsed() > Duration::from_secs(2)
    }

    pub fn percent(&self) -> u64 {
        (self.progress.load(Ordering::Relaxed) * 100 / self.total.max(1)).min(100)
    }

    /// "42%", or the count done when the total is unknown (`u64::MAX`).
    pub fn done_text(&self) -> String {
        match self.total {
            u64::MAX => format!("{} done", self.progress.load(Ordering::Relaxed)),
            _ => format!("{}%", self.percent()),
        }
    }
}

/// Delete question with its answers; remote panels have no trash.
/// "Delete a?" or "Delete n items (size): a, b, c…?" from the first names and the total count.
pub(super) fn delete_question(names: &[Cow<str>], count: usize, size: Option<u64>) -> String {
    if count == 1 {
        return format!("Delete {}?", names[0]);
    }
    let more = if count > names.len() { "…" } else { "" };
    let size = size.map_or_else(String::new, |s| format!(" ({})", edit::human(s)));
    format!("Delete {count} items{size}: {}{more}?", names.join(", "))
}

pub(super) fn delete_text(mut question: String, recursive: bool, remote: bool) -> String {
    if recursive {
        question.push_str("\nDirectories are deleted recursively.");
    }
    if remote {
        return question + "\n\nNo trash on remote panels.\n[p] Delete permanently   [Esc] Cancel";
    }
    question + "\n\n[y] Trash   [p] Delete permanently   [Esc] Cancel"
}

/// Copy or move `jobs` (source, target) between the sides; errors go to `c.on_error`.
pub(super) fn copy_jobs(
    op: Op,
    jobs: &[(PathBuf, PathBuf)],
    sides: &(Side, Side),
    c: &ops::Progress,
    ask: ops::Ask,
) -> tc_core::Result<()> {
    let (src_side, dst_side) = sides;
    let srcs: Vec<PathBuf> = jobs.iter().map(|(s, _)| s.clone()).collect();
    vfs::scan(src_side, &srcs, c);
    let local = matches!((src_side, dst_side), (Side::Local, Side::Local));
    let mv = op == Op::Move;
    jobs.iter().try_for_each(|(src, target)| {
        // local copies also ask per file inside a tree; this catches the rest (remote, mkdir)
        c.retry(src, || {
            if !local {
                vfs::transfer(src_side, src, dst_side, target, mv, &mut *ask, c)
            } else if mv {
                ops::move_(src, target, &mut *ask, c)
            } else {
                ops::copy(src, target, &mut *ask, c)
            }
        })
        .map(drop)
    })
}

/// Conflict on `target`: the sticky policy (Overwrite all, ...), else the user's answer.
fn conflict(
    line: &Mutex<Line>,
    sticky: &mut Option<Policy>,
    (from, to): &(Side, Side),
    src: &Path,
    target: &Path,
) -> ops::Decision {
    let policy = match *sticky {
        Some(p) => p,
        None => {
            let msg = XMsg::Ask(src.to_owned(), target.to_owned());
            match line.lock().ok().and_then(|l| l.ask(msg)) {
                Some(Answer::One(p)) => p,
                Some(Answer::All(p)) => *sticky.insert(p),
                Some(Answer::Rename(n)) => return ops::Decision::Rename(n.into()),
                _ => return ops::Decision::Cancel,
            }
        }
    };
    match policy {
        Policy::Overwrite => ops::Decision::Overwrite,
        Policy::Newer if vfs::is_newer(from, src, to, target) => ops::Decision::Overwrite,
        Policy::Newer | Policy::Skip => ops::Decision::Skip,
    }
}

impl App {
    /// Archives are only a source: nothing is written into them, nothing is removed from them.
    pub(super) fn confirm(&mut self, op: Op) {
        let in_archive = |i: usize| self.panels[i].archive().is_some();
        if (in_archive(self.active) && op != Op::Copy)
            || (in_archive(1 - self.active) && op != Op::Delete)
        {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        let targets = self.panels[self.active].targets();
        match targets.as_slice() {
            [] => {}
            _ if op == Op::Delete => {
                let text = self.delete_question();
                self.mode = Mode::Confirm(op, targets, text);
            }
            [one] => {
                let name = one.file_name().unwrap_or_default();
                let text = self.other_dir().join(name).display().to_string();
                self.mode = Mode::Transfer(op, targets, LineEdit::from(text));
            }
            _ => {
                let text = self.other_dir().display().to_string();
                self.mode = Mode::Transfer(op, targets, LineEdit::from(text));
            }
        }
    }

    /// "Delete a, b, c…?" from the entries in memory, no I/O.
    pub(super) fn delete_question(&self) -> String {
        let entries = self.panels[self.active].target_entries();
        let names: Vec<_> = entries
            .iter()
            .take(3)
            .map(|e| e.name.to_string_lossy())
            .collect();
        let question = delete_question(&names, entries.len(), None);
        // a symlink is removed as a link, not recursed into
        let recursive = entries.iter().any(|e| e.is_dir && !e.is_symlink);
        delete_text(
            question,
            recursive,
            self.panels[self.active].remote().is_some(),
        )
    }

    pub(super) fn delete(&mut self, targets: &[PathBuf], to_trash: bool) {
        let side = self.panels[self.active].side();
        let targets = targets.to_vec();
        let sides = (side.clone(), side.clone());
        self.spawn_xfer("Delete", sides, move |p, _| {
            vfs::delete_all(&side, &targets, to_trash, p, || {})
        });
    }

    /// Copy or move to `text`. A relative `text` is in the active panel (rename), an absolute
    /// one on the other panel's side (local disk or its remote). A single item goes to exactly
    /// that path; several items, or a trailing separator, mean a directory.
    /// Runs in a worker; conflicts are put to the user through `Mode::Xfer`.
    pub(super) fn run(&mut self, op: Op, targets: &[PathBuf], text: &str) {
        let active = &self.panels[self.active];
        let to = if Path::new(text).has_root() {
            &self.panels[1 - self.active]
        } else {
            active
        };
        let (dst_side, dest) = (to.side(), to.path().join(text));
        let into_dir = targets.len() > 1 || text.ends_with(['/', std::path::MAIN_SEPARATOR]);
        let jobs: Vec<(PathBuf, PathBuf)> = targets
            .iter()
            .map(|src| match src.file_name() {
                Some(name) if into_dir => (src.clone(), dest.join(name)),
                _ => (src.clone(), dest.clone()),
            })
            .collect();
        let arch = active.archive().cloned();
        // archive entries are extracted to local temp files first
        let src_side = if arch.is_some() {
            Side::Local
        } else {
            active.side()
        };
        let sides = (src_side, dst_side);
        self.spawn_xfer(op.verb(), sides.clone(), move |c, ask| {
            // shortcut: needs temp space for the whole selection, upgrade to copy straight from the stream
            let (jobs, _tmp) = match arch {
                None => (jobs, None),
                Some(a) => {
                    let srcs: Vec<PathBuf> = jobs.iter().map(|(s, _)| s.clone()).collect();
                    let (tmp, out) = a.extract_all(&srcs, &c.cancel)?;
                    let targets = jobs.into_iter().map(|(_, t)| t);
                    (out.into_iter().zip(targets).collect(), Some(tmp))
                }
            };
            copy_jobs(op, &jobs, &sides, c, ask)
        });
    }

    /// Ctrl+C / Ctrl+X: remember the selection (or cursor entry) of the active panel for Ctrl+V.
    pub(super) fn clip_files(&mut self, op: Op) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message("Copy from an archive with F5 instead.".into());
            return;
        }
        let paths = panel.targets();
        let side = panel.side();
        if paths.is_empty() {
            return;
        }
        if matches!(side, Side::Local) {
            // also on the host clipboard, so Explorer, Finder or a file manager can paste them
            #[cfg(not(test))]
            {
                let host = paths.clone();
                thread::spawn(move || tc_core::hostclip::write_files(&host));
            }
            // our own list is not a new host paste at the next Ctrl+V
            self.last_host = paths.clone();
        }
        self.file_clip = Some(FileClip { op, paths, side });
    }

    /// Ctrl+V: copy or move the files of the host clipboard (if they changed since the last
    /// Ctrl+V) or of Ctrl+C / Ctrl+X into the active panel's directory; conflicts ask.
    pub fn paste(&mut self) {
        // the host tools (PowerShell starts in about a second) run in a worker
        let dest = self.panels[self.active].path().to_owned();
        self.spawn("Reading clipboard", dest, 1, false, |_, cancel| {
            // tests never read the developer's real clipboard
            let files = if cfg!(test) {
                Vec::new()
            } else {
                tc_core::hostclip::read_files()
            };
            Done::HostFiles((!cancel.load(Ordering::Relaxed)).then_some(files))
        });
    }

    pub(super) fn paste_with(&mut self, host: Vec<PathBuf>) {
        let fresh = !host.is_empty() && host != self.last_host;
        let host_clip = fresh.then(|| FileClip {
            op: Op::Copy,
            paths: host.clone(),
            side: Side::Local,
        });
        self.last_host = host;
        let Some(clip) = host_clip.or_else(|| self.file_clip.clone()) else {
            self.mode =
                Mode::Message("Nothing copied or cut yet: Ctrl+C or Ctrl+X on files first.".into());
            return;
        };
        let active = &self.panels[self.active];
        if active.archive().is_some() {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        let dest = active.path().to_owned();
        let sides = (clip.side.clone(), active.side());
        let jobs: Vec<(PathBuf, PathBuf)> = clip
            .paths
            .iter()
            .filter_map(|src| Some((src.clone(), dest.join(src.file_name()?))))
            .filter(|(src, target)| src != target)
            .collect();
        if jobs.is_empty() {
            self.mode = Mode::Message("Those files are already in this directory.".into());
            return;
        }
        let op = clip.op;
        self.cut_pending = op == Op::Move;
        self.spawn_xfer(op.verb(), sides.clone(), move |c, ask| {
            copy_jobs(op, &jobs, &sides, c, ask)
        });
    }

    /// Run `work` in a worker with progress, the conflict dialog (`sides` decide "newer") and
    /// retry / skip / cancel for failed files.
    pub(super) fn spawn_xfer(
        &mut self,
        label: &'static str,
        sides: (Side, Side),
        work: impl FnOnce(&ops::Progress, ops::Ask) -> tc_core::Result<()> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let (reply, answers) = mpsc::channel();
        let done = tx.clone();
        let line = Arc::new(Mutex::new(Line {
            tx,
            answers,
            sticky: None,
        }));
        let progress = Arc::new(ops::Progress {
            on_error: Some(error_handler(line.clone())),
            ..Default::default()
        });
        let c = progress.clone();
        thread::spawn(move || {
            let mut sticky: Option<Policy> = None;
            let mut ask =
                |src: &Path, target: &Path| conflict(&line, &mut sticky, &sides, src, target);
            let result = work(&c, &mut ask);
            let error = result.err().filter(|e| !matches!(e, Error::Cancelled));
            let _ = done.send(XMsg::Done(error.map(|e| e.to_string())));
        });
        self.mode = Mode::Xfer(Xfer {
            label,
            ask: None,
            error: None,
            rename: None,
            rx,
            reply,
            progress,
            rates: Default::default(),
            shown: Shown::default(),
            started: Instant::now(),
        });
    }

    pub(super) fn mkdir(&mut self, name: &str) {
        let panel = &self.panels[self.active];
        let side = panel.side();
        if side.remote().is_some() {
            let (dir, name) = (panel.path().to_owned(), OsString::from(name));
            let sides = (side.clone(), side.clone());
            return self.spawn_xfer("Make directory", sides, move |_, _| {
                side.mkdir_in(&dir, &name).map(drop)
            });
        }
        let result = ops::mkdir(panel.path(), &OsString::from(name));
        self.finish(result.err().map(|e| e.to_string()));
    }

    pub(super) fn finish(&mut self, error: Option<String>) {
        let mut errors: Vec<String> = error.into_iter().collect();
        self.free = Default::default();
        for panel in &mut self.panels {
            if let Err(e) = panel.reload() {
                errors.push(e.to_string());
            }
        }
        if !errors.is_empty() {
            self.mode = Mode::Message(errors.join("\n"));
        }
    }

    pub(super) fn xfer_tick(&mut self) {
        let Mode::Xfer(x) = &mut self.mode else {
            return;
        };
        x.sample(Instant::now());
        match x.rx.try_recv() {
            Ok(XMsg::Ask(src, target)) => x.ask = Some((src, target)),
            Ok(XMsg::Error(text)) => x.error = Some(text),
            Ok(XMsg::Done(error)) => {
                let progress = x.progress.clone();
                let skipped = progress.skipped.load(Ordering::Relaxed);
                let error = error.or_else(|| {
                    (skipped > 0).then(|| format!("{skipped} item(s) skipped after errors."))
                });
                self.mode = Mode::Normal;
                self.rename_done(error.as_ref(), &progress);
                // a failed cut keeps its clipboard, so Ctrl+V can retry
                if mem::take(&mut self.cut_pending) && error.is_none() {
                    self.file_clip = None;
                }
                if let Some(st) = &mut self.stats
                    && let Some((names, count)) = st.deleting.take()
                {
                    // whatever the error: what was deleted leaves the tree
                    let n = count.load(Ordering::Relaxed).min(names.len());
                    st.drop_names(&names[..n]);
                }
                self.finish(error);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.rename_running = None;
                self.mode = Mode::Message("worker thread failed".into())
            }
        }
    }

    // no trash on remote panels: only `p`
    pub(super) fn confirm_key(
        &mut self,
        op: Op,
        targets: Vec<PathBuf>,
        text: String,
        code: KeyCode,
    ) {
        match code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter
                if self.panels[self.active].remote().is_none() =>
            {
                self.delete(&targets, true)
            }
            KeyCode::Char('p' | 'P') => self.delete(&targets, false),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
            _ => self.mode = Mode::Confirm(op, targets, text),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::super::xfer::{FileClip, delete_text};
    use super::super::{App, Mode, Op};

    use crate::{KeyCode, KeyModifiers};

    use super::{Shown, Xfer};
    use std::ffi::OsStr;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use tc_core::panel::Panel;

    use tc_core::vfs::Side;

    use super::super::tests::{key, rows, settle, two_panels, type_keys, type_text, wait, with};

    #[test]
    fn delete_confirm_text_trash_choice_and_permanent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        for n in ["a", "b", "c"] {
            std::fs::write(dir.path().join(n), "").unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        for c in "+".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        type_text(&mut app, ""); // select all files
        key(&mut app, KeyCode::Down); // "d"
        key(&mut app, KeyCode::Char(' ')); // select the dir too
        key(&mut app, KeyCode::F(8));
        let Mode::Confirm(_, _, text) = &app.mode else {
            panic!("no confirm")
        };
        assert!(text.contains("recursively") && text.contains('…'));
        key(&mut app, KeyCode::Esc);
        assert!(dir.path().join("a").exists() && dir.path().join("d").exists());
        key(&mut app, KeyCode::F(8));
        key(&mut app, KeyCode::Char('p'));
        wait(&mut app);
        assert!(!dir.path().join("a").exists() && !dir.path().join("d").exists());
    }

    #[test]
    fn clipboard_and_paste() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        with(&mut app, KeyCode::Insert, KeyModifiers::CONTROL);
        assert_eq!(
            app.copied.take(),
            Some(dir.path().join("a.txt").display().to_string())
        );
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        with(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.copied.as_deref(), Some("o"));
        app.on_paste("a\r\nb".into());
        assert_eq!(rows(&mut app)[..2], ["a", "bne"]);
    }

    /// A file that fails asks retry / skip / cancel; a skip copies the rest and says so.
    #[cfg(unix)] // file modes are Unix-only
    #[test]
    fn failed_file_asks_retry_skip_cancel() {
        use std::os::unix::fs::PermissionsExt;
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let dir = left.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let locked = dir.join("a");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&locked).is_ok() {
            return; // root reads anything
        }
        let mut app = App::new(
            Panel::open(left.path()).unwrap(),
            Panel::open(right.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(matches!(&app.mode, Mode::Xfer(x) if x.error.is_some()));
        key(&mut app, KeyCode::Char('r')); // still unreadable: asked again
        wait(&mut app);
        assert!(matches!(&app.mode, Mode::Xfer(x) if x.error.is_some()));
        key(&mut app, KeyCode::Char('s'));
        wait(&mut app);
        assert!(matches!(&app.mode, Mode::Message(m) if m.contains("1 item(s) skipped")));
        assert!(right.path().join("d/b").exists() && !right.path().join("d/a").exists());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn conflict_dialog_overwrite_all_and_rename() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(left.path().join(f), "new").unwrap();
            std::fs::write(right.path().join(f), "old").unwrap();
        }
        let mut app = App::new(
            Panel::open(left.path()).unwrap(),
            Panel::open(right.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        key(&mut app, KeyCode::Char('r'));
        key(&mut app, KeyCode::Char('2'));
        key(&mut app, KeyCode::Enter); // "a2"
        wait(&mut app);
        key(&mut app, KeyCode::Enter); // overwrite "b"
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        let read = |f| std::fs::read_to_string(right.path().join(f)).unwrap();
        assert_eq!(
            (read("a"), read("a2"), read("b")),
            ("old".into(), "new".into(), "new".into())
        );
    }

    #[test]
    fn mkdir_then_copy_to_other_panel() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Panel::open(left.path()).unwrap(),
            Panel::open(right.path()).unwrap(),
        );
        key(&mut app, KeyCode::F(7));
        for c in "new".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        assert!(left.path().join("new").is_dir());
        assert_eq!(app.panels[0].current().unwrap().name, "..");
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(5));
        assert!(matches!(app.mode, Mode::Transfer(Op::Copy, _, _)));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(right.path().join("new").is_dir());
        assert!(matches!(app.mode, Mode::Normal));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Normal)); // existing dir is merged, no conflict
        // copy in the current dir under a new name (relative to the active panel)
        key(&mut app, KeyCode::F(5));
        for _ in 0..right.path().display().to_string().len() + 5 {
            key(&mut app, KeyCode::Backspace);
        }
        for c in "copy".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(left.path().join("copy").is_dir());
    }

    #[test]
    fn rates_refresh_once_a_second_and_show_all_units() {
        let (_tx, rx) = mpsc::channel();
        let (reply, _answers) = mpsc::channel();
        let t0 = Instant::now();
        let mut x = Xfer {
            label: "Copy",
            ask: None,
            error: None,
            rename: None,
            rx,
            reply,
            progress: Arc::default(),
            rates: Default::default(),
            shown: Shown::default(),
            started: t0,
        };
        let p = x.progress.clone();
        p.total.store(1000, Ordering::Relaxed);
        p.scanned.store(true, Ordering::Release);
        let step = |x: &mut Xfer, ms: u64| {
            let n = ms / 10;
            p.done.store(n * 2, Ordering::Relaxed);
            p.files.store(n, Ordering::Relaxed);
            p.dirs_done.store(n / 10, Ordering::Relaxed);
            x.sample(t0 + Duration::from_millis(ms));
        };
        step(&mut x, 0);
        assert_eq!(x.shown.rates, [None; 3]);
        for ms in (250..=750).step_by(250) {
            step(&mut x, ms);
        }
        assert_eq!(x.shown.rates, [None; 3], "no refresh within 1 s");
        step(&mut x, 1000);
        let [b, f, d] = x.shown.rates.map(Option::unwrap);
        assert!((b - 200.0).abs() < 1.0 && (f - 100.0).abs() < 1.0 && (d - 10.0).abs() < 1.0);
        assert!((x.shown.eta.unwrap() - 4.0).abs() < 0.1);
        let text = crate::fmt::progress_text(&x);
        assert!(
            text.contains("dirs") && text.contains("100.0/s") && text.contains("ETA 0:04"),
            "{text}"
        );
    }

    #[test]
    fn remote_panel_copy_delete_mkdir_and_blocked_keys() {
        let left = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a"), "data").unwrap();
        let mem = tc_core::vfs::mem::Mem::new();
        let mut app = two_panels(left.path(), left.path());
        app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
        settle(&mut app);
        assert_eq!(app.panels[1].title(), "mem:///");
        // upload: the prefilled absolute path is on the other panel's side
        app.panels[0].focus(OsStr::new("a"));
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(mem.get("/a").unwrap(), b"data");
        // download it back under a new name
        key(&mut app, KeyCode::Tab);
        app.panels[1].focus(OsStr::new("a"));
        key(&mut app, KeyCode::F(5));
        let Mode::Transfer(_, _, l) = &mut app.mode else {
            panic!("no transfer prompt")
        };
        l.text.push_str(".back");
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(std::fs::read(left.path().join("a.back")).unwrap(), b"data");
        // delete: no trash on remote, only `p`
        key(&mut app, KeyCode::F(8));
        assert!(matches!(&app.mode, Mode::Confirm(_, _, t) if t.contains("No trash")));
        key(&mut app, KeyCode::Char('y'));
        assert!(matches!(app.mode, Mode::Confirm(..)));
        key(&mut app, KeyCode::Char('p'));
        settle(&mut app);
        assert!(mem.get("/a").is_none());
        key(&mut app, KeyCode::F(7));
        type_keys(&mut app, "d");
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(mem.paths(), ["/", "/d"]);
        with(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(matches!(&app.mode, Mode::FindForm(_)));
        key(&mut app, KeyCode::Esc);
        // ".." at the remote root goes back to the local directory
        app.panels[1].set_cursor(0);
        key(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert!(app.panels[1].remote().is_none());
        assert_eq!(app.panels[1].path(), left.path());
    }

    #[test]
    fn delete_text_lists_the_answers_and_no_trash_on_remote() {
        let local = delete_text("Delete a?".into(), true, false);
        assert!(
            local.contains("recursively")
                && local.ends_with("[y] Trash   [p] Delete permanently   [Esc] Cancel")
        );
        let remote = delete_text("Delete a?".into(), false, true);
        assert!(
            !remote.contains("[y]") && remote.ends_with("[p] Delete permanently   [Esc] Cancel")
        );
    }

    #[test]
    fn ctrl_v_pastes_changed_host_files_before_app_clip() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("h.txt"), "host\n").unwrap();
        std::fs::write(src.path().join("a.txt"), "app\n").unwrap();
        let mut app = two_panels(src.path(), dst.path());
        key(&mut app, KeyCode::Tab); // paste into the destination panel
        // host files: pasted once, then the app clipboard is used again
        app.paste_with(vec![src.path().join("h.txt")]);
        wait(&mut app);
        assert!(dst.path().join("h.txt").exists());
        app.file_clip = Some(FileClip {
            op: Op::Copy,
            paths: vec![src.path().join("a.txt")],
            side: Side::Local,
        });
        app.paste_with(vec![src.path().join("h.txt")]);
        wait(&mut app);
        assert!(dst.path().join("a.txt").exists());
    }

    #[test]
    fn failed_cut_paste_keeps_the_clip() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let mut app = two_panels(src.path(), dst.path());
        app.file_clip = Some(FileClip {
            op: Op::Move,
            paths: vec![src.path().join("gone.txt")],
            side: Side::Local,
        });
        key(&mut app, KeyCode::Tab);
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(app.file_clip.is_some() && !dst.path().join("gone.txt").exists());
    }

    #[test]
    fn ctrl_c_v_copies_and_ctrl_x_v_moves_between_panels() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(src.path().join("b.txt"), "two\n").unwrap();
        let mut app = two_panels(src.path(), dst.path());
        key(&mut app, KeyCode::Down); // a.txt, the first entry after ".."
        with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Tab); // paste into the other panel
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(dst.path().join("a.txt").exists() && src.path().join("a.txt").exists());
        // cut b.txt from the left panel and paste it into the right one: it moves
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Down); // b.txt
        with(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Tab);
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(dst.path().join("b.txt").exists() && !src.path().join("b.txt").exists());
        assert!(app.file_clip.is_none());
        // nothing left to paste
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(matches!(&app.mode, Mode::Message(m) if m.starts_with("Nothing copied")));
    }
}
