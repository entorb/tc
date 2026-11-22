//! Popular directories (Ctrl+Z): visited directories ranked by frecency.

use super::{App, Mode, list_nav, list_rows};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::time::{SystemTime, UNIX_EPOCH};
use tc_core::popular;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl App {
    /// Count the directories the panels just showed (local user navigation only).
    pub(super) fn popular_tick(&mut self) {
        let mut changed = false;
        for p in &mut self.panels {
            if let Some(dir) = p.take_visited() {
                popular::record(&mut self.popular, &dir, now());
                changed = true;
            }
        }
        if changed {
            self.popular_save();
        }
    }

    fn popular_save(&mut self) {
        if let Some(file) = &self.popular_file
            && let Err(e) = popular::save(file, &self.popular)
        {
            self.mode = Mode::Message(e.to_string());
        }
    }

    /// Ctrl+Z: best ranked first; the order is fixed while the list is open.
    pub(super) fn popular_open(&mut self) {
        popular::rank(&mut self.popular, now());
        self.open_list(Mode::Popular(0));
    }

    pub(super) fn popular_key(&mut self, key: KeyEvent, mut sel: usize) {
        sel = sel.min(self.popular.len().saturating_sub(1));
        if list_nav(&mut self.list_filter, &self.popular, &mut sel, key) {
            self.mode = Mode::Popular(sel);
            return;
        }
        let shown = list_rows(&self.popular, &self.list_filter).contains(&sel);
        match key.code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
            KeyCode::Enter if shown => {
                let path = self.popular[sel].path.clone();
                if let Err(e) = self.panels[self.active].go(path) {
                    self.mode = Mode::Message(e.to_string());
                }
                return;
            }
            KeyCode::Delete | KeyCode::Backspace if shown => {
                self.popular.remove(sel);
                self.popular_save();
                sel = list_rows(&self.popular, &self.list_filter)
                    .first()
                    .copied()
                    .unwrap_or(0);
                if matches!(self.mode, Mode::Message(_)) {
                    return;
                }
            }
            _ => {}
        }
        self.mode = Mode::Popular(sel);
    }
}

#[cfg(test)]
mod tests {
    use super::super::Mode;
    use super::super::tests::{key, two_panels, type_keys, with};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use tc_core::popular;

    #[test]
    fn visits_are_ranked_filtered_jumped_to_and_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("alpha"), tmp.path().join("beta"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut app = two_panels(tmp.path(), tmp.path());
        let file = tmp.path().join("cfg").join("popular");
        app.popular_file = Some(file.clone());
        for d in [&a, &b, &b] {
            app.panels[0].go(d.clone()).unwrap();
            while app.panels[0].loading().is_some() {
                app.tick();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            app.tick();
        }
        assert_eq!(popular::load(&file).unwrap().len(), 2);
        with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.popular[0].path, b);
        key(&mut app, KeyCode::Char('/'));
        type_keys(&mut app, "ALPH");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.panels[0].path(), a);
        with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Delete);
        assert!(matches!(app.mode, Mode::Popular(_)));
        assert_eq!(popular::load(&file).unwrap().len(), 1);
    }
}
