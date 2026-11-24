use super::chrome::{line_edit, list_button, text_box};
use super::help::show_help;
use super::{ACCENT, MUTED, TEXT, modal};
use eframe::egui::{
    self, Button, Color32, ComboBox, Context, Label, Response, RichText, ScrollArea, Sense,
    TextStyle, Ui, vec2,
};
use tc::app::{App, FindForm, FormRow, Mode, PropsForm, RenameForm, SettingsForm, SyncForm};
use tc::fmt::{self, Dialog, dialog, input_dialog, stats_progress};
use tc::fmt::{ProcInfo, procs_confirm, procs_info};
use tc::{KeyCode, KeyEvent, KeyModifiers};

/// What a mouse did in a form; applied through the app like the matching keys.
enum FormAct {
    /// Row clicked: focus it, toggle a checkbox.
    Row(usize),
    /// Choice row, option picked in its dropdown.
    Choose(usize, usize),
    /// OK / Cancel button: Enter / Esc.
    Key(KeyCode),
    /// List row clicked: move the selection by this many rows, Enter when double-clicked.
    Pick(isize, bool),
}
pub(super) fn overlays(ctx: &Context, app: &mut App) {
    let mut pressed = None;
    if let Some(d) = dialog(app) {
        pressed = show_dialog(ctx, "dialog", &d);
    }
    let act = match (&app.search_form, &app.mode) {
        (Some(form), _) => show_form(
            ctx,
            ("search", "Search"),
            (&form.rows, form.focus, form.error.as_deref()),
            Some("OK"),
        ),
        (None, Mode::FindForm(form)) => show_find_form(ctx, form),
        (None, Mode::MultiRename(form)) => show_multi_rename(ctx, form),
        (None, Mode::SyncForm(form)) => show_sync_form(ctx, form),
        (None, Mode::Settings(form)) => show_settings(ctx, form),
        (None, Mode::Props(form)) => show_props(ctx, form),
        (None, Mode::Procs(p)) => match (&p.draft, procs_info(p)) {
            (Some(d), _) => show_form(
                ctx,
                ("proc-filter", "Process filter"),
                (&d.rows, d.focus, d.error.as_deref()),
                Some("OK"),
            ),
            (None, Some(info)) => show_info(ctx, &info),
            (None, None) => None,
        },
        _ => None,
    };
    if let Mode::Sync(s) = &app.mode
        && let Some(text) = &s.confirm
    {
        let d = Dialog::Text {
            title: "Synchronize",
            text: text.clone(),
        };
        pressed = pressed.or(show_dialog(ctx, "sync-confirm", &d));
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = &st.confirm
    {
        let d = Dialog::Text {
            title: "Delete",
            text: text.clone(),
        };
        pressed = pressed.or(show_dialog(ctx, "stats-confirm", &d));
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = stats_progress(st)
    {
        let d = Dialog::Text {
            title: "Scanning",
            text,
        };
        pressed = pressed.or(show_dialog(ctx, "stats-progress", &d));
    }
    if let Mode::Procs(p) = &app.mode
        && let Some((pid, name)) = &p.confirm
    {
        let d = Dialog::Text {
            title: "Kill",
            text: procs_confirm(*pid, name),
        };
        pressed = pressed.or(show_dialog(ctx, "procs-confirm", &d));
    }
    if let Some(input) = &app.input {
        pressed = pressed.or(show_dialog(ctx, "input", &input_dialog(input)));
    }
    if app.help {
        show_help(ctx, app);
    }
    match act.or(pressed) {
        Some(FormAct::Row(i)) => app.form_click(i),
        Some(FormAct::Choose(i, k)) => app.form_choose(i, k),
        Some(FormAct::Key(c)) => app.on_key(KeyEvent::new(c, KeyModifiers::NONE)),
        Some(FormAct::Pick(by, double)) => {
            let key = if by < 0 { KeyCode::Up } else { KeyCode::Down };
            for _ in 0..by.unsigned_abs() {
                app.on_key(KeyEvent::new(key, KeyModifiers::NONE));
            }
            if double {
                app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
        }
        None => {}
    }
}

/// Dialog as egui widgets; returns the key of a clicked button or checkbox (the app owns the keys).
fn show_dialog(ctx: &Context, id: &str, d: &Dialog) -> Option<FormAct> {
    modal(id)
        .show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.set_max_width(720.0);
            match d {
                Dialog::Text { title, text } => {
                    ui.heading(*title);
                    ui.separator();
                    text_with_keys(ui, text).map(FormAct::Key)
                }
                Dialog::Edit {
                    title,
                    head,
                    line,
                    tail,
                } => {
                    ui.heading(*title);
                    ui.separator();
                    if !head.is_empty() {
                        ui.label(head.as_str());
                    }
                    line_edit(ui, line);
                    text_with_keys(ui, tail.trim_start()).map(FormAct::Key)
                }
                Dialog::List {
                    title,
                    rows,
                    sel,
                    empty,
                } => {
                    ui.heading(title);
                    ui.separator();
                    let act = list_rows(ui, rows, *sel);
                    if rows.is_empty() {
                        ui.label(*empty);
                    }
                    act
                }
            }
        })
        .inner
}

fn list_rows(ui: &mut Ui, rows: &[String], sel: usize) -> Option<FormAct> {
    let mut act = None;
    // scroll to the selection only when it moved: every frame never settles on fractional DPI (flicker, CPU)
    let id = ui.scope_id().with("list-sel");
    let follow = ui.data(|d| d.get_temp::<(usize, usize)>(id)) != Some((sel, rows.len()));
    ui.data_mut(|d| d.insert_temp(id, (sel, rows.len())));
    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
        for (k, p) in rows.iter().enumerate() {
            let r = list_button(ui, p.as_str(), k == sel, false);
            if k == sel && follow {
                r.scroll_to_me(None);
            }
            if r.clicked() || r.double_clicked() {
                act = Some(FormAct::Pick(k as isize - sel as isize, r.double_clicked()));
            }
        }
    });
    act
}

/// `[k] Label` as a key of the app; None when `k` is no key name.
fn hint_key(k: &str) -> Option<KeyCode> {
    match k {
        "Enter" => Some(KeyCode::Enter),
        "Esc" => Some(KeyCode::Esc),
        "Space" => Some(KeyCode::Char(' ')),
        _ => {
            let mut c = k.chars();
            c.next().filter(|_| c.next().is_none()).map(KeyCode::Char)
        }
    }
}

/// One line of `[key] Label` hints split on 3 spaces; None when any part is not a hint.
fn hint_line(line: &str) -> Option<Vec<(&str, &str, KeyCode)>> {
    line.split("   ")
        .map(|p| {
            let (k, label) = p.strip_prefix('[')?.split_once("] ")?;
            Some((k, label, hint_key(k)?))
        })
        .collect()
}

/// Text lines, then the trailing `[key] Label` hint lines as buttons (key shown as shortcut) and
/// `[x] / [ ]` lines as checkboxes. "any key" becomes an OK button. Returns the key clicked.
fn text_with_keys(ui: &mut Ui, text: &str) -> Option<KeyCode> {
    let lines: Vec<&str> = text.lines().collect();
    let mut body = lines.len();
    while body > 0 && (hint_line(lines[body - 1]).is_some() || lines[body - 1] == "any key") {
        body -= 1;
    }
    let mut pressed = None;
    for line in &lines[..body] {
        if let Some((on, label)) = line
            .strip_prefix("[x] ")
            .map(|l| (true, l))
            .or_else(|| line.strip_prefix("[ ] ").map(|l| (false, l)))
        {
            let mut on = on; // a copy: the app toggles the real one on Space
            let r = ui.checkbox(&mut on, label);
            unfocus(&r);
            if r.clicked() {
                pressed = Some(KeyCode::Char(' '));
            }
        } else {
            ui.label(*line);
        }
    }
    for line in &lines[body..] {
        let hints = hint_line(line).unwrap_or_else(|| vec![("Esc", "OK", KeyCode::Esc)]);
        ui.horizontal(|ui| {
            for (k, label, code) in hints {
                let b = Button::new(label).shortcut_text(k).sense(Sense::CLICK);
                if ui.add(b).clicked() {
                    pressed = Some(code);
                }
            }
        });
    }
    pressed
}

/// Find files form: fields and checkboxes, the focused one highlighted; keys go through the app.
fn show_find_form(ctx: &Context, form: &FindForm) -> Option<FormAct> {
    let (rows, err) = (&form.rows, form.error.as_deref());
    show_form(
        ctx,
        ("find-form", "Find files"),
        (rows, form.focus, err),
        Some("Search"),
    )
}

fn show_sync_form(ctx: &Context, form: &SyncForm) -> Option<FormAct> {
    let title = "Synchronize directories (left <-> right panel)";
    let (rows, err) = (&form.rows, form.error.as_deref());
    show_form(
        ctx,
        ("sync-form", title),
        (rows, form.focus, err),
        Some("Compare"),
    )
}

fn show_settings(ctx: &Context, form: &SettingsForm) -> Option<FormAct> {
    show_form(
        ctx,
        ("settings", "Settings"),
        (&form.rows, form.focus, None),
        None,
    )
}

/// Modal form: (id, title), (rows, focus, error), then OK / Cancel (none when `ok` is None).
fn show_form(
    ctx: &Context,
    (id, title): (&str, &str),
    (rows, focus, error): (&[FormRow], usize, Option<&str>),
    ok: Option<&str>,
) -> Option<FormAct> {
    modal(id)
        .show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(title);
            ui.separator();
            let act = form_rows(ui, rows, focus, error);
            act.or(ok.and_then(|ok| ok_cancel(ui, ok)))
        })
        .inner
}

/// Process info: label / value grid; a click outside closes it (as Esc).
fn show_info(ctx: &Context, info: &ProcInfo) -> Option<FormAct> {
    let r = modal("proc-info").show(ctx, |ui| {
        ui.set_min_width(480.0);
        ui.heading(info.title.trim());
        ui.separator();
        egui::Grid::new("proc-info-grid")
            .num_columns(2)
            .spacing(vec2(16.0, 6.0))
            .show(ui, |ui| {
                for (label, value) in &info.rows {
                    ui.label(RichText::new(*label).color(MUTED));
                    ui.add(Label::new(value.as_str()).wrap());
                    ui.end_row();
                }
            });
    });
    r.backdrop_response
        .clicked()
        .then_some(FormAct::Key(KeyCode::Esc))
}

/// OK / Cancel buttons; not focusable, so Tab, Space and Enter stay with the app.
fn ok_cancel(ui: &mut Ui, ok: &str) -> Option<FormAct> {
    ui.separator();
    ok_cancel_buttons(ui, ok)
}

fn ok_cancel_buttons(ui: &mut Ui, ok: &str) -> Option<FormAct> {
    ui.horizontal(|ui| {
        let button = |ui: &mut Ui, t: &str| ui.add(Button::new(t).sense(Sense::CLICK)).clicked();
        if button(ui, ok) {
            Some(FormAct::Key(KeyCode::Enter))
        } else if button(ui, "Cancel") {
            Some(FormAct::Key(KeyCode::Esc))
        } else {
            None
        }
    })
    .inner
}

/// Multi-rename over the whole window: form, then as many old -> new rows as fit; keys go through the app.
/// Properties: the fields, then the mode preview and file info, OK / Cancel.
fn show_props(ctx: &Context, form: &PropsForm) -> Option<FormAct> {
    let lines = form.lines().join("\n");
    modal("props")
        .show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Properties");
            ui.separator();
            let act = form_rows(ui, &form.rows, form.focus, form.error.as_deref());
            ui.separator();
            ui.label(RichText::new(lines).monospace());
            act.or(ok_cancel(ui, "Apply"))
        })
        .inner
}

fn show_multi_rename(ctx: &Context, form: &RenameForm) -> Option<FormAct> {
    let size = ctx.content_rect().shrink(24.0).size();
    modal("multi-rename")
        .show(ctx, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.heading("Multi-rename");
            ui.separator();
            let act = form_rows(ui, &form.rows, form.focus, form.error.as_deref());
            ui.separator();
            ui.label(format!("{} files:", form.preview.len()));
            let row_h = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;
            let keys_h = 7.0 * row_h; // key help lines and the buttons
            let fit = ((ui.available_height() - keys_h) / row_h).max(1.0) as usize;
            for (old, new) in form.preview.iter().skip(form.scroll).take(fit) {
                let (text, bad) = fmt::rename_row(old, new);
                let t = RichText::new(text);
                ui.label(if bad { t.color(Color32::LIGHT_RED) } else { t });
            }
            let buttons = ui
                .with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    let b = ok_cancel_bottom_up(ui);
                    ui.label(RichText::new(fmt::RENAME_FORM_KEYS).color(MUTED));
                    b
                })
                .inner;
            act.or(buttons)
        })
        .inner
}

/// [`ok_cancel`] in a bottom-up layout: buttons first, the separator above them.
fn ok_cancel_bottom_up(ui: &mut Ui) -> Option<FormAct> {
    let act = ok_cancel_buttons(ui, "Rename");
    ui.separator();
    act
}

/// Form as a label | widget grid, the focused row's label highlighted, and the error under it.
/// Returns what the mouse did (a focused text field keeps its own clicks).
fn form_rows(ui: &mut Ui, rows: &[FormRow], focus: usize, error: Option<&str>) -> Option<FormAct> {
    let mut act = None;
    egui::Grid::new("form")
        .num_columns(2)
        .spacing(vec2(12.0, 6.0))
        .show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                if let Some(a) = form_row(ui, i, row, i == focus) {
                    act = Some(a);
                }
                ui.end_row();
            }
        });
    if let Some(e) = error {
        ui.label(RichText::new(e).color(Color32::LIGHT_RED));
    }
    act
}

/// One grid row: a text field, checkbox or dropdown drawn by egui, its state owned by the app.
fn form_row(ui: &mut Ui, i: usize, row: &FormRow, focused: bool) -> Option<FormAct> {
    let color = if focused { ACCENT } else { TEXT };
    let label = |t: &str| RichText::new(t.to_owned()).color(color);
    let click = |ui: &mut Ui, t: RichText| ui.add(Label::new(t).sense(Sense::click())).clicked();
    let hit = match row {
        FormRow::Text(name, l) => {
            let hit = click(ui, label(name).strong());
            text_box(ui, l, focused).clicked() || hit
        }
        FormRow::Check(name, on) => {
            ui.label("");
            let mut on = *on; // a copy: the app toggles the real one on the click
            let r = ui.checkbox(&mut on, label(name));
            unfocus(&r);
            r.clicked()
        }
        FormRow::Choice(name, opts, k) => {
            let hit = click(ui, label(name).strong());
            let mut pick = *k;
            let r = ComboBox::from_id_salt(("form-choice", i)).show_index(
                ui,
                &mut pick,
                opts.len(),
                |j| opts[j],
            );
            unfocus(&r);
            if r.changed() || r.clicked() || hit {
                return Some(FormAct::Choose(i, pick));
            }
            false
        }
    };
    hit.then_some(FormAct::Row(i))
}

/// egui focuses clicked widgets and would then take Space / Enter too; the app owns the keys.
fn unfocus(r: &Response) {
    if r.has_focus() {
        r.surrender_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyCode, hint_line};

    #[test]
    fn hint_lines_split_into_keys() {
        let h = hint_line("[Enter] Open release page   [Space] Toggle   [Esc] Close").unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[1], ("Space", "Toggle", KeyCode::Char(' ')));
        assert_eq!(
            hint_line("[O] Overwrite all").unwrap()[0].2,
            KeyCode::Char('O')
        );
        assert!(hint_line("File exists").is_none());
        assert!(hint_line("[abc] no key").is_none());
    }
}
