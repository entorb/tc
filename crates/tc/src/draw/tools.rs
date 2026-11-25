//! Full-screen tool forms: multi-rename, find files, synchronize. Each has the options on the
//! left half (a tab bar when they do not fit one page) and what they act on or produce on the
//! right half; the F-key bar runs them. Keys go through the app.

use super::dialogs::{FormAct, apply, form_row};
use super::{MUTED, TEXT};
use eframe::egui::{self, Button, Color32, RichText, Sense, TextStyle, Ui, vec2};
use tc::KeyCode;
use tc::app::{App, FindForm, FormRow, Mode, RenameForm};
use tc::fmt;

/// Title of a half of the window, its optional note on the right, and the rule under both:
/// the same on the left and right, so the two halves line up.
fn panel_head(ui: &mut Ui, title: &str, note: Option<String>) {
    ui.horizontal(|ui| {
        ui.heading(title);
        if let Some(note) = note {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(note);
            });
        }
    });
    ui.separator();
}

/// One selectable title per tab, with a dot on a tab whose options are set (`changed`).
fn tab_bar(
    ui: &mut Ui,
    titles: &[&str],
    tab: usize,
    changed: impl Fn(usize) -> bool,
) -> Option<FormAct> {
    let mut act = None;
    ui.horizontal(|ui| {
        for (t, title) in titles.iter().enumerate() {
            let text = if changed(t) {
                format!("{title} •")
            } else {
                (*title).to_owned()
            };
            if ui.selectable_label(tab == t, text).clicked() {
                act = Some(FormAct::Tab(t));
            }
        }
    });
    ui.separator();
    act
}

/// Form rows `shown` (indexes into `rows`) as label | widget grid rows, `focus` highlighted.
fn field_grid(
    ui: &mut Ui,
    id: &str,
    rows: &[FormRow],
    shown: impl Iterator<Item = usize>,
    focus: usize,
) -> Option<FormAct> {
    let mut act = None;
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(vec2(12.0, 6.0))
        .show(ui, |ui| {
            for i in shown {
                let row = &rows[i];
                let label = fmt::rename_shown(row.label());
                if let Some(a) = form_row(ui, i, row, i == focus, label) {
                    act = Some(a);
                }
                ui.end_row();
            }
        });
    act
}

/// What the fields above take, in muted text, then the form's error or note.
fn info_and_error(ui: &mut Ui, info: &[&str], error: Option<&str>) {
    ui.add_space(8.0);
    // not justified: the columns layout would stretch the words of wrapped lines
    ui.vertical(|ui| {
        for line in info {
            ui.label(RichText::new(*line).color(MUTED));
        }
        if let Some(e) = error {
            ui.add_space(4.0);
            ui.label(RichText::new(e).color(Color32::LIGHT_RED));
        }
    });
}

/// A button that sends `code` like its key; the key shows on its right.
fn key_button(ui: &mut Ui, label: &str, key: &str, code: KeyCode) -> Option<FormAct> {
    let b = Button::new(label).shortcut_text(key).sense(Sense::CLICK);
    ui.add(b).clicked().then_some(FormAct::Key(code))
}

/// Find files: the tabs of options on the left, the saved searches on the right.
pub(super) fn draw_find_form(ui: &mut Ui, app: &mut App) {
    let Mode::FindForm(form) = &app.mode else {
        return;
    };
    let act = ui.columns(2, |cols| {
        let options = find_options(&mut cols[0], app, form);
        options.or(find_saved(&mut cols[1], app, form))
    });
    apply(app, act);
}

/// The tab bar, the shown tab's fields, what they take and the error.
fn find_options(ui: &mut Ui, app: &App, form: &FindForm) -> Option<FormAct> {
    let dir = app.panels[app.active].path().display();
    panel_head(ui, "Find files", Some(format!("in {dir}")));
    let tabs = tab_bar(ui, &fmt::FIND_TABS, form.tab, |t| form.tab_changed(t));
    let body = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let act = field_grid(ui, "find-fields", &form.rows, form.shown_rows(), form.focus);
            let info = [fmt::FIND_TAB_INFO[form.tab]];
            info_and_error(ui, &info, form.error.as_deref());
            act
        })
        .inner;
    tabs.or(body)
}

/// The saved search name, its F6 / F7 / F8 buttons and the list of saved searches.
fn find_saved(ui: &mut Ui, app: &App, form: &FindForm) -> Option<FormAct> {
    panel_head(ui, "Saved searches", None);
    let saved = form.saved_row();
    let name = field_grid(ui, "find-saved", &form.rows, saved..=saved, form.focus);
    let buttons = ui
        .horizontal(|ui| {
            let save = key_button(ui, "Save", "F6", KeyCode::F(6));
            let load = key_button(ui, "Load next", "F7", KeyCode::F(7));
            save.or(load)
                .or(key_button(ui, "Delete", "F8", KeyCode::F(8)))
        })
        .inner;
    ui.separator();
    let list = saved_list(ui, app, form.saved_name());
    name.or(buttons).or(list)
}

/// The saved searches by name, the one named in the form highlighted; a click loads one.
fn saved_list(ui: &mut Ui, app: &App, current: &str) -> Option<FormAct> {
    let mut act = None;
    egui::ScrollArea::vertical()
        .id_salt("find-saved-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut any = false;
            for (i, name) in app.saved_searches().enumerate() {
                any = true;
                if ui.selectable_label(name == current, name).clicked() {
                    act = Some(FormAct::Load(i));
                }
            }
            if !any {
                let none = "None yet: type a name above, F6 saves the form under it.";
                ui.label(RichText::new(none).color(MUTED));
            }
        });
    act
}

/// Synchronize: the options on the left, the two directories and what comes next on the right.
pub(super) fn draw_sync_form(ui: &mut Ui, app: &mut App) {
    let Mode::SyncForm(form) = &app.mode else {
        return;
    };
    let act = ui.columns(2, |cols| {
        let ui = &mut cols[0];
        panel_head(ui, "Synchronize", None);
        let act = field_grid(
            ui,
            "sync-fields",
            &form.rows,
            0..form.rows.len(),
            form.focus,
        );
        info_and_error(ui, &[], form.error.as_deref());
        sync_dirs(&mut cols[1], app);
        act
    });
    apply(app, act);
}

/// The two panel directories, then what the compare result offers.
fn sync_dirs(ui: &mut Ui, app: &App) {
    panel_head(ui, "Directories", None);
    egui::Grid::new("sync-dirs")
        .num_columns(2)
        .spacing(vec2(12.0, 6.0))
        .show(ui, |ui| {
            for (side, p) in ["Left", "Right"].iter().zip(&app.panels) {
                ui.label(RichText::new(*side).strong());
                ui.label(RichText::new(p.path().display().to_string()).color(TEXT));
                ui.end_row();
            }
        });
    ui.add_space(8.0);
    ui.label(RichText::new("After the compare").strong());
    info_and_error(ui, &fmt::SYNC_INFO, None);
}

/// Multi-rename: the options on the left half, the preview on the right half.
pub(super) fn draw_multi_rename(ui: &mut Ui, app: &mut App) {
    let Mode::MultiRename(form) = &mut app.mode else {
        return;
    };
    form.editor_focused = false;
    let mut act = None;
    ui.columns(2, |cols| {
        act = rename_options(&mut cols[0], form);
        rename_preview(&mut cols[1], form);
    });
    apply(app, act);
}

/// The options: tabs and fields scroll; the Editor's text box fills the height.
fn rename_options(ui: &mut Ui, form: &mut RenameForm) -> Option<FormAct> {
    let mut body = None;
    ui.vertical(|ui| {
        panel_head(ui, "Multi-rename", None);
        let changed = form.changed;
        body = tab_bar(ui, &fmt::RENAME_TABS, form.tab, |t| changed[t]).or_else(|| {
            // the Editor takes all the height left; the other tabs scroll
            if form.tab == fmt::RENAME_TABS.len() - 1 {
                names_editor(ui, form)
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| rename_fields(ui, form))
                    .inner
            }
        });
    });
    body
}

/// The Editor: one new name per line, starting from the other tabs' result, filling the height
/// below the tabs. While it has the keyboard, egui takes the typing (see
/// `App::rename_editor_focused`).
fn names_editor(ui: &mut Ui, form: &mut RenameForm) -> Option<FormAct> {
    ui.label(RichText::new("New names, one per line").strong());
    names_info(ui);
    if let Some(e) = &form.error {
        ui.label(RichText::new(e).color(Color32::LIGHT_RED));
    }
    let size = egui::vec2(ui.available_width(), ui.available_height());
    let out = egui::TextEdit::multiline(&mut form.editor)
        .font(egui::FontId::monospace(14.0))
        .min_size(size)
        .desired_width(f32::INFINITY)
        .show(ui);
    let r = out.response.response;
    form.editor_focused = r.has_focus();
    if !r.has_focus() && ui.ctx().memory(|m| m.focused().is_none()) {
        r.request_focus();
    }
    r.changed().then_some(FormAct::Editor)
}

/// The shown tab's fields as label | widget rows, then what the tab adds: the placeholder
/// buttons (Name) or the placeholder examples; the error under them.
fn rename_fields(ui: &mut Ui, form: &RenameForm) -> Option<FormAct> {
    let shown = form.shown_rows().filter(|&i| !form.hidden(i));
    let act = field_grid(ui, "rename-fields", &form.rows, shown, form.focus);
    let extra = match form.tab {
        0 => rename_chips(ui),
        2 => {
            placeholder_examples(ui);
            None
        }
        _ => None,
    };
    if let Some(e) = &form.error {
        ui.label(RichText::new(e).color(Color32::LIGHT_RED));
    }
    act.or(extra)
}

/// Buttons that insert a placeholder into the focused field; the hover text says what it is.
fn rename_chips(ui: &mut Ui) -> Option<FormAct> {
    let mut act = None;
    ui.horizontal(|ui| {
        ui.label("Insert:");
        for (chip, tip) in fmt::RENAME_CHIPS {
            if ui.button(chip).on_hover_text(tip).clicked() {
                act = Some(FormAct::Insert(chip));
            }
        }
    });
    act
}

/// Placeholder mode is the hard one: show what the letters do, with examples, always visible.
fn placeholder_examples(ui: &mut Ui) {
    egui::Grid::new("rename-placeholder")
        .num_columns(3)
        .spacing(vec2(16.0, 4.0))
        .show(ui, |ui| {
            ui.label(RichText::new("Search").color(MUTED));
            ui.label(RichText::new("Replace").color(MUTED));
            ui.label(RichText::new("Example").color(MUTED));
            ui.end_row();
            for (search, replace, example) in fmt::RENAME_PLACEHOLDER_EXAMPLES {
                ui.label(RichText::new(search).monospace());
                ui.label(RichText::new(replace).monospace());
                ui.label(example);
                ui.end_row();
            }
        });
}

/// What the Editor is, above its text box.
fn names_info(ui: &mut Ui) {
    ui.label("The Editor starts from the result of the other tabs.");
    ui.label(
        RichText::new(
            "Edit the names in the box. A setting above starts the Editor over (F7 restores).",
        )
        .color(MUTED),
    );
}

/// Count of changes, then the old -> new table of the items that fit.
fn rename_preview(ui: &mut Ui, form: &mut RenameForm) {
    panel_head(
        ui,
        "Preview",
        Some(format!("{} of {} change", form.count, form.preview.len())),
    );
    let row_h = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;
    let fit = (ui.available_height() / row_h).max(1.0) as usize;
    form.page = fit; // the PgUp / PgDn step
    egui::Grid::new("rename-preview")
        .num_columns(3)
        .spacing(vec2(8.0, 2.0))
        .show(ui, |ui| {
            for (old, new) in form.preview.iter().skip(form.scroll).take(fit) {
                rename_preview_row(ui, old, new);
                ui.end_row();
            }
        });
}

/// One preview row: old name, arrow (`=` unchanged, `!` a problem), new name.
fn rename_preview_row(ui: &mut Ui, old: &str, new: &Result<String, String>) {
    let cell = fmt::rename_cell(old, new);
    let same = matches!(cell, fmt::RenameCell::Same);
    let (arrow, text, color) = match cell {
        fmt::RenameCell::Same => ("=", "unchanged", MUTED),
        fmt::RenameCell::New(n) => ("->", n, TEXT),
        fmt::RenameCell::Blank => ("", "", TEXT),
        fmt::RenameCell::Bad(e) => ("!", e, Color32::LIGHT_RED),
    };
    ui.label(RichText::new(old).color(if same { MUTED } else { TEXT }));
    ui.label(RichText::new(arrow).color(MUTED));
    ui.label(RichText::new(text).color(color));
}
