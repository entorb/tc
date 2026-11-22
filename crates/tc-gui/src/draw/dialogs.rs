use super::chrome::{line_edit, text_box};
use super::help::show_help;
use super::{ACCENT, MUTED, SELECT_BG, TEXT, modal};
use eframe::egui::{
    self, Button, Color32, ComboBox, Context, Label, Response, RichText, ScrollArea, Sense,
    TextStyle, Ui, vec2,
};
use tc_app::app::{App, FindForm, FormRow, Mode, RenameForm, SettingsForm, SyncForm};
use tc_app::fmt::procs_confirm;
use tc_app::fmt::{self, Dialog, dialog, input_dialog, stats_progress};
use tc_app::{KeyCode, KeyEvent, KeyModifiers};

/// What a mouse did in a form; applied through the app like the matching keys.
enum FormAct {
    /// Row clicked: focus it, toggle a checkbox.
    Row(usize),
    /// Choice row, option picked in its dropdown.
    Choose(usize, usize),
    /// OK / Cancel button: Enter / Esc.
    Key(KeyCode),
}
pub(super) fn overlays(ctx: &Context, app: &mut App) {
    if let Some(d) = dialog(app) {
        show_dialog(ctx, "dialog", &d);
    }
    let act = match &app.mode {
        Mode::FindForm(form) => show_find_form(ctx, form),
        Mode::MultiRename(form) => show_multi_rename(ctx, form),
        Mode::SyncForm(form) => show_sync_form(ctx, form),
        Mode::Settings(form) => show_settings(ctx, form),
        _ => None,
    };
    match act {
        Some(FormAct::Row(i)) => app.form_click(i),
        Some(FormAct::Choose(i, k)) => app.form_choose(i, k),
        Some(FormAct::Key(c)) => app.on_key(KeyEvent::new(c, KeyModifiers::NONE)),
        None => {}
    }
    if let Mode::Sync(s) = &app.mode
        && let Some(text) = &s.confirm
    {
        let d = Dialog::Text {
            title: "Synchronize",
            text: text.clone(),
        };
        show_dialog(ctx, "sync-confirm", &d);
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = &st.confirm
    {
        let d = Dialog::Text {
            title: "Delete",
            text: text.clone(),
        };
        show_dialog(ctx, "stats-confirm", &d);
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = stats_progress(st)
    {
        let d = Dialog::Text {
            title: "Scanning",
            text,
        };
        show_dialog(ctx, "stats-progress", &d);
    }
    if let Mode::Procs(p) = &app.mode
        && let Some((pid, name)) = &p.confirm
    {
        let d = Dialog::Text {
            title: "Kill",
            text: procs_confirm(*pid, name),
        };
        show_dialog(ctx, "procs-confirm", &d);
    }
    if let Some(input) = &app.input {
        show_dialog(ctx, "input", &input_dialog(input));
    }
    if app.help {
        show_help(ctx, app);
    }
}

fn show_dialog(ctx: &Context, id: &str, d: &Dialog) {
    modal(id).show(ctx, |ui| {
        ui.set_min_width(420.0);
        ui.set_max_width(720.0);
        match d {
            Dialog::Text { title, text } => {
                ui.heading(*title);
                ui.separator();
                ui.label(text.as_str());
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
                ui.label(RichText::new(tail.trim_start()).color(MUTED));
            }
            Dialog::List {
                title,
                rows,
                sel,
                empty,
                keys,
            } => {
                ui.heading(title);
                ui.separator();
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (k, p) in rows.iter().enumerate() {
                        let mut t = RichText::new(p.as_str());
                        if k == *sel {
                            t = t.background_color(SELECT_BG).color(Color32::WHITE);
                        }
                        let r = ui.label(t);
                        if k == *sel {
                            r.scroll_to_me(None);
                        }
                    }
                });
                if rows.is_empty() {
                    ui.label(*empty);
                }
                ui.label(RichText::new(*keys).color(MUTED));
            }
        }
    });
}

/// Find files form: fields and checkboxes, the focused one highlighted; keys go through the app.
fn show_find_form(ctx: &Context, form: &FindForm) -> Option<FormAct> {
    let (rows, err) = (&form.rows, form.error.as_deref());
    let keys = fmt::FIND_FORM_KEYS;
    show_form(
        ctx,
        ("find-form", "Find files"),
        (rows, form.focus, err),
        keys,
        "Search",
    )
}

fn show_sync_form(ctx: &Context, form: &SyncForm) -> Option<FormAct> {
    let title = "Synchronize directories (left <-> right panel)";
    let (rows, err) = (&form.rows, form.error.as_deref());
    let keys = fmt::SYNC_FORM_KEYS;
    show_form(
        ctx,
        ("sync-form", title),
        (rows, form.focus, err),
        keys,
        "Compare",
    )
}

fn show_settings(ctx: &Context, form: &SettingsForm) -> Option<FormAct> {
    let keys = fmt::SETTINGS_FORM_KEYS;
    show_form(
        ctx,
        ("settings", "Settings"),
        (&form.rows, form.focus, None),
        keys,
        "Save",
    )
}

/// Modal form: (id, title), (rows, focus, error), key help, then OK / Cancel.
fn show_form(
    ctx: &Context,
    (id, title): (&str, &str),
    (rows, focus, error): (&[FormRow], usize, Option<&str>),
    keys: &str,
    ok: &str,
) -> Option<FormAct> {
    modal(id)
        .show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(title);
            ui.separator();
            let act = form_rows(ui, rows, focus, error);
            ui.label(RichText::new(keys).color(MUTED));
            act.or(ok_cancel(ui, ok))
        })
        .inner
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
