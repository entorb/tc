//! Compare panels (Ctrl+Y): choose what to select, repeat the last choice with Enter.

use super::{App, Mode};
use ratatui::crossterm::event::KeyCode;
use tc_core::panel::CompareKind;

impl App {
    pub(super) fn compare_open(&mut self) {
        self.mode = Mode::Compare(self.compare_last);
    }

    pub(super) fn compare_key(&mut self, code: KeyCode, sel: usize) {
        let last = CompareKind::ALL.len() - 1;
        let mut sel = sel.min(last);
        match code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => return,
            KeyCode::Up => sel = sel.saturating_sub(1),
            KeyCode::Down => sel = (sel + 1).min(last),
            KeyCode::Enter => {
                self.compare_last = sel;
                let [a, b] = &mut self.panels;
                a.compare_with(b, CompareKind::ALL[sel]);
                return;
            }
            _ => {}
        }
        self.mode = Mode::Compare(sel);
    }
}
