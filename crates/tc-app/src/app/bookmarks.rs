//! Hotlist (Ctrl+D) of bookmarked directories.

use super::{App, Filter, Mode, REMOTE, edit_key, list_nav, list_rows};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::cmdline::LineEdit;
use tc_core::hotlist;

impl App {
    /// Bookmarks the active panel's directory; the row of the new or existing entry.
    pub(super) fn hotlist_add(&mut self) -> Option<usize> {
        if self.panels[self.active].remote().is_some() {
            self.mode = Mode::Message(REMOTE.into());
            return None;
        }
        let path = self.panels[self.active].path().to_owned();
        let row = self.hotlist.iter().position(|b| b.path == path);
        if row.is_none() {
            self.hotlist.push(hotlist::Bookmark::new(path));
            self.hotlist_save();
        }
        row.or(Some(self.hotlist.len() - 1))
    }

    /// Ctrl/Shift+Up/Down: swaps bookmark `sel` with its neighbour and saves; the new row.
    pub(super) fn hotlist_move(&mut self, sel: usize, down: bool) -> usize {
        let to = if down { sel + 1 } else { sel.wrapping_sub(1) };
        if to >= self.hotlist.len() || sel >= self.hotlist.len() {
            return sel;
        }
        self.hotlist.swap(sel, to);
        self.hotlist_save();
        to
    }

    pub(super) fn hotlist_save(&mut self) {
        if let Some(file) = &self.hotlist_file
            && let Err(e) = hotlist::save_bookmarks(file, &self.hotlist)
        {
            self.mode = Mode::Message(e.to_string());
        }
    }

    /// Bookmark or connection list, shown without a filter.
    pub(super) fn open_list(&mut self, mode: Mode) {
        self.list_filter = Filter::default();
        self.mode = mode;
    }

    pub(super) fn hotlist_key(&mut self, key: KeyEvent, sel: usize) {
        let mut sel = sel.min(self.hotlist.len().saturating_sub(1));
        let moved = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        if let (true, KeyCode::Up | KeyCode::Down) = (moved, key.code) {
            if !self.list_filter.active() && !self.list_filter.editing {
                sel = self.hotlist_move(sel, key.code == KeyCode::Down);
            }
            if !matches!(self.mode, Mode::Message(_)) {
                self.mode = Mode::Hotlist(sel);
            }
            return;
        }
        if list_nav(&mut self.list_filter, &self.hotlist, &mut sel, key) {
            self.mode = Mode::Hotlist(sel);
            return;
        }
        let shown = list_rows(&self.hotlist, &self.list_filter).contains(&sel);
        match key.code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
            KeyCode::Insert | KeyCode::Char('a') => {
                self.list_filter = Filter::default();
                match self.hotlist_add() {
                    Some(row) if !matches!(self.mode, Mode::Message(_)) => sel = row,
                    _ => return,
                }
            }
            KeyCode::F(4) | KeyCode::Char('e') if shown => {
                let alias = self.hotlist[sel].alias.clone();
                self.mode = Mode::HotlistAlias(sel, LineEdit::from(alias));
                return;
            }
            KeyCode::Enter if shown => {
                let path = self.hotlist[sel].path.clone();
                if let Err(e) = self.panels[self.active].go(path) {
                    self.mode = Mode::Message(e.to_string());
                }
                return;
            }
            KeyCode::Delete | KeyCode::Backspace if shown => {
                self.hotlist.remove(sel);
                self.hotlist_save();
                sel = list_rows(&self.hotlist, &self.list_filter)
                    .first()
                    .copied()
                    .unwrap_or(0);
                if matches!(self.mode, Mode::Message(_)) {
                    return;
                }
            }
            _ => {}
        }
        self.mode = Mode::Hotlist(sel);
    }

    pub(super) fn hotlist_alias_key(&mut self, key: KeyEvent, row: usize, mut l: LineEdit) {
        match key.code {
            KeyCode::Enter => {
                self.hotlist[row].alias = l.text.replace('\t', "").trim().to_owned();
                self.hotlist_save();
                if !matches!(self.mode, Mode::Message(_)) {
                    self.mode = Mode::Hotlist(row);
                }
            }
            KeyCode::Esc => self.mode = Mode::Hotlist(row),
            _ => {
                edit_key(&mut l, key);
                self.mode = Mode::HotlistAlias(row, l);
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::super::{App, ListRow, Mode};

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use tc_core::panel::Panel;

    use super::super::tests::{key, two_panels, type_keys, with};
    use tc_core::hotlist;

    #[test]
    fn slash_filters_bookmarks_and_enter_goes_to_the_shown_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("one"), tmp.path().join("two"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        app.hotlist = vec![hotlist::Bookmark::new(a), hotlist::Bookmark::new(b.clone())];
        with(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, "TW");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.panels[app.active].path(), b);
    }

    #[test]
    fn hotlist_insert_alias_and_reorder() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut app = two_panels(&a, &b);
        let file = tmp.path().join("cfg").join("hotlist");
        app.hotlist_file = Some(file.clone());
        with(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert); // no duplicate
        assert_eq!(app.hotlist.len(), 1);
        app.hotlist = vec![hotlist::Bookmark::new(a), hotlist::Bookmark::new(b)];
        key(&mut app, KeyCode::F(4));
        type_keys(&mut app, "Work");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Hotlist(0)));
        let saved = hotlist::load_bookmarks(&file).unwrap();
        assert_eq!(saved[0].alias, "Work");
        assert!(saved[0].label().starts_with("Work  "));
        with(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
        assert!(matches!(app.mode, Mode::Hotlist(1)));
        assert_eq!(hotlist::load_bookmarks(&file).unwrap()[1].alias, "Work");
        // filter active: positions are ambiguous, nothing moves
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, "work");
        key(&mut app, KeyCode::Enter);
        with(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(app.hotlist[1].alias, "Work");
    }

    #[test]
    fn hotlist_add_jump_delete_and_history() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = App::new(Panel::open(&sub).unwrap(), Panel::open(tmp.path()).unwrap());
        let file = tmp.path().join("cfg").join("hotlist");
        app.hotlist_file = Some(file.clone());
        let ctrl =
            |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        ctrl(&mut app, 'd');
        ctrl(&mut app, 'd'); // no duplicate
        assert_eq!(
            hotlist::load_bookmarks(&file).unwrap(),
            [hotlist::Bookmark::new(app.panels[0].path().to_owned())]
        );
        key(&mut app, KeyCode::Tab);
        ctrl(&mut app, 'b');
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.panels[1].path(), sub);
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.panels[1].path(), tmp.path());
        // vanished dir: error popup, panel unchanged
        std::fs::remove_dir(&sub).unwrap();
        ctrl(&mut app, 'b');
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Message(_)));
        assert_eq!(app.panels[1].path(), tmp.path());
        key(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'b');
        key(&mut app, KeyCode::Backspace); // same as Delete
        assert!(app.hotlist.is_empty() && hotlist::load_bookmarks(&file).unwrap().is_empty());
    }
}
