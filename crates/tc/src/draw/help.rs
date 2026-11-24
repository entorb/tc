use super::chrome::text_box;
use super::{ACCENT, MUTED, TEXT, modal};
use eframe::egui::{self, Context, FontId, RichText, Sense};
use egui_extras::{Column, TableBuilder, TableRow};
use tc::app::App;
use tc::fmt::{self};
use tc::{KeyCode, KeyModifiers};
use tc_core::cmdline::LineEdit;

/// Help as a table: section titles, then key | info rows (continuation lines join the info).
pub(super) fn show_help(ctx: &Context, app: &mut App) {
    const KEY_W: f32 = 150.0;
    let title = app.help_text().0;
    let text = app.help_entries();
    let row_h = ctx.fonts_mut(|f| f.row_height(&FontId::proportional(14.0))) + 2.0;
    let max_h = ctx.content_rect().height() * 0.7;
    let rows = (max_h / row_h) as usize;
    app.help_rows = rows;
    app.help_scroll = app.help_scroll.min(text.len().saturating_sub(rows));
    // force egui's offset only when a key moved the row; else the wheel's smooth steps are undone
    let last_id = egui::Id::unique("help-scroll-row");
    let forced = ctx.data(|d| d.get_temp::<usize>(last_id)) != Some(app.help_scroll);
    let mut clicked = None;
    let mut scrolled = None;
    modal("help").show(ctx, |ui| {
        ui.set_width(720.0);
        ui.heading(title);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Find").strong());
            text_box(ui, &LineEdit::from(app.help_filter.clone()), true);
        });
        ui.separator();
        let mut table = TableBuilder::new(ui)
            .id_salt("help")
            .sense(Sense::CLICK)
            .auto_shrink([false, true])
            .animate_scrolling(false)
            .max_scroll_height(max_h)
            .column(Column::exact(KEY_W).clip(false))
            .column(Column::remainder().clip(true));
        if forced {
            table = table.vertical_scroll_offset(app.help_scroll as f32 * row_h);
        }
        let out = table.body(|body| {
            body.rows(row_h, text.len(), |mut row| {
                let i = row.index();
                let (line, action) = text[i];
                row.set_selected(i == app.help_sel && is_entry(line));
                help_cells(&mut row, line, action);
                let r = row.response();
                if action.is_some() {
                    r.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
                }
                if r.clicked() {
                    clicked = Some(i);
                }
            });
        });
        scrolled = Some(out.state.clamped_offset().y);
    });
    // wheel and scrollbar drag move egui's offset; keep the app's row in sync
    if let Some(y) = scrolled {
        app.help_scroll = (y / row_h) as usize;
        ctx.data_mut(|d| d.insert_temp(last_id, app.help_scroll));
    }
    if let Some(i) = clicked {
        app.help_click(i);
    }
}

fn is_entry(line: &str) -> bool {
    matches!(fmt::help_line(line), fmt::HelpLine::Entry(..))
}

/// One help row: a section title or prose in the first column (drawn over both), or key | info.
fn help_cells(row: &mut TableRow, line: &str, action: Option<(KeyCode, KeyModifiers)>) {
    let color = if action.is_some() { ACCENT } else { TEXT };
    let (key, info) = match fmt::help_line(line) {
        fmt::HelpLine::Section(t) => (RichText::new(t).strong().color(ACCENT), ""),
        fmt::HelpLine::Entry(k, info) => (RichText::new(k).strong().color(color), info),
        fmt::HelpLine::More(info) => (RichText::new(""), info),
        fmt::HelpLine::Text(t) => (RichText::new(t).color(MUTED), ""),
    };
    row.col(|ui| {
        ui.add(egui::Label::new(key).wrap_mode(egui::TextWrapMode::Extend));
    });
    row.col(|ui| {
        ui.label(RichText::new(info).color(color));
    });
}
