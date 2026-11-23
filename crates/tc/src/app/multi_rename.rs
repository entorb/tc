//! Multi-rename (Ctrl+M) form, presets, names from the editor, undo; F2 / Shift+F6 rename.

use super::{
    App, FormRow, Mode, Op, PresetKind, READ_ONLY, form_checked, form_text, paste_line, preset_key,
    rows_from_line, rows_to_line,
};
use crate::fmt;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tc_core::archive::TempDir;
use tc_core::cmdline::LineEdit;
use tc_core::vfs::Side;
use tc_core::{ops, rename};

// Multi-rename form row labels.
pub(super) const R_NAME: &str = "Name mask";

pub(super) const R_EXT: &str = "Extension mask";

pub(super) const R_FIND: &str = "Find";

pub(super) const R_REPLACE: &str = "Replace with";

pub(super) const R_REGEX: &str = "Find is a regex";

pub(super) const R_CASE_SENS: &str = "Find case-sensitive";

pub(super) const R_CASE: &str = "Case";

pub(super) const R_START: &str = "Counter start";

pub(super) const R_STEP: &str = "Counter step";

pub(super) const R_WIDTH: &str = "Counter digits";

pub(super) const R_PRESET: &str = "Preset";

pub(super) const RENAME_NAMES_FILE: &str = "new-names.txt";

pub(super) const NAMES_NOTE: &str = "New names from the list; edit a field to use the masks again.";

pub(super) const CASES: [(&str, rename::Case); 5] = [
    ("unchanged", rename::Case::Keep),
    ("lower", rename::Case::Lower),
    ("UPPER", rename::Case::Upper),
    ("First letter", rename::Case::First),
    ("Every Word", rename::Case::Words),
];

pub(super) const CASE_NAMES: [&str; 5] =
    [CASES[0].0, CASES[1].0, CASES[2].0, CASES[3].0, CASES[4].0];

/// A finished multi-rename: where it ran and what it renamed, for Ctrl+Z.
#[derive(Clone)]
pub(super) struct RenameBatch {
    // shortcut: a remote side's session stays open in the undo memory, upgrade by clearing it on disconnect
    pub(super) side: Side,
    pub(super) dir: PathBuf,
    pub(super) pairs: Vec<(String, String)>,
}

pub(super) const RENAME_KIND: PresetKind = PresetKind {
    none: "No presets yet: F2 saves one.",
    label: R_PRESET,
    one: "preset",
    all: "presets",
};

/// Multi-rename form rows in focus order, with the defaults (name unchanged).
pub(super) fn rename_rows() -> Vec<FormRow> {
    let text = |label, v: &str| FormRow::Text(label, LineEdit::from(v.to_owned()));
    vec![
        text(R_NAME, "[N]"),
        text(R_EXT, "[E]"),
        text(R_FIND, ""),
        text(R_REPLACE, ""),
        FormRow::Check(R_REGEX, false),
        FormRow::Check(R_CASE_SENS, false),
        FormRow::Choice(R_CASE, &CASE_NAMES, 0),
        text(R_START, "1"),
        text(R_STEP, "1"),
        text(R_WIDTH, "1"),
        text(R_PRESET, ""),
    ]
}

/// Multi-rename (Ctrl+M): form, the items (selection in panel order) and their new names.
pub struct RenameForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub(super) dir: PathBuf,
    pub(super) items: Vec<(OsString, Option<std::time::SystemTime>)>,
    /// Old name and the new one, or why there is none (empty: the form has an error).
    pub preview: Vec<(String, Result<String, String>)>,
    /// First preview row shown.
    pub scroll: usize,
    pub error: Option<String>,
    /// Position in the settings history for Alt+Up/Down (= its length: not from history).
    pub(super) hist: usize,
    /// New names pasted as a list, one per item; they replace the masks.
    pub(super) names: Option<Vec<String>>,
}

impl RenameForm {
    pub(super) fn spec(&self) -> Result<rename::Spec, String> {
        let rows = &self.rows;
        let num = |label: &str| {
            let t = form_text(rows, label).trim();
            t.parse::<i64>()
                .map_err(|_| format!("{label}: not a number: {t}"))
        };
        let width = num(R_WIDTH)?;
        let case = rows.iter().find_map(|r| match r {
            FormRow::Choice(R_CASE, _, i) => Some(CASES[*i].1),
            _ => None,
        });
        Ok(rename::Spec {
            name_mask: form_text(rows, R_NAME).into(),
            ext_mask: form_text(rows, R_EXT).into(),
            find: form_text(rows, R_FIND).into(),
            replace: form_text(rows, R_REPLACE).into(),
            regex: form_checked(rows, R_REGEX),
            case_sensitive: form_checked(rows, R_CASE_SENS),
            case: case.unwrap_or_default(),
            start: num(R_START)?,
            step: num(R_STEP)?,
            width: usize::try_from(width)
                .ok()
                .filter(|w| (1..=20).contains(w))
                .ok_or(format!("{R_WIDTH}: 1 to 20"))?,
        })
    }

    /// New names for the form as it is; the first problem goes to `error`.
    pub(super) fn refresh(&mut self) {
        let rule = self.spec().and_then(rename::Spec::compile);
        self.preview = (self.items.iter().enumerate())
            .map(|(i, (name, modified))| {
                let new = match (&self.names, &rule) {
                    (Some(n), _) => rename::explicit(name, &n[i]),
                    (None, Ok(r)) => r.new_name(name, *modified, i),
                    (None, Err(_)) => Err(String::new()),
                };
                (name.to_string_lossy().into_owned(), new)
            })
            .collect();
        self.error = match rule {
            _ if self.names.is_some() => self.pairs().err().or(Some(NAMES_NOTE.into())),
            Err(e) => Some(e),
            Ok(_) => self.pairs().err(),
        };
    }

    /// Renames to run, or why not.
    pub(super) fn pairs(&self) -> Result<Vec<(String, String)>, String> {
        let mut old = Vec::new();
        let mut new = Vec::new();
        for (o, n) in &self.preview {
            match n {
                Ok(n) => new.push(n.clone()),
                Err(e) => return Err(format!("{o}: {e}")),
            }
            // a new name exists only for UTF-8 old names, so this is the real name
            old.push(o.clone());
        }
        rename::plan(&old, &new)
    }

    /// One line of the presets file: `name`, then the [`rows_to_line`] of the form.
    pub(super) fn to_line(&self, name: &str) -> String {
        format!(
            "{}\t{}",
            name.replace('\t', " "),
            rows_to_line(&self.rows, R_PRESET)
        )
    }

    /// Set the rows from a preset line (name first); the focus stays.
    pub(super) fn load_line(&mut self, line: &str) {
        self.names = None;
        self.rows = rename_rows();
        rows_from_line(&mut self.rows, line);
        if let Some((name, _)) = line.split_once('\t')
            && let Some(FormRow::Text(_, l)) = self.rows.last_mut()
        {
            l.text = name.into();
            l.end();
        }
        self.refresh();
    }

    /// Pasted text with line breaks is a list of new names; else it goes into the field.
    pub(super) fn paste(&mut self, text: &str) {
        if text.contains('\n') {
            let lines = text.lines().take(self.items.len() + 1);
            return self.set_names(lines.map(str::to_owned).collect());
        }
        if let FormRow::Text(_, l) = &mut self.rows[self.focus] {
            paste_line(l, text);
        }
        self.names = None;
        self.refresh();
    }

    /// Use `lines` as the new names, one per item; a wrong count is an error and changes nothing.
    pub(super) fn set_names(&mut self, lines: Vec<String>) {
        if lines.len() == self.items.len() {
            self.names = Some(lines);
            self.refresh();
        } else {
            self.error = Some(format!(
                "Pasted {} names for {} files.",
                lines.len(),
                self.items.len()
            ));
        }
    }
}

impl App {
    /// Shift+F6: rename the cursor entry through the move prompt (bare name = same directory).
    pub(super) fn rename(&mut self) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        let Some(entry) = panel.current().filter(|e| !e.is_parent()) else {
            return;
        };
        let name = entry.name.to_string_lossy().into_owned();
        let path = panel.path().join(&entry.name);
        self.mode = Mode::Transfer(Op::Move, vec![path], LineEdit::from(name));
    }

    /// Ctrl+M: multi-rename the selection (else the cursor entry), opened with the last settings.
    pub(super) fn multi_rename(&mut self) {
        let panel = &self.panels[self.active];
        if panel.archive().is_some() {
            self.mode = Mode::Message(READ_ONLY.into());
            return;
        }
        if panel.is_virtual() {
            self.mode =
                Mode::Message("Multi-rename works in a directory, not in a found list.".into());
            return;
        }
        let items: Vec<_> = (panel.target_entries().into_iter())
            .map(|e| (e.name.clone(), e.modified))
            .collect();
        if items.is_empty() {
            return;
        }
        let mut form = RenameForm {
            rows: self.rename_hist(self.rename_history.len().saturating_sub(1)),
            focus: 0,
            dir: panel.path().to_owned(),
            items,
            preview: Vec::new(),
            scroll: 0,
            error: None,
            hist: self.rename_history.len(),
            names: None,
        };
        form.refresh();
        self.mode = Mode::MultiRename(form);
    }

    pub(super) fn multi_rename_key(&mut self, key: KeyEvent, mut form: RenameForm) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let last = form.rows.len() - 1;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => return,
            KeyCode::Enter => return self.multi_rename_run(form),
            KeyCode::Up | KeyCode::Down if alt => {
                let len = self.rename_history.len();
                form.hist = match key.code {
                    KeyCode::Up => form.hist.saturating_sub(1),
                    _ => (form.hist + 1).min(len),
                };
                form.rows = self.rename_hist(form.hist);
                form.names = None;
                form.refresh();
            }
            KeyCode::F(2 | 3 | 8) => self.rename_preset_key(key.code, &mut form),
            KeyCode::F(4) => return self.rename_edit_names(form),
            KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.rename_undo_run(form);
            }
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => form.focus = form.focus.saturating_sub(1),
            KeyCode::PageUp => form.scroll = form.scroll.saturating_sub(fmt::RENAME_PREVIEW_ROWS),
            KeyCode::PageDown => {
                let end = form.preview.len().saturating_sub(fmt::RENAME_PREVIEW_ROWS);
                form.scroll = (form.scroll + fmt::RENAME_PREVIEW_ROWS).min(end);
            }
            code => {
                let ctrl = key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                form.rows[form.focus].key(code, ctrl);
                form.names = None;
                form.refresh();
            }
        }
        self.mode = Mode::MultiRename(form);
    }

    /// Enter: rename in a worker (remote panels too); the form stays open on a problem.
    pub(super) fn multi_rename_run(&mut self, mut form: RenameForm) {
        let pairs = match form.pairs() {
            Ok(p) if p.is_empty() => Err("No name changes.".to_owned()),
            r => r,
        };
        let pairs = match pairs {
            Ok(p) => p,
            Err(e) => {
                form.error = Some(e);
                self.mode = Mode::MultiRename(form);
                return;
            }
        };
        if form.names.is_none() {
            self.note = self.remember("rename-history", &rows_to_line(&form.rows, R_PRESET));
        }
        let side = self.panels[self.active].side();
        let dir = form.dir;
        let sides = (side.clone(), side.clone());
        self.rename_running = Some(RenameBatch {
            side: side.clone(),
            dir: dir.clone(),
            pairs: pairs.clone(),
        });
        self.spawn_xfer(sides, move |p, _| rename::apply(&side, &dir, &pairs, p));
    }

    /// Ctrl+Z in the form: rename the last finished batch back in a worker.
    pub(super) fn rename_undo_run(&mut self, mut form: RenameForm) {
        let Some(RenameBatch { side, dir, pairs }) = self.rename_undo.take() else {
            form.error = Some("Nothing to undo.".into());
            self.mode = Mode::MultiRename(form);
            return;
        };
        let sides = (side.clone(), side.clone());
        self.spawn_xfer(sides, move |p, _| rename::undo(&side, &dir, &pairs, p));
    }

    /// A multi-rename that renamed every pair becomes the one Ctrl+Z undoes (a cancel or an
    /// error leaves `files` short of the total).
    pub(super) fn rename_done(&mut self, error: Option<&String>, progress: &ops::Progress) {
        if let Some(batch) = self.rename_running.take()
            && error.is_none()
            && progress.files.load(Ordering::Relaxed)
                == progress.files_total.load(Ordering::Relaxed)
        {
            self.rename_undo = Some(batch);
        }
    }

    /// Back to the multi-rename form after the editor of its new names closed.
    pub(super) fn restore_rename_back(&mut self) {
        if matches!(self.mode, Mode::Normal)
            && let Some((mut form, dir)) = self.rename_back.take()
        {
            let file =
                fs::File::open(dir.path().join(RENAME_NAMES_FILE)).map(std::io::BufReader::new);
            // bounded: one line more than items is already a count error
            match file.and_then(|r| r.lines().take(form.items.len() + 1).collect()) {
                Ok(lines) => form.set_names(lines),
                Err(e) => form.error = Some(format!("Cannot read the names: {e}")),
            }
            self.mode = Mode::MultiRename(form);
        }
    }

    /// F4 in the multi-rename form: the new names, one per line, in the internal editor.
    pub(super) fn rename_edit_names(&mut self, mut form: RenameForm) {
        let written = TempDir::new().and_then(|dir| {
            let path = dir.path().join(RENAME_NAMES_FILE);
            let mut w = std::io::BufWriter::new(fs::File::create(&path)?);
            for (old, new) in &form.preview {
                writeln!(w, "{}", new.as_ref().unwrap_or(old))?;
            }
            w.flush()?;
            Ok((dir, path))
        });
        match written {
            Ok((dir, path)) => {
                self.rename_back = Some((form, dir));
                self.edit_path_at(path, None);
            }
            Err(e) => {
                form.error = Some(format!("Cannot write the names: {e}"));
                self.mode = Mode::MultiRename(form);
            }
        }
    }

    /// F2/F3/F8 in the multi-rename form: presets, as for the find form.
    pub(super) fn rename_preset_key(&mut self, code: KeyCode, form: &mut RenameForm) {
        let name = form_text(&form.rows, R_PRESET).trim().to_owned();
        let line = form.to_line(&name);
        let (load, note) = preset_key(
            &mut self.rename_presets,
            self.rename_presets_file.as_deref(),
            code,
            &name,
            line,
            &RENAME_KIND,
        );
        if let Some(l) = load {
            form.load_line(&l);
        }
        form.error = Some(note);
    }
}

#[cfg(test)]
mod tests {

    use super::super::multi_rename::{R_NAME, R_PRESET, RENAME_NAMES_FILE};
    use super::super::widgets::form_text;
    use super::rename_rows;

    use super::super::{App, Mode, Op};

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use super::super::tests::{key, two_panels, type_keys, wait, with};

    #[test]
    fn f2_renames_one_or_multi_renames_several() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a", "b"] {
            std::fs::write(dir.path().join(n), "").unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(2));
        assert!(matches!(app.mode, Mode::Transfer(Op::Move, _, _)));
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::F(2));
        assert!(matches!(app.mode, Mode::MultiRename(_)));
    }

    #[test]
    fn multi_rename_counter_history_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        for _ in 0..3 {
            key(&mut app, KeyCode::Backspace); // "[N]"
        }
        for c in "x_[Q]".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.error.as_deref().is_some_and(|e| e.contains("[Q]")));
        key(&mut app, KeyCode::Enter); // refused, form stays
        assert!(matches!(app.mode, Mode::MultiRename(_)));
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Char('C'));
        key(&mut app, KeyCode::Char(']'));
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(f.preview[1].1.as_deref(), Ok("x_2.txt"));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        let read = |n: &str| std::fs::read_to_string(dir.path().join(n)).unwrap();
        assert_eq!(
            (read("x_1.txt"), read("x_2.txt")),
            ("a.txt".into(), "b.txt".into())
        );
        assert!(dir.path().join("c.txt").exists() && !dir.path().join("a.txt").exists());
        // reopens with the last settings; Alt+Down past the history gives the defaults
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(form_text(&f.rows, R_NAME), "x_[C]");
        with(&mut app, KeyCode::Down, KeyModifiers::ALT);
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(form_text(&f.rows, R_NAME), "[N]");
        assert_eq!(f.preview[0].1.as_deref(), Ok(f.preview[0].0.as_str()));
        key(&mut app, KeyCode::Enter); // nothing changes: refused
        assert!(matches!(&app.mode, Mode::MultiRename(f) if f.error.is_some()));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn rename_presets_save_load_delete() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        let file = dir.path().join("presets");
        app.rename_presets_file = Some(file.clone());
        key(&mut app, KeyCode::Down); // skip ".."
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        for _ in 0..3 {
            key(&mut app, KeyCode::Backspace);
        }
        type_keys(&mut app, "x[N]");
        let preset_row = |app: &mut App| {
            let Mode::MultiRename(f) = &mut app.mode else {
                panic!("no form");
            };
            f.focus = f.rows.len() - 1;
        };
        preset_row(&mut app);
        key(&mut app, KeyCode::F(2)); // no name yet
        assert!(
            matches!(&app.mode, Mode::MultiRename(f) if f.error.as_deref().is_some_and(|e| e.contains("name")))
        );
        type_keys(&mut app, "p1");
        key(&mut app, KeyCode::F(2));
        assert!(std::fs::read_to_string(&file).unwrap().starts_with("p1\t"));
        let Mode::MultiRename(f) = &mut app.mode else {
            panic!("no form");
        };
        f.rows = rename_rows();
        key(&mut app, KeyCode::F(3));
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(
            (form_text(&f.rows, R_NAME), form_text(&f.rows, R_PRESET)),
            ("x[N]", "p1")
        );
        assert_eq!(f.preview[0].1.as_deref(), Ok("xa.txt"));
        key(&mut app, KeyCode::F(8));
        assert!(app.rename_presets.is_empty());
        assert!(std::fs::read_to_string(&file).unwrap().is_empty());
    }

    #[test]
    fn rename_pasted_names() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        app.on_paste("x\ny\n".into());
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(f.preview[1].1.as_deref(), Ok("y"));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(dir.path().join("x").exists() && dir.path().join("y").exists());
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        app.on_paste("z".into()); // one line: into the field
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.names.is_none() && form_text(&f.rows, R_NAME).ends_with('z'));
        app.on_paste("x\ny\nz".into());
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.names.is_none() && f.error.as_deref().is_some_and(|e| e.contains("names for")));
    }

    #[test]
    fn rename_f4_edits_names() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::F(4));
        let names_file = |app: &App| {
            let (_, tmp) = app.rename_back.as_ref().expect("no form stashed");
            tmp.path().join(RENAME_NAMES_FILE)
        };
        assert_eq!(std::fs::read_to_string(names_file(&app)).unwrap(), "a\nb\n");
        assert!(!matches!(app.mode, Mode::MultiRename(_)));
        for (text, ok) in [("p\nq\nr\n", false), ("p\nq\n", true)] {
            let file = names_file(&app);
            std::fs::write(&file, text).unwrap();
            app.mode = Mode::Normal; // the editor closed
            app.tick();
            let Mode::MultiRename(f) = &app.mode else {
                panic!("no form");
            };
            assert_eq!(f.names.is_some(), ok);
            if ok {
                assert_eq!(f.preview[1].1.as_deref(), Ok("q"));
            } else {
                assert!(f.error.as_deref().is_some_and(|e| e.contains("names for")));
                key(&mut app, KeyCode::F(4)); // again, for the good list
            }
        }
    }

    #[test]
    fn rename_ctrl_z_undoes() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let other = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), other.path());
        let open = |app: &mut App| {
            key(app, KeyCode::Down); // skip ".."
            key(app, KeyCode::Insert);
            key(app, KeyCode::Insert);
            with(app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        };
        open(&mut app);
        app.on_paste("x\ny\n".into());
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(dir.path().join("x").exists());
        // an unrelated transfer keeps the undo memory
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(5));
        key(&mut app, KeyCode::Enter);
        wait(&mut app);
        assert!(other.path().join("x").exists() || other.path().join("y").exists());
        open(&mut app);
        with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(dir.path().join("a").exists() && dir.path().join("b").exists());
        assert!(!dir.path().join("x").exists());
        open(&mut app);
        with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(
            matches!(&app.mode, Mode::MultiRename(f) if f.error.as_deref() == Some("Nothing to undo."))
        );
    }
}
