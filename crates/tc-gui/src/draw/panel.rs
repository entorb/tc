use super::{ACCENT, LINK, Look, MARK, MUTED, TEXT};
use eframe::egui::{
    self, Align, Button, Color32, Label, Layout, RichText, Sense, Stroke, TextStyle, Ui,
};
use egui_extras::{Column, TableBuilder};
use tc_app::app::App;
use tc_app::fmt::{entry_cols, panel_info, panel_title};
use tc_core::panel::SortKey;
enum Action {
    Sort(SortKey),
    Click(usize, bool, bool),
    Tab(usize),
    CloseTab(usize),
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
    let shift = ui.input(|inp| inp.modifiers.shift);
    let mut action = None;
    let stroke = if active {
        Stroke::new(1.0, ACCENT)
    } else {
        Stroke::new(1.0, MUTED)
    };
    let mut rows = 1;
    egui::Frame::group(ui.style())
        .stroke(stroke)
        .show(ui, |ui| {
            if !app.tabs[i].is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (k, name) in app.tab_names(i).into_iter().enumerate() {
                        let b = Button::selectable(k == app.tab[i], format!("{} {name}", k + 1));
                        let r = ui
                            .add(b.sense(Sense::CLICK))
                            .on_hover_text("Right click closes");
                        if r.clicked() {
                            action = Some(Action::Tab(k));
                        } else if r.secondary_clicked() {
                            action = Some(Action::CloseTab(k));
                        }
                    }
                });
            }
            let title = RichText::new(panel_title(app, i)).strong();
            ui.add(Label::new(if active { title.color(ACCENT) } else { title }).truncate());
            let height = (ui.available_height() - 2.0 * row_h - 8.0).max(row_h);
            rows = ((height / row_h) as usize).saturating_sub(1).max(1);
            let panel = &app.panels[i];
            let cursor = panel.cursor();
            let mut table = TableBuilder::new(ui)
                .id_salt(i)
                .striped(true)
                .sense(Sense::CLICK)
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
                .header(row_h, |mut h| {
                    for (label, key) in [
                        ("Name", SortKey::Name),
                        ("Size", SortKey::Size),
                        ("Date", SortKey::Date),
                    ] {
                        let label = match panel.sort() {
                            (k, false) if k == key => format!("{label} ⏶"),
                            (k, true) if k == key => format!("{label} ⏷"),
                            _ => label.to_owned(),
                        };
                        h.col(|ui| {
                            let l = Label::new(RichText::new(label).strong()).sense(Sense::CLICK);
                            if ui.add(l).clicked() {
                                action = Some(Action::Sort(key));
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, panel.entries().len(), |mut row| {
                        let idx = row.index();
                        let e = &panel.entries()[idx];
                        if idx == cursor {
                            if active {
                                row.set_selected(true);
                            } else {
                                row.set_hovered(true);
                            }
                        }
                        let color = if panel.is_selected(e) {
                            MARK
                        } else if e.is_symlink {
                            LINK
                        } else if e.is_dir {
                            Color32::WHITE
                        } else {
                            TEXT
                        };
                        let [name, size, date] = entry_cols(app, i, e, today);
                        let name = RichText::new(name).color(color);
                        row.col(|ui| {
                            ui.add(
                                Label::new(if e.is_dir { name.strong() } else { name }).truncate(),
                            );
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
                            action = Some(Action::Click(idx, false, true));
                        } else if r.clicked() {
                            action = Some(Action::Click(idx, shift, false));
                        }
                    });
                });
            ui.add(Label::new(RichText::new(panel_info(app, i)).color(MUTED)).truncate());
        });
    app.page = rows;
    match action {
        Some(Action::Sort(key)) => app.sort_by(i, key),
        Some(Action::Click(idx, shift, double)) => app.click_entry(i, idx, shift, double),
        Some(Action::Tab(k)) => app.select_tab(i, k),
        Some(Action::CloseTab(k)) => app.close_tab(i, k),
        None => {}
    }
}
