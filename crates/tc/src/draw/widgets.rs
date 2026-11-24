//! Reusable egui pieces: text fields, list rows, table cells and click tables.
//! Check here before writing a new helper; shared ones go here.

use super::{MUTED, mono};
use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{
    self, Align, Button, Context, Event, Id, Label, Layout, RichText, Sense, TextEdit, TextStyle,
    Ui,
};
use egui_extras::{Column, TableBuilder, TableRow};
use tc::app::App;
use tc_core::cmdline::LineEdit;
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

pub(super) fn caret_id() -> Id {
    Id::unique("tc-text-box-caret")
}

/// Caret a click put into the focused text box, set in the app's field.
pub(super) fn apply_caret(ctx: &Context, app: &mut App) {
    let Some(n) = ctx.data_mut(|d| d.remove_temp::<usize>(caret_id())) else {
        return;
    };
    app.set_caret(n);
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

/// Table cell content, vertically centered so every cell of a row shares one baseline;
/// `right` for number columns. Use for every header and body cell next to a right-aligned one.
pub(super) fn cell<R>(ui: &mut Ui, right: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let layout = if right {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    ui.with_layout(layout, add).inner
}

/// Row height and rows per page of a table below a title (page-wise: only the page holding the cursor is built).
pub(super) fn page_rows(ui: &Ui) -> (f32, usize) {
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;
    // egui_extras adds the item spacing under every row
    let step = row_h + ui.spacing().item_spacing.y;
    let h = ((ui.available_height() / step) as usize)
        .saturating_sub(1)
        .max(1);
    (row_h, h)
}

pub(super) fn right(ui: &mut Ui, text: impl Into<String>) {
    cell(ui, true, |ui| ui.label(text.into()));
}

pub(super) fn left(ui: &mut Ui, text: impl Into<String>) {
    ui.add(Label::new(text.into()).truncate());
}

/// Header cells; the columns in `right_cols` are right-aligned.
pub(super) fn head_row(mut head: TableRow, titles: &[&str], right_cols: &[usize]) {
    for (k, title) in titles.iter().enumerate() {
        head.col(|ui| {
            let t = RichText::new(*title).strong().color(MUTED);
            cell(ui, right_cols.contains(&k), |ui| ui.label(t));
        });
    }
}

/// Click table of `shown` rows from `start`; `fill` draws one row's cells. `Some((index, double))` on click.
pub(super) fn click_table(
    ui: &mut Ui,
    cols: &[Column],
    titles: &[&str],
    right_cols: &[usize],
    (row_h, start, shown, sel): (f32, usize, usize, usize),
    mut fill: impl FnMut(&mut TableRow, usize),
) -> Option<(usize, bool)> {
    let mut clicked = None;
    let mut table = TableBuilder::new(ui)
        .striped(false)
        .vscroll(false)
        .sense(Sense::CLICK);
    for c in cols {
        table = table.column(*c);
    }
    table
        .header(row_h, |head| head_row(head, titles, right_cols))
        .body(|body| {
            body.rows(row_h, shown, |mut row| {
                let i = start + row.index();
                row.set_selected(i == sel);
                fill(&mut row, i);
                let r = row.response();
                if r.clicked() || r.double_clicked() {
                    clicked = Some((i, r.double_clicked()));
                }
            });
        });
    clicked
}
