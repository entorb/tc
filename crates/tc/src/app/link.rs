//! Symbolic and hard links (F9 → File): the cursor entry linked into the other panel's directory.

use super::{App, Mode, edit_key};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::path::{Path, PathBuf};
use tc_core::cmdline::LineEdit;
use tc_core::ops;

/// Two prompts in a row: `target` is `None` while the target is asked, then the link path.
pub struct LinkAsk {
    hard: bool,
    target: Option<String>,
    pub line: LineEdit,
    /// Link path suggested for the second prompt.
    link: String,
}

impl LinkAsk {
    pub fn title(&self) -> &'static str {
        match (self.hard, self.target.is_some()) {
            (false, false) => "Symlink target (what the link points to)",
            (false, true) => "Symlink path (new link to create)",
            (true, false) => "Hard link target (existing file)",
            (true, true) => "Hard link path (new link to create)",
        }
    }
}

impl App {
    /// Local panels only: the active cursor entry, and the other panel's directory.
    pub(super) fn link_open(&mut self, hard: bool) {
        let (a, o) = (&self.panels[self.active], &self.panels[1 - self.active]);
        let local = |p: &tc_core::panel::Panel| {
            p.remote().is_none() && p.archive().is_none() && !p.is_virtual()
        };
        if !local(a) || !local(o) {
            self.mode = Mode::Message("Links: local directories only.".into());
            return;
        }
        let Some(e) = a.current().filter(|e| e.name != "..") else {
            return;
        };
        let target = a.path().join(&e.name);
        let link = o.path().join(&e.name);
        self.mode = Mode::Link(LinkAsk {
            hard,
            target: None,
            line: LineEdit::from(target.to_string_lossy().into_owned()),
            link: link.to_string_lossy().into_owned(),
        });
    }

    pub(super) fn link_key(&mut self, key: KeyEvent, mut ask: LinkAsk) {
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => match ask.target.take() {
                None => {
                    ask.target = Some(std::mem::take(&mut ask.line.text));
                    ask.line = LineEdit::from(ask.link.clone());
                    self.mode = Mode::Link(ask);
                }
                Some(target) => self.link_make(ask.hard, &target, &ask.line.text),
            },
            _ => {
                edit_key(&mut ask.line, key);
                self.mode = Mode::Link(ask);
            }
        }
    }

    fn link_make(&mut self, hard: bool, target: &str, link: &str) {
        let (target, link) = (Path::new(target), PathBuf::from(link));
        // a bare name lands in the active directory
        let link = self.panels[self.active].path().join(link);
        let result = if hard {
            ops::hard_link(target, &link)
        } else {
            ops::symlink(target, &link)
        };
        let ok = result.is_ok();
        self.finish(result.err().map(|e| e.to_string()));
        if ok
            && link.parent() == Some(self.panels[self.active].path())
            && let Some(name) = link.file_name()
        {
            self.panels[self.active].focus(name);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;
    use tc_core::panel::Panel;

    #[test]
    fn symlink_into_other_panel_via_two_prompts() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("f"), "x").unwrap();
        let mut app = App::new(
            Panel::open(a.path()).unwrap(),
            Panel::open(b.path()).unwrap(),
        );
        assert!(app.panels[0].focus("f".as_ref()));
        app.on_key(KeyEvent::new(KeyCode::Null, KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Link(_)));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let link = b.path().join("f");
        assert_eq!(std::fs::read_link(&link).unwrap(), a.path().join("f"));
        // again: refused, link intact
        app.on_key(KeyEvent::new(KeyCode::Null, KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Message(_)));
        assert_eq!(std::fs::read_link(&link).unwrap(), a.path().join("f"));
    }
}
