use super::{ACCENT, LINK, Look, MARK, MUTED, TEXT, icons};
use eframe::egui::{
    self, Align, Button, Color32, Label, Layout, RichText, Sense, Stroke, TextStyle, Ui,
};
use egui_extras::{Column, TableBuilder};
use tc::app::{App, ENTRY_MENU, Mode};
use tc::fmt::{entry_cols, panel_info, panel_title};
use tc::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::panel::SortKey;
enum Action {
    Sort(SortKey),
    /// Entry, Shift / Ctrl held, double click.
    Click(usize, KeyModifiers, bool),
    /// Middle click: the entry's directory in a new tab.
    NewTab(usize),
    Tab(usize),
    CloseTab(usize),
    /// Title button: activate the panel, then run the key.
    Key(KeyCode, KeyModifiers),
}

pub(super) fn draw_panel(
    ui: &mut Ui,
    app: &mut App,
    look: &mut Look,
    i: usize,
    today: jiff::civil::Date,
) {
    let active = i == app.active;
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;
    let mods = ui.input(|inp| click_mods(inp.modifiers));
    let mut action = None;
    let stroke = if active {
        Stroke::new(1.0, ACCENT)
    } else {
        Stroke::new(1.0, MUTED)
    };
    let mut rows = 1;
    let frame = egui::Frame::group(ui.style())
        .stroke(stroke)
        .show(ui, |ui| {
            tab_bar(ui, app, i, &mut action);
            title_row(ui, app, i, &mut action);
            let height = (ui.available_height() - 2.0 * row_h - 8.0).max(row_h);
            rows = ((height / row_h) as usize).saturating_sub(1).max(1);
            let panel = &app.panels[i];
            let cursor = panel.cursor();
            // no coasting, no rubber-band past first/last row: an export tool, not a phone
            ui.spacing_mut().scroll.kinetic.decay_time = 0.0;
            ui.spacing_mut().scroll.kinetic.rubber_band = false;
            let mut table = TableBuilder::new(ui)
                .id_salt(i)
                .striped(true)
                .auto_shrink([false, false])
                .sense(Sense::CLICK)
                .animate_scrolling(false)
                .min_scrolled_height(height)
                .max_scroll_height(height)
                .column(Column::remainder().clip(true))
                .column(Column::exact(80.0))
                .column(Column::exact(120.0));
            if look.cursor[i] != Some(cursor) {
                look.cursor[i] = Some(cursor);
                table = table.scroll_to_row(cursor, None);
            }
            table
                .header(row_h, |h| header_cols(h, panel.sort(), &mut action))
                .body(|body| {
                    body.rows(row_h, panel.entries().len(), |row| {
                        let idx = row.index();
                        let hl = if idx != cursor {
                            None
                        } else if active {
                            Some(true)
                        } else {
                            Some(false)
                        };
                        let e = &panel.entries()[idx];
                        let color = entry_color(panel.is_selected(e), e);
                        let cols = entry_cols(app, i, e, today);
                        entry_row(row, (e.is_dir, color, cols), hl, mods, &mut action);
                    });
                });
            ui.add(Label::new(RichText::new(panel_info(app, i)).color(MUTED)).truncate());
        });
    app.page = rows;
    // a click anywhere in the panel (empty rows, title, status) activates it
    if action.is_none()
        && ui.input(|inp| inp.pointer.primary_pressed())
        && frame.response.contains_pointer()
    {
        app.focus_panel(i);
    }
    match action {
        Some(Action::Sort(key)) => app.sort_by(i, key),
        Some(Action::Click(idx, mods, double)) => app.click_entry(i, idx, mods, double),
        Some(Action::NewTab(idx)) => app.open_tab(i, idx),
        Some(Action::Tab(k)) => app.select_tab(i, k),
        Some(Action::CloseTab(k)) => app.close_tab(i, k),
        Some(Action::Key(code, mods)) => {
            app.focus_panel(i);
            if app.active == i && matches!(app.mode, Mode::Normal) {
                app.on_key(KeyEvent::new(code, mods));
            }
        }
        None => {}
    }
}

/// Path title, with the bookmarks and directory history buttons at its right end.
fn title_row(ui: &mut Ui, app: &App, i: usize, action: &mut Option<Action>) {
    const BUTTONS: [(&str, &str, KeyCode, KeyModifiers); 2] = [
        (
            "🕒",
            "Directory history (Alt+Down)",
            KeyCode::Down,
            KeyModifiers::ALT,
        ),
        (
            "★",
            "Bookmarks: go, add, edit (Ctrl+B)",
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ),
    ];
    let title = RichText::new(panel_title(app, i, false)).strong();
    let title = if i == app.active {
        title.color(ACCENT)
    } else {
        title
    };
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        for (icon, tip, code, mods) in BUTTONS {
            let b = Button::new(RichText::new(icon).font(icons())).frame(false);
            if ui.add(b.sense(Sense::CLICK)).on_hover_text(tip).clicked() {
                *action = Some(Action::Key(code, mods));
            }
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add(Label::new(title).truncate());
        });
    });
}

fn tab_bar(ui: &mut Ui, app: &App, i: usize, action: &mut Option<Action>) {
    if app.tabs[i].is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (k, name) in app.tab_names(i).into_iter().enumerate() {
            let b = Button::selectable(k == app.tab[i], format!("{} {name}", k + 1));
            let r = ui
                .add(b.sense(Sense::CLICK))
                .on_hover_text("Right click closes");
            if r.clicked() {
                *action = Some(Action::Tab(k));
            } else if r.secondary_clicked() {
                *action = Some(Action::CloseTab(k));
            }
        }
    });
}

/// Column titles; the name column has two: Name and Ext (the extension stays part of the name).
fn header_cols(mut h: egui_extras::TableRow, sort: (SortKey, bool), action: &mut Option<Action>) {
    let cols: [&[(&str, SortKey)]; 3] = [
        &[("Name", SortKey::Name), ("Ext", SortKey::Ext)],
        &[("Size", SortKey::Size)],
        &[("Date", SortKey::Date)],
    ];
    for titles in cols {
        h.col(|ui| {
            ui.horizontal(|ui| sort_titles(ui, titles, sort, action));
        });
    }
}

fn sort_titles(
    ui: &mut Ui,
    titles: &[(&str, SortKey)],
    sort: (SortKey, bool),
    action: &mut Option<Action>,
) {
    for &(label, key) in titles {
        let label = match sort {
            (k, false) if k == key => format!("{label} ⏶"),
            (k, true) if k == key => format!("{label} ⏷"),
            _ => label.to_owned(),
        };
        let l = Label::new(RichText::new(label).strong()).sense(Sense::CLICK);
        if ui.add(l).clicked() {
            *action = Some(Action::Sort(key));
        }
    }
}

fn entry_color(selected: bool, e: &tc_core::panel::Entry) -> Color32 {
    if selected {
        MARK
    } else if e.ignored {
        MUTED
    } else if e.is_symlink {
        LINK
    } else if e.is_dir {
        Color32::WHITE
    } else {
        TEXT
    }
}

/// `hl`: `Some(true)` selected cursor row, `Some(false)` hovered cursor row of the inactive panel.
fn entry_row(
    mut row: egui_extras::TableRow,
    (is_dir, color, [name, size, date]): (bool, Color32, [String; 3]),
    hl: Option<bool>,
    mods: KeyModifiers,
    action: &mut Option<Action>,
) {
    let idx = row.index();
    match hl {
        Some(true) => row.set_selected(true),
        Some(false) => row.set_hovered(true),
        None => {}
    }
    let name = RichText::new(name).color(color);
    row.col(|ui| {
        ui.add(Label::new(if is_dir { name.strong() } else { name }).truncate());
    });
    row.col(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(size).color(color));
        });
    });
    row.col(|ui| {
        ui.label(RichText::new(date).color(MUTED));
    });
    let r = row.response();
    if r.double_clicked() {
        *action = Some(Action::Click(idx, KeyModifiers::NONE, true));
    } else if r.clicked() {
        *action = Some(Action::Click(idx, mods, false));
    } else if r.middle_clicked() {
        *action = Some(Action::NewTab(idx));
    } else if r.secondary_clicked() {
        // the menu acts on the clicked entry (or the selection)
        *action = Some(Action::Click(idx, KeyModifiers::NONE, false));
    }
    r.context_menu(|ui| entry_menu(ui, action));
}

/// Right-click menu: each item runs its key.
fn entry_menu(ui: &mut Ui, action: &mut Option<Action>) {
    for (label, code, mods) in ENTRY_MENU {
        if ui.button(RichText::new(label).monospace()).clicked() {
            *action = Some(Action::Key(code, mods));
            ui.close();
        }
    }
}

/// Shift / Ctrl (Cmd on macOS) held during a click.
fn click_mods(m: egui::Modifiers) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if m.shift {
        out |= KeyModifiers::SHIFT;
    }
    if m.command {
        out |= KeyModifiers::CONTROL;
    }
    out
}
