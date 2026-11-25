//! Multi-rename (Ctrl+M) form, names from the editor, undo; F2 / Shift+F6 rename.

use crate::app::widgets::{step_focus, tab_key, tab_of, tab_range};
use crate::app::{
    App, FormRow, Mode, Op, READ_ONLY, form_checked, form_text, paste_line, rows_to_line,
};
use crate::{KeyCode, KeyEvent, KeyModifiers};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tc_core::cmdline::LineEdit;
use tc_core::vfs::Side;
use tc_core::{ops, rename};

// Multi-rename form row labels.
pub(in crate::app) const R_NAME: &str = "Name mask";

pub(in crate::app) const R_EXT: &str = "Extension mask";

pub(in crate::app) const R_FIND: &str = "Find";

// row keys (form lookup, history); shown as "Search" / "Replace" (`fmt::rename_shown`)
pub(in crate::app) const R_REPLACE: &str = "Replace with";

pub(in crate::app) const R_PH_FIND: &str = "Search";

pub(in crate::app) const R_PH_REPLACE: &str = "Replace";

pub(in crate::app) const R_REGEX: &str = "Regex";

pub(in crate::app) const R_CASE_SENS: &str = "Find case-sensitive";

pub(in crate::app) const R_UNDERSCORE: &str = "Underscore to space";

pub(in crate::app) const R_COLLAPSE: &str = "Remove multiple spaces";

pub(in crate::app) const R_SPACE_CAPS: &str = "Space before capitals";

pub(in crate::app) const R_DOTS: &str = "Remove dots";

pub(in crate::app) const R_TRIM: &str = "Trim spaces";

/// Shown when a setting replaced names typed in the Editor or pasted as a list.
pub(in crate::app) const LOST_NOTE: &str =
    "The settings replaced your edited names: F8 restores them.";

pub(in crate::app) const R_ADD: &str = "Add to first number";

pub(in crate::app) const R_CASE: &str = "Case";

/// Rows per tab (titles in `fmt::RENAME_TABS`): the rows of all tabs in this order; the Editor
/// tab has none.
pub(in crate::app) const TAB_ROWS: [usize; 5] = [5, 4, 2, 7, 0];

const _: () = assert!(TAB_ROWS.len() == crate::fmt::RENAME_TABS.len());

pub(in crate::app) const R_START: &str = "Counter start";

pub(in crate::app) const R_STEP: &str = "Counter step";

pub(in crate::app) const R_WIDTH: &str = "Counter digits";

pub(in crate::app) const CASES: [(&str, rename::Case); 6] = [
    ("unchanged", rename::Case::Keep),
    ("lower", rename::Case::Lower),
    ("UPPER", rename::Case::Upper),
    ("First letter", rename::Case::First),
    ("Every Word", rename::Case::Words),
    ("Every Word, small kept", rename::Case::WordsSmall),
];

pub(in crate::app) const CASE_NAMES: [&str; 6] = [
    CASES[0].0, CASES[1].0, CASES[2].0, CASES[3].0, CASES[4].0, CASES[5].0,
];

/// A finished multi-rename: where it ran and what it renamed, for Ctrl+Z.
#[derive(Clone)]
pub(in crate::app) struct RenameBatch {
    // shortcut: a remote side's session stays open in the undo memory, upgrade by clearing it on disconnect
    pub(in crate::app) side: Side,
    pub(in crate::app) dir: PathBuf,
    pub(in crate::app) pairs: Vec<(String, String)>,
}

/// Multi-rename form rows in focus order, with the defaults (name unchanged).
pub(in crate::app) fn rename_rows() -> Vec<FormRow> {
    let text = |label, v: &str| FormRow::Text(label, LineEdit::from(v.to_owned()));
    vec![
        text(R_NAME, "[N]"),
        text(R_EXT, "[E]"),
        text(R_START, "1"),
        text(R_STEP, "1"),
        text(R_WIDTH, "1"),
        text(R_FIND, ""),
        text(R_REPLACE, ""),
        FormRow::Check(R_REGEX, false),
        FormRow::Check(R_CASE_SENS, false),
        text(R_PH_FIND, ""),
        text(R_PH_REPLACE, ""),
        FormRow::Check(R_UNDERSCORE, false),
        FormRow::Check(R_COLLAPSE, false),
        FormRow::Check(R_SPACE_CAPS, false),
        FormRow::Check(R_DOTS, false),
        FormRow::Check(R_TRIM, false),
        text(R_ADD, ""),
        FormRow::Choice(R_CASE, &CASE_NAMES, 0),
    ]
}

fn row_differs(a: &FormRow, b: &FormRow) -> bool {
    match (a, b) {
        (FormRow::Text(_, x), FormRow::Text(_, y)) => x.text != y.text,
        (FormRow::Check(_, x), FormRow::Check(_, y)) => x != y,
        (FormRow::Choice(_, _, x), FormRow::Choice(_, _, y)) => x != y,
        _ => false,
    }
}

/// Multi-rename (Ctrl+M): form, the items (selection in panel order) and their new names.
pub struct RenameForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub(in crate::app) dir: PathBuf,
    pub(in crate::app) items: Vec<(OsString, Option<std::time::SystemTime>)>,
    /// Old name and the new one, or why there is none (empty: the form has an error).
    pub preview: Vec<(String, Result<String, String>)>,
    /// First preview row shown.
    pub scroll: usize,
    pub error: Option<String>,
    /// Position in the settings history for Alt+Up/Down (= its length: not from history).
    pub(in crate::app) hist: usize,
    /// New names pasted as a list, one per item; they replace the masks.
    pub(in crate::app) names: Option<Vec<String>>,
    /// The Editor tab's text: one new name per line. It shows the masks' result until edited.
    pub editor: String,
    /// The editor has the keyboard (set by the drawing; typed keys then go to it).
    pub editor_focused: bool,
    /// Tab shown (`TAB_ROWS`).
    pub tab: usize,
    /// Per tab: a row differs from its default (the tab title gets a dot).
    pub changed: [bool; TAB_ROWS.len()],
    /// Items that get a new name.
    pub count: usize,
    /// Preview rows that fit (set by the drawing): the PgUp / PgDn step.
    pub page: usize,
    /// Edited names a setting replaced (Editor text), until F8 restores them.
    pub(in crate::app) lost: Option<String>,
}

impl RenameForm {
    pub(in crate::app) fn spec(&self) -> Result<rename::Spec, String> {
        let rows = &self.rows;
        let num = |label: &str| {
            let t = form_text(rows, label).trim();
            t.parse::<i64>()
                .map_err(|_| format!("{label}: not a number: {t}"))
        };
        let width = num(R_WIDTH)?;
        let add = match form_text(rows, R_ADD).trim() {
            "" => 0,
            _ => num(R_ADD)?,
        };
        let choice = |label: &str| {
            rows.iter().find_map(|r| match r {
                FormRow::Choice(l, _, i) if *l == label => Some(*i),
                _ => None,
            })
        };
        let find_mode = if form_checked(rows, R_REGEX) {
            rename::Find::Regex
        } else {
            rename::Find::Plain
        };
        Ok(rename::Spec {
            name_mask: form_text(rows, R_NAME).into(),
            ext_mask: form_text(rows, R_EXT).into(),
            find: form_text(rows, R_FIND).into(),
            replace: form_text(rows, R_REPLACE).into(),
            find_mode,
            ph_find: form_text(rows, R_PH_FIND).into(),
            ph_replace: form_text(rows, R_PH_REPLACE).into(),
            case_sensitive: form_checked(rows, R_CASE_SENS),
            case: choice(R_CASE).map(|i| CASES[i].1).unwrap_or_default(),
            start: num(R_START)?,
            step: num(R_STEP)?,
            width: usize::try_from(width)
                .ok()
                .filter(|w| (1..=20).contains(w))
                .ok_or(format!("{R_WIDTH}: 1 to 20"))?,
            underscore: form_checked(rows, R_UNDERSCORE),
            collapse: form_checked(rows, R_COLLAPSE),
            space_caps: form_checked(rows, R_SPACE_CAPS),
            dots: form_checked(rows, R_DOTS),
            trim: form_checked(rows, R_TRIM),
            add,
        })
    }

    /// New names for the form as it is; the first problem goes to `error`.
    pub(in crate::app) fn refresh(&mut self) {
        let rule = self.spec().and_then(rename::Spec::compile);
        self.preview = (self.items.iter().enumerate())
            .map(|(i, (name, modified))| {
                let new = match (&self.names, &rule) {
                    (Some(n), _) => n
                        .get(i)
                        .map_or(Err(String::new()), |x| rename::explicit(name, x)),
                    (None, Ok(r)) => r.new_name(name, *modified, i),
                    (None, Err(_)) => Err(String::new()),
                };
                (name.to_string_lossy().into_owned(), new)
            })
            .collect();
        self.count = (self.preview.iter())
            .filter(|(old, new)| matches!(new, Ok(n) if n != old))
            .count();
        let defaults = rename_rows();
        self.changed = std::array::from_fn(|t| {
            let r = tab_range(&TAB_ROWS, t);
            self.rows[r.clone()]
                .iter()
                .zip(&defaults[r])
                .any(|(a, b)| row_differs(a, b))
        });
        // the Names tab has no rows: its dot shows the names list is in use
        self.changed[TAB_ROWS.len() - 1] = self.names.is_some();
        if self.names.is_none() {
            // the editor starts from the masks' result; a name that cannot be made keeps its old name
            self.editor = (self.preview.iter())
                .map(|(old, new)| match new {
                    Ok(n) => n.as_str(),
                    Err(_) => old.as_str(),
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        self.error = match (&self.names, rule) {
            (Some(n), _) if n.len() != self.items.len() => {
                Some(format!("{} names for {} files.", n.len(), self.items.len()))
            }
            (Some(_), _) => self.pairs().err(),
            (None, Err(e)) => Some(e),
            (None, Ok(_)) => self.pairs().err(),
        };
    }

    /// The case-sensitive box is only shown for plain text.
    pub fn hidden(&self, i: usize) -> bool {
        matches!(self.rows[i], FormRow::Check(R_CASE_SENS, _)) && form_checked(&self.rows, R_REGEX)
    }

    /// Flat row indexes of the shown tab.
    pub fn shown_rows(&self) -> std::ops::Range<usize> {
        tab_range(&TAB_ROWS, self.tab)
    }

    /// Show tab `t` and focus its first row.
    pub(in crate::app) fn goto_tab(&mut self, t: usize) {
        self.tab = t.min(TAB_ROWS.len() - 1);
        // keys after the switch go to the form until the drawing focuses the editor again
        self.editor_focused = false;
        self.focus = tab_range(&TAB_ROWS, self.tab).start;
    }

    /// Focus moved (Tab, click): the tab follows it.
    pub(in crate::app) fn follow_focus(&mut self) {
        if let Some(t) = tab_of(&TAB_ROWS, self.focus) {
            self.tab = t;
        }
    }

    /// Scroll the preview by `rows`, keeping the last page full.
    pub(in crate::app) fn scroll_preview(&mut self, rows: isize) {
        let end = self.preview.len().saturating_sub(self.page.max(1));
        self.scroll = self.scroll.saturating_add_signed(rows).min(end);
    }

    /// Up / Down: move inside the shown tab, over the rows that are shown.
    pub(in crate::app) fn step_in_tab(&mut self, delta: isize) {
        self.focus = step_focus(self.shown_rows(), self.focus, delta, |i| self.hidden(i));
    }

    /// Tab / Shift+Tab: the next row that is shown (`forward` or back), staying in the form.
    pub(in crate::app) fn next_visible(&mut self, forward: bool) {
        let delta = if forward { 1 } else { -1 };
        self.focus = step_focus(0..self.rows.len(), self.focus, delta, |i| self.hidden(i));
    }

    /// Renames to run, or why not.
    pub(in crate::app) fn pairs(&self) -> Result<Vec<(String, String)>, String> {
        // a wrong count shifts names onto the wrong files
        if let Some(n) = &self.names
            && n.len() != self.items.len()
        {
            return Err(format!("{} names for {} files.", n.len(), self.items.len()));
        }
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

    /// Pasted text with line breaks is a list of new names; else it goes into the field.
    pub(in crate::app) fn paste(&mut self, text: &str) {
        if text.contains('\n') {
            let lines = text.lines().take(self.items.len() + 1);
            return self.set_names(lines.map(str::to_owned).collect());
        }
        // the Editor tab has no field
        if let Some(FormRow::Text(_, l)) = self.rows.get_mut(self.focus) {
            paste_line(l, text);
            self.settings_changed();
        }
    }

    /// A setting changed: the masks make the names again; hand-made names are kept for F8.
    pub(in crate::app) fn settings_changed(&mut self) {
        let lost = self.names.take().map(|_| std::mem::take(&mut self.editor));
        self.refresh();
        if lost.is_some() {
            self.lost = lost;
            self.error.get_or_insert_with(|| LOST_NOTE.into());
        }
    }

    /// F8: back to the edited names a setting replaced.
    pub(in crate::app) fn restore_lost(&mut self) {
        if let Some(text) = self.lost.take() {
            self.editor = text;
            self.goto_tab(TAB_ROWS.len() - 1);
            self.sync_editor();
        }
    }

    /// The editor text changed: its lines are the new names (a final line break adds none).
    pub(in crate::app) fn sync_editor(&mut self) {
        let lines: Vec<String> = {
            let text = self.editor.strip_suffix('\n').unwrap_or(&self.editor);
            text.split('\n').map(str::to_owned).collect()
        };
        self.names = Some(lines);
        self.refresh();
    }

    /// Use `lines` as the new names, one per item; a wrong count is an error and changes nothing.
    pub(in crate::app) fn set_names(&mut self, lines: Vec<String>) {
        if lines.len() == self.items.len() {
            self.lost = None;
            self.editor = lines.join("\n");
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
    pub(in crate::app) fn rename(&mut self) {
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
    pub(in crate::app) fn multi_rename(&mut self) {
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
            editor: String::new(),
            editor_focused: false,
            tab: 0,
            changed: [false; TAB_ROWS.len()],
            count: 0,
            page: 1,
            lost: None,
        };
        form.refresh();
        self.mode = Mode::MultiRename(form);
    }

    pub(in crate::app) fn multi_rename_key(&mut self, key: KeyEvent, mut form: RenameForm) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if let Some(t) = tab_key(key, form.tab, TAB_ROWS.len()) {
            form.goto_tab(t);
            self.mode = Mode::MultiRename(form);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::F(9) => return,
            // F10 renames and closes (the form stays open on a problem)
            KeyCode::Enter | KeyCode::F(10) => return self.multi_rename_run(form),
            KeyCode::Up | KeyCode::Down if alt => {
                let len = self.rename_history.len();
                form.hist = match key.code {
                    KeyCode::Up => form.hist.saturating_sub(1),
                    _ => (form.hist + 1).min(len),
                };
                form.rows = self.rename_hist(form.hist);
                form.settings_changed();
            }
            KeyCode::F(7) => form.restore_lost(),
            KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.rename_undo_run(form);
            }
            KeyCode::Tab => {
                form.next_visible(true);
                form.follow_focus();
            }
            KeyCode::BackTab => {
                form.next_visible(false);
                form.follow_focus();
            }
            KeyCode::Down => form.step_in_tab(1),
            KeyCode::Up => form.step_in_tab(-1),
            KeyCode::PageUp => form.scroll_preview(-(form.page.max(1) as isize)),
            KeyCode::PageDown => form.scroll_preview(form.page.max(1) as isize),
            _ => {
                // the Editor tab has no row: its keys go to the editor
                if let Some(row) = form.rows.get_mut(form.focus) {
                    row.key(key);
                    form.settings_changed();
                }
            }
        }
        self.mode = Mode::MultiRename(form);
    }

    /// Enter: rename in a worker (remote panels too); the form stays open on a problem.
    pub(in crate::app) fn multi_rename_run(&mut self, mut form: RenameForm) {
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
            self.note = self.remember("rename-history", &rows_to_line(&form.rows, ""));
        }
        let side = self.panels[self.active].side();
        let dir = form.dir;
        let sides = (side.clone(), side.clone());
        self.rename_running = Some(RenameBatch {
            side: side.clone(),
            dir: dir.clone(),
            pairs: pairs.clone(),
        });
        self.spawn_xfer("Rename", sides, move |p, _| {
            rename::apply(&side, &dir, &pairs, p)
        });
    }

    /// Ctrl+Z in the form: rename the last finished batch back in a worker.
    pub(in crate::app) fn rename_undo_run(&mut self, mut form: RenameForm) {
        let Some(RenameBatch { side, dir, pairs }) = self.rename_undo.take() else {
            form.error = Some("Nothing to undo.".into());
            self.mode = Mode::MultiRename(form);
            return;
        };
        let sides = (side.clone(), side.clone());
        self.spawn_xfer("Undo rename", sides, move |p, _| {
            rename::undo(&side, &dir, &pairs, p)
        });
    }

    /// A multi-rename that renamed every pair becomes the one Ctrl+Z undoes (a cancel or an
    /// error leaves `files` short of the total).
    pub(in crate::app) fn rename_done(&mut self, error: Option<&String>, progress: &ops::Progress) {
        if let Some(batch) = self.rename_running.take()
            && error.is_none()
            && progress.files.load(Ordering::Relaxed)
                == progress.files_total.load(Ordering::Relaxed)
        {
            self.rename_undo = Some(batch);
        }
    }

    /// The inline editor's text changed (drawn in the Editor tab).
    pub fn rename_editor_changed(&mut self) {
        if let Mode::MultiRename(form) = &mut self.mode {
            form.sync_editor();
        }
    }

    /// The inline editor has the keyboard: typed keys belong to it, not to the form.
    pub fn rename_editor_focused(&self) -> bool {
        matches!(&self.mode, Mode::MultiRename(f) if f.editor_focused)
    }

    /// Click on a tab title.
    pub fn tab_click(&mut self, tab: usize) {
        match &mut self.mode {
            Mode::MultiRename(form) => form.goto_tab(tab),
            Mode::FindForm(form) => form.goto_tab(tab),
            _ => {}
        }
    }

    /// Click on a placeholder button: insert it into the focused field.
    pub fn rename_insert(&mut self, text: &str) {
        if let Mode::MultiRename(form) = &mut self.mode {
            form.paste(text);
        }
    }
}

#[cfg(test)]
mod tests {

    use super::rename_rows;
    use crate::app::tools::rename::R_NAME;
    use crate::app::widgets::form_text;

    use super::FormRow;
    use crate::app::{App, Mode, Op};

    use crate::{KeyCode, KeyModifiers};

    use crate::app::tests::{key, two_panels, type_keys, wait, with};

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
    fn rename_form_has_f1_help() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::F(1));
        assert!(app.help, "F1 opens the help of the form");
        assert!(
            matches!(app.mode, Mode::MultiRename(_)),
            "F1 leaves the form"
        );
        key(&mut app, KeyCode::F(1));
        assert!(!app.help);
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
    fn rename_editor_is_the_last_rule_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b"] {
            std::fs::write(dir.path().join(f), f).unwrap();
        }
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::F(6)); // the Editor tab
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!((f.tab, f.editor.as_str()), (4, "a\nb"));
        // edited text wins over the masks
        let Mode::MultiRename(f) = &mut app.mode else {
            panic!("no form");
        };
        f.editor = "p\nq\n".into();
        app.rename_editor_changed();
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(f.preview[1].1.as_deref(), Ok("q"));
        assert!(f.names.is_some() && f.error.is_none());
        // a setting above resets the editor to the masks' result
        with(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
        type_keys(&mut app, "x");
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.names.is_none());
        assert_eq!(f.editor, "ax\nbx");
        // a wrong count is an error
        let Mode::MultiRename(f) = &mut app.mode else {
            panic!("no form");
        };
        f.editor = "p".into();
        app.rename_editor_changed();
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert!(f.error.as_deref().is_some_and(|e| e.contains("names for")));
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

    /// A form over one item for the tab and option tests (no App needed).
    fn form_for(name: &str) -> super::RenameForm {
        let mut f = super::RenameForm {
            rows: rename_rows(),
            focus: 0,
            dir: std::path::PathBuf::new(),
            items: vec![(name.into(), None)],
            preview: Vec::new(),
            scroll: 0,
            error: None,
            hist: 0,
            names: None,
            editor: String::new(),
            editor_focused: false,
            tab: 0,
            changed: [false; super::TAB_ROWS.len()],
            count: 0,
            page: 1,
            lost: None,
        };
        f.refresh();
        f
    }

    #[test]
    fn rename_tabs_mark_changes_and_follow_focus() {
        let mut f = form_for("a_b  c.txt");
        assert_eq!((f.tab, f.count, f.changed), (0, 0, [false; 5]));
        // tab 3 holds the clean-up options; Tab moves focus, the shown tab follows it
        f.goto_tab(3);
        assert_eq!(f.shown_rows(), 11..18);
        f.focus = 11;
        if let FormRow::Check(_, on) = &mut f.rows[11] {
            *on = true; // underscore to space
        }
        if let FormRow::Check(_, on) = &mut f.rows[12] {
            *on = true; // collapse spaces
        }
        f.refresh();
        assert_eq!(f.preview[0].1, Ok("a b c.txt".into()));
        assert_eq!(
            (f.count, f.changed),
            (1, [false, false, false, true, false])
        );
        f.focus = 4;
        f.follow_focus();
        assert_eq!(f.tab, 0);
        // step_in_tab stays inside the shown tab
        f.step_in_tab(-1);
        assert_eq!(f.focus, 3);
        f.step_in_tab(9);
        assert_eq!(f.focus, 4);
    }

    #[test]
    fn placeholder_tab_search_and_replace_apply_last() {
        let mut f = form_for("tom_jerry.txt");
        // Placeholder tab: Search (row 9) and Replace (row 10)
        f.focus = 9;
        f.paste("%a_%b");
        f.focus = 10;
        f.paste("%b - %a");
        assert_eq!(f.preview[0].1.as_deref(), Ok("jerry - tom.txt"));
    }

    #[test]
    fn rename_placeholder_button_inserts_into_focus() {
        let mut f = form_for("x.txt");
        f.focus = 1; // Extension mask
        f.paste("[N]");
        // the caret starts at the end of the field
        assert_eq!(form_text(&f.rows, super::R_EXT), "[E][N]");
    }

    #[test]
    fn editor_tab_without_focus_ignores_keys_and_paste() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "a").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // skip ".."
        with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::F(6)); // the Editor tab has no row to type into
        type_keys(&mut app, "x ");
        app.on_paste("y".into());
        assert!(matches!(&app.mode, Mode::MultiRename(f) if f.editor == "a"));
    }

    #[test]
    fn wrong_name_count_never_renames() {
        let mut f = form_for("a.txt");
        f.editor = "x\ny".into();
        f.sync_editor();
        assert_eq!(f.preview[0].1.as_deref(), Ok("x"));
        assert!(f.pairs().unwrap_err().contains("2 names for 1 files"));
    }

    #[test]
    fn f8_restores_names_a_setting_replaced() {
        let mut f = form_for("a.txt");
        f.editor = "mine.txt".into();
        f.sync_editor();
        f.focus = 0;
        f.paste("x"); // a setting: the masks win again
        assert_eq!((f.names.is_none(), f.editor.as_str()), (true, "ax.txt"));
        assert_eq!(f.error.as_deref(), Some(super::LOST_NOTE));
        f.restore_lost();
        assert_eq!((f.tab, f.editor.as_str()), (4, "mine.txt"));
        assert_eq!(f.preview[0].1.as_deref(), Ok("mine.txt"));
    }
}
