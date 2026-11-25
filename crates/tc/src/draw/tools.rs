//! Full-screen tool forms: multi-rename, find files, synchronize. Each has the options on the
//! left half (a tab bar when they do not fit one page) and what they act on or produce on the
//! right half (find files has no right half: full width); the F-key bar runs them. Keys go through the app.

use super::dialogs::{FormAct, apply, field_grid, form_row};
use super::{MUTED, TEXT};
use eframe::egui::{self, Color32, RichText, TextStyle, Ui, vec2};
use tc::app::{App, Mode, RenameForm, Sync};
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

/// Find files: the tab bar, the shown tab's fields, what they take and the error; full width.
pub(super) fn draw_find_form(ui: &mut Ui, app: &mut App) {
    let Mode::FindForm(form) = &app.mode else {
        return;
    };
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
    let act = tabs.or(body);
    apply(app, act);
}

/// Synchronize options above the rows: the mask field (row 0) on one line, the checkboxes
/// wrapped on the next. The focused row is highlighted only while they have the keyboard.
pub(super) fn sync_options(ui: &mut Ui, s: &Sync) -> Option<FormAct> {
    let focus = if s.editing { s.form.focus } else { usize::MAX };
    let rows = &s.form.rows;
    let mask = field_grid(ui, "sync-mask", rows, 0..1, focus);
    let mut act = None;
    ui.horizontal_wrapped(|ui| {
        // a wrapped layout would break the checkbox texts onto each other
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        for (i, row) in rows.iter().enumerate().skip(1) {
            if let Some(a) = form_row(ui, i, row, i == focus, row.label()) {
                act = Some(a);
            }
        }
    });
    let act = mask.or(act);
    if let Some(e) = &s.form.error {
        ui.label(RichText::new(e).color(Color32::LIGHT_RED));
    }
    ui.separator();
    act
}

/// Before the first compare: what the rows will offer.
pub(super) fn sync_hints(ui: &mut Ui) {
    ui.label(RichText::new("Enter compares the two panel directories.").strong());
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
