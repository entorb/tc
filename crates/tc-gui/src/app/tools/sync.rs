//! Sync Dirs (Ctrl+E): opening lists the files that differ between the two panel
//! directories (top level only); F5 / F6 make the right / left side match the other.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};

use crate::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::sync::{Item, Plan, Status};
use tc_core::vfs::Side;
use tc_core::{Error, ops, sync};

use crate::app::widgets::step_key;
use crate::app::{App, FormRow, Mode, form_checked, rows_from_line, rows_to_line};
use tc_core::edit::human;

pub(in crate::app) const S_NODATE: &str = "Ignore date";
pub(in crate::app) const S_DELETE: &str = "Mirror";

/// The sync options: checkboxes, always with the keyboard.
#[derive(Clone)]
pub struct SyncForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
}

impl SyncForm {
    fn new() -> Self {
        SyncForm {
            rows: vec![
                FormRow::Check(S_NODATE, false),
                FormRow::Check(S_DELETE, false),
            ],
            focus: 0,
        }
    }

    pub fn ignore_date(&self) -> bool {
        form_checked(&self.rows, S_NODATE)
    }

    fn delete_extras(&self) -> bool {
        form_checked(&self.rows, S_DELETE)
    }
}

/// What a finished sync compares again: options, sides and roots.
pub type SyncSetup = (SyncForm, [Side; 2], [PathBuf; 2]);

/// Sync Dirs screen: the options on top, the files that differ below.
pub struct Sync {
    pub sides: [Side; 2],
    pub roots: [PathBuf; 2],
    pub form: SyncForm,
    pub items: Vec<Item>,
    /// Indices into `items` of the files that differ, with their status.
    pub view: Vec<(usize, Status)>,
    /// First shown row.
    pub top: usize,
    job: Option<JoinHandle<tc_core::Result<Vec<Item>>>>,
    pub progress: Arc<ops::Progress>,
    /// Sync question (source side, text), shown while set.
    pub confirm: Option<(usize, String)>,
}

impl Sync {
    /// Ignore date is checked.
    pub fn ignore_date(&self) -> bool {
        self.form.ignore_date()
    }

    pub fn comparing(&self) -> bool {
        self.job.is_some()
    }

    /// Takes F1 and footer clicks: no compare running, no question open.
    pub fn idle(&self) -> bool {
        !self.comparing() && self.confirm.is_none()
    }

    /// The listed files and their status.
    pub fn shown(&self) -> impl Iterator<Item = (&Item, Status)> {
        self.view.iter().map(|&(i, st)| (&self.items[i], st))
    }

    fn rebuild(&mut self) {
        let nodate = self.form.ignore_date();
        self.view = (self.items.iter().enumerate())
            .map(|(i, it)| (i, it.status(nodate)))
            .filter(|&(_, st)| st != Status::Equal)
            .collect();
        self.top = self.top.min(self.view.len().saturating_sub(1));
    }

    fn plan(&self, from: usize) -> Plan {
        sync::plan(
            &self.items,
            from,
            self.form.ignore_date(),
            self.form.delete_extras(),
        )
    }

    fn confirm_text(&self, from: usize) -> String {
        let plan = self.plan(from);
        if plan.copy.is_empty() && plan.delete.is_empty() {
            return "Nothing to do: both sides have the same files.\n\nany key".into();
        }
        let to = 1 - from;
        let copy: std::collections::HashSet<_> = plan.copy.iter().collect();
        let copied = self.items.iter().filter(|it| copy.contains(&it.name));
        let (bytes, over) = copied.fold((0, 0), |(b, o), it| {
            let size = it.side(from).map_or(0, |e| e.size);
            (b + size, o + usize::from(it.side(to).is_some()))
        });
        let names = ["left", "right"];
        let how = if self.sides[to].remote().is_some() {
            "permanently"
        } else {
            "to trash"
        };
        format!(
            "Sync Dirs {} -> {}?\n\nCopy: {} ({})\nOverwrite existing: {over}\n\
             Delete on {} ({how}): {}\n\n[y] Start   [Esc] Cancel",
            names[from],
            names[to],
            plan.copy.len(),
            human(bytes),
            names[to],
            plan.delete.len()
        )
    }

    fn scroll(&mut self, code: KeyCode, page: usize) {
        let last = self.view.len().saturating_sub(1);
        self.top = match code {
            KeyCode::PageUp => self.top.saturating_sub(page),
            KeyCode::PageDown => (self.top + page).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => self.top,
        };
    }
}

impl App {
    /// Ctrl+E: compare the two panel directories at once, with the options last used.
    pub(in crate::app) fn sync_open(&mut self) {
        if self
            .panels
            .iter()
            .any(|p| p.archive().is_some() || p.is_virtual())
        {
            self.mode = Mode::Message(
                "Sync Dirs works on two directories, not in archives or found lists.".into(),
            );
            return;
        }
        let mut form = SyncForm::new();
        if let Some(line) = self.syncs.last() {
            rows_from_line(&mut form.rows, line);
        }
        let sides = [self.panels[0].side(), self.panels[1].side()];
        let roots = [0, 1].map(|i| self.panels[i].path().to_owned());
        self.sync_spawn((form, sides, roots));
    }

    /// Compare in a worker; the status line shows progress until it is done.
    pub(in crate::app) fn sync_spawn(&mut self, (form, sides, roots): SyncSetup) {
        let progress = Arc::new(ops::Progress::default());
        let (p, s, r) = (progress.clone(), sides.clone(), roots.clone());
        let job = thread::spawn(move || sync::compare(&s, &r, &p));
        self.mode = Mode::Sync(Box::new(Sync {
            sides,
            roots,
            form,
            items: Vec::new(),
            view: Vec::new(),
            top: 0,
            job: Some(job),
            progress,
            confirm: None,
        }));
    }

    pub(in crate::app) fn sync_key(&mut self, key: KeyEvent, mut s: Box<Sync>) {
        if let Some((from, _)) = s.confirm.take() {
            return self.sync_confirm_key(s, from, key.code);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // the options work while comparing: the result is listed with them
            KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up => {
                s.form.focus = step_key(key.code, s.form.focus, s.form.rows.len());
            }
            KeyCode::Char(' ') => {
                let focus = s.form.focus;
                if let Some(row) = s.form.rows.get_mut(focus) {
                    row.key(key);
                }
                s.rebuild();
                self.note = self.remember("sync-forms", &rows_to_line(&s.form.rows, ""));
            }
            KeyCode::Esc | KeyCode::F(10) if s.comparing() => {
                s.progress.cancel.store(true, Ordering::Relaxed);
            }
            _ if s.comparing() => {}
            KeyCode::Esc | KeyCode::F(10) => return,
            KeyCode::F(2) => return self.sync_spawn((s.form, s.sides, s.roots)),
            KeyCode::Char('r') if ctrl => return self.sync_spawn((s.form, s.sides, s.roots)),
            KeyCode::F(5) => s.confirm = Some((0, s.confirm_text(0))),
            KeyCode::F(6) => s.confirm = Some((1, s.confirm_text(1))),
            code => s.scroll(code, self.page),
        }
        self.mode = Mode::Sync(s);
    }

    fn sync_confirm_key(&mut self, s: Box<Sync>, from: usize, code: KeyCode) {
        let plan = s.plan(from);
        let empty = plan.copy.is_empty() && plan.delete.is_empty();
        if empty || code != KeyCode::Char('y') {
            self.mode = Mode::Sync(s);
            return;
        }
        let (sides, roots) = (s.sides.clone(), s.roots.clone());
        self.sync_again = Some((s.form, s.sides, s.roots));
        let pair = (sides[0].clone(), sides[1].clone());
        self.spawn_xfer("Sync Dirs", pair, move |p, _| {
            sync::run(&sides, &roots, from, &plan, p)
        });
    }

    /// Collect a finished compare.
    pub(in crate::app) fn sync_tick(&mut self) {
        let Mode::Sync(s) = &mut self.mode else {
            return;
        };
        if !s.job.as_ref().is_some_and(JoinHandle::is_finished) {
            return;
        }
        let Some(job) = s.job.take() else {
            return;
        };
        match job.join() {
            Ok(Ok(items)) => {
                s.items = items;
                s.rebuild();
            }
            // nothing listed; F2 reloads
            Ok(Err(Error::Cancelled)) => {}
            Ok(Err(e)) => self.mode = Mode::Message(format!("Sync Dirs: {e}")),
            Err(_) => self.mode = Mode::Message("Sync Dirs: compare failed.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{App, Mode, S_DELETE, S_NODATE};
    use crate::{KeyCode, KeyEvent, KeyModifiers};
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};
    use tc_core::panel::Panel;
    use tc_core::sync::Status;

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn put(p: &Path, text: &str, age: u64) {
        fs::write(p, text).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age);
        let f = fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    /// Ticks until the compare result is listed.
    fn compared(app: &mut App) -> Vec<(String, Status)> {
        for _ in 0..5000 {
            app.tick();
            if let Mode::Sync(s) = &app.mode
                && !s.comparing()
            {
                let rows = s
                    .shown()
                    .map(|(i, st)| (i.name.to_string_lossy().into(), st));
                return rows.collect();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("no compare result");
    }

    fn setup() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        App,
    ) {
        let t = tempfile::tempdir().unwrap();
        let (l, r) = (t.path().join("l"), t.path().join("r"));
        fs::create_dir_all(l.join("sub")).unwrap();
        fs::create_dir_all(&r).unwrap();
        put(&l.join("same.txt"), "s", 100);
        put(&r.join("same.txt"), "s", 100);
        put(&l.join("a.txt"), "old", 100);
        put(&r.join("a.txt"), "new", 10);
        put(&l.join("left.txt"), "l", 100);
        put(&r.join("right.txt"), "r", 100);
        let app = App::new(Panel::open(&l).unwrap(), Panel::open(&r).unwrap());
        (t, l, r, app)
    }

    fn checked(app: &App, label: &str) -> bool {
        matches!(&app.mode, Mode::Sync(s) if super::form_checked(&s.form.rows, label))
    }

    #[test]
    fn ctrl_e_lists_differences_and_f5_makes_right_match_left() {
        let (_t, l, r, mut app) = setup();
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(
            compared(&mut app),
            [
                ("a.txt".into(), Status::Differs),
                ("left.txt".into(), Status::LeftOnly),
                ("right.txt".into(), Status::RightOnly),
            ]
        );
        key(&mut app, KeyCode::F(5));
        assert!(matches!(&app.mode, Mode::Sync(s)
            if s.confirm.as_ref().is_some_and(|(_, c)| c.contains("Copy: 2") && c.contains("Overwrite existing: 1"))));
        key(&mut app, KeyCode::Char('y'));
        let after = compared(&mut app);
        // the newer right file is overwritten, the right extra stays
        assert_eq!(fs::read_to_string(r.join("a.txt")).unwrap(), "old");
        assert!(r.join("left.txt").exists() && !r.join("sub").exists());
        assert_eq!(after, [("right.txt".into(), Status::RightOnly)]);
        assert!(l.join("left.txt").exists());
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn f6_with_delete_extras_makes_left_match_right() {
        let (_t, l, _r, mut app) = setup();
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        compared(&mut app);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char(' '));
        assert!(checked(&app, S_DELETE));
        key(&mut app, KeyCode::F(6));
        key(&mut app, KeyCode::Esc); // no sync
        assert!(l.join("left.txt").exists());
        key(&mut app, KeyCode::F(6));
        let Mode::Sync(s) = &app.mode else {
            panic!("no sync");
        };
        assert!(
            s.confirm
                .as_ref()
                .is_some_and(|(_, c)| c.contains("Delete on left (to trash): 1"))
        );
    }

    #[test]
    fn ignore_date_relists_without_compare_and_f10_closes() {
        let (_t, _l, _r, mut app) = setup();
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        compared(&mut app);
        key(&mut app, KeyCode::Char(' '));
        assert!(checked(&app, S_NODATE));
        let Mode::Sync(s) = &app.mode else {
            panic!("no sync");
        };
        assert!(!s.comparing());
        let names: Vec<_> = s
            .shown()
            .map(|(i, _)| i.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["left.txt", "right.txt"]);
        key(&mut app, KeyCode::F(10));
        assert!(matches!(app.mode, Mode::Normal));
    }
}
