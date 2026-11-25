//! Reusable egui pieces: text fields, list rows, table cells and click tables.
//! Check here before writing a new helper; shared ones go here.

use super::{MUTED, mono};
use crate::keys;
use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{
    self, Align, Button, Context, Event, Id, Label, Layout, RichText, Sense, TextEdit, TextStyle,
    Ui,
};
use egui_extras::{Column, TableBuilder, TableRow};
use tc_core::cmdline::LineEdit;
use tc_gui::app::App;
/// Single-line text of `l` with a caret at its cursor.
pub(super) fn line_edit(ui: &mut Ui, l: &LineEdit) {
    text_box(ui, l, true);
}

/// `l` as a native egui text field. A focused field edits itself (typing, IME, selection,
/// clipboard, caret and delete keys); the app gets text and caret back through `apply_field`
/// and runs its per-key side effects. Enter, Esc, Tab, arrows up/down and F-keys stay with the
/// app: they are hidden from the field. The field takes keys one frame after it appeared
/// (`field_active`), so the key that opened a prompt is not typed into it.
/// `focused`: the field has egui focus (caret, IME window); else a click on it reports `clicked`.
pub(super) fn text_box(ui: &mut Ui, l: &LineEdit, focused: bool) -> egui::Response {
    let ctx = ui.ctx().clone();
    let id = ui.next_auto_id();
    let own = focused && !l.secret;
    let active = own && field_active(&ctx);
    if own {
        let now = ctx.cumulative_pass_nr();
        let flag = field_flag_id(&ctx);
        ctx.data_mut(|d| d.insert_temp(flag, now));
    }
    let caret = l.text[..l.cur].chars().count();
    // the app changed the text (history, Ctrl+U): its caret wins; else egui keeps its selection
    let seen_id = id.with("seen");
    let seen: Option<String> = ctx.data(|d| d.get_temp(seen_id));
    if seen.as_deref() != Some(l.text.as_str()) {
        let mut state = TextEdit::load_state(&ctx, id).unwrap_or_default();
        let one = CCursorRange::one(CCursor::new(caret));
        state.cursor.set_char_range(Some(one));
        TextEdit::store_state(&ctx, id, state);
    }
    if focused {
        ctx.memory_mut(|m| m.request_focus(id));
    }
    let mut text = l.text.clone();
    let edit = TextEdit::singleline(&mut text)
        .id(id)
        .font(mono())
        .desired_width(f32::INFINITY);
    let out = filter_events(ui, active, |ui| edit.show(ui));
    let r = out.response.response;
    ctx.data_mut(|d| d.insert_temp(seen_id, text.clone()));
    if !focused && r.has_focus() {
        r.surrender_focus();
    }
    // hand typing and mouse caret to the app (see `apply_field`)
    if focused
        && let Some(c) = out.state.cursor.char_range()
        && (text != l.text || c.primary.index.0 != caret)
    {
        let edited = (text, c.primary.index.0);
        let slot = field_edit_id(&ctx);
        ctx.data_mut(|d| d.insert_temp(slot, edited));
    }
    r
}

/// Runs `f` with this frame's keyboard events hidden (they went to `App::on_key` already);
/// `keep`: the ones a text field handles itself stay.
fn filter_events<R>(ui: &mut Ui, keep: bool, f: impl FnOnce(&mut Ui) -> R) -> R {
    let (all, alt) = ui
        .ctx()
        .input(|i| (i.events.clone(), i.modifiers.alt && !i.modifiers.ctrl));
    let rest = (all.iter())
        .filter(|e| !is_key_input(e) || (keep && keys::field_event(e, alt)))
        .cloned()
        .collect();
    ui.ctx().input_mut(|i| i.events = rest);
    let out = f(ui);
    ui.ctx().input_mut(|i| i.events = all);
    out
}

fn is_key_input(e: &Event) -> bool {
    matches!(
        e,
        Event::Text(_)
            | Event::Key { .. }
            | Event::Paste(_)
            | Event::Cut
            | Event::Copy
            | Event::Ime(_)
    )
}

fn field_edit_id(ctx: &Context) -> Id {
    Id::unique(("tc-field-edit", ctx.viewport_id()))
}

fn field_flag_id(ctx: &Context) -> Id {
    Id::unique(("tc-field-pass", ctx.viewport_id()))
}

/// A text field handled the keys last frame: the app skips what the field edits itself.
pub(crate) fn field_active(ctx: &Context) -> bool {
    let flag = field_flag_id(ctx);
    let seen: Option<u64> = ctx.data(|d| d.get_temp(flag));
    seen.is_some_and(|n| n + 1 == ctx.cumulative_pass_nr())
}

/// Text and caret (chars) a field edited this frame, taken.
pub(super) fn take_field_edit(ctx: &Context) -> Option<(String, usize)> {
    let slot = field_edit_id(ctx);
    ctx.data_mut(|d| d.remove_temp(slot))
}

/// Edit of the focused field goes into the app's prompt.
pub(super) fn apply_field(ctx: &Context, app: &mut App) {
    if let Some((text, caret)) = take_field_edit(ctx) {
        app.set_line(text, caret);
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
