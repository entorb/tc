//! F1 help popup: filter, scroll, run the key of a line.

use super::{App, Mode, help_action};
use crate::fmt;
use crate::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    /// F1 opens the help here: the footer screens plus lists and the conflict question.
    pub(super) fn takes_f1(&self) -> bool {
        self.footer_active()
            || self.search_form.is_some()
            || (self.input.is_none()
                && match &self.mode {
                    Mode::Bookmarks(_)
                    | Mode::BookmarkEdit(_)
                    | Mode::Popular(_)
                    | Mode::Connections(_)
                    | Mode::Drives(..)
                    | Mode::DirHistory(..)
                    | Mode::Compare(_) => true,
                    Mode::Xfer(x) => (x.ask.is_some() || x.error.is_some()) && x.rename.is_none(),
                    _ => false,
                })
    }

    /// Title and text of the F1 help as shown: the search dialog's own over the text.
    pub fn help_text(&self) -> (&'static str, &'static str) {
        if self.search_form.is_some() {
            ("Help: search", fmt::HELP_SEARCH)
        } else {
            fmt::help(&self.mode)
        }
    }

    /// Help lines as shown (matching the filter, all when it is empty) with the key each runs
    /// (only the main help has keys). Built once per frame or key, not per line.
    pub fn help_entries(&self) -> Vec<(&'static str, Option<(KeyCode, KeyModifiers)>)> {
        let (_, text) = self.help_text();
        let main = text == fmt::HELP_MAIN;
        fmt::help_filter(text, &self.help_filter.to_lowercase())
            .into_iter()
            .map(|l| (l, if main { help_action(l) } else { None }))
            .collect()
    }

    /// Key a shown help line runs.
    pub fn help_action_at(&self, line: usize) -> Option<(KeyCode, KeyModifiers)> {
        self.help_entries().get(line).and_then(|e| e.1)
    }

    /// Click on help text line `line`: run the action the line describes; the help stays open.
    pub fn help_click(&mut self, line: usize) {
        if let Some((c, m)) = self.help_action_at(line) {
            self.on_key(KeyEvent::new(c, m));
        }
    }

    /// Close button or a click beside the help.
    pub fn help_close(&mut self) {
        self.help = false;
    }

    /// New filter text: selects the first key line and scrolls to the top.
    pub(super) fn help_set_filter(&mut self, filter: String) {
        self.help_filter = filter;
        self.help_scroll = 0;
        self.help_sel = self
            .help_entries()
            .iter()
            .position(|e| is_item(e.0))
            .unwrap_or(0);
    }

    /// Moves the selection to the next key line in `dir` (runnable or not), or scrolls when there are none.
    pub(super) fn help_move(&mut self, dir: isize) {
        let sel = self.help_sel as isize;
        let next = self
            .help_entries()
            .iter()
            .enumerate()
            .filter(|(i, e)| is_item(e.0) && (*i as isize - sel).signum() == dir)
            .map(|(i, _)| i)
            .min_by_key(|&i| (i as isize - sel).abs());
        match next {
            Some(i) => {
                self.help_sel = i;
                self.help_follow();
            }
            None if dir < 0 => self.help_scroll = self.help_scroll.saturating_sub(1),
            None => self.help_scroll += 1,
        }
    }

    /// Keeps the selected line inside the popup.
    pub(super) fn help_follow(&mut self) {
        let rows = self.help_rows.max(1);
        if self.help_sel < self.help_scroll {
            self.help_scroll = self.help_sel;
        } else if self.help_sel >= self.help_scroll + rows {
            self.help_scroll = self.help_sel + 1 - rows;
        }
    }

    /// A key typed in the help window.
    pub fn help_key(&mut self, key: KeyEvent) {
        let page = self.page;
        let typing = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc if !self.help_filter.is_empty() => self.help_set_filter(String::new()),
            KeyCode::Esc | KeyCode::F(1) | KeyCode::F(10) => self.help = false,
            KeyCode::Enter if self.help_action_at(self.help_sel).is_some() => {
                self.help_click(self.help_sel)
            }
            KeyCode::Enter => self.help = false,
            KeyCode::Backspace => {
                let mut f = self.help_filter.clone();
                f.pop();
                self.help_set_filter(f);
            }
            KeyCode::Char(c) if typing => {
                let mut f = self.help_filter.clone();
                f.push(c);
                self.help_set_filter(f);
            }
            KeyCode::Up => self.help_move(-1),
            KeyCode::Down => self.help_move(1),
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(page),
            KeyCode::PageDown => self.help_scroll += page,
            KeyCode::Home => self.help_scroll = 0,
            KeyCode::End => self.help_scroll = usize::MAX,
            _ => {}
        }
    }
}

/// Key line of a help text: the cursor stops there.
fn is_item(line: &str) -> bool {
    matches!(fmt::help_line(line), fmt::HelpLine::Entry(..))
}

#[cfg(test)]
mod tests {

    use super::super::{App, Mode};
    use crate::fmt;
    use crate::{KeyCode, KeyEvent, KeyModifiers};

    use tc_core::panel::Panel;

    use super::super::tests::key;

    /// A key typed in the help window.
    fn hk(app: &mut App, code: KeyCode) {
        app.help_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn f1_help_opens_on_forms_and_lists() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        for (code, mods) in [
            (KeyCode::Char('f'), KeyModifiers::CONTROL),
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
            (KeyCode::Char('n'), KeyModifiers::CONTROL),
            (KeyCode::F(1), KeyModifiers::ALT),
        ] {
            app.on_key(KeyEvent::new(code, mods));
            let title = app.help_text().0;
            key(&mut app, KeyCode::F(1));
            assert!(app.help && title != "Help", "{title} {code:?}");
            hk(&mut app, KeyCode::Esc);
            assert!(!app.help && !matches!(app.mode, Mode::Normal));
            key(&mut app, KeyCode::Esc);
            assert!(matches!(app.mode, Mode::Normal));
        }
    }

    #[test]
    fn help_line_click_runs_action() {
        let tmp = tempfile::tempdir().unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        key(&mut app, KeyCode::F(1));
        let line = fmt::HELP_MAIN
            .lines()
            .position(|l| l.starts_with("Ctrl+B "))
            .unwrap();
        app.help_click(line);
        assert!(app.help && matches!(app.mode, Mode::Bookmarks(_)));
    }

    #[test]
    fn f1_help_esc_closes_typing_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(1));
        assert!(app.help);
        for c in "ncdu".chars() {
            hk(&mut app, KeyCode::Char(c));
        }
        let e = app.help_entries();
        // section title, the entry and its continuation line
        assert_eq!(e[0].0, "== Highlights ==");
        assert!(e[1].0.contains("FILE STATS") && e[2].0.starts_with(' '));
        hk(&mut app, KeyCode::Esc);
        assert!(app.help && app.help_filter.is_empty());
        hk(&mut app, KeyCode::Esc);
        assert!(!app.help && matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn help_cursor_stops_on_key_lines_without_action() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        key(&mut app, KeyCode::F(1));
        let first = app.help_sel;
        hk(&mut app, KeyCode::Down);
        assert!(app.help_sel > first && app.help_action_at(app.help_sel).is_none());
    }

    #[test]
    fn help_down_scrolls_without_key_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(1));
        for c in "type to filter".chars() {
            hk(&mut app, KeyCode::Char(c));
        }
        assert!(app.help_entries().iter().all(|e| e.1.is_none()));
        hk(&mut app, KeyCode::Down);
        assert_eq!(app.help_scroll, 1);
    }

    #[test]
    fn help_filter_and_enter_runs_selected_key() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        key(&mut app, KeyCode::F(1));
        // first key line is selected; Down moves to the next key line
        let first = app.help_sel;
        hk(&mut app, KeyCode::Down);
        assert!(app.help_sel > first && app.help_action_at(app.help_sel).is_some());
        hk(&mut app, KeyCode::Up);
        assert_eq!(app.help_sel, first);
        // filter to Ctrl+B bookmarks, Enter runs it
        for c in "bookmarks".chars() {
            hk(&mut app, KeyCode::Char(c));
        }
        assert!(app.help_action_at(app.help_sel).is_some());
        hk(&mut app, KeyCode::Enter);
        assert!(app.help && matches!(app.mode, Mode::Bookmarks(_)));
        // the main window works with the help open; F1 there closes it
        key(&mut app, KeyCode::Esc);
        assert!(app.help && matches!(app.mode, Mode::Normal));
        key(&mut app, KeyCode::F(1));
        assert!(!app.help);
    }
}
