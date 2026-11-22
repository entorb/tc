use super::chrome::line_edit;
use super::help::show_help;
use super::{ACCENT, MUTED, SELECT_BG, TEXT, modal, mono};
use eframe::egui::{self, Color32, Context, RichText, ScrollArea, TextStyle, Ui, vec2};
use tc_app::app::{App, FindForm, FormRow, Mode, RenameForm, SettingsForm, SyncForm};
use tc_app::fmt::procs_confirm;
use tc_app::fmt::{self, Dialog, dialog, input_dialog, stats_progress};
pub(super) fn overlays(ctx: &Context, app: &mut App) {
    if let Some(d) = dialog(app) {
        show_dialog(ctx, "dialog", &d);
    }
    if let Mode::FindForm(form) = &app.mode {
        show_find_form(ctx, form);
    }
    if let Mode::MultiRename(form) = &app.mode {
        show_multi_rename(ctx, form);
    }
    if let Mode::SyncForm(form) = &app.mode {
        show_sync_form(ctx, form);
    }
    if let Mode::Settings(form) = &app.mode {
        show_settings(ctx, form);
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
fn show_find_form(ctx: &Context, form: &FindForm) {
    modal("find-form").show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("Find files");
        ui.separator();
        form_rows(ui, &form.rows, form.focus, form.error.as_deref());
        ui.label(RichText::new(fmt::FIND_FORM_KEYS).color(MUTED));
    });
}

fn show_sync_form(ctx: &Context, form: &SyncForm) {
    modal("sync-form").show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("Synchronize directories (left <-> right panel)");
        ui.separator();
        form_rows(ui, &form.rows, form.focus, form.error.as_deref());
        ui.label(RichText::new(fmt::SYNC_FORM_KEYS).color(MUTED));
    });
}

fn show_settings(ctx: &Context, form: &SettingsForm) {
    modal("settings").show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Settings");
        ui.separator();
        form_rows(ui, &form.rows, form.focus, None);
        ui.label(RichText::new(fmt::SETTINGS_FORM_KEYS).color(MUTED));
    });
}

/// Multi-rename over the whole window: form, then as many old -> new rows as fit; keys go through the app.
fn show_multi_rename(ctx: &Context, form: &RenameForm) {
    let size = ctx.content_rect().shrink(24.0).size();
    modal("multi-rename").show(ctx, |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        ui.heading("Multi-rename");
        ui.separator();
        form_rows(ui, &form.rows, form.focus, form.error.as_deref());
        ui.separator();
        ui.label(format!("{} files:", form.preview.len()));
        let row_h = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;
        let keys_h = 2.0 * row_h;
        let fit = ((ui.available_height() - keys_h) / row_h).max(1.0) as usize;
        for (old, new) in form.preview.iter().skip(form.scroll).take(fit) {
            let (text, bad) = fmt::rename_row(old, new);
            let t = RichText::new(text);
            ui.label(if bad { t.color(Color32::LIGHT_RED) } else { t });
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.label(RichText::new(fmt::RENAME_FORM_KEYS).color(MUTED));
        });
    });
}

/// Form as a label | value grid, the focused row highlighted, and the error under it.
fn form_rows(ui: &mut Ui, rows: &[FormRow], focus: usize, error: Option<&str>) {
    egui::Grid::new("form")
        .num_columns(2)
        .spacing(vec2(12.0, 4.0))
        .show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                let focused = i == focus;
                let color = if focused { ACCENT } else { TEXT };
                let label = |t: &str| RichText::new(t.to_owned()).color(color);
                match row {
                    FormRow::Text(name, l) => {
                        ui.label(label(name).strong());
                        if focused {
                            line_edit(ui, l);
                        } else {
                            ui.label(RichText::new(l.text.as_str()).font(mono()));
                        }
                    }
                    FormRow::Check(name, on) => {
                        ui.label("");
                        ui.label(label(&format!(
                            "{} {name}",
                            if *on { "[x]" } else { "[ ]" }
                        )));
                    }
                    FormRow::Choice(name, opts, k) => {
                        ui.label(label(name).strong());
                        ui.label(label(&format!("< {} >", opts[*k])));
                    }
                }
                ui.end_row();
            }
        });
    if let Some(e) = error {
        ui.label(RichText::new(e).color(Color32::LIGHT_RED));
    }
}
