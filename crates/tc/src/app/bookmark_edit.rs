//! Bookmarks edit page (F4 in the bookmarks): order, alias, path, add, delete; saved on Enter.

use super::{App, Mode, edit_key};
use crate::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use tc_core::cmdline::LineEdit;
use tc_core::hotlist::Bookmark;

/// One bookmark being edited; `orig` keeps a path the text field cannot show (non-UTF-8).
pub struct BookmarkRow {
    pub alias: LineEdit,
    pub path: LineEdit,
    orig: Option<PathBuf>,
}

/// All bookmarks as editable rows; the list changes only on Save.
pub struct BookmarkForm {
    pub rows: Vec<BookmarkRow>,
    /// Focused row and column (0 alias, 1 path).
    pub focus: (usize, usize),
    pub error: Option<String>,
}

/// Mouse on the edit page, applied through [`App::bookmark_click`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmClick {
    /// Text field of a row: 0 alias, 1 path.
    Field(usize, usize),
    Up(usize),
    Down(usize),
    Delete(usize),
    Add,
    Save,
    Cancel,
}

impl BookmarkForm {
    pub(super) fn new(list: &[Bookmark], sel: usize) -> Self {
        let rows = list
            .iter()
            .map(|b| BookmarkRow {
                alias: LineEdit::from(b.alias.clone()),
                path: LineEdit::from(b.path.to_string_lossy().into_owned()),
                orig: Some(b.path.clone()),
            })
            .collect::<Vec<_>>();
        let row = sel.min(rows.len().saturating_sub(1));
        Self {
            rows,
            focus: (row, 0),
            error: None,
        }
    }

    /// Focused text field, if there is a row.
    pub(super) fn line_mut(&mut self) -> Option<&mut LineEdit> {
        let (r, c) = self.focus;
        self.rows.get_mut(r).map(|row| {
            if c == 0 {
                &mut row.alias
            } else {
                &mut row.path
            }
        })
    }

    /// The edited list, or why it cannot be saved (rows count from 1).
    fn apply(&self) -> Result<Vec<Bookmark>, String> {
        let mut out = Vec::with_capacity(self.rows.len());
        for (i, r) in self.rows.iter().enumerate() {
            let path = r.path.text.trim();
            if path.is_empty() {
                return Err(format!("Row {}: path is empty.", i + 1));
            }
            if path.contains('\t') {
                return Err(format!("Row {}: path must not contain a tab.", i + 1));
            }
            let path = match &r.orig {
                Some(p) if p.to_string_lossy().trim() == path => p.clone(),
                _ => PathBuf::from(path),
            };
            out.push(Bookmark {
                alias: r.alias.text.replace('\t', "").trim().to_owned(),
                path,
            });
        }
        Ok(out)
    }

    /// Tab order: alias, path, next row's alias, wrapping.
    fn step(&mut self, forward: bool) {
        let n = self.rows.len() * 2;
        if n == 0 {
            return;
        }
        let i = self.focus.0 * 2 + self.focus.1;
        let i = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.focus = (i / 2, i % 2);
    }

    /// Swap the focused row with its neighbour; focus follows it.
    fn swap(&mut self, down: bool) {
        let r = self.focus.0;
        let to = if down { r + 1 } else { r.wrapping_sub(1) };
        if to < self.rows.len() {
            self.rows.swap(r, to);
            self.focus.0 = to;
        }
    }

    fn delete(&mut self) {
        if self.focus.0 < self.rows.len() {
            self.rows.remove(self.focus.0);
            self.focus.0 = self.focus.0.min(self.rows.len().saturating_sub(1));
        }
    }

    /// New row with `path` (empty: type one), focused on the path.
    fn add(&mut self, path: Option<PathBuf>) {
        self.rows.push(BookmarkRow {
            alias: LineEdit::default(),
            path: LineEdit::from(
                path.as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            orig: path,
        });
        self.focus = (self.rows.len() - 1, 1);
    }
}

impl App {
    /// Enter saves, Esc discards; Insert adds the panel directory (or an empty row),
    /// F8 / Ctrl+Delete deletes the row, Ctrl+Up/Down (or Shift) moves it.
    pub(super) fn bookmark_edit_key(&mut self, key: KeyEvent, mut f: BookmarkForm) {
        let moved = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = f.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => {
                self.mode = Mode::Bookmarks(f.focus.0.min(self.hotlist.len().saturating_sub(1)));
                return;
            }
            KeyCode::Enter => match f.apply() {
                Ok(list) => {
                    self.hotlist = list;
                    self.mode = Mode::Bookmarks(f.focus.0);
                    self.hotlist_save();
                    return;
                }
                Err(e) => f.error = Some(e),
            },
            KeyCode::Up | KeyCode::Down if moved => f.swap(key.code == KeyCode::Down),
            KeyCode::Up => f.focus.0 = f.focus.0.saturating_sub(1),
            KeyCode::Down => f.focus.0 = (f.focus.0 + 1).min(last),
            KeyCode::Tab => f.step(true),
            KeyCode::BackTab => f.step(false),
            KeyCode::F(8) => f.delete(),
            KeyCode::Delete if ctrl => f.delete(),
            KeyCode::Insert => {
                let panel = &self.panels[self.active];
                let path = panel.remote().is_none().then(|| panel.path().to_owned());
                f.add(path);
            }
            _ => {
                if let Some(l) = f.line_mut() {
                    edit_key(l, key);
                }
            }
        }
        self.mode = Mode::BookmarkEdit(Box::new(f));
    }

    /// A click on the edit page: focus the row, then act like its key.
    pub fn bookmark_click(&mut self, click: BmClick) {
        let Mode::BookmarkEdit(f) = &mut self.mode else {
            return;
        };
        let (row, code, mods) = match click {
            BmClick::Field(r, c) => {
                f.focus = (r, c);
                return;
            }
            BmClick::Up(r) => (Some(r), KeyCode::Up, KeyModifiers::CONTROL),
            BmClick::Down(r) => (Some(r), KeyCode::Down, KeyModifiers::CONTROL),
            BmClick::Delete(r) => (Some(r), KeyCode::F(8), KeyModifiers::NONE),
            BmClick::Add => (None, KeyCode::Insert, KeyModifiers::NONE),
            BmClick::Save => (None, KeyCode::Enter, KeyModifiers::NONE),
            BmClick::Cancel => (None, KeyCode::Esc, KeyModifiers::NONE),
        };
        if let Some(r) = row {
            f.focus.0 = r;
        }
        self.on_key(KeyEvent::new(code, mods));
    }
}

#[cfg(test)]
mod tests {
    use super::super::Mode;
    use super::super::tests::{key, two_panels, type_keys, with};
    use super::BmClick;
    use crate::{KeyCode, KeyModifiers};
    use std::path::Path;
    use tc_core::hotlist::Bookmark;

    #[test]
    fn edit_page_orders_edits_adds_deletes_and_saves_on_enter_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        app.hotlist = vec![Bookmark::new("/a".into()), Bookmark::new("/b".into())];
        with(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::F(4));
        // alias of a, then move a below b
        type_keys(&mut app, "A");
        with(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
        // add the panel dir, delete b by click
        key(&mut app, KeyCode::Insert);
        app.bookmark_click(BmClick::Delete(0));
        // Esc discards everything
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.hotlist.len(), 2);
        assert_eq!(app.hotlist[0].path, Path::new("/a"));
        key(&mut app, KeyCode::F(4));
        type_keys(&mut app, "A");
        with(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Insert);
        app.bookmark_click(BmClick::Delete(0));
        app.bookmark_click(BmClick::Save);
        assert!(matches!(app.mode, Mode::Bookmarks(_)));
        let got: Vec<_> = app
            .hotlist
            .iter()
            .map(|b| (b.alias.as_str(), b.path.clone()))
            .collect();
        assert_eq!(got, [("A", "/a".into()), ("", tmp.path().to_owned())]);
    }

    #[test]
    fn edit_page_refuses_empty_path_and_edits_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        app.hotlist = vec![Bookmark::new("/a".into())];
        with(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::F(4));
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Enter);
        assert!(matches!(&app.mode, Mode::BookmarkEdit(f) if f.error.is_some()));
        type_keys(&mut app, "/c");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.hotlist[0].path, Path::new("/c"));
    }
}
