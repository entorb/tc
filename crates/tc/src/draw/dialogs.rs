use super::chrome::{line_edit, list_button, text_box};
use super::help::show_help;
use super::{ACCENT, MUTED, TEXT, icons, modal};
use eframe::egui::{
    self, Button, Color32, ComboBox, Context, Label, ModalResponse, Response, RichText, ScrollArea,
    Sense, Sides, TextStyle, Ui, vec2,
};
use tc::app::{
    App, BmClick, BookmarkForm, BookmarkRow, FilterForm, FindForm, FormRow, Mode, PropsForm,
    SettingsForm, SyncForm,
};
use tc::fmt::{self, Dialog, dialog, input_dialog, stats_progress};
use tc::fmt::{ProcInfo, procs_confirm, procs_info};
use tc::{KeyCode, KeyEvent, KeyModifiers};

/// What a mouse did in a form; applied through the app like the matching keys.
pub(super) enum FormAct {
    /// Row clicked: focus it, toggle a checkbox.
    Row(usize),
    /// Choice row, option picked in its dropdown.
    Choose(usize, usize),
    /// OK / Cancel button: Enter / Esc.
    Key(KeyCode),
    /// List row clicked: move the selection by this many rows, Enter when double-clicked.
    Pick(isize, bool),
    /// Bookmarks edit page.
    Bookmark(BmClick),
}
pub(super) fn overlays(ctx: &Context, app: &mut App) {
    let mut pressed = None;
    if let Some(d) = dialog(app) {
        // a stray click must not cancel a running copy or connect: only the close button does
        let busy = matches!(
            app.mode,
            Mode::Work(_) | Mode::Xfer(_) | Mode::Connecting(_)
        );
        pressed = show_dialog(ctx, ("dialog", busy), &d);
    }
    let act = match (&app.search_form, &app.mode) {
        (Some(form), _) => show_form(
            ctx,
            ("search", "Search"),
            (&form.rows, form.focus, form.error.as_deref()),
            Some("OK"),
        ),
        (None, Mode::FindForm(form)) => show_find_form(ctx, form),
        (None, Mode::SyncForm(form)) => show_sync_form(ctx, form),
        (None, Mode::Settings(form)) => show_settings(ctx, form),
        (None, Mode::Props(form)) => show_props(ctx, form),
        (None, Mode::BookmarkEdit(form)) => show_bookmark_edit(ctx, form),
        (None, Mode::Filter(form)) => show_filter(ctx, form),
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
        pressed = pressed.or(show_dialog(ctx, ("sync-confirm", false), &d));
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = &st.confirm
    {
        let d = Dialog::Text {
            title: "Delete",
            text: text.clone(),
        };
        pressed = pressed.or(show_dialog(ctx, ("stats-confirm", false), &d));
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = stats_progress(st)
    {
        let d = Dialog::Text {
            title: "Scanning",
            text,
        };
        pressed = pressed.or(show_dialog(ctx, ("stats-progress", true), &d));
    }
    if let Mode::Procs(p) = &app.mode
        && let Some((pid, name)) = &p.confirm
    {
        let d = Dialog::Text {
            title: "Kill",
            text: procs_confirm(*pid, name),
        };
        pressed = pressed.or(show_dialog(ctx, ("procs-confirm", false), &d));
    }
    if let Some(input) = &app.input {
        pressed = pressed.or(show_dialog(ctx, ("input", false), &input_dialog(input)));
    }
    if app.help {
        show_help(ctx, app);
    }
    apply(app, act.or(pressed));
}

/// Runs what the mouse did through the app, like the matching keys.
fn apply(app: &mut App, act: Option<FormAct>) {
    match act {
        Some(FormAct::Row(i)) => app.form_click(i),
        Some(FormAct::Choose(i, k)) => app.form_choose(i, k),
        Some(FormAct::Key(c)) => app.on_key(KeyEvent::new(c, KeyModifiers::NONE)),
        Some(FormAct::Bookmark(c)) => app.bookmark_click(c),
        Some(FormAct::Pick(by, double)) => {
            let key = if by < 0 { KeyCode::Up } else { KeyCode::Down };
            for _ in 0..by.unsigned_abs() {
                app.on_key(KeyEvent::new(key, KeyModifiers::NONE));
            }
            if double || app.list_click_opens() {
                app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
        }
        None => {}
    }
}

/// Dialog as egui widgets; returns the key of a clicked button or checkbox (the app owns the keys).
/// A click outside closes it (Esc) unless `busy`.
fn show_dialog(ctx: &Context, (id, busy): (&str, bool), d: &Dialog) -> Option<FormAct> {
    let r = modal(id).show(ctx, |ui| {
        // progress popups keep one width while paths change
        ui.set_min_width(if busy { 640.0 } else { 420.0 });
        ui.set_max_width(720.0);
        match d {
            Dialog::Text { title, text } => {
                let x = title_bar(ui, title);
                x.or(text_with_keys(ui, text).map(FormAct::Key))
            }
            Dialog::Edit {
                title,
                head,
                line,
                tail,
            } => {
                let x = title_bar(ui, title);
                if !head.is_empty() {
                    ui.label(head.as_str());
                }
                line_edit(ui, line);
                x.or(text_with_keys(ui, tail.trim_start()).map(FormAct::Key))
            }
            Dialog::List {
                title,
                rows,
                sel,
                empty,
                buttons,
            } => {
                let x = title_bar(ui, title);
                let act = x.or(list_rows(ui, rows, *sel));
                if rows.is_empty() {
                    ui.label(*empty);
                }
                act.or(list_buttons(ui, buttons))
            }
        }
    });
    if busy {
        return r.inner;
    }
    or_outside(r)
}

/// Close icon of every popup title bar; must exist in the icon font (test).
pub(super) const CLOSE_ICON: &str = "✖";

/// Heading with a close button at the right, then a separator; the button is Esc.
pub(super) fn title_bar(ui: &mut Ui, title: &str) -> Option<FormAct> {
    let (_, x) = Sides::new().show(
        ui,
        |ui| ui.heading(title),
        |ui| icon_button(ui, CLOSE_ICON, "Close (Esc)"),
    );
    ui.separator();
    x.then_some(FormAct::Key(KeyCode::Esc))
}

/// The popup's action, or Esc when the click went beside it.
fn or_outside(r: ModalResponse<Option<FormAct>>) -> Option<FormAct> {
    let outside = r
        .backdrop_response
        .clicked()
        .then_some(FormAct::Key(KeyCode::Esc));
    r.inner.or(outside)
}

/// Buttons under a list; the key of the clicked one.
/// One-glyph labels are icons (icon font), the tooltip names the key.
fn list_buttons(ui: &mut Ui, buttons: &[(&str, &str, KeyCode, bool)]) -> Option<FormAct> {
    if buttons.is_empty() {
        return None;
    }
    ui.separator();
    ui.horizontal(|ui| {
        let mut act = None;
        for &(label, tip, code, enabled) in buttons {
            let text = match label.chars().count() {
                1 => RichText::new(label).font(icons()),
                _ => RichText::new(label),
            };
            let b = Button::new(text).sense(Sense::CLICK);
            if ui
                .add_enabled(enabled, b)
                .on_hover_text(tip)
                .on_disabled_hover_text(tip)
                .clicked()
            {
                act = Some(FormAct::Key(code));
            }
        }
        act
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
        // `o/Enter`: alternatives of one action, the first is clicked
        _ if k.contains('/') => k.split('/').next().and_then(hint_key),
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
    // a grid: several hint lines line up in columns (conflict dialog)
    egui::Grid::new(ui.scope_id().with("hints")).show(ui, |ui| {
        for line in &lines[body..] {
            let hints = hint_line(line).unwrap_or_else(|| vec![("Esc", "OK", KeyCode::Esc)]);
            for (k, label, code) in hints {
                let b = Button::new(label)
                    .shortcut_text(k)
                    .min_size(vec2(110.0, 0.0))
                    .sense(Sense::CLICK);
                if ui.add(b).clicked() {
                    pressed = Some(code);
                }
            }
            ui.end_row();
        }
    });
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
    or_outside(modal(id).show(ctx, |ui| {
        ui.set_width(560.0);
        let x = title_bar(ui, title);
        let act = x.or(form_rows(ui, rows, focus, error));
        act.or(ok.and_then(|ok| ok_cancel(ui, ok)))
    }))
}

/// Process info: label / value grid; a click outside closes it (as Esc).
fn show_info(ctx: &Context, info: &ProcInfo) -> Option<FormAct> {
    let r = modal("proc-info").show(ctx, |ui| {
        ui.set_min_width(480.0);
        let x = title_bar(ui, info.title.trim());
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
        x
    });
    or_outside(r)
}

/// Icon buttons of the bookmark list and edit page; must exist in the icon font (test).
pub(super) const BM_ICONS: [&str; 4] = ["✚", "➖", "⏶", "⏷"];

fn icon_button(ui: &mut Ui, icon: &str, tip: &str) -> bool {
    ui.add(Button::new(RichText::new(icon).font(icons())).sense(Sense::CLICK))
        .on_hover_text(tip)
        .clicked()
}

/// All bookmarks as rows: order buttons, alias, path, delete; Add, Save, Cancel below.
fn show_bookmark_edit(ctx: &Context, form: &BookmarkForm) -> Option<FormAct> {
    let r = modal("bookmark-edit").show(ctx, |ui| {
        ui.set_width(860.0);
        let mut act = title_bar(ui, "Edit bookmarks").map(|_| BmClick::Cancel);
        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            egui::Grid::new("bookmark-edit-grid")
                .num_columns(5)
                .spacing(vec2(8.0, 6.0))
                .show(ui, |ui| {
                    ui.label("");
                    ui.label("");
                    ui.label(RichText::new("Alias").strong().color(MUTED));
                    ui.label(RichText::new("Path").strong().color(MUTED));
                    ui.end_row();
                    for (i, row) in form.rows.iter().enumerate() {
                        if let Some(c) = bookmark_row(ui, i, row, form.focus) {
                            act = Some(c);
                        }
                        ui.end_row();
                    }
                });
        });
        if form.rows.is_empty() {
            ui.label(RichText::new("(no bookmarks, ✚ adds the current directory)").color(MUTED));
        }
        if let Some(e) = &form.error {
            ui.label(RichText::new(e).color(Color32::LIGHT_RED));
        }
        ui.separator();
        ui.horizontal(|ui| {
            if icon_button(ui, BM_ICONS[0], "Add the current directory (Insert)") {
                act = Some(BmClick::Add);
            }
            if ui.add(Button::new("Save").sense(Sense::CLICK)).clicked() {
                act = Some(BmClick::Save);
            }
            if ui.add(Button::new("Cancel").sense(Sense::CLICK)).clicked() {
                act = Some(BmClick::Cancel);
            }
        });
        act.map(FormAct::Bookmark)
    });
    or_outside(r)
}

/// One edit row: up / down, alias and path fields, delete.
fn bookmark_row(
    ui: &mut Ui,
    i: usize,
    row: &BookmarkRow,
    focus: (usize, usize),
) -> Option<BmClick> {
    let mut act = None;
    ui.horizontal(|ui| {
        if icon_button(ui, BM_ICONS[2], "Move up (Ctrl+Up)") {
            act = Some(BmClick::Up(i));
        }
        if icon_button(ui, BM_ICONS[3], "Move down (Ctrl+Down)") {
            act = Some(BmClick::Down(i));
        }
    });
    ui.label(RichText::new(format!("{}", i + 1)).color(MUTED));
    for (c, (l, w)) in [(&row.alias, 200.0), (&row.path, 480.0)]
        .into_iter()
        .enumerate()
    {
        let r = ui.allocate_ui(vec2(w, ui.spacing().interact_size.y), |ui| {
            ui.set_width(w);
            text_box(ui, l, focus == (i, c))
        });
        if r.inner.clicked() {
            act = Some(BmClick::Field(i, c));
        }
    }
    if icon_button(ui, BM_ICONS[1], "Delete this bookmark (F8)") {
        act = Some(BmClick::Delete(i));
    }
    act
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
/// Panel filter: text fields, then the file types of the directory as checkboxes, OK / Cancel.
fn show_filter(ctx: &Context, form: &FilterForm) -> Option<FormAct> {
    or_outside(modal("panel-filter").show(ctx, |ui| {
        ui.set_width(560.0);
        let x = title_bar(ui, "Filter");
        let act = x.or(form_rows(ui, &form.rows, form.focus, form.error.as_deref()));
        ui.separator();
        ui.label(RichText::new("File types (none checked: all)").strong());
        let types = ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| filter_types(ui, form))
            .inner;
        act.or(types).or(ok_cancel(ui, "OK"))
    }))
}

/// File type checkboxes, wrapped; a click reports its form row (after the text rows).
fn filter_types(ui: &mut Ui, form: &FilterForm) -> Option<FormAct> {
    let first = form.rows.len();
    ui.horizontal_wrapped(|ui| {
        let mut act = None;
        for (k, (ext, n, on)) in form.exts.iter().enumerate() {
            let name = if ext.is_empty() {
                "(none)".to_owned()
            } else {
                format!(".{ext}")
            };
            let color = if form.focus == first + k {
                ACCENT
            } else {
                TEXT
            };
            let mut on = *on; // a copy: the app toggles the real one on the click
            let r = ui.checkbox(&mut on, RichText::new(format!("{name} ({n})")).color(color));
            unfocus(&r);
            if r.clicked() {
                act = Some(FormAct::Row(first + k));
            }
        }
        act
    })
    .inner
}

/// Properties: the fields, then the mode preview and file info, OK / Cancel.
fn show_props(ctx: &Context, form: &PropsForm) -> Option<FormAct> {
    let lines = form.lines().join("\n");
    or_outside(modal("props").show(ctx, |ui| {
        ui.set_width(560.0);
        let x = title_bar(ui, "Properties");
        let act = x.or(form_rows(ui, &form.rows, form.focus, form.error.as_deref()));
        ui.separator();
        ui.label(RichText::new(lines).monospace());
        act.or(ok_cancel(ui, "Apply"))
    }))
}

/// Multi-rename screen: form, then as many old -> new rows as fit, the placeholder legend.
pub(super) fn draw_multi_rename(ui: &mut Ui, app: &mut App) {
    let Mode::MultiRename(form) = &app.mode else {
        return;
    };
    ui.heading("Multi-rename");
    ui.separator();
    let act = form_rows(ui, &form.rows, form.focus, form.error.as_deref());
    ui.separator();
    ui.label(format!("{} files:", form.preview.len()));
    let row_h = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;
    let legend_h = 3.0 * row_h; // separator and the two legend lines
    let fit = ((ui.available_height() - legend_h) / row_h).max(1.0) as usize;
    for (old, new) in form.preview.iter().skip(form.scroll).take(fit) {
        let (text, bad) = fmt::rename_row(old, new);
        let t = RichText::new(text);
        ui.label(if bad { t.color(Color32::LIGHT_RED) } else { t });
    }
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        ui.label(RichText::new(fmt::RENAME_FORM_KEYS).color(MUTED));
        ui.separator();
    });
    apply(app, act);
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
