use super::{MUTED, icons, mono};
use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{
    self, Align, Button, Context, Event, Id, Layout, RichText, Sense, TextEdit, Ui,
};
use tc::app::{App, MENUS, Mode, TOOLBAR, ToolItem};
use tc::{KeyCode, KeyEvent, KeyModifiers};
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

/// Single-line text of `l` with a caret at its cursor.
pub(super) fn line_edit(ui: &mut Ui, l: &LineEdit) {
    text_box(ui, l, true);
}

/// `l` as a native egui text field. The app owns every key (typing, paste, IME commit, Enter,
/// Esc, Tab, history): keyboard events are hidden from the field, so it only takes the mouse
/// (caret placement, drag selection, copy of the selection) and shows the caret where `l` has it.
/// `focused`: the field has egui focus (caret, IME window); else a click on it reports `clicked`.
pub(super) fn text_box(ui: &mut Ui, l: &LineEdit, focused: bool) -> egui::Response {
    let id = ui.next_auto_id();
    let caret = l.text[..l.cur].chars().count();
    let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
    // keep a mouse selection while its moving end is still the app's caret
    if state.cursor.char_range().map(|r| r.primary.index.0) != Some(caret) {
        let one = CCursorRange::one(CCursor::new(caret));
        state.cursor.set_char_range(Some(one));
        TextEdit::store_state(ui.ctx(), id, state);
    }
    if focused {
        ui.ctx().memory_mut(|m| m.request_focus(id));
    }
    let mut text = l.text.clone();
    let edit = TextEdit::singleline(&mut text)
        .id(id)
        .font(mono())
        .desired_width(f32::INFINITY);
    let out = without_keys(ui, |ui| edit.show(ui));
    let r = out.response.response;
    if !focused && r.has_focus() {
        r.surrender_focus();
    }
    // without keys, only the mouse moves egui's caret: hand it to the app (see `apply_caret`)
    // `out.state`, not `out.cursor_range`: egui handles the mouse after computing the latter
    if focused
        && let Some(c) = out.state.cursor.char_range()
        && c.primary.index.0 != caret
    {
        ui.ctx()
            .data_mut(|d| d.insert_temp(caret_id(), c.primary.index.0));
    }
    r
}

/// Runs `f` with this frame's keyboard events hidden (they went to `App::on_key` already).
fn without_keys<R>(ui: &mut Ui, f: impl FnOnce(&mut Ui) -> R) -> R {
    let all = ui.ctx().input_mut(|i| std::mem::take(&mut i.events));
    let rest = all.iter().filter(|e| !is_key_input(e)).cloned().collect();
    ui.ctx().input_mut(|i| i.events = rest);
    let out = f(ui);
    ui.ctx().input_mut(|i| i.events = all);
    out
}

fn is_key_input(e: &Event) -> bool {
    matches!(
        e,
        Event::Text(_) | Event::Key { .. } | Event::Paste(_) | Event::Cut | Event::Ime(_)
    )
}

fn caret_id() -> Id {
    Id::unique("tc-text-box-caret")
}

/// Caret a click put into the focused text box, set in the app's field.
pub(super) fn apply_caret(ctx: &Context, app: &mut App) {
    let Some(n) = ctx.data_mut(|d| d.remove_temp::<usize>(caret_id())) else {
        return;
    };
    // the help filter has no caret of its own
    if !app.help {
        app.set_caret(n);
    }
}

/// Full-width selectable list row (egui look); not focusable, so keys stay with the app.
pub(super) fn list_button(
    ui: &mut Ui,
    text: impl Into<String>,
    selected: bool,
    mono_font: bool,
) -> egui::Response {
    let mut t = RichText::new(text);
    if mono_font {
        t = t.font(mono());
    }
    ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
        ui.add(
            Button::selectable(selected, t)
                .sense(Sense::CLICK)
                .truncate(),
        )
    })
    .inner
}
