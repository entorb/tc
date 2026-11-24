use super::widgets::line_edit;
use super::{MUTED, icons, mono};

use eframe::egui::{self, Button, Id, RichText, Sense, Ui};
use tc::app::{App, MENUS, Mode, TOOLBAR, ToolItem};
use tc::{KeyCode, KeyEvent, KeyModifiers};
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
                    ui.label(tc::fmt::key_help(help, label));
                })
                .clicked();
            if clicked && app.footer_active() {
                app.on_key(KeyEvent::new(code, mods));
            }
        }
    });
}

/// Prompt and command being typed; only shown in command mode (`:`).
pub(super) fn cmdline(ui: &mut Ui, app: &mut App) {
    ui.horizontal(|ui| {
        let prompt = format!("{}> ", app.panels[app.active].path().display());
        ui.label(RichText::new(prompt).font(mono()).color(MUTED));
        if let Mode::Cmd(l) = &app.mode {
            line_edit(ui, l);
        }
        ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
    });
}
