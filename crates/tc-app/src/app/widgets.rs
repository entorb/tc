//! Shared dialog pieces: form rows, line editing, `/` list filter, saved presets.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tc_core::cmdline::{self, LineEdit};
use tc_core::hotlist;

use super::{App, Mode};

/// One line of a form: a text field, a checkbox or a choice (options, selected).
#[derive(Clone)]
pub enum FormRow {
    Text(&'static str, LineEdit),
    Check(&'static str, bool),
    Choice(&'static str, &'static [&'static str], usize),
}

impl FormRow {
    pub(super) fn label(&self) -> &'static str {
        match self {
            FormRow::Text(l, _) | FormRow::Check(l, _) | FormRow::Choice(l, _, _) => l,
        }
    }

    /// Typing edits a text field, Space toggles a checkbox, Space/Left/Right cycle a choice.
    pub(super) fn key(&mut self, code: KeyCode, ctrl: bool) {
        match (self, code) {
            (FormRow::Check(_, on), KeyCode::Char(' ')) => *on = !*on,
            (FormRow::Choice(_, opts, i), KeyCode::Char(' ') | KeyCode::Right) => {
                *i = (*i + 1) % opts.len();
            }
            (FormRow::Choice(_, opts, i), KeyCode::Left) => *i = (*i + opts.len() - 1) % opts.len(),
            (FormRow::Text(_, l), KeyCode::Char(c)) if !ctrl => l.insert(c),
            (FormRow::Text(_, l), KeyCode::Backspace) => l.backspace(),
            (FormRow::Text(_, l), KeyCode::Delete) => l.delete(),
            (FormRow::Text(_, l), KeyCode::Left) => l.left(),
            (FormRow::Text(_, l), KeyCode::Right) => l.right(),
            (FormRow::Text(_, l), KeyCode::Home) => l.home(),
            (FormRow::Text(_, l), KeyCode::End) => l.end(),
            _ => {}
        }
    }
}

/// Form rows as tab separated `label=value` (checkboxes `1`/`0`, choices their index),
/// without the `skip` row; tabs inside values become spaces.
pub(super) fn rows_to_line(rows: &[FormRow], skip: &str) -> String {
    let parts: Vec<String> = (rows.iter().filter(|r| r.label() != skip))
        .map(|row| {
            let value = match row {
                FormRow::Text(_, l) => l.text.replace('\t', " "),
                FormRow::Check(_, on) => if *on { "1" } else { "0" }.into(),
                FormRow::Choice(_, _, i) => i.to_string(),
            };
            format!("{}={value}", row.label())
        })
        .collect();
    parts.join("\t")
}

/// Set rows from a [`rows_to_line`] line; unknown labels are ignored, missing ones keep their value.
pub(super) fn rows_from_line(rows: &mut [FormRow], line: &str) {
    let values: HashMap<&str, &str> = line.split('\t').filter_map(|p| p.split_once('=')).collect();
    for row in rows {
        let Some(&v) = values.get(row.label()) else {
            continue;
        };
        match row {
            FormRow::Text(_, l) => {
                l.text = v.into();
                l.end();
            }
            FormRow::Check(_, on) => *on = v == "1",
            FormRow::Choice(_, opts, i) => {
                *i = v.parse().ok().filter(|&v| v < opts.len()).unwrap_or(0);
            }
        }
    }
}

impl Mode {
    /// Rows and focus of an open form.
    fn form_mut(&mut self) -> Option<(&[FormRow], &mut usize)> {
        match self {
            Mode::FindForm(f) => Some((&f.rows, &mut f.focus)),
            Mode::SyncForm(f) => Some((&f.rows, &mut f.focus)),
            Mode::MultiRename(f) => Some((&f.rows, &mut f.focus)),
            Mode::Settings(f) => Some((&f.rows, &mut f.focus)),
            Mode::Procs(p) => p.draft.as_mut().map(|d| (d.rows.as_slice(), &mut d.focus)),
            _ => None,
        }
    }
}

impl App {
    /// Rows and focus of the open form: the search dialog over the text, else the form screen.
    fn form_rows_mut(&mut self) -> Option<(&[FormRow], &mut usize)> {
        if let Some(f) = &mut self.search_form {
            return Some((&f.rows, &mut f.focus));
        }
        self.mode.form_mut()
    }

    /// Click on form row `row`: focuses it; a checkbox toggles, a choice cycles (as Space would).
    pub fn form_click(&mut self, row: usize) {
        let Some((rows, focus)) = self.form_rows_mut() else {
            return;
        };
        let Some(r) = rows.get(row) else { return };
        let toggle = !matches!(r, FormRow::Text(..));
        *focus = row;
        if toggle {
            self.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        }
    }

    /// Pick option `k` of choice row `row` (GUI dropdown); cycles with Space so the form reacts as to keys.
    pub fn form_choose(&mut self, row: usize, k: usize) {
        let Some((rows, focus)) = self.form_rows_mut() else {
            return;
        };
        let Some(FormRow::Choice(_, opts, i)) = rows.get(row) else {
            return;
        };
        let steps = (k % opts.len() + opts.len() - i) % opts.len();
        *focus = row;
        for _ in 0..steps {
            self.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        }
    }

    /// Terminal click inside the form rows drawn at `form_area` (first row shown: `form_scroll`).
    pub(super) fn form_mouse(&mut self, col: u16, row: u16) -> bool {
        if self.form_rows_mut().is_none() || !self.form_area.contains((col, row).into()) {
            return false;
        }
        self.form_click(self.form_scroll + usize::from(row - self.form_area.y));
        true
    }
}

pub(super) fn form_text<'a>(rows: &'a [FormRow], label: &str) -> &'a str {
    match rows.iter().find(|r| r.label() == label) {
        Some(FormRow::Text(_, l)) => &l.text,
        _ => "",
    }
}

pub(super) fn form_checked(rows: &[FormRow], label: &str) -> bool {
    rows.iter()
        .any(|r| matches!(r, FormRow::Check(l, true) if *l == label))
}

/// Wording of one kind of saved form lines (see [`preset_key`]).
pub(super) struct PresetKind {
    /// Note when the list is empty.
    pub(super) none: &'static str,
    /// Label of the name row.
    pub(super) label: &'static str,
    /// What one entry is called in "No ... x.".
    pub(super) one: &'static str,
    /// What the file holds in "Cannot save ...".
    pub(super) all: &'static str,
}

/// F2 saves `line` (`name\t...`) under `name`, F3 returns the line after `name`'s (cycling),
/// F8 deletes `name`; the file is written at once. Returns the line to load and the note.
pub(super) fn preset_key(
    list: &mut Vec<String>,
    file: Option<&Path>,
    code: KeyCode,
    name: &str,
    line: String,
    kind: &PresetKind,
) -> (Option<String>, String) {
    let pos = list.iter().position(|l| l.split('\t').next() == Some(name));
    let (load, note) = match (code, pos) {
        (KeyCode::F(3), _) if list.is_empty() => (None, kind.none.to_owned()),
        (KeyCode::F(3), _) => {
            let next = list[pos.map_or(0, |i| (i + 1) % list.len())].clone();
            let note = format!("Loaded {}.", next.split('\t').next().unwrap_or_default());
            (Some(next), note)
        }
        _ if name.is_empty() => (None, format!("Type a name in \"{}\" first.", kind.label)),
        (KeyCode::F(2), Some(i)) => {
            list[i] = line;
            (None, format!("Saved {name}."))
        }
        (KeyCode::F(2), None) => {
            list.push(line);
            (None, format!("Saved {name}."))
        }
        (_, Some(i)) => {
            list.remove(i);
            (None, format!("Deleted {name}."))
        }
        (_, None) => (None, format!("No {} {name}.", kind.one)),
    };
    if let Some(Err(e)) = file.map(|f| cmdline::save_history(f, list))
        && code != KeyCode::F(3)
    {
        return (None, format!("Cannot save {}: {e}", kind.all));
    }
    (load, note)
}

/// Edit keys shared by all text prompts; true if `key` was one of them.
pub(super) fn edit_key(l: &mut LineEdit, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Left => l.left(),
        KeyCode::Right => l.right(),
        KeyCode::Home => l.home(),
        KeyCode::End => l.end(),
        KeyCode::Backspace => l.backspace(),
        KeyCode::Delete => l.delete(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => l.clear(),
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            l.insert(c)
        }
        _ => return false,
    }
    true
}

/// Pasted text goes into a one-line prompt without line breaks.
pub(super) fn paste_line(l: &mut LineEdit, text: &str) {
    text.chars()
        .filter(|c| !matches!(c, '\r' | '\n'))
        .for_each(|c| l.insert(c));
}

/// `/` filter of a list screen: case-insensitive substring, typed live; Enter keeps it,
/// Esc clears it.
#[derive(Default)]
pub struct Filter {
    pub line: LineEdit,
    pub editing: bool,
}

impl Filter {
    pub fn active(&self) -> bool {
        !self.line.text.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        !self.active() || name.to_lowercase().contains(&self.line.text.to_lowercase())
    }

    /// Filter part of a key: typing, `/` to start, Esc to clear an active filter.
    /// True when the key was used and the rows need a rebuild.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if self.editing {
            match key.code {
                KeyCode::Enter => self.editing = false,
                KeyCode::Esc => *self = Self::default(),
                _ => {
                    edit_key(&mut self.line, key);
                }
            }
            return true;
        }
        match key.code {
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Esc if self.active() => self.line = LineEdit::default(),
            _ => return false,
        }
        true
    }

    /// Title suffix: ` [filter: x]`, with `_` while typing.
    pub fn tag(&self) -> String {
        match (self.active(), self.editing) {
            (false, false) => String::new(),
            (_, editing) => format!(
                " [filter: {}{}]",
                self.line.text,
                if editing { "_" } else { "" }
            ),
        }
    }
}

/// A row of a path list (drives, bookmarks, connections).
pub trait ListRow {
    /// Text shown, and matched by the `/` filter.
    fn label(&self) -> Cow<'_, str>;
}

impl ListRow for PathBuf {
    fn label(&self) -> Cow<'_, str> {
        self.to_string_lossy()
    }
}

impl ListRow for tc_core::panel::CompareKind {
    fn label(&self) -> Cow<'_, str> {
        Self::label(*self).into()
    }
}

impl ListRow for tc_core::popular::Popular {
    fn label(&self) -> Cow<'_, str> {
        self.path.to_string_lossy()
    }
}

impl ListRow for hotlist::Bookmark {
    fn label(&self) -> Cow<'_, str> {
        if self.alias.is_empty() {
            self.path.to_string_lossy()
        } else {
            format!("{}  {}", self.alias, self.path.to_string_lossy()).into()
        }
    }
}

/// Indices of the `rows` a list shows.
pub fn list_rows(rows: &[impl ListRow], filter: &Filter) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| filter.matches(&rows[i].label()))
        .collect()
}

/// Filter and Up/Down keys of a path list; `sel` is a row of `rows`. True when `key` was used.
pub(super) fn list_nav(
    filter: &mut Filter,
    rows: &[impl ListRow],
    sel: &mut usize,
    key: KeyEvent,
) -> bool {
    if filter.key(key) {
        *sel = list_rows(rows, filter).first().copied().unwrap_or(0);
        return true;
    }
    let view = list_rows(rows, filter);
    let pos = view.iter().position(|&i| i == *sel).unwrap_or(0);
    let pos = match key.code {
        KeyCode::Up => pos.saturating_sub(1),
        KeyCode::Down => (pos + 1).min(view.len().saturating_sub(1)),
        _ => return false,
    };
    *sel = view.get(pos).copied().unwrap_or(0);
    true
}
