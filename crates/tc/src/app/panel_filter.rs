//! Panel filter popup (`/`): name, size, age and the file types of the directory.

use super::widgets::form_text;
use super::{App, FormRow, Mode};
use crate::{KeyCode, KeyEvent, KeyModifiers};
use std::time::SystemTime;
use tc_core::cmdline::LineEdit;
use tc_core::find::{parse_age, parse_size};
use tc_core::panel::{EntryFilter, ext_counts};

const F_NAME: &str = "Name contains";
const F_MIN: &str = "Min size (10k, 5M)";
const F_MAX: &str = "Max size";
const F_NEWER: &str = "Newer than (30m, 12h, 7d)";
const F_OLDER: &str = "Older than";

/// Filter popup being edited: text rows, then one checkbox per file type; `focus` runs over both.
#[derive(Clone)]
pub struct FilterForm {
    pub rows: Vec<FormRow>,
    /// Extension (`""` = none), file count, checked.
    pub exts: Vec<(String, usize, bool)>,
    pub focus: usize,
    pub error: Option<String>,
}

fn text_rows() -> Vec<FormRow> {
    let text = |label| FormRow::Text(label, LineEdit::default());
    vec![
        text(F_NAME),
        text(F_MIN),
        text(F_MAX),
        text(F_NEWER),
        text(F_OLDER),
    ]
}

/// Form values as a filter; `Err` names the field that does not parse.
fn filter_from(form: &FilterForm) -> Result<EntryFilter, String> {
    let rows = &form.rows;
    let field = |label: &str| form_text(rows, label).trim().to_owned();
    let size = |label: &str| {
        let t = field(label);
        (!t.is_empty())
            .then(|| parse_size(&t).map_err(|e| format!("{label}: {e}")))
            .transpose()
    };
    let age = |label: &str| {
        let t = field(label);
        (!t.is_empty())
            .then(|| {
                parse_age(&t)
                    .and_then(|ts| SystemTime::try_from(ts).map_err(|e| e.to_string()))
                    .map_err(|e| format!("{label}: {e}"))
            })
            .transpose()
    };
    let exts: Vec<String> = (form.exts.iter().filter(|x| x.2))
        .map(|x| x.0.clone())
        .collect();
    let mut label: Vec<String> = Vec::new();
    let name = field(F_NAME);
    if !name.is_empty() {
        label.push(name.clone());
    }
    label.extend(exts.iter().map(|x| format!(".{x}")));
    for (tag, l) in [
        (">=", F_MIN),
        ("<=", F_MAX),
        ("newer ", F_NEWER),
        ("older ", F_OLDER),
    ] {
        let t = field(l);
        if !t.is_empty() {
            label.push(format!("{tag}{t}"));
        }
    }
    Ok(EntryFilter {
        name: name.to_lowercase(),
        exts,
        min_size: size(F_MIN)?,
        max_size: size(F_MAX)?,
        newer: age(F_NEWER)?,
        older: age(F_OLDER)?,
        label: label.join(" "),
    })
}

impl App {
    /// `/`: the popup, prefilled with the panel's active filter; types from the unfiltered list.
    pub(super) fn filter_open(&mut self) {
        let side = self.active;
        let panel = &self.panels[side];
        // the form of the filter still shown (not of another tab or directory)
        let old = (self.filter_forms[side].take())
            .filter(|f| filter_from(f).is_ok_and(|x| x.label == panel.filter().label));
        let checked = |x: &str| panel.filter().exts.iter().any(|e| e == x);
        let exts = ext_counts(panel.all_entries()).into_iter();
        let exts = exts.map(|(x, n)| {
            let on = checked(&x);
            (x, n, on)
        });
        self.mode = Mode::Filter(Box::new(FilterForm {
            rows: old.map_or_else(text_rows, |f| f.rows),
            exts: exts.collect(),
            focus: 0,
            error: None,
        }));
    }

    /// Popup keys: Tab / Up / Down move, Space toggles a type, Enter applies, Esc cancels.
    pub(super) fn filter_key(&mut self, mut f: Box<FilterForm>, key: KeyEvent) {
        let n = f.rows.len() + f.exts.len();
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => match filter_from(&f) {
                Ok(filter) => {
                    self.panels[self.active].apply_filter(filter);
                    self.filter_forms[self.active] = Some(*f);
                    return;
                }
                Err(e) => f.error = Some(e),
            },
            KeyCode::Tab | KeyCode::Down => f.focus = (f.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up => f.focus = (f.focus + n - 1) % n,
            KeyCode::Char(' ') if f.focus >= f.rows.len() => {
                let x = &mut f.exts[f.focus - f.rows.len()];
                x.2 = !x.2;
            }
            code => {
                if let Some(row) = f.rows.get_mut(f.focus) {
                    row.key(code, key.modifiers.contains(KeyModifiers::CONTROL));
                }
            }
        }
        self.mode = Mode::Filter(f);
    }
}

#[cfg(test)]
mod tests {
    use super::super::Mode;
    use super::super::tests::{key, type_keys};
    use crate::KeyCode;
    use crate::app::App;
    use tc_core::panel::Panel;

    #[test]
    fn popup_filters_by_type_and_size_and_esc_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sub");
        std::fs::create_dir_all(dir.join("d")).unwrap();
        for (n, len) in [("a.rs", 5), ("b.rs", 1), ("c.txt", 3)] {
            std::fs::write(dir.join(n), "x".repeat(len)).unwrap();
        }
        let mut app = App::new(Panel::open(&dir).unwrap(), Panel::open(&dir).unwrap());
        let names = |app: &App| -> Vec<String> {
            let list = app.panels[0].entries().iter();
            list.map(|e| e.name.to_string_lossy().into_owned())
                .collect()
        };
        key(&mut app, KeyCode::Char('/'));
        let Mode::Filter(f) = &app.mode else { panic!() };
        assert_eq!(f.exts.len(), 2); // rs (2), txt (1)
        let first_ext = f.rows.len();
        for _ in 0..first_ext {
            key(&mut app, KeyCode::Tab);
        }
        key(&mut app, KeyCode::Char(' ')); // .rs
        key(&mut app, KeyCode::Enter);
        assert_eq!(names(&app), ["..", "a.rs", "b.rs"]);
        // reopened: prefilled; a bad size keeps the popup open
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab);
        type_keys(&mut app, "x");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(&app.mode, Mode::Filter(f) if f.error.is_some()));
        key(&mut app, KeyCode::Backspace);
        type_keys(&mut app, "2");
        key(&mut app, KeyCode::Enter);
        assert_eq!(names(&app), ["..", "a.rs"]);
        key(&mut app, KeyCode::Esc); // panel: clears the filter
        assert_eq!(app.panels[0].entries().len(), 5);
    }
}
