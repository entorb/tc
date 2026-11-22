//! Daily update check of release builds (GitHub latest release) and the Settings form (F9 Tools).

use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, FormRow, Mode, form_checked, zoom_step};

/// Release tag CI builds into release binaries; `None` in dev builds, which never check.
pub const VERSION: Option<&str> = match option_env!("TC_VERSION") {
    Some(v) if !v.is_empty() => Some(v),
    _ => None,
};

const RELEASES: &str = "https://github.com/entorb/tc/releases/latest";
const CHECK_DAILY: &str = "Check for updates daily";
const FONT_SIZE: &str = "Font size (tc-gui)";
/// Font scales 50 %..300 % in 10 % steps, index = `zoom * 10 - 5` (see `zoom_step`).
const ZOOMS: [&str; 26] = [
    "50%", "60%", "70%", "80%", "90%", "100%", "110%", "120%", "130%", "140%", "150%", "160%",
    "170%", "180%", "190%", "200%", "210%", "220%", "230%", "240%", "250%", "260%", "270%", "280%",
    "290%", "300%",
];

/// Settings form (F9 Tools > Settings); Enter applies and saves.
#[derive(Clone)]
pub struct SettingsForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
}

fn zoom_index(zoom: f32) -> usize {
    ((zoom * 10.0).round() as usize).clamp(5, 30) - 5
}

/// `v1.2.3` as numbers; shortcut: a `-rc1` suffix is ignored, so rc and final of one number are equal.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let v = v.split_once('-').map_or(v, |(n, _)| n);
    let mut it = v.split('.').map(str::parse::<u64>);
    let n = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    it.next().is_none().then_some(n)
}

fn is_newer(running: &str, latest: &str) -> bool {
    matches!((parse_version(running), parse_version(latest)), (Some(r), Some(l)) if l > r)
}

/// Check enabled and not yet done on `today`.
fn due(enabled: bool, last_day: &str, today: &str) -> bool {
    enabled && last_day != today
}

fn today() -> String {
    jiff::ZonedDateTime::now().date().to_string()
}

impl App {
    /// First start of the day of a release build: ask GitHub in a worker; `tick` shows the result.
    pub fn start_update_check(&mut self) {
        let today = today();
        if VERSION.is_none() || !due(self.update_check, &self.update_day, &today) {
            return;
        }
        self.update_day = today;
        // offline or rate-limited: no dialog, next try tomorrow
        self.update_job = Some(thread::spawn(|| tc_remote::update::latest_tag().ok()));
    }

    /// A finished check shows the dialog once no other dialog is open.
    pub(super) fn update_tick(&mut self) {
        if !matches!(self.mode, Mode::Normal)
            || !self
                .update_job
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
        {
            return;
        }
        let tag = self.update_job.take().and_then(|j| j.join().ok().flatten());
        if let (Some(running), Some(tag)) = (VERSION, tag) {
            self.show_update(running, tag);
        }
    }

    fn show_update(&mut self, running: &str, tag: String) {
        if is_newer(running, &tag) {
            self.mode = Mode::Update {
                tag,
                check: self.update_check,
            };
        }
    }

    /// Space toggles the daily check, Enter opens the release page; closing keeps the checkbox.
    pub(super) fn update_key(&mut self, code: KeyCode, tag: String, check: bool) {
        match code {
            KeyCode::Char(' ') => {
                self.mode = Mode::Update { tag, check: !check };
                return;
            }
            KeyCode::Enter => {
                self.spawn_detached(PathBuf::from(RELEASES), tc_core::cmdline::open_command)
            }
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') => {}
            _ => {
                self.mode = Mode::Update { tag, check };
                return;
            }
        }
        self.set_update_check(check);
    }

    fn set_update_check(&mut self, on: bool) {
        self.update_check = on;
        self.save_or_message();
    }

    /// Apply and save now: the settings file is otherwise only written on quit.
    fn apply_settings(&mut self, rows: &[FormRow]) {
        self.update_check = form_checked(rows, CHECK_DAILY);
        if let Some(FormRow::Choice(_, _, i)) = rows.iter().find(|r| r.label() == FONT_SIZE) {
            self.zoom = zoom_step((*i + 5) as f32 / 10.0, 0);
        }
        self.save_or_message();
    }

    fn save_or_message(&mut self) {
        if let Err(e) = self.save_session() {
            self.mode = Mode::Message(format!("{e:#}"));
        }
    }

    pub(super) fn open_settings(&mut self) {
        self.mode = Mode::Settings(SettingsForm {
            rows: vec![
                FormRow::Check(CHECK_DAILY, self.update_check),
                FormRow::Choice(FONT_SIZE, &ZOOMS, zoom_index(self.zoom)),
            ],
            focus: 0,
        });
    }

    pub(super) fn settings_key(&mut self, key: KeyEvent, mut form: SettingsForm) {
        let last = form.rows.len() - 1;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => return,
            KeyCode::Enter => return self.apply_settings(&form.rows),
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => form.focus = form.focus.saturating_sub(1),
            code => {
                let ctrl = key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                form.rows[form.focus].key(code, ctrl);
            }
        }
        self.mode = Mode::Settings(form);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;
    use tc_core::panel::Panel;

    use super::{App, CHECK_DAILY, FormRow, Mode, due, form_checked, is_newer, parse_version};

    #[test]
    fn versions_compare() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert!(is_newer("v0.1.9", "v0.1.10"));
        assert!(is_newer("v0.9.0", "v1.0.0"));
        assert!(!is_newer("v1.0.0", "v1.0.0"));
        assert!(!is_newer("v1.0.1", "v1.0.0"));
        assert!(!is_newer("v1.0.0", "garbage"));
    }

    #[test]
    fn due_once_a_day_when_enabled() {
        assert!(due(true, "", "2026-11-22"));
        assert!(due(true, "2026-11-21", "2026-11-22"));
        assert!(!due(true, "2026-11-22", "2026-11-22"));
        assert!(!due(false, "", "2026-11-22"));
    }

    fn app() -> App {
        let tmp = std::env::temp_dir();
        App::new(Panel::open(&tmp).unwrap(), Panel::open(&tmp).unwrap())
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn dialog_only_for_newer_and_space_turns_check_off() {
        let mut a = app();
        a.show_update("v1.0.0", "v1.0.0".into());
        assert!(matches!(a.mode, Mode::Normal));
        a.show_update("v1.0.0", "v1.0.1".into());
        assert!(matches!(a.mode, Mode::Update { check: true, .. }));
        key(&mut a, KeyCode::Char(' '));
        assert!(matches!(a.mode, Mode::Update { check: false, .. }));
        key(&mut a, KeyCode::Esc);
        assert!(matches!(a.mode, Mode::Normal));
        assert!(!a.update_check);
    }

    #[test]
    fn click_toggles_checkbox_cli_and_gui() {
        let mut a = app();
        a.open_settings();
        // GUI reports the row
        a.form_click(0);
        assert!(matches!(&a.mode, Mode::Settings(f) if !form_checked(&f.rows, CHECK_DAILY)));
        a.form_click(5); // past the rows: ignored
        // CLI: click inside the drawn rows
        (a.form_area, a.form_scroll) = (Rect::new(10, 5, 40, 3), 0);
        let click = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row,
            modifiers: KeyModifiers::NONE,
        };
        a.on_mouse(click(5));
        assert!(matches!(&a.mode, Mode::Settings(f) if form_checked(&f.rows, CHECK_DAILY)));
        a.on_mouse(click(9)); // outside
        // GUI dropdown picks an option, forwards and backwards
        let choice = |a: &App| match &a.mode {
            Mode::Settings(f) => match f.rows[1] {
                FormRow::Choice(_, _, k) => (k, f.focus),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        let start = choice(&a).0;
        a.form_choose(1, start + 2);
        assert_eq!(choice(&a), (start + 2, 1));
        a.form_choose(1, 0);
        assert_eq!(choice(&a).0, 0);
        key(&mut a, KeyCode::Enter);
        assert!(a.update_check);
    }

    #[test]
    fn settings_enter_applies_esc_discards() {
        let mut a = app();
        a.open_settings();
        key(&mut a, KeyCode::Char(' '));
        key(&mut a, KeyCode::Esc);
        assert!(a.update_check);
        a.open_settings();
        key(&mut a, KeyCode::Char(' '));
        key(&mut a, KeyCode::Enter);
        assert!(matches!(a.mode, Mode::Normal));
        assert!(!a.update_check);
    }
}
