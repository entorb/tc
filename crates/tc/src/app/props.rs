//! File properties (Alt+Enter, F9 File): info on the marked entries or the cursor entry;
//! Unix: mode, owner and group, elsewhere the read-only flag; applied in a worker.

use super::form_text;
#[cfg(unix)]
use super::widgets::step_key;
use super::{App, Done, FormRow, Mode, form_checked};
use crate::{KeyCode, KeyEvent};
use std::path::PathBuf;
#[cfg(unix)]
use tc_core::cmdline::LineEdit;
use tc_core::edit::human;
use tc_core::props::{self, Change};

#[cfg(unix)]
const MODE: &str = "Mode (octal)";
#[cfg(unix)]
const OWNER: &str = "Owner";
#[cfg(unix)]
const GROUP: &str = "Group";
#[cfg(not(unix))]
const READONLY: &str = "Read-only";
const RECURSIVE: &str = "Recursive";

/// Properties form. One file: fields prefilled, only changed ones applied; several entries or a
/// directory: fields start empty, every filled one is applied to all.
pub struct PropsForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub error: Option<String>,
    /// Name or count, size, type, date, link target of the first target.
    info: Vec<String>,
    /// Rows as opened, to tell changed fields.
    initial: Vec<FormRow>,
    targets: Vec<PathBuf>,
}

/// `label  value`, aligned with the form fields.
fn row(label: &str, value: &str) -> String {
    format!("{label:<18} {value}")
}

impl PropsForm {
    /// Lines under the fields: the live mode preview (Unix), then the info.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.info.len() + 1);
        #[cfg(unix)]
        {
            let m = form_text(&self.rows, MODE);
            out.push(match props::parse_mode(m) {
                Some(v) => row("Permissions", &props::mode_string(v)),
                None if m.trim().is_empty() => row("Permissions", "(unchanged)"),
                None => row("Permissions", "? octal, 1 to 4 digits 0-7"),
            });
        }
        out.extend(self.info.iter().cloned());
        out
    }

    /// Text of field `label` when changed and not emptied.
    #[cfg(unix)]
    fn edited(&self, label: &str) -> Option<&str> {
        let t = form_text(&self.rows, label).trim();
        (!t.is_empty() && t != form_text(&self.initial, label).trim()).then_some(t)
    }

    /// What the changed fields ask for, or why they are invalid.
    fn change(&self) -> Result<Change, String> {
        let mut c = Change {
            recursive: form_checked(&self.rows, RECURSIVE),
            ..Change::default()
        };
        #[cfg(unix)]
        {
            if let Some(m) = self.edited(MODE) {
                let mode = props::parse_mode(m).ok_or("Mode: 1 to 4 octal digits, like 644")?;
                c.mode = Some(mode);
            }
            if let Some(u) = self.edited(OWNER) {
                c.uid = Some(props::resolve_user(u).map_err(|e| e.to_string())?);
            }
            if let Some(g) = self.edited(GROUP) {
                c.gid = Some(props::resolve_group(g).map_err(|e| e.to_string())?);
            }
        }
        #[cfg(not(unix))]
        {
            let ro = form_checked(&self.rows, READONLY);
            if ro != form_checked(&self.initial, READONLY) {
                c.readonly = Some(ro);
            }
        }
        Ok(c)
    }
}

/// Fields from `info`: prefilled, else empty with the values in a `Current` info line.
#[cfg(unix)]
fn prop_rows(info: &props::Info, prefill: bool, lines: &mut Vec<String>) -> Vec<FormRow> {
    let (mode, user, group) = (
        format!("{:03o}", info.mode),
        props::user_name(info.uid),
        props::group_name(info.gid),
    );
    if prefill {
        return vec![
            FormRow::Text(MODE, LineEdit::from(mode)),
            FormRow::Text(OWNER, LineEdit::from(user)),
            FormRow::Text(GROUP, LineEdit::from(group)),
        ];
    }
    let current = format!("{mode} {}  {user}:{group}", props::mode_string(info.mode));
    lines.push(row("Current (first)", &current));
    [MODE, OWNER, GROUP]
        .map(|l| FormRow::Text(l, LineEdit::default()))
        .into()
}

#[cfg(not(unix))]
fn prop_rows(info: &props::Info, _prefill: bool, _lines: &mut Vec<String>) -> Vec<FormRow> {
    vec![FormRow::Check(READONLY, info.readonly)]
}

impl App {
    /// Local panels only (a found-files list too); remote and archive panels get a message.
    pub(super) fn props_open(&mut self) {
        let panel = &self.panels[self.active];
        if panel.remote().is_some() || panel.archive().is_some() {
            self.mode = Mode::Message("Properties: local files only.".into());
            return;
        }
        let entries = panel.target_entries();
        let Some(first) = entries.first() else {
            return;
        };
        let targets = panel.targets();
        let info = match props::info(&targets[0]) {
            Ok(i) => i,
            Err(e) => {
                self.mode = Mode::Message(e.to_string());
                return;
            }
        };
        let name = first.name.to_string_lossy();
        let mut lines = if entries.len() == 1 {
            vec![row("Name", &name), row("Size", &human(info.size))]
        } else {
            let files = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            vec![
                row("Name", &format!("{} entries, first {name}", entries.len())),
                row("Size", &format!("{} (files only)", human(files))),
            ]
        };
        lines.push(row("Type", info.kind));
        if let Some(z) = info
            .modified
            .and_then(|t| jiff::ZonedDateTime::try_from(t).ok())
        {
            lines.push(row(
                "Modified",
                &z.strftime("%Y-%m-%d %H:%M:%S").to_string(),
            ));
        }
        if let Some(l) = &info.link {
            lines.push(row("Link to", &l.to_string_lossy()));
        }
        let any_dir = entries.iter().any(|e| e.is_dir);
        let mut rows = prop_rows(&info, entries.len() == 1 && !any_dir, &mut lines);
        if any_dir {
            rows.push(FormRow::Check(RECURSIVE, false));
        }
        self.mode = Mode::Props(Box::new(PropsForm {
            initial: rows.clone(),
            rows,
            focus: 0,
            error: None,
            info: lines,
            targets,
        }));
    }

    pub(super) fn props_key(&mut self, key: KeyEvent, mut form: Box<PropsForm>) {
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => return,
            KeyCode::Enter => match form.change() {
                Ok(c) if c.is_empty() => return,
                Ok(c) => return self.props_spawn(form.targets, c),
                Err(e) => form.error = Some(e),
            },
            KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up => {
                form.focus = step_key(key.code, form.focus, form.rows.len());
            }
            _ => {
                form.rows[form.focus].key(key);
                form.error = None;
            }
        }
        self.mode = Mode::Props(form);
    }

    /// Progress counts entries changed (total unknown when recursive).
    fn props_spawn(&mut self, targets: Vec<PathBuf>, change: Change) {
        let path = targets.first().cloned().unwrap_or_default();
        let total = if change.recursive {
            u64::MAX
        } else {
            targets.len() as u64
        };
        self.spawn("Properties", path, total, false, move |done, cancel| {
            Done::Props(props::apply(&targets, &change, done, cancel))
        });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::KeyModifiers;
    use std::os::unix::fs::PermissionsExt;
    use tc_core::panel::Panel;

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn mode(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn alt_enter_chmods_marked_files_with_empty_fields() {
        let tmp = tempfile::tempdir().unwrap();
        for (n, m) in [("a", 0o640), ("b", 0o600), ("c", 0o640)] {
            std::fs::write(tmp.path().join(n), "").unwrap();
            let perm = std::fs::Permissions::from_mode(m);
            std::fs::set_permissions(tmp.path().join(n), perm).unwrap();
        }
        let mut app = App::new(
            Panel::open(tmp.path()).unwrap(),
            Panel::open(tmp.path()).unwrap(),
        );
        assert!(app.panels[0].focus("a".as_ref()));
        key(&mut app, KeyCode::Insert);
        key(&mut app, KeyCode::Insert);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        let Mode::Props(f) = &app.mode else {
            panic!("no properties form")
        };
        // several entries: fields empty, first entry's values in the info
        assert_eq!(form_text(&f.rows, MODE), "");
        assert!(f.lines()[0].ends_with("(unchanged)"));
        assert!(f.lines()[1].contains("2 entries, first a"));
        assert!(f.lines().iter().any(|l| l.contains("640 rw-r-----")));
        // no recursive checkbox: no directory marked
        assert_eq!(f.rows.len(), 3);
        // invalid mode keeps the form with an error
        key(&mut app, KeyCode::Char('9'));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(&app.mode, Mode::Props(f) if f.error.is_some()));
        key(&mut app, KeyCode::Backspace);
        for c in "644".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        super::super::tests::settle(&mut app);
        assert!(matches!(app.mode, Mode::Normal), "{:?}", app.note);
        let modes = ["a", "b", "c"].map(|n| mode(&tmp.path().join(n)));
        assert_eq!(modes, [0o644, 0o644, 0o640]);
    }

    #[test]
    fn single_file_prefilled_applies_only_changed_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        std::fs::write(&f, "").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut app = App::new(
            Panel::open(tmp.path()).unwrap(),
            Panel::open(tmp.path()).unwrap(),
        );
        assert!(app.panels[0].focus("f".as_ref()));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        let Mode::Props(form) = &app.mode else {
            panic!("no properties form")
        };
        assert_eq!(form_text(&form.rows, MODE), "640");
        assert!(form.lines()[0].ends_with("rw-r-----"));
        assert!(form.change().unwrap().is_empty());
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Char('4'));
        key(&mut app, KeyCode::Enter);
        super::super::tests::settle(&mut app);
        assert_eq!(mode(&f), 0o644);
    }
}
