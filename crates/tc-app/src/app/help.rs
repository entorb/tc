//! F1 help popup: filter, scroll, run the key of a line.

use super::{App, Mode, help_action};
use crate::fmt;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    /// F1 opens the help here: the footer screens plus lists and the conflict question.
    pub(super) fn takes_f1(&self) -> bool {
        self.footer_active()
            || (self.input.is_none()
                && !self.help
                && match &self.mode {
                    Mode::Hotlist(_) | Mode::Connections(_) | Mode::Drives(..) => true,
                    Mode::Xfer(x) => x.ask.is_some() && x.rename.is_none(),
                    _ => false,
                })
    }

    /// Help lines as shown (matching the filter, all when it is empty) with the key each runs
    /// (only the main help has keys). Built once per frame or key, not per line.
    pub fn help_entries(&self) -> Vec<(&'static str, Option<(KeyCode, KeyModifiers)>)> {
        let (_, text) = fmt::help(&self.mode);
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

    /// Click on help text line `line`: close the help and run the action the line describes.
    pub fn help_click(&mut self, line: usize) {
        if let Some((c, m)) = self.help_action_at(line) {
            self.help = false;
            self.on_key(KeyEvent::new(c, m));
        }
    }

    /// New filter text: selects the first key line and scrolls to the top.
    pub(super) fn help_set_filter(&mut self, filter: String) {
        self.help_filter = filter;
        self.help_scroll = 0;
        self.help_sel = self
            .help_entries()
            .iter()
            .position(|e| e.1.is_some())
            .unwrap_or(0);
    }

    /// Moves the selection to the next key line in `dir`, or scrolls when there are none.
    pub(super) fn help_move(&mut self, dir: isize) {
        let sel = self.help_sel as isize;
        let next = self
            .help_entries()
            .iter()
            .enumerate()
            .filter(|(i, e)| e.1.is_some() && (*i as isize - sel).signum() == dir)
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

    pub(super) fn help_key(&mut self, key: KeyEvent) {
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
