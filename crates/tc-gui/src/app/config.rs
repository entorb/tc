//! Config directory: loading the saved lists, histories and session (tabs), saving them,
//! and deleting all of tc's files there (F9 Tools menu).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::KeyCode;
use anyhow::Context;
use tc_core::panel::Panel;
use tc_core::{cmdline, hotlist, popular, settings};

use super::{App, FindForm, Mode, rename_rows, rows_from_line};

const SIDES: [&str; 2] = ["left", "right"];

impl App {
    /// Load hotlist, connections, histories and the last session from the config directory.
    pub fn load_config(&mut self) -> anyhow::Result<()> {
        let Some(dir) = hotlist::default_path().and_then(|p| p.parent().map(Path::to_owned)) else {
            return Ok(());
        };
        let lines = |name: &str| {
            cmdline::load_history(&dir.join(name)).with_context(|| format!("cannot read {name}"))
        };
        self.history = lines("history")?;
        self.finds = lines("find-forms")?;
        self.syncs = lines("sync-forms")?;
        self.find_history = lines("find-history")?;
        self.rename_history = lines("rename-history")?;
        let paths = |name: &str| {
            hotlist::load(&dir.join(name)).with_context(|| format!("cannot read {name}"))
        };
        self.hotlist =
            hotlist::load_bookmarks(&dir.join("hotlist")).context("cannot read hotlist")?;
        self.popular = popular::load(&dir.join("popular")).context("cannot read popular")?;
        self.connections = paths("connections")?;
        let kv = settings::load(&dir.join("settings")).context("cannot read settings")?;
        self.restore_session(&kv);
        self.hotlist_file = Some(dir.join("hotlist"));
        self.popular_file = Some(dir.join("popular"));
        self.connections_file = Some(dir.join("connections"));
        self.history_file = Some(dir.join("history"));
        self.config_dir = Some(dir);
        Ok(())
    }

    /// Add `line` to the history list `name` and write its file; the error text if that fails.
    pub(super) fn remember(&mut self, name: &str, line: &str) -> String {
        let hist = match name {
            "find-forms" => &mut self.finds,
            "find-history" => &mut self.find_history,
            "sync-forms" => &mut self.syncs,
            _ => &mut self.rename_history,
        };
        cmdline::push_history(hist, line);
        let Some(dir) = &self.config_dir else {
            return String::new();
        };
        match cmdline::save_history(&dir.join(name), hist) {
            Ok(()) => String::new(),
            Err(e) => format!("Cannot save {name}: {e}"),
        }
    }

    /// Find form `i` of the history (past the end: an empty form), positioned there.
    pub(super) fn find_hist(&self, i: usize) -> FindForm {
        let form = self
            .finds
            .get(i)
            .map_or_else(FindForm::new, |l| FindForm::from_line(l));
        FindForm { hist: i, ..form }
    }

    /// Multi-rename rows of history entry `i` (past the end: the defaults).
    pub(super) fn rename_hist(&self, i: usize) -> Vec<super::FormRow> {
        let mut rows = rename_rows();
        if let Some(line) = self.rename_history.get(i) {
            rows_from_line(&mut rows, &old_rename_line(line));
        }
        rows
    }

    /// Local tabs of both sides in tab order, the shown one, and the active side.
    fn session(&self) -> Vec<(String, OsString)> {
        let mut kv = vec![
            ("active".to_owned(), self.active.to_string().into()),
            ("zoom".to_owned(), format!("{:.1}", self.zoom).into()),
            (
                "update_check".to_owned(),
                u8::from(self.update_check).to_string().into(),
            ),
            ("update_day".to_owned(), self.update_day.clone().into()),
            ("editor".to_owned(), self.editor.clone().into_os_string()),
        ];
        for (side, name) in SIDES.iter().enumerate() {
            // ring layout of `tab_key`: shown panel is tab `tab[side]`, `tabs[side]` follow it
            let mut ring: Vec<&Panel> = std::iter::once(&self.panels[side])
                .chain(&self.tabs[side])
                .collect();
            let len = ring.len();
            ring.rotate_right(self.tab[side] % len);
            let (mut n, mut shown) = (0, 0);
            for (i, p) in ring
                .iter()
                .enumerate()
                .filter(|(_, p)| p.remote().is_none())
            {
                if i == self.tab[side] {
                    shown = n;
                }
                kv.push(((*name).to_owned(), p.path().into()));
                n += 1;
            }
            kv.push((format!("{name}.tab"), shown.to_string().into()));
        }
        kv
    }

    /// Write the session (tabs per side) to the settings file; called on quit.
    pub fn save_session(&self) -> anyhow::Result<()> {
        if let Some(dir) = &self.config_dir {
            settings::save(&dir.join("settings"), &self.session())
                .context("cannot save settings")?;
        }
        Ok(())
    }

    /// Reopen saved tabs; directories gone since are skipped, a side without any keeps its panel.
    fn restore_session(&mut self, kv: &[(String, OsString)]) {
        let num = |key: &str| {
            kv.iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.to_str()?.parse::<usize>().ok())
        };
        if let Some(z) = kv.iter().find(|(k, _)| k == "zoom")
            && let Some(z) = z.1.to_str().and_then(|v| v.parse::<f32>().ok())
            && z.is_finite()
        {
            self.zoom = super::zoom_step(z, 0);
        }
        for (side, name) in SIDES.iter().enumerate() {
            let mut list: VecDeque<Panel> = (kv.iter().filter(|(k, _)| k == name))
                .map(|(_, v)| Path::new(v))
                .filter(|p| p.is_dir())
                .filter_map(|p| Panel::open(p).ok())
                .collect();
            let tab = num(&format!("{name}.tab"))
                .unwrap_or(0)
                .min(list.len().saturating_sub(1));
            list.rotate_left(tab.min(list.len()));
            let Some(shown) = list.pop_front() else {
                continue;
            };
            self.panels[side] = shown;
            self.tabs[side] = list;
            self.tab[side] = tab;
        }
        if let Some(a) = num("active").filter(|&a| a < 2) {
            self.active = a;
        }
        self.update_check = num("update_check") != Some(0);
        if let Some((_, day)) = kv.iter().find(|(k, _)| k == "update_day") {
            self.update_day = day.to_string_lossy().into_owned();
        }
        if let Some((_, ed)) = kv.iter().find(|(k, _)| k == "editor") {
            self.editor = PathBuf::from(ed);
        }
    }

    /// F9 Tools menu: ask before deleting tc's files in the config directory.
    pub(super) fn ask_reset_config(&mut self) {
        let dir = (self.config_dir.clone())
            .or_else(|| hotlist::default_path()?.parent().map(Path::to_owned));
        self.mode = match dir {
            Some(dir) => Mode::ResetConfig(dir),
            None => Mode::Message("No config directory (HOME / APPDATA not set).".into()),
        };
    }

    /// `y` deletes tc's own files (others stay) and stops saving until restart.
    pub(super) fn reset_config_key(&mut self, code: KeyCode, dir: PathBuf) {
        match code {
            KeyCode::Char('y' | 'Y') => {}
            KeyCode::Char('n' | 'N') | KeyCode::Esc => return,
            _ => return self.mode = Mode::ResetConfig(dir),
        }
        self.mode = Mode::Message(match settings::remove_all(&dir) {
            Ok(n) => {
                (self.hotlist_file, self.connections_file) = (None, None);
                self.popular_file = None;
                (self.history_file, self.config_dir) = (None, None);
                format!(
                    "Deleted {n} file(s) in {}. Nothing is saved until tc restarts.",
                    dir.display()
                )
            }
            Err(e) => format!("Cannot delete config files: {e}"),
        });
    }
}

/// A rename-history line from before the Regex label: its regex flag under the new label, and a
/// case-sensitive regex as `(?-i)` (regexes have no case-sensitive box now).
fn old_rename_line(line: &str) -> String {
    let parts: Vec<&str> = line.split('\t').collect();
    if !parts.contains(&"Find is a regex=1") {
        return line.to_owned();
    }
    let sensitive = parts.contains(&"Find case-sensitive=1");
    let fixed: Vec<String> = (parts.iter())
        .map(|&p| match p.split_once('=') {
            Some(("Find is a regex", v)) => format!("Regex={v}"),
            Some(("Find", v)) if sensitive => format!("Find=(?-i){v}"),
            _ => p.to_owned(),
        })
        .collect();
    fixed.join("\t")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{KeyCode, KeyEvent, KeyModifiers};
    use tc_core::panel::Panel;
    use tc_core::settings;

    use super::super::tools::find::F_NAME;
    use super::super::{
        App, FormRow, Mode, form_checked, rename_rows, rows_from_line, rows_to_line,
    };

    #[test]
    fn old_rename_history_keeps_its_regex() {
        let old = "Find=a+\tFind is a regex=1\tFind case-sensitive=1";
        assert_eq!(
            super::old_rename_line(old),
            "Find=(?-i)a+\tRegex=1\tFind case-sensitive=1"
        );
        assert_eq!(super::old_rename_line("Find=a\tRegex=1"), "Find=a\tRegex=1");
    }

    fn app(dir: &std::path::Path) -> App {
        let mut app = App::new(Panel::open(dir).unwrap(), Panel::open(dir).unwrap());
        app.config_dir = Some(dir.join("cfg"));
        app
    }

    #[test]
    fn session_roundtrip_skips_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, gone) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("gone"),
        );
        for d in [&a, &b, &gone] {
            std::fs::create_dir(d).unwrap();
        }
        let mut src = app(tmp.path());
        src.tabs[1].push_back(Panel::open(&a).unwrap());
        src.tabs[1].push_back(Panel::open(&gone).unwrap());
        src.tabs[1].push_back(Panel::open(&b).unwrap());
        src.tab[1] = 1; // shown = tmp is tab 1, ring: tmp, a, gone, b -> tabs b, tmp, a, gone
        src.active = 1;
        src.zoom = 1.3;
        (src.update_check, src.update_day) = (false, "2026-11-22".into());
        src.editor = "C:\\Program Files\\ed.exe".into();
        let kv = src.session();
        let order: Vec<PathBuf> = (kv.iter().filter(|(k, _)| k == "right"))
            .map(|(_, v)| v.into())
            .collect();
        assert_eq!(
            order,
            [b.clone(), tmp.path().to_owned(), a.clone(), gone.clone()]
        );
        std::fs::remove_dir(&gone).unwrap();
        let mut dst = app(&a);
        dst.restore_session(&kv);
        assert_eq!(
            (dst.active, dst.tab[1], dst.panels[1].path()),
            (1, 1, tmp.path())
        );
        assert!((dst.zoom - 1.3).abs() < 1e-6);
        assert!(!dst.update_check && dst.update_day == "2026-11-22");
        assert_eq!(dst.editor, PathBuf::from("C:\\Program Files\\ed.exe"));
        assert_eq!(
            dst.tabs[1].iter().map(Panel::path).collect::<Vec<_>>(),
            [a.as_path(), b.as_path()]
        );
        assert_eq!(dst.panels[0].path(), tmp.path());
    }

    #[cfg(unix)]
    #[test]
    fn editor_path_persists_without_lossy_conversion() {
        use std::os::unix::ffi::OsStringExt;

        let tmp = tempfile::tempdir().unwrap();
        let mut src = app(tmp.path());
        let editor = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/editor-\xff".to_vec()));
        src.editor = editor.clone();
        src.save_session().unwrap();

        let kv = settings::load(&tmp.path().join("cfg/settings")).unwrap();
        let mut dst = app(tmp.path());
        dst.restore_session(&kv);
        assert_eq!(dst.editor, editor);
    }

    #[test]
    fn histories_persist_and_rename_rows_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = app(tmp.path());
        let mut rows = rename_rows();
        rows_from_line(
            &mut rows,
            "Name mask=x[N]\tCase=2\tFind is a regex=1\tbogus=1",
        );
        let line = rows_to_line(&rows, "");
        assert_eq!(a.remember("rename-history", &line), "");
        assert_eq!(a.remember("find-history", "fo+"), "");
        let saved = tc_core::cmdline::load_history(&tmp.path().join("cfg/rename-history")).unwrap();
        assert_eq!(saved, std::slice::from_ref(&line));
        assert_eq!(rows_to_line(&a.rename_hist(0), ""), line);
        assert_eq!(
            rows_to_line(&a.rename_hist(1), ""),
            rows_to_line(&rename_rows(), "")
        );
        assert_eq!(a.find_history, ["fo+"]);
    }

    #[test]
    fn reset_deletes_only_tc_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = app(tmp.path());
        let dir = tmp.path().join("cfg");
        a.remember("find-history", "x");
        settings::save(&dir.join("settings"), &[]).unwrap();
        std::fs::write(dir.join("mine.txt"), "keep").unwrap();
        a.ask_reset_config();
        a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)); // not an answer: still asking
        assert!(matches!(a.mode, Mode::ResetConfig(_)));
        a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(matches!(&a.mode, Mode::Message(m) if m.starts_with("Deleted 2")));
        assert!(dir.join("mine.txt").exists() && !dir.join("settings").exists());
        assert!(a.config_dir.is_none());
    }

    #[test]
    fn prompt_and_find_form_recall_history() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f.txt");
        std::fs::write(&file, "hello\n").unwrap();
        let mut a = app(tmp.path());
        a.open_viewer(file);
        for k in [
            KeyCode::F(7),
            KeyCode::Char('l'),
            KeyCode::Enter,
            KeyCode::F(7),
        ] {
            a.on_key(KeyEvent::new(k, KeyModifiers::NONE));
        }
        a.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        let form = a.search_form.as_ref().unwrap();
        assert!(matches!(&form.rows[0], FormRow::Text("Find", l) if l.text == "l"));
        a.search_form = None;
        a.mode = Mode::Normal;
        a.remember("find-forms", "\tName (masks)=*.rs");
        a.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        let Mode::FindForm(f) = &a.mode else { panic!() };
        assert_eq!(f.text(F_NAME), "*.rs"); // last search after a restart
        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
        let Mode::FindForm(f) = &a.mode else { panic!() };
        assert_eq!(f.text(F_NAME), "");
        a.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        let Mode::FindForm(f) = &a.mode else { panic!() };
        assert_eq!(f.text(F_NAME), "*.rs");
    }

    #[test]
    fn sync_options_persist_and_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = app(tmp.path());
        let line = format!("{}=1", super::super::tools::sync::S_NODATE);
        a.remember("sync-forms", &line);
        let nodate = |a: &App| match &a.mode {
            Mode::Sync(s) => form_checked(&s.form.rows, super::super::tools::sync::S_NODATE),
            _ => panic!("no sync screen"),
        };
        a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(nodate(&a));
        a.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!nodate(&a));
        let saved = tc_core::cmdline::load_history(&tmp.path().join("cfg/sync-forms")).unwrap();
        let off = format!("{}=0", super::super::tools::sync::S_NODATE);
        assert!(saved.last().is_some_and(|l| l.starts_with(&off)));
    }
}
