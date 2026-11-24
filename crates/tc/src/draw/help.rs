use super::widgets::{caret_id, text_box};
use super::{ACCENT, MUTED, TEXT};
use crate::keys;
use eframe::egui::{
    self, Context, FontId, RichText, Sense, Ui, ViewportBuilder, ViewportClass, ViewportId,
};
use egui_extras::{Column, TableBuilder, TableRow};
use tc::app::App;
use tc::fmt::{self};
use tc::{KeyCode, KeyModifiers};
use tc_core::cmdline::LineEdit;

/// Help in its own OS window, movable beside tc; tc stays usable. Keys typed there go to the help.
pub(super) fn show_help(ctx: &Context, app: &mut App) {
    let title = format!("tc - {}", app.help_text().0);
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([760.0, 720.0]);
    ctx.show_viewport_immediate(ViewportId::from_hash_of("help"), builder, |ui, class| {
        // embedded (backend without native windows, tests): the input is tc's, already handled
        if class != ViewportClass::EmbeddedWindow {
            help_keys(ui, app);
        }
        if ui.input(|i| i.viewport().close_requested()) {
            app.help_close();
        }
        egui::CentralPanel::default().show(ui, |ui| help_body(ui, app));
    });
}

/// Keys of the help window, through the app like the main window's.
fn help_keys(ui: &Ui, app: &mut App) {
    let (events, alt) = ui.input(|i| (i.events.clone(), i.modifiers.alt && !i.modifiers.ctrl));
    for ev in &events {
        if let Some(keys::Input::Key(k)) = keys::translate(ev, alt) {
            app.help_key(k);
        }
    }
}

/// Help as a table: section titles, then key | info rows (continuation lines join the info).
fn help_body(ui: &mut Ui, app: &mut App) {
    const KEY_W: f32 = 150.0;
    let text = app.help_entries();
    let row_h = ui.fonts_mut(|f| f.row_height(&FontId::proportional(14.0))) + 2.0;
    // force egui's offset only when a key moved the row; else the wheel's smooth steps are undone
    let last_id = egui::Id::unique("help-scroll-row");
    ui.horizontal(|ui| {
        ui.label(RichText::new("Find").strong());
        text_box(ui, &LineEdit::from(app.help_filter.clone()), true);
    });
    // the filter has no caret of its own: a click there must not move the main window's caret
    ui.data_mut(|d| d.remove_temp::<usize>(caret_id()));
    ui.separator();
    let max_h = ui.available_height();
    // a table row takes its height plus the item spacing; offsets count in that pitch
    let pitch = row_h + ui.spacing().item_spacing.y;
    // End sets a huge row: egui clamps the offset to the real bottom, read back below
    let forced = ui.data(|d| d.get_temp::<usize>(last_id)) != Some(app.help_scroll);
    let mut table = TableBuilder::new(ui)
        .id_salt("help")
        .sense(Sense::CLICK)
        .auto_shrink([false, true])
        .animate_scrolling(false)
        .max_scroll_height(max_h)
        .column(Column::exact(KEY_W).clip(false))
        .column(Column::remainder().clip(true));
    if forced {
        table = table.vertical_scroll_offset(app.help_scroll.min(text.len()) as f32 * pitch);
    }
    let mut clicked = None;
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
    // wheel and scrollbar drag move egui's offset; keep the app's row in sync (a forced frame
    // still reports the old offset, the next one the clamped new one)
    if !forced {
        app.help_scroll = (out.state.clamped_offset().y / pitch).round() as usize;
    }
    // rows really visible (the table can be shorter than the space reserved): PgUp / PgDn
    app.help_rows = ((out.inner_rect.height() / pitch) as usize).max(1);
    ui.data_mut(|d| d.insert_temp(last_id, app.help_scroll));
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
    // prose runs across the info column too: painted on the layer, not clipped to the cell
    let prose = matches!(fmt::help_line(line), fmt::HelpLine::Text(_));
    row.col(|ui| {
        // painted, not a label: a long title runs over the info column without widening the
        // popup (a wider popup is pushed off the window's left edge while the line is visible)
        let painter = if prose {
            ui.ctx().layer_painter(ui.layer_id())
        } else {
            ui.painter().clone()
        };
        let g = egui::WidgetText::from(key).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Body,
        );
        let size = egui::vec2(ui.available_width(), g.size().y);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        painter.galley(rect.left_top(), g, TEXT);
    });
    row.col(|ui| {
        ui.label(RichText::new(info).color(color));
    });
}
