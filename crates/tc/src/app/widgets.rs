//! Shared dialog pieces: form rows, line editing, `/` list filter, saved presets.

use crate::{KeyCode, KeyEvent, KeyModifiers};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use tc_core::cmdline::LineEdit;
use tc_core::hotlist;

use super::{App, Connecting, Mode, link::LinkAsk, xfer::Xfer};
use tc_remote::Prompt;

/// One line of a form: a text field, a checkbox or a choice (options, selected).
#[derive(Clone)]
pub enum FormRow {
    Text(&'static str, LineEdit),
    Check(&'static str, bool),
    Choice(&'static str, &'static [&'static str], usize),
}

impl FormRow {
    pub fn label(&self) -> &'static str {
        match self {
            FormRow::Text(l, _) | FormRow::Check(l, _) | FormRow::Choice(l, _, _) => l,
        }
    }

    /// Typing edits a text field, Space toggles a checkbox, Space/Left/Right cycle a choice.
    pub(super) fn key(&mut self, key: KeyEvent) {
        match (self, key.code) {
            (FormRow::Check(_, on), KeyCode::Char(' ')) => *on = !*on,
            (FormRow::Choice(_, opts, i), KeyCode::Char(' ') | KeyCode::Right) => {
                *i = (*i + 1) % opts.len();
            }
            (FormRow::Choice(_, opts, i), KeyCode::Left) => *i = (*i + opts.len() - 1) % opts.len(),
            (FormRow::Text(_, l), _) => {
                edit_key(l, key);
            }
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
            Mode::Sync(s) => Some((&s.form.rows, &mut s.form.focus)),
            Mode::MultiRename(f) => Some((&f.rows, &mut f.focus)),
            Mode::Settings(f) => Some((&f.rows, &mut f.focus)),
            Mode::Props(f) => Some((&f.rows, &mut f.focus)),
            Mode::Procs(p) => p.draft.as_mut().map(|d| (d.rows.as_slice(), &mut d.focus)),
            Mode::Filter(f) => Some((&f.rows, &mut f.focus)),
            _ => None,
        }
    }
}

fn text_row(rows: &mut [FormRow], focus: usize) -> Option<&mut LineEdit> {
    match rows.get_mut(focus) {
        Some(FormRow::Text(_, l)) => Some(l),
        _ => None,
    }
}

impl Mode {
    /// The text field keys go to now, if any.
    fn line_mut(&mut self) -> Option<&mut LineEdit> {
        match self {
            Mode::Cmd(l)
            | Mode::Transfer(_, _, l)
            | Mode::Mkdir(l)
            | Mode::Link(LinkAsk { line: l, .. })
            | Mode::NewFile(l)
            | Mode::ConnEdit(_, l)
            | Mode::Editor(_, l)
            | Mode::Connecting(Connecting {
                ask: Some((Prompt::Password(_), l)),
                ..
            })
            | Mode::Xfer(Xfer {
                rename: Some(l), ..
            }) => Some(l),
            Mode::FindForm(f) => text_row(&mut f.rows, f.focus),
            Mode::MultiRename(f) => text_row(&mut f.rows, f.focus),
            Mode::Settings(f) => text_row(&mut f.rows, f.focus),
            Mode::Props(f) => text_row(&mut f.rows, f.focus),
            Mode::BookmarkEdit(f) => f.line_mut(),
            Mode::Filter(f) => text_row(&mut f.rows, f.focus),
            Mode::Procs(p) => p
                .draft
                .as_mut()
                .and_then(|d| text_row(&mut d.rows, d.focus)),
            _ => None,
        }
    }
}

impl App {
    /// Mouse caret: puts the cursor of the focused text field before char `chars` (clamped).
    /// No text changes, so no per-key side effects.
    pub fn set_caret(&mut self, chars: usize) {
        if let Some(l) = self.field_mut() {
            l.cur = l
                .text
                .char_indices()
                .nth(chars)
                .map_or(l.text.len(), |(i, _)| i);
        }
    }

    fn field_mut(&mut self) -> Option<&mut LineEdit> {
        if let Some(i) = &mut self.input {
            Some(&mut i.text)
        } else if let Some(f) = &mut self.search_form {
            text_row(&mut f.rows, f.focus)
        } else {
            self.mode.line_mut()
        }
    }

    /// The egui text field of the focused prompt edited `text`, caret before char `caret`:
    /// take both, then run the per-key side effects (preview, filter, error reset) once.
    pub fn set_line(&mut self, text: String, caret: usize) {
        let text = text.replace(['\r', '\n'], "");
        let Some(l) = self.field_mut() else { return };
        if l.text != text {
            l.replace(text, 0);
            self.on_key(KeyEvent::new(KeyCode::Null, KeyModifiers::NONE));
        }
        self.set_caret(caret);
    }

    /// Rows and focus of the open form: the search dialog over the text, else the form screen.
    fn form_rows_mut(&mut self) -> Option<(&[FormRow], &mut usize)> {
        if let Some(f) = &mut self.search_form {
            return Some((&f.rows, &mut f.focus));
        }
        self.mode.form_mut()
    }

    /// Click on form row `row`: focuses it; a checkbox toggles, a choice cycles (as Space would).
    pub fn form_click(&mut self, row: usize) {
        // filter popup: rows past the text fields are its file type checkboxes
        if let Mode::Filter(f) = &mut self.mode
            && row >= f.rows.len()
        {
            if row < f.rows.len() + f.exts.len() {
                f.focus = row;
                self.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
            }
            return;
        }
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

/// Rows of tab `t` of a tool form whose tabs hold `sizes[t]` rows each, in row order
/// (0: a tab without rows, like the multi-rename Editor).
pub(super) fn tab_range(sizes: &[usize], t: usize) -> std::ops::Range<usize> {
    let start: usize = sizes[..t].iter().sum();
    start..start + sizes[t]
}

/// Tab that holds row `row`; none for a row past the tabs.
pub(super) fn tab_of(sizes: &[usize], row: usize) -> Option<usize> {
    (0..sizes.len()).find(|&t| tab_range(sizes, t).contains(&row))
}

/// Tab keys of the tool forms (`n` tabs): F2.. and Alt+1.. show a tab, Ctrl+PgUp/PgDn step.
pub(super) fn tab_key(key: KeyEvent, tab: usize, n: usize) -> Option<usize> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let t = match key.code {
        KeyCode::F(f) if !alt && !ctrl && (2..2 + n).contains(&usize::from(f)) => {
            usize::from(f) - 2
        }
        KeyCode::Char(d @ '1'..='9') if alt => d as usize - '1' as usize,
        KeyCode::PageDown if ctrl => tab + 1,
        KeyCode::PageUp if ctrl => tab.saturating_sub(1),
        _ => return None,
    };
    Some(t.min(n - 1))
}

/// Tab / Down: the next of `len` form rows, Shift+Tab / Up: the previous (no wrap).
pub(super) fn step_key(code: KeyCode, focus: usize, len: usize) -> usize {
    let delta = if matches!(code, KeyCode::Tab | KeyCode::Down) {
        1
    } else {
        -1
    };
    step_focus(0..len, focus, delta, |_| false)
}

/// Focus `delta` rows on from `focus` inside `r`, skipping `hidden` rows (an end row stops it).
pub(super) fn step_focus(
    r: std::ops::Range<usize>,
    focus: usize,
    delta: isize,
    hidden: impl Fn(usize) -> bool,
) -> usize {
    if r.is_empty() {
        return focus;
    }
    let (first, last) = (r.start as isize, r.end as isize - 1);
    let mut f = focus as isize;
    loop {
        f = (f + delta).clamp(first, last);
        let at = f as usize;
        if !hidden(at) || f == first || f == last {
            return at;
        }
    }
}

/// Edit keys shared by all text prompts; true if `key` was one of them.
pub(super) fn edit_key(l: &mut LineEdit, key: KeyEvent) -> bool {
    // Alt: macOS Option+arrow, Ctrl+arrow switches Spaces there
    let word = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        KeyCode::Left if word => l.word_left(),
        KeyCode::Right if word => l.word_right(),
        KeyCode::Backspace if word => l.delete_word(false),
        KeyCode::Delete if word => l.delete_word(true),
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
        // the egui field changed the text: run the caller's per-key side effects
        KeyCode::Null => {}
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

#[cfg(test)]
mod tests {
    use super::{KeyCode, KeyEvent, KeyModifiers, step_focus, tab_key, tab_of, tab_range};

    #[test]
    fn tabs_split_rows_and_keys_pick_one() {
        let sizes = [2, 0, 3];
        assert_eq!(tab_range(&sizes, 1), 2..2);
        assert_eq!(tab_range(&sizes, 2), 2..5);
        assert_eq!(tab_of(&sizes, 4), Some(2));
        assert_eq!(tab_of(&sizes, 5), None);
        let k = |c, m| tab_key(KeyEvent::new(c, m), 1, 3);
        assert_eq!(k(KeyCode::F(4), KeyModifiers::NONE), Some(2));
        assert_eq!(k(KeyCode::F(5), KeyModifiers::NONE), None);
        assert_eq!(k(KeyCode::Char('1'), KeyModifiers::ALT), Some(0));
        assert_eq!(k(KeyCode::PageDown, KeyModifiers::CONTROL), Some(2));
        assert_eq!(k(KeyCode::Char('1'), KeyModifiers::NONE), None);
    }

    #[test]
    fn focus_steps_over_hidden_rows_and_stays_inside() {
        let hidden = |i| i == 3;
        assert_eq!(step_focus(2..6, 2, 1, hidden), 4);
        assert_eq!(step_focus(2..6, 4, -1, hidden), 2);
        assert_eq!(step_focus(2..6, 5, 1, hidden), 5);
        // a focus outside the range (a row past the tabs) comes back into it
        assert_eq!(step_focus(0..3, 7, -1, |_| false), 2);
        assert_eq!(step_focus(0..0, 7, 1, |_| false), 7);
    }
}
