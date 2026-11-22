use super::{MUTED, icons, mono};
use eframe::egui::{self, Button, Id, RichText, Sense, Stroke, Ui, vec2};
use tc_app::app::{App, MENUS, Mode, TOOLBAR, ToolItem};
use tc_app::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::cmdline::LineEdit;
/// Top menu bar (F9 or click); the open group drops down below its title.
pub(super) fn menu_bar(ui: &mut Ui, app: &mut App) {
    let open = if let Mode::Menu(g, _) = app.mode {
        Some(g)
    } else {
        None
    };
    let mut run = None;
    ui.horizontal(|ui| {
        for (g, (title, items)) in MENUS.iter().enumerate() {
            // not focusable: Tab stays with the app instead of walking into the menu
            let r = ui.add(Button::selectable(open == Some(g), *title).sense(Sense::CLICK));
            if r.clicked() {
                app.mode = if open == Some(g) {
                    Mode::Normal
                } else {
                    Mode::Menu(g, 0)
                };
            }
            if open != Some(g) {
                continue;
            }
            let sel = if let Mode::Menu(_, s) = app.mode {
                s
            } else {
                0
            };
            if let Some(i) = menu_drop(ui, r.rect.left_bottom(), items, sel) {
                run = Some((g, i));
            }
        }
    });
    if let Some((g, i)) = run {
        app.menu_run(g, i);
    }
}

/// Dropdown of one menu group; returns the clicked item.
fn menu_drop(
    ui: &Ui,
    pos: egui::Pos2,
    items: &[(&str, KeyCode, KeyModifiers)],
    sel: usize,
) -> Option<usize> {
    let mut run = None;
    egui::Area::new(Id::unique("menu-drop"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(260.0);
                for (i, (label, ..)) in items.iter().enumerate() {
                    let (name, key) = label.split_once("  ").unwrap_or((label, ""));
                    let b = Button::selectable(i == sel, name)
                        .shortcut_text(key.trim())
                        .sense(Sense::CLICK)
                        .min_size(egui::vec2(ui.available_width(), 0.0));
                    if ui.add(b).clicked() {
                        run = Some(i);
                    }
                }
            });
        });
    run
}

/// Icon toolbar under the menu bar; clicks send the key the menu lists.
pub(super) fn toolbar(ui: &mut Ui, app: &mut App) {
    // toolbar keys only act on the panels; greyed out on sub-screens, dialogs and prompts
    let enabled = matches!(app.mode, Mode::Normal) && app.idle();
    ui.horizontal(|ui| {
        if !enabled {
            ui.disable();
        }
        for (g, group) in TOOLBAR.iter().enumerate() {
            if g > 0 {
                ui.separator();
            }
            toolbar_group(ui, app, group);
        }
    });
}

fn toolbar_group(ui: &mut Ui, app: &mut App, group: &[ToolItem]) {
    for &(icon, tip, code, mods) in group {
        let clicked = ui
            .add(Button::new(RichText::new(icon).font(icons())).sense(Sense::CLICK))
            .on_hover_text(tip)
            .clicked();
        if clicked && app.idle() {
            app.on_key(KeyEvent::new(code, mods));
        }
    }
}

/// F-key bar; buttons are not focusable, so Tab and Space stay with the app.
/// Holding Shift shows the Shift alternates; the tooltip is the key's F1 help.
pub(super) fn footer(ui: &mut Ui, app: &mut App) {
    let shift = ui.input(|i| i.modifiers.shift);
    let help = app.help_text().1;
    ui.horizontal(|ui| {
        for &item in app.footer_keys() {
            let (label, code, mods) = if shift {
                app.footer_shifted(item)
            } else {
                item
            };
            let clicked = ui
                .add(Button::new(label).sense(Sense::CLICK))
                .on_hover_ui(|ui| {
                    ui.label(tc_app::fmt::key_help(help, label));
                })
                .clicked();
            if clicked && app.footer_active() {
                app.on_key(KeyEvent::new(code, mods));
            }
        }
    });
}

/// Prompt and command being typed; a click starts typing like `:`.
pub(super) fn cmdline(ui: &mut Ui, app: &mut App) {
    let r = ui.horizontal(|ui| {
        let prompt = format!("{}> ", app.panels[app.active].path().display());
        ui.label(RichText::new(prompt).font(mono()).color(MUTED));
        if let Mode::Cmd(l) = &app.mode {
            line_edit(ui, l);
        }
        ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
    });
    let click = r.response.interact(Sense::CLICK);
    if click.on_hover_cursor(egui::CursorIcon::Text).clicked()
        && matches!(app.mode, Mode::Normal)
        && app.idle()
    {
        app.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
    }
}

/// Single-line text of `l` with a caret at its cursor.
pub(super) fn line_edit(ui: &mut Ui, l: &LineEdit) {
    text_box(ui, l, true);
}

/// `l` drawn like an egui text field (theme colors); keys stay with the app, so egui never
/// focuses it. `focused`: accent border and the caret; else clickable.
pub(super) fn text_box(ui: &mut Ui, l: &LineEdit, focused: bool) -> egui::Response {
    let v = ui.visuals();
    let (text_color, bg, radius) = (
        v.strong_text_color(),
        v.text_edit_bg_color(),
        v.widgets.inactive.corner_radius,
    );
    let (border, caret_stroke) = if focused {
        (v.selection.stroke, v.text_cursor.stroke)
    } else {
        (v.widgets.inactive.bg_stroke, Stroke::NONE)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(l.text.clone(), mono(), text_color);
    let pad = vec2(6.0, 3.0);
    let width = ui.available_width().max(galley.size().x + 2.0 * pad.x);
    let sense = if focused {
        Sense::hover()
    } else {
        Sense::CLICK
    };
    let (rect, r) = ui.allocate_exact_size(vec2(width, galley.size().y + 2.0 * pad.y), sense);
    let caret = galley
        .pos_from_cursor(egui::text::CCursor::new(l.text[..l.cur].chars().count()))
        .min
        .x;
    let p = ui.painter();
    p.rect_filled(rect, radius, bg);
    p.rect_stroke(rect, radius, border, egui::StrokeKind::Inside);
    let origin = rect.min + pad;
    p.galley(origin, galley, text_color);
    if focused {
        let x = origin.x + caret;
        p.vline(x, (origin.y)..=(rect.max.y - pad.y), caret_stroke);
    }
    r.on_hover_cursor(egui::CursorIcon::Text)
}
