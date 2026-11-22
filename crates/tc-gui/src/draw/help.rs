use super::{ACCENT, MUTED, SELECT_BG, TEXT, modal};
use eframe::egui::{self, Color32, Context, FontId, RichText, ScrollArea, Sense};
use tc_app::app::App;
use tc_app::fmt::{self};
/// Help as a table: section titles, then key | info rows (continuation lines join the info).
pub(super) fn show_help(ctx: &Context, app: &mut App) {
    let title = app.help_text().0;
    let text = app.help_entries();
    let row_h = ctx.fonts_mut(|f| f.row_height(&FontId::proportional(14.0)));
    let max_h = ctx.content_rect().height() * 0.7;
    let rows = (max_h / row_h) as usize;
    app.help_rows = rows;
    app.help_scroll = app.help_scroll.min(text.len().saturating_sub(rows));
    let offset = app.help_scroll as f32 * row_h;
    let mut clicked = None;
    modal("help").show(ctx, |ui| {
        ui.set_width(640.0);
        ui.heading(title);
        ui.label(RichText::new(format!("Find: {}_", app.help_filter)).color(ACCENT));
        ui.separator();
        ScrollArea::vertical()
            .max_height(max_h)
            .vertical_scroll_offset(offset)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, (line, action)) in text.iter().enumerate() {
                    let selected = action.is_some() && i == app.help_sel;
                    let color = match (selected, action.is_some()) {
                        (true, _) => Color32::WHITE,
                        (_, true) => ACCENT,
                        _ => TEXT,
                    };
                    let r = ui.horizontal(|ui| help_cells(ui, line, color, selected, row_h));
                    let r = r.response.interact(if action.is_some() {
                        Sense::CLICK
                    } else {
                        Sense::hover()
                    });
                    let r = match action {
                        Some(_) => r.on_hover_cursor(egui::CursorIcon::PointingHand),
                        None => r,
                    };
                    if r.clicked() {
                        clicked = Some(i);
                    }
                }
            });
        ui.label(
            RichText::new("Up/Down select  Enter or click runs  type to filter  Esc closes")
                .color(MUTED),
        );
    });
    if let Some(i) = clicked {
        app.help_click(i);
    }
}

/// One help row: a section title, or key | info.
fn help_cells(ui: &mut egui::Ui, line: &str, color: Color32, selected: bool, row_h: f32) {
    const KEY_W: f32 = 150.0;
    ui.set_min_height(row_h);
    if selected {
        ui.painter()
            .rect_filled(ui.max_rect().expand(1.0), 2.0, SELECT_BG);
    }
    let (key, info) = match fmt::help_line(line) {
        fmt::HelpLine::Section(t) => {
            ui.label(RichText::new(t).strong().color(ACCENT));
            return;
        }
        fmt::HelpLine::Entry(k, info) => (k, info),
        fmt::HelpLine::More(info) => ("", info),
        fmt::HelpLine::Text(t) => {
            ui.label(RichText::new(t).color(MUTED));
            return;
        }
    };
    let left = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(egui::vec2(KEY_W, row_h), left, |ui| {
        ui.set_min_width(KEY_W);
        ui.label(RichText::new(key).strong().color(color));
    });
    ui.label(RichText::new(info).color(color));
}
