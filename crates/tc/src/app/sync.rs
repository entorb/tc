//! Synchronize directories (Ctrl+E, like Total Commander): options form, compare worker,
//! result list with per-file direction, then copy / delete in a transfer worker.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::cmdline::LineEdit;
use tc_core::sync::{Action, Item};
use tc_core::vfs::Side;
use tc_core::{Error, ops, sync};

use super::{
    App, Filter, FormRow, Mode, form_checked, form_text, paste_line, rows_from_line, rows_to_line,
};
use crate::fmt;
use tc_core::edit::human;

const S_MASK: &str = "File mask";
const S_SUBDIRS: &str = "Subdirectories";
const S_CONTENT: &str = "By content (same size: compare bytes)";
const S_NODATE: &str = "Ignore date (same size = equal)";
const S_ASYM: &str = "Asymmetric (right becomes a copy, deletes)";

/// Sync options form; Enter compares the two panel directories.
#[derive(Clone)]
pub struct SyncForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub error: Option<String>,
    /// History entry shown (`App::syncs.len()` = fresh).
    hist: usize,
}

impl SyncForm {
    pub fn new() -> Self {
        SyncForm {
            rows: vec![
                FormRow::Text(S_MASK, LineEdit::from("*".to_owned())),
                FormRow::Check(S_SUBDIRS, true),
                FormRow::Check(S_CONTENT, false),
                FormRow::Check(S_NODATE, false),
                FormRow::Check(S_ASYM, false),
            ],
            focus: 0,
            error: None,
            hist: 0,
        }
    }

    fn options(&self) -> sync::Options {
        let on = |l| form_checked(&self.rows, l);
        sync::Options {
            mask: form_text(&self.rows, S_MASK).trim().to_owned(),
            subdirs: on(S_SUBDIRS),
            by_content: on(S_CONTENT),
            ignore_date: on(S_NODATE),
            asymmetric: on(S_ASYM),
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let FormRow::Text(_, l) = &mut self.rows[self.focus] {
            paste_line(l, text);
        }
    }
}

impl Default for SyncForm {
    fn default() -> Self {
        Self::new()
    }
}

/// What a finished sync compares again: options, sides and roots.
pub type SyncSetup = (SyncForm, [Side; 2], [PathBuf; 2]);

type DiffJob = JoinHandle<tc_core::Result<Option<(u64, u64)>>>;

/// Compare result screen.
pub struct Sync {
    pub sides: [Side; 2],
    pub roots: [PathBuf; 2],
    pub form: SyncForm,
    pub items: Vec<Item>,
    /// Indices into `items` of the shown rows.
    pub view: Vec<usize>,
    /// Shown groups by compare result: `->`, `=`, `!=`, `<-` (with `del`).
    pub show: [bool; 4],
    pub sel: usize,
    /// `/` filter on the relative path.
    pub filter: Filter,
    job: Option<JoinHandle<tc_core::Result<Vec<Item>>>>,
    /// `c`: first difference of one file pair (path below the roots).
    diff: Option<(PathBuf, DiffJob)>,
    pub progress: Arc<ops::Progress>,
    /// Sync question, shown while set.
    pub confirm: Option<String>,
}

/// Show-filter group of a compare result.
fn group(a: Action) -> usize {
    match a {
        Action::ToRight => 0,
        Action::Equal => 1,
        Action::NotEqual | Action::Skip => 2,
        Action::ToLeft | Action::DeleteRight | Action::DeleteLeft | Action::DeleteBoth => 3,
    }
}

fn acts(a: Action) -> bool {
    matches!(
        a,
        Action::ToRight
            | Action::ToLeft
            | Action::DeleteRight
            | Action::DeleteLeft
            | Action::DeleteBoth
    )
}

/// Planned deletes on side `i` (a both-delete counts on each side).
fn deletes(plan: &[Item], i: usize) -> usize {
    let (one, both) = if i == 0 {
        (Action::DeleteLeft, Action::DeleteBoth)
    } else {
        (Action::DeleteRight, Action::DeleteBoth)
    };
    plan.iter()
        .filter(|it| it.action == one || it.action == both)
        .count()
}

impl Sync {
    pub fn comparing(&self) -> bool {
        self.job.is_some()
    }

    /// The file pair being compared with `c`.
    pub fn diffing(&self) -> Option<&Path> {
        self.diff.as_ref().map(|(rel, _)| rel.as_path())
    }

    /// A tree compare or a file compare runs.
    fn busy(&self) -> bool {
        self.comparing() || self.diff.is_some()
    }

    /// Takes F1 and footer clicks: nothing running, no question open.
    pub fn idle(&self) -> bool {
        !self.busy() && self.confirm.is_none()
    }

    pub fn shown(&self) -> impl Iterator<Item = &Item> {
        self.view.iter().map(|&i| &self.items[i])
    }

    fn rebuild(&mut self) {
        let show = self.show;
        self.view = (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                show[group(it.default)] && self.filter.matches(&it.rel.to_string_lossy())
            })
            .collect();
        self.sel = self.sel.min(self.view.len().saturating_sub(1));
    }

    /// Shown rows a sync acts on (hidden rows are left alone).
    pub fn plan(&self) -> Vec<Item> {
        self.shown().filter(|it| acts(it.action)).cloned().collect()
    }

    fn confirm_text(&self) -> String {
        let plan = self.plan();
        if plan.is_empty() {
            return "Nothing to do: no shown row copies or deletes.\n\nany key".into();
        }
        let sum = |a: Action| {
            let it = plan.iter().filter(|it| it.action == a);
            let entries = it.map(|it| {
                if a == Action::ToLeft {
                    &it.right
                } else {
                    &it.left
                }
            });
            entries
                .flatten()
                .fold((0, 0), |(n, b), e| (n + 1, b + e.size))
        };
        let (right, rb) = sum(Action::ToRight);
        let (left, lb) = sum(Action::ToLeft);
        let over = (plan.iter())
            .filter(|it| match it.action {
                Action::ToRight => it.right.is_some(),
                Action::ToLeft => it.left.is_some(),
                _ => false,
            })
            .count();
        let trash = |i: usize| {
            if self.sides[i].remote().is_some() {
                "permanently"
            } else {
                "to trash"
            }
        };
        format!(
            "Synchronize?\n\nCopy left -> right: {right} ({})\nCopy right -> left: {left} ({})\n\
             Overwrite existing: {over}\nDelete on left ({}): {}\nDelete on right ({}): {}\n\n[y] Start   [Esc] Cancel",
            human(rb),
            human(lb),
            trash(0),
            deletes(&plan, 0),
            trash(1),
            deletes(&plan, 1)
        )
    }

    fn nav(&mut self, code: KeyCode, page: usize) {
        let last = self.view.len().saturating_sub(1);
        self.sel = match code {
            KeyCode::Up => self.sel.saturating_sub(1),
            KeyCode::Down => (self.sel + 1).min(last),
            KeyCode::PageUp => self.sel.saturating_sub(page),
            KeyCode::PageDown => (self.sel + page).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => self.sel,
        };
    }

    /// Left / Right set the copy direction (needs the source), `l` `r` `b` delete left / right / both, Space drops or restores the action.
    fn set_action(&mut self, code: KeyCode) {
        let Some(it) = self.view.get(self.sel).map(|&i| &mut self.items[i]) else {
            return;
        };
        it.action = match code {
            KeyCode::Right if it.left.is_some() => Action::ToRight,
            KeyCode::Left if it.right.is_some() => Action::ToLeft,
            KeyCode::Char('l') if it.left.is_some() => Action::DeleteLeft,
            KeyCode::Char('r') if it.right.is_some() => Action::DeleteRight,
            KeyCode::Char('b') if it.left.is_some() && it.right.is_some() => Action::DeleteBoth,
            KeyCode::Char(' ') if it.action == Action::Skip => it.default,
            KeyCode::Char(' ') if acts(it.action) => Action::Skip,
            _ => it.action,
        };
    }

    /// `*`: flip `->` and `<-` of the shown rows that exist on both sides.
    fn reverse(&mut self) {
        for &i in &self.view {
            let it = &mut self.items[i];
            if it.left.is_none() || it.right.is_none() {
                continue;
            }
            it.action = match it.action {
                Action::ToRight => Action::ToLeft,
                Action::ToLeft => Action::ToRight,
                a => a,
            };
        }
    }

    /// `c`: compare the two files of the cursor row in a worker.
    fn diff_spawn(&mut self) {
        let Some(it) = self.view.get(self.sel).map(|&i| &self.items[i]) else {
            return;
        };
        let (Some(l), Some(r)) = (&it.left, &it.right) else {
            return;
        };
        if l.is_dir || r.is_dir {
            return;
        }
        let paths = [0, 1].map(|i| self.roots[i].join(&it.rel));
        let (rel, total, sides) = (it.rel.clone(), l.size, self.sides.clone());
        // fresh progress: the finished tree compare may have its cancel flag set
        let p = Arc::new(ops::Progress::default());
        p.total.store(total, Ordering::Relaxed);
        self.progress = p.clone();
        let job = thread::spawn(move || sync::first_diff(&sides, &paths, &p));
        self.diff = Some((rel, job));
    }

    fn toggle_show(&mut self, c: char) {
        if let Some(g) = ['>', '=', '!', '<'].iter().position(|&k| k == c) {
            self.show[g] = !self.show[g];
            self.rebuild();
        }
    }
}

impl App {
    /// Ctrl+E: options form for the two panel directories, opened with the last settings (also after a restart).
    pub(super) fn sync_open(&mut self) {
        if self
            .panels
            .iter()
            .any(|p| p.archive().is_some() || p.is_virtual())
        {
            self.mode = Mode::Message(
                "Synchronize works on two directories, not in archives or found lists.".into(),
            );
            return;
        }
        let len = self.syncs.len();
        let form = match self.last_sync.take() {
            Some(f) => SyncForm { hist: len, ..f },
            None => self.sync_hist(len.saturating_sub(1)),
        };
        self.mode = Mode::SyncForm(form);
    }

    /// Sync form of history entry `i` (past the end: the defaults), positioned there.
    fn sync_hist(&self, i: usize) -> SyncForm {
        let mut form = SyncForm::new();
        if let Some(line) = self.syncs.get(i) {
            rows_from_line(&mut form.rows, line);
        }
        SyncForm { hist: i, ..form }
    }

    pub(super) fn sync_form_key(&mut self, key: KeyEvent, mut form: SyncForm) {
        let last = form.rows.len() - 1;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => {
                self.last_sync = Some(form);
                return;
            }
            KeyCode::Enter => {
                self.note = self.remember("sync-forms", &rows_to_line(&form.rows, ""));
                self.last_sync = Some(form.clone());
                let sides = [self.panels[0].side(), self.panels[1].side()];
                let roots = [0, 1].map(|i| self.panels[i].path().to_owned());
                return self.sync_spawn((form, sides, roots));
            }
            KeyCode::Up | KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                let i = match key.code {
                    KeyCode::Up => form.hist.saturating_sub(1),
                    _ => (form.hist + 1).min(self.syncs.len()),
                };
                let focus = form.focus;
                form = SyncForm {
                    focus,
                    ..self.sync_hist(i)
                };
            }
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => form.focus = form.focus.saturating_sub(1),
            code => {
                let ctrl = key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                form.rows[form.focus].key(code, ctrl);
            }
        }
        self.mode = Mode::SyncForm(form);
    }

    /// Compare in a worker; the result screen shows progress until it is done.
    pub(super) fn sync_spawn(&mut self, (form, sides, roots): SyncSetup) {
        let progress = Arc::new(ops::Progress::default());
        let (p, s, r, o) = (
            progress.clone(),
            sides.clone(),
            roots.clone(),
            form.options(),
        );
        self.mode = Mode::Sync(Box::new(Sync {
            sides,
            roots,
            form,
            items: Vec::new(),
            view: Vec::new(),
            show: [true; 4],
            sel: 0,
            filter: Filter::default(),
            job: Some(thread::spawn(move || sync::compare(&s, &r, &o, &p))),
            diff: None,
            progress,
            confirm: None,
        }));
    }

    pub(super) fn sync_key(&mut self, key: KeyEvent, mut s: Box<Sync>) {
        if s.confirm.is_some() {
            return self.sync_confirm_key(s, key.code);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if !s.busy() && s.filter.key(key) {
            s.rebuild();
            self.mode = Mode::Sync(s);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') if s.busy() => {
                s.progress.cancel.store(true, Ordering::Relaxed)
            }
            _ if s.busy() => {}
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
            KeyCode::Char('r') if ctrl => return self.sync_spawn((s.form, s.sides, s.roots)),
            KeyCode::F(3 | 4) => return self.sync_row_open(s, key.code == KeyCode::F(4)),
            KeyCode::Enter => s.confirm = Some(s.confirm_text()),
            KeyCode::Left | KeyCode::Right => s.set_action(key.code),
            KeyCode::Char(' ' | 'l' | 'r' | 'b') if !ctrl && !alt => s.set_action(key.code),
            KeyCode::Char('*') if !ctrl && !alt => s.reverse(),
            KeyCode::Char('c') if !ctrl && !alt => s.diff_spawn(),
            KeyCode::Char(c) if !ctrl && !alt => s.toggle_show(c),
            code => s.nav(code, self.page),
        }
        self.mode = Mode::Sync(s);
    }

    /// F3 views, F4 edits the file of the row on the left, or on the right when only that has it.
    fn sync_row_open(&mut self, s: Box<Sync>, edit: bool) {
        let row = s.view.get(s.sel).map(|&i| &s.items[i]);
        let Some(it) = row.filter(|it| !it.is_dir()) else {
            self.mode = Mode::Sync(s);
            return;
        };
        let side = usize::from(it.left.is_none());
        let path = s.roots[side].join(&it.rel);
        let remote = s.sides[side].remote().is_some();
        self.sync_back = Some(s);
        if remote {
            self.mode = Mode::Message("Remote rows are only listed.".into());
        } else {
            self.open_at(path, None, edit);
        }
    }

    fn sync_confirm_key(&mut self, mut s: Box<Sync>, code: KeyCode) {
        let plan = s.plan();
        s.confirm = None;
        if plan.is_empty() || code != KeyCode::Char('y') {
            self.mode = Mode::Sync(s);
            return;
        }
        let (sides, roots) = (s.sides.clone(), s.roots.clone());
        self.sync_again = Some((s.form, s.sides, s.roots));
        let pair = (sides[0].clone(), sides[1].clone());
        self.spawn_xfer(pair, move |p, _| sync::run(&sides, &roots, &plan, p));
    }

    /// Collect a finished tree compare or file compare.
    pub(super) fn sync_tick(&mut self) {
        let Mode::Sync(s) = &self.mode else {
            return;
        };
        if s.job.as_ref().is_some_and(JoinHandle::is_finished) {
            self.compare_tick();
        } else if s.diff.as_ref().is_some_and(|(_, j)| j.is_finished()) {
            self.diff_tick();
        }
    }

    fn compare_tick(&mut self) {
        let Mode::Sync(s) = &mut self.mode else {
            return;
        };
        let Some(job) = s.job.take() else {
            return;
        };
        match job.join() {
            Ok(Ok(items)) => {
                s.items = items;
                s.rebuild();
            }
            Ok(Err(Error::Cancelled)) => self.mode = Mode::Normal,
            Ok(Err(e)) => self.mode = Mode::Message(format!("Synchronize: {e}")),
            Err(_) => self.mode = Mode::Message("Synchronize: compare failed.".into()),
        }
    }

    /// Show the result of `c` as a message over the rows (cancelled: back to the rows).
    fn diff_tick(&mut self) {
        let Mode::Sync(s) = &mut self.mode else {
            return;
        };
        let Some((rel, job)) = s.diff.take() else {
            return;
        };
        let size = |e: &Option<tc_core::panel::Entry>| e.as_ref().map_or(0, |e| e.size);
        let item = s.items.iter().find(|it| it.rel == rel);
        let sizes = item.map_or([0, 0], |it| [size(&it.left), size(&it.right)]);
        let text = match job.join() {
            Ok(Err(Error::Cancelled)) => return,
            Ok(res) => fmt::sync_diff_text(&rel, sizes, &res),
            Err(_) => "Compare: failed.".into(),
        };
        if let Mode::Sync(s) = std::mem::replace(&mut self.mode, Mode::Message(text)) {
            self.sync_back = Some(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{App, Mode};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::fs;
    use std::time::Duration;
    use tc_core::panel::Panel;
    use tc_core::sync::Action;

    fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
        app.on_key(KeyEvent::new(code, mods));
    }

    /// Ticks until the compare result is shown.
    fn compared(app: &mut App) -> Vec<(String, Action)> {
        for _ in 0..5000 {
            app.tick();
            if let Mode::Sync(s) = &app.mode
                && !s.comparing()
            {
                let rows = s.shown().map(|i| (i.rel.display().to_string(), i.action));
                return rows.collect();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("no compare result");
    }

    #[test]
    fn f3_opens_the_left_file_else_the_right_and_returns_to_the_rows() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        fs::write(l.join("a.txt"), "left").unwrap();
        fs::write(r.join("a.txt"), "right").unwrap();
        fs::write(r.join("b.txt"), "only right").unwrap();
        let mut app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        compared(&mut app);
        for (name, text) in [("a.txt", "left"), ("b.txt", "only right")] {
            let Mode::Sync(s) = &app.mode else {
                panic!("no sync");
            };
            assert_eq!(s.shown().nth(s.sel).unwrap().rel.to_str(), Some(name));
            key(&mut app, KeyCode::F(3), KeyModifiers::NONE);
            let Mode::View { view, .. } = &mut app.mode else {
                panic!("no viewer");
            };
            assert_eq!(view.lines(1, 80).unwrap(), [text]);
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            app.tick();
            key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        }
        assert!(matches!(&app.mode, Mode::Sync(s) if s.sel == 1));
    }

    #[test]
    fn slash_filters_sync_rows() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        fs::write(l.join("a.txt"), "a").unwrap();
        fs::write(l.join("B.txt"), "b").unwrap();
        let mut app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        compared(&mut app);
        key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('b'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(compared(&mut app), [("B.txt".into(), Action::ToRight)]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        compared(&mut app);
        assert!(r.join("B.txt").exists() && !r.join("a.txt").exists());
    }

    #[test]
    fn ctrl_e_compares_syncs_shown_rows_and_compares_again() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        fs::write(l.join("a.txt"), "a").unwrap();
        fs::write(l.join("skip.txt"), "s").unwrap();
        fs::write(r.join("b.txt"), "b").unwrap();
        let mut app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(matches!(app.mode, Mode::SyncForm(_)));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            compared(&mut app),
            [
                ("a.txt".into(), Action::ToRight),
                ("b.txt".into(), Action::ToLeft),
                ("skip.txt".into(), Action::ToRight)
            ]
        );
        key(&mut app, KeyCode::Char('<'), KeyModifiers::NONE); // hide b.txt
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE); // skip.txt: no action
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            matches!(&app.mode, Mode::Sync(s) if s.confirm.as_ref().is_some_and(|c| c.contains("left -> right: 1")))
        );
        key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        let after = compared(&mut app);
        assert_eq!(fs::read_to_string(r.join("a.txt")).unwrap(), "a");
        assert!(!l.join("b.txt").exists() && !r.join("skip.txt").exists());
        assert_eq!(after[0], ("a.txt".into(), Action::Equal));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn l_r_b_set_deletes_and_confirm_counts_them() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        fs::write(l.join("a.txt"), "same").unwrap();
        fs::write(r.join("a.txt"), "same").unwrap();
        fs::write(l.join("b.txt"), "b").unwrap();
        let mut app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        compared(&mut app);
        key(&mut app, KeyCode::Char('b'), KeyModifiers::NONE); // a.txt: both sides
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE); // b.txt: no right entry, no change
        key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let Mode::Sync(s) = &app.mode else {
            panic!("no sync");
        };
        let text = s.confirm.as_deref().unwrap();
        assert!(text.contains("left (to trash): 2") && text.contains("right (to trash): 1"));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // Enter does not start
        assert!(matches!(app.mode, Mode::Sync(_)));
        assert!(l.join("a.txt").exists() && l.join("b.txt").exists() && r.join("a.txt").exists());
    }

    #[test]
    fn modified_letters_do_not_mark_deletes() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        fs::write(l.join("a.txt"), "same").unwrap();
        fs::write(r.join("a.txt"), "same").unwrap();
        let mut app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        compared(&mut app);
        for m in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            for c in ['l', 'b', 'r', '*'] {
                key(&mut app, KeyCode::Char(c), m);
            }
        }
        let Mode::Sync(s) = &app.mode else {
            panic!("no sync");
        };
        assert!(s.items.iter().all(|it| it.action == Action::Equal));
    }

    #[test]
    fn star_reverses_only_shown_two_sided_rows() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        for (d, name, text, age) in [
            (&l, "a.txt", "new", 10),
            (&r, "a.txt", "old", 100),
            (&l, "z.txt", "new", 10),
            (&r, "z.txt", "old", 100),
        ] {
            let f = d.join(name);
            fs::write(&f, text).unwrap();
            let t = std::time::SystemTime::now() - Duration::from_secs(age);
            fs::File::options()
                .write(true)
                .open(f)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        fs::write(r.join("ba.txt"), "only right").unwrap();
        let mut app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        compared(&mut app);
        for c in ['/', 'a'] {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // shows a.txt, ba.txt; z.txt hidden
        compared(&mut app);
        key(&mut app, KeyCode::Char('*'), KeyModifiers::NONE);
        let Mode::Sync(s) = &app.mode else {
            panic!("no sync");
        };
        let acts: Vec<_> = s.items.iter().map(|i| i.action).collect();
        assert_eq!(acts, [Action::ToLeft, Action::ToLeft, Action::ToRight]);
    }

    #[test]
    fn c_compares_pair_and_returns_to_rows() {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        fs::write(l.join("a.txt"), "ab\ncd").unwrap();
        fs::write(r.join("a.txt"), "ab\ncX!").unwrap();
        let mut app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        key(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        compared(&mut app);
        key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        for _ in 0..5000 {
            app.tick();
            if matches!(app.mode, Mode::Message(_)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let Mode::Message(text) = &app.mode else {
            panic!("no result");
        };
        assert!(text.contains("First difference at byte 4 (line 2)"));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.tick();
        assert!(matches!(&app.mode, Mode::Sync(s) if s.sel == 0 && s.idle()));
    }
}
