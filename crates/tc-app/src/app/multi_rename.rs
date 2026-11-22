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
