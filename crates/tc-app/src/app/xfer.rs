//! Copy, move, delete and Ctrl+C / Ctrl+X / Ctrl+V of files: workers, conflicts, progress.

use super::{App, Done, Mode, Op, READ_ONLY, edit_key};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::borrow::Cow;
use std::ffi::OsString;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
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
    Cancel,
}

pub struct Xfer {
    pub ask: Option<(PathBuf, PathBuf)>,
    /// Name being typed after `r`.
    pub rename: Option<LineEdit>,
    pub(super) rx: Receiver<XMsg>,
    pub(super) reply: mpsc::Sender<Answer>,
    pub progress: Arc<ops::Progress>,
    /// Fed from `App::tick`.
    pub eta: Estimator,
    pub(super) started: Instant,
}

impl Xfer {
    /// Quick copies finish before a popup would only flicker.
    pub fn visible(&self) -> bool {
        self.ask.is_some() || self.started.elapsed() > Duration::from_secs(1)
    }

    pub(super) fn answer(&mut self, a: Answer) {
        self.ask = None;
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
        } else if let Some((_, target)) = &self.ask {
            use Policy::*;
            match code {
                KeyCode::Char('o') => self.answer(Answer::One(Overwrite)),
                KeyCode::Char('s') => self.answer(Answer::One(Skip)),
                KeyCode::Char('n') => self.answer(Answer::One(Newer)),
                KeyCode::Char('O') => self.answer(Answer::All(Overwrite)),
                KeyCode::Char('S') => self.answer(Answer::All(Skip)),
                KeyCode::Char('N') => self.answer(Answer::All(Newer)),
                KeyCode::Char('r') => {
                    let name = target.file_name().unwrap_or_default();
                    self.rename = Some(LineEdit::from(name.to_string_lossy().into_owned()));
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('c') => self.answer(Answer::Cancel),
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

/// Copy or move `jobs` (source, target) between the sides; stops at the first error.
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
        if !local {
            vfs::transfer(src_side, src, dst_side, target, mv, ask, c)
        } else if mv {
            ops::move_(src, target, ask, c)
        } else {
            ops::copy(src, target, ask, c)
        }
    })
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
        if side.remote().is_some() {
            let targets = targets.to_vec();
            let sides = (side.clone(), side.clone());
            return self.spawn_xfer(sides, move |_, _| {
                targets.iter().try_for_each(|t| side.delete(t))
            });
        }
        let result = targets.iter().try_for_each(|src| {
            if to_trash {
                ops::trash(src)
            } else {
                ops::delete(src)
            }
        });
        self.finish(result.err().map(|e| e.to_string()));
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
        self.spawn_xfer(sides.clone(), move |c, ask| {
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
        self.spawn_xfer(sides.clone(), move |c, ask| {
            copy_jobs(op, &jobs, &sides, c, ask)
        });
    }

    /// Run `work` in a worker with progress and the conflict dialog (`sides` decide "newer").
    pub(super) fn spawn_xfer(
        &mut self,
        sides: (Side, Side),
        work: impl FnOnce(&ops::Progress, ops::Ask) -> tc_core::Result<()> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let (reply, answers) = mpsc::channel();
        let progress = Arc::new(ops::Progress::default());
        let c = progress.clone();
        thread::spawn(move || {
            let mut sticky: Option<Policy> = None;
            let (from, to) = sides;
            let mut ask = |src: &Path, target: &Path| {
                let policy = match sticky {
                    Some(p) => p,
                    None => {
                        let msg = XMsg::Ask(src.to_owned(), target.to_owned());
                        if tx.send(msg).is_err() {
                            return ops::Decision::Cancel;
                        }
                        match answers.recv() {
                            Ok(Answer::One(p)) => p,
                            Ok(Answer::All(p)) => *sticky.insert(p),
                            Ok(Answer::Rename(n)) => return ops::Decision::Rename(n.into()),
                            Ok(Answer::Cancel) | Err(_) => return ops::Decision::Cancel,
                        }
                    }
                };
                match policy {
                    Policy::Overwrite => ops::Decision::Overwrite,
                    Policy::Newer if vfs::is_newer(&from, src, &to, target) => {
                        ops::Decision::Overwrite
                    }
                    Policy::Newer | Policy::Skip => ops::Decision::Skip,
                }
            };
            let result = work(&c, &mut ask);
            let error = result.err().filter(|e| !matches!(e, Error::Cancelled));
            let _ = tx.send(XMsg::Done(error.map(|e| e.to_string())));
        });
        self.mode = Mode::Xfer(Xfer {
            ask: None,
            rename: None,
            rx,
            reply,
            progress,
            eta: Estimator::default(),
            started: Instant::now(),
        });
    }

    pub(super) fn mkdir(&mut self, name: &str) {
        let panel = &self.panels[self.active];
        let side = panel.side();
        if side.remote().is_some() {
            let (dir, name) = (panel.path().to_owned(), OsString::from(name));
            let sides = (side.clone(), side.clone());
            return self.spawn_xfer(sides, move |_, _| side.mkdir_in(&dir, &name).map(drop));
        }
        let result = ops::mkdir(panel.path(), &OsString::from(name));
        self.finish(result.err().map(|e| e.to_string()));
    }

    pub(super) fn finish(&mut self, error: Option<String>) {
        let mut errors: Vec<String> = error.into_iter().collect();
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
        let done = x.progress.done.load(Ordering::Relaxed);
        x.eta.push(x.started.elapsed().as_secs_f64(), done as f64);
        match x.rx.try_recv() {
            Ok(XMsg::Ask(src, target)) => x.ask = Some((src, target)),
            Ok(XMsg::Done(error)) => {
                let progress = x.progress.clone();
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
