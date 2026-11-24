//! Panel tabs and mouse input on the panels.

use super::{App, Mode};
use crate::{KeyCode, KeyEvent, KeyModifiers};
use std::mem;
use std::path::Path;
use tc_core::archive::Format;
use tc_core::panel::{Panel, SortKey};

impl App {
    /// Tabs per side: `panels[side]` is the visible tab, rotated through the ring in `tabs[side]`.
    pub(super) fn tab_key(&mut self, code: KeyCode) {
        let side = self.active;
        let bg = &mut self.tabs[side];
        let n = bg.len() + 1;
        let next = match code {
            KeyCode::Char('t') => {
                match self.panels[side].duplicate() {
                    Ok(p) => self.push_tab(side, p),
                    Err(e) => self.mode = Mode::Message(e.to_string()),
                }
                return;
            }
            KeyCode::Char('w') => {
                let Some(p) = bg.pop_front() else { return };
                let old = mem::replace(&mut self.panels[side], p);
                self.tab[side] %= n - 1;
                self.remember_closed(side, &old);
                None
            }
            KeyCode::PageDown | KeyCode::Tab => bg.pop_front().map(|p| {
                self.tab[side] = (self.tab[side] + 1) % n;
                (p, false)
            }),
            _ => bg.pop_back().map(|p| {
                self.tab[side] = (self.tab[side] + n - 1) % n;
                (p, true)
            }),
        };
        if let Some((p, prev)) = next {
            let old = mem::replace(&mut self.panels[side], p);
            if prev {
                self.tabs[side].push_front(old);
            } else {
                self.tabs[side].push_back(old);
            }
        } else if code != KeyCode::Char('w') {
            return;
        }
        // the directory may have changed while the tab was hidden
        if let Err(e) = self.panels[side].reload() {
            self.mode = Mode::Message(e.to_string());
        }
    }

    /// Show `p` as a new tab of `side`, after the visible one.
    fn push_tab(&mut self, side: usize, p: Panel) {
        self.tabs[side].push_back(mem::replace(&mut self.panels[side], p));
        self.tab[side] += 1;
    }

    /// Keep a closed local tab's directory for Ctrl+Shift+T (last 10); remote, archive and found tabs are not kept.
    fn remember_closed(&mut self, side: usize, p: &Panel) {
        if p.remote().is_none() && p.archive().is_none() && !p.is_virtual() {
            self.closed_tabs.push((side, p.path().to_owned()));
            let over = self.closed_tabs.len().saturating_sub(10);
            self.closed_tabs.drain(..over);
        }
    }

    /// Ctrl+Shift+T: open the last closed tab again on its side.
    pub(super) fn reopen_tab(&mut self) {
        let Some((side, dir)) = self.closed_tabs.pop() else {
            return;
        };
        match Panel::open(&dir) {
            Ok(p) => {
                self.active = side;
                self.push_tab(side, p);
            }
            Err(e) => self.mode = Mode::Message(e.to_string()),
        }
    }

    /// Ctrl+Shift+O: move the visible tab to the other side; the last tab of a side stays.
    pub(super) fn tab_to_other_side(&mut self) {
        let side = self.active;
        let Some(next) = self.tabs[side].pop_front() else {
            return;
        };
        let moved = mem::replace(&mut self.panels[side], next);
        self.tab[side] %= self.tabs[side].len() + 1;
        self.active = 1 - side;
        self.push_tab(1 - side, moved);
        if let Err(e) = self.panels[side].reload() {
            self.mode = Mode::Message(e.to_string());
        }
    }

    /// Middle click on entry `idx` of panel `i`: its directory in a new tab.
    pub fn open_tab(&mut self, i: usize, idx: usize) {
        if self.input.is_some() || !matches!(self.mode, Mode::Normal) {
            return;
        }
        self.active = i;
        let panel = &mut self.panels[i];
        panel.set_cursor(idx);
        let dir = (panel.current())
            .filter(|e| e.is_dir && !e.is_parent() && panel.archive().is_none())
            .map(|e| panel.path().join(&e.name));
        let Some(dir) = dir else { return };
        let tab = match panel.remote().cloned() {
            // the duplicate is still connecting: open the directory on that remote, not by side
            Some(r) => (panel.duplicate()).and_then(|mut p| p.open_remote(r, dir).map(|()| p)),
            None => Panel::open(&dir),
        };
        match tab {
            Ok(p) => self.push_tab(i, p),
            Err(e) => self.mode = Mode::Message(e.to_string()),
        }
    }

    /// Directory names of one side's tabs in cycle order; `self.tab[side]` is the visible one.
    pub fn tab_names(&self, side: usize) -> Vec<String> {
        let (bg, p) = (&self.tabs[side], self.tab[side]);
        let n = bg.len() + 1;
        (0..n)
            .map(|k| {
                let panel = if k == p {
                    &self.panels[side]
                } else {
                    &bg[(k + n - p - 1) % n]
                };
                panel
                    .path()
                    .file_name()
                    .map_or_else(|| panel.title(), |name| name.to_string_lossy().into_owned())
            })
            .collect()
    }

    /// Shows tab `k` of `side` (click on a tab button).
    /// Close tab `k` of `side` (right click); the last tab of a side stays.
    pub fn close_tab(&mut self, side: usize, k: usize) {
        if !self.tabs[side].is_empty() {
            self.select_tab(side, k);
            self.tab_key(KeyCode::Char('w'));
        }
    }

    pub fn select_tab(&mut self, side: usize, k: usize) {
        self.active = side;
        let n = self.tabs[side].len() + 1;
        for _ in 0..(k + n - self.tab[side]) % n {
            self.tab_key(KeyCode::PageDown);
        }
    }

    /// Wheel / touchpad scroll by `lines` in viewer, editor and process list (editor: moves the cursor, the window follows).
    pub fn on_wheel(&mut self, lines: isize) {
        if self.input.is_some() {
            return;
        }
        match &mut self.mode {
            Mode::View { view, .. } => {
                if let Err(e) = view.scroll(lines) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            Mode::Edit {
                editor,
                error: None,
            } => editor.move_v(lines),
            Mode::Procs(p) if p.idle() => {
                let last = p.rows.len().saturating_sub(1);
                p.sel = p.sel.saturating_add_signed(lines).min(last);
            }
            Mode::Stats(st) if st.confirm.is_none() => {
                let last = st.view.len().saturating_sub(1);
                st.sel = st.sel.saturating_add_signed(lines).min(last);
            }
            _ => {}
        }
    }

    /// Press (or drag, `extend`) on the editor text at `row`, `col` of a `width` columns wide text area.
    pub fn editor_click(&mut self, row: usize, col: usize, width: usize, extend: bool) {
        if self.input.is_none()
            && self.search_form.is_none()
            && self.replacing.is_none()
            && let Mode::Edit {
                editor,
                error: None,
            } = &mut self.mode
        {
            editor.click(row, col, width, extend);
        }
    }

    /// Sort panel `i` by `key` (same key again reverses) and make it active.
    pub fn sort_by(&mut self, i: usize, key: SortKey) {
        self.active = i;
        self.panels[i].set_sort(key);
    }

    /// Click into panel `i` outside its entries: make it active.
    pub fn focus_panel(&mut self, i: usize) {
        if self.input.is_none() && matches!(self.mode, Mode::Normal) {
            self.active = i;
        }
    }

    /// Click on entry `idx` of panel `i`: cursor there, Shift selects the range from the old cursor,
    /// Ctrl toggles the entry's selection, double click opens.
    pub fn click_entry(&mut self, i: usize, idx: usize, mods: KeyModifiers, double: bool) {
        if self.input.is_some() || !matches!(self.mode, Mode::Normal) {
            return;
        }
        self.active = i;
        let panel = &mut self.panels[i];
        if mods.contains(KeyModifiers::SHIFT) {
            panel.select_range(panel.cursor(), idx);
            return;
        }
        panel.set_cursor(idx);
        if mods.contains(KeyModifiers::CONTROL) {
            panel.toggle_select();
            panel.set_cursor(idx);
            return;
        }
        if double {
            let is_dir = self.panels[i]
                .current()
                .is_some_and(|e| e.is_dir || Format::detect(Path::new(&e.name)).is_some());
            let code = if is_dir {
                KeyCode::Enter
            } else {
                KeyCode::F(3)
            };
            self.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }
}

#[cfg(test)]
mod tests {

    use super::super::App;

    use crate::{KeyCode, KeyEvent, KeyModifiers};

    use tc_core::panel::{Panel, SortKey};

    use super::super::tests::{key, settle, two_panels, with};

    #[test]
    fn tabs_open_cycle_close_and_swap() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        let ctrl = KeyModifiers::CONTROL;
        with(&mut app, KeyCode::Char('t'), ctrl);
        assert_eq!((app.tabs[0].len(), app.tab[0]), (1, 1));
        app.go(0, sub.clone());
        with(&mut app, KeyCode::PageDown, ctrl);
        assert_eq!((app.panels[0].path(), app.tab[0]), (dir.path(), 0));
        with(&mut app, KeyCode::PageUp, ctrl);
        assert_eq!((app.panels[0].path(), app.tab[0]), (sub.as_path(), 1));
        with(&mut app, KeyCode::Char('u'), ctrl);
        assert_eq!(
            (app.tabs[1].len(), app.tab[1], app.tabs[0].len()),
            (1, 1, 0)
        );
        key(&mut app, KeyCode::Tab);
        with(&mut app, KeyCode::Char('w'), ctrl);
        assert_eq!((app.panels[1].path(), app.tab[1]), (dir.path(), 0));
        assert!(app.tabs[1].is_empty());
        with(&mut app, KeyCode::Char('w'), ctrl); // last tab stays
        assert_eq!(app.panels[1].path(), dir.path());
        app.go(0, sub.clone());
        app.active = 0;
        with(&mut app, KeyCode::Char('t'), ctrl);
        app.go(0, dir.path().to_owned());
        app.close_tab(0, 0); // the first tab, while the second is shown
        assert_eq!((app.panels[0].path(), app.tabs[0].len()), (dir.path(), 0));
        app.close_tab(0, 0); // last one stays
        assert_eq!(app.panels[0].path(), dir.path());
    }

    #[test]
    fn swap_root_and_sort_click() {
        let (l, r) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(l.path().join(".h"), "").unwrap();
        let mut app = two_panels(l.path(), r.path());
        with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(app.panels[0].path(), r.path());
        with(&mut app, KeyCode::Char('='), KeyModifiers::ALT); // right gets left's dir
        assert_eq!(app.panels[1].path(), r.path());
        with(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL);
        settle(&mut app); // listing may take over the 50 ms wait on a loaded machine
        assert!(app.panels[0].path().parent().is_none());
        app.sort_by(1, SortKey::Size); // click on the size column title
        assert_eq!(app.panels[1].sort().0, SortKey::Size);
        with(&mut app, KeyCode::F(5), KeyModifiers::CONTROL); // active panel is 1 now: by date
        assert_eq!(app.panels[1].sort().0, SortKey::Date);
    }

    #[test]
    fn shift_arrows_and_clicks_select() {
        let tmp = tempfile::tempdir().unwrap();
        // subdir: ".." then loads the small tempdir at once, not the slow-to-read $TMPDIR (macOS)
        let dir = tmp.path().join("sub");
        std::fs::create_dir(&dir).unwrap();
        for n in ["a", "b", "c", "d"] {
            std::fs::write(dir.join(n), "").unwrap();
        }
        let mut app = App::new(Panel::open(&dir).unwrap(), Panel::open(&dir).unwrap());
        let shift = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        app.on_key(shift(KeyCode::Down)); // ".." is not selectable
        app.on_key(shift(KeyCode::Down));
        assert_eq!(app.panels[0].targets().len(), 1);
        let (none, ctrl) = (KeyModifiers::NONE, KeyModifiers::CONTROL);
        app.click_entry(1, 2, none, false); // entry 2 ("b") in right panel
        assert_eq!((app.active, app.panels[1].cursor()), (1, 2));
        app.click_entry(1, 4, KeyModifiers::SHIFT, false); // entries 2..=4
        assert_eq!(app.panels[1].targets().len(), 3);
        app.click_entry(1, 3, ctrl, false); // toggles "c" off, cursor stays
        assert_eq!(
            (app.panels[1].targets().len(), app.panels[1].cursor()),
            (2, 3)
        );
        app.click_entry(1, 3, ctrl, false);
        assert_eq!(app.panels[1].targets().len(), 3);
        with(
            &mut app,
            KeyCode::Char('A'),
            ctrl.union(KeyModifiers::SHIFT),
        ); // unselect all
        assert_eq!(app.panels[1].selection_info().0, 0);
        // Shift+End / Shift+Home select to the last / first entry
        app.on_key(shift(KeyCode::End)); // "c", "d"
        assert_eq!(app.panels[1].targets().len(), 2);
        app.on_key(shift(KeyCode::Home)); // cursor was on "d": all
        assert_eq!(app.panels[1].targets().len(), 4);
        with(
            &mut app,
            KeyCode::Char('A'),
            ctrl.union(KeyModifiers::SHIFT),
        );
        with(&mut app, KeyCode::Char('a'), ctrl);
        assert_eq!(app.panels[1].targets().len(), 4);
        let before = app.panels[1].path().to_path_buf();
        app.click_entry(1, 0, none, true); // double click on "..": enters the parent
        assert_ne!(app.panels[1].path(), before);
    }

    #[test]
    fn middle_click_undo_close_and_move_tabs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sub");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let mut app = App::new(Panel::open(&dir).unwrap(), Panel::open(&dir).unwrap());
        app.open_tab(0, 1); // middle click on "inner"
        assert_eq!(app.panels[0].path(), dir.join("inner"));
        assert_eq!((app.tabs[0].len(), app.tab[0]), (1, 1));
        let cs = KeyModifiers::CONTROL.union(KeyModifiers::SHIFT);
        with(&mut app, KeyCode::Char('O'), cs); // to the right side
        assert_eq!(
            (app.active, app.panels[1].path()),
            (1, dir.join("inner").as_path())
        );
        assert_eq!((app.tabs[0].len(), app.tabs[1].len()), (0, 1));
        with(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(app.panels[1].path(), dir);
        with(&mut app, KeyCode::Char('T'), cs); // undo close
        assert_eq!(app.panels[1].path(), dir.join("inner"));
        with(&mut app, KeyCode::Char('T'), cs); // nothing more to reopen
        assert_eq!(app.tabs[1].len(), 1);
    }
}
