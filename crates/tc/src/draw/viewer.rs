use std::path::Path;

use super::{ACCENT, MARK, MUTED, SELECT_BG, TEXT, mono};
use eframe::egui::{
    Color32, Response, RichText, Sense, Stroke, TextFormat, TextStyle, Ui, Vec2, text::LayoutJob,
    vec2,
};
use tc::app::{App, Mode};
use tc::fmt::{self, Mark, editor_info};
use tc_core::syntax::{self, Kind};
/// Monospace cell size.
pub(super) fn cell(ui: &Ui) -> Vec2 {
    ui.painter().layout_no_wrap("M".into(), mono(), TEXT).size()
}

fn text_format(m: Mark) -> TextFormat {
    let mut f = TextFormat::simple(mono(), TEXT);
    match m.kind {
        Some(Kind::Keyword) => f.color = Color32::from_rgb(198, 120, 221),
        Some(Kind::Str) => f.color = Color32::from_rgb(152, 195, 121),
        Some(Kind::Number) => f.color = Color32::from_rgb(209, 154, 102),
        Some(Kind::Comment) => {
            f.color = MUTED;
            f.italics = true;
        }
        Some(Kind::Bold) => f.color = Color32::WHITE,
        Some(Kind::Italic) => f.italics = true,
        None => {}
    }
    if m.hit {
        f.color = Color32::BLACK;
        f.background = MARK;
    }
    if m.selected {
        f.background = SELECT_BG;
    }
    if m.bracket {
        f.underline = Stroke::new(1.0, ACCENT);
    }
    f
}

fn push_runs(job: &mut LayoutJob, runs: Vec<(String, Mark)>) {
    for (t, m) in runs {
        job.append(&t, 0.0, text_format(m));
    }
    job.append("\n", 0.0, TextFormat::simple(mono(), TEXT));
}

/// Text grid filling the rest of `ui` minus `reserve` px: (columns, rows, cell size).
pub(super) fn grid(ui: &Ui, reserve: f32) -> (usize, usize, Vec2) {
    let c = cell(ui);
    let avail = ui.available_size() - vec2(0.0, reserve);
    (
        (avail.x / c.x).max(1.0) as usize,
        (avail.y / c.y).max(1.0) as usize,
        c,
    )
}

/// Paints `job` at the top left of the rest of `ui`; the response senses clicks and drags.
fn paint_text(ui: &mut Ui, job: LayoutJob, reserve: f32) -> Response {
    let size = ui.available_size() - vec2(0.0, reserve);
    let (rect, r) = ui.allocate_exact_size(size, Sense::CLICK | Sense::DRAG);
    let galley = ui.painter().layout_job(job);
    ui.painter().rect_filled(rect, 4.0, Color32::from_gray(18));
    ui.painter().galley(rect.min, galley, TEXT);
    r
}

pub(super) fn draw_view(ui: &mut Ui, app: &mut App) {
    let status_h = ui.text_style_height(&TextStyle::Body) + 8.0;
    let Mode::View { title, .. } = &app.mode else {
        return;
    };
    ui.label(RichText::new(title).strong().color(ACCENT));
    let (cols, rows, _) = grid(ui, status_h);
    app.page = rows;
    let (search, numbers) = (app.search.clone(), app.view_numbers);
    let Mode::View { title, view } = &mut app.mode else {
        return;
    };
    // numbers only while the top line is known, see `Viewer::top_line`
    let top = view.top_line().filter(|_| numbers && !view.hex());
    let gutter = if top.is_some() { 7 } else { 0 };
    let lang = syntax::lang(Path::new(title.as_str())).filter(|_| !view.hex());
    let mut fence = false;
    let mut job = LayoutJob::default();
    match view.rows(rows, cols.saturating_sub(gutter)) {
        Ok(lines) => {
            let mut n = top.unwrap_or(0);
            for (first, text) in lines {
                if top.is_some() {
                    let num = if first {
                        format!("{:>6} ", n + 1)
                    } else {
                        " ".repeat(7)
                    };
                    job.append(&num, 0.0, TextFormat::simple(mono(), MUTED));
                }
                n += u64::from(first);
                let runs = fmt::highlight(&text, lang, &mut fence, search.as_ref(), None, (0, &[]));
                push_runs(&mut job, runs);
            }
        }
        Err(e) => job.append(&e.to_string(), 0.0, TextFormat::simple(mono(), MARK)),
    }
    paint_text(ui, job, status_h);
    ui.label(RichText::new(&app.note).color(MUTED));
}

pub(super) fn draw_editor(ui: &mut Ui, app: &mut App) {
    let status_h = ui.text_style_height(&TextStyle::Body) + 8.0;
    let search = app.search.clone();
    let note = app.note.clone();
    let Some(editor) = app.mode.editor_mut() else {
        return;
    };
    ui.label(
        RichText::new(editor.path().display().to_string())
            .strong()
            .color(ACCENT),
    );
    let (cols, rows, c) = grid(ui, status_h);
    let gutter = if editor.numbers {
        editor.line_count().to_string().len() + 1
    } else {
        0
    };
    let width = cols.saturating_sub(gutter);
    editor.scroll_to_cursor(rows, width);
    let brackets = editor.brackets();
    let lang = syntax::lang(editor.path());
    let mut fence = lang.is_some_and(|l| l.markdown) && editor.in_fence(editor.top);
    let mut job = LayoutJob::default();
    for (l, start, row) in editor.rows(rows, width) {
        if editor.numbers {
            // continuation rows of a wrapped line get a blank gutter
            let num = if start == 0 || !editor.wrap {
                format!("{:>1$} ", l + 1, gutter - 1)
            } else {
                " ".repeat(gutter)
            };
            job.append(&num, 0.0, TextFormat::simple(mono(), MUTED));
        }
        let marks: Vec<usize> = brackets
            .iter()
            .flatten()
            .filter(|p| p.0 == l)
            .filter_map(|p| p.1.checked_sub(start))
            .collect();
        let sel = editor.selection_cols(l);
        let runs = fmt::highlight(
            &row,
            lang,
            &mut fence,
            search.as_ref(),
            sel,
            (start, &marks),
        );
        push_runs(&mut job, runs);
    }
    let r = paint_text(ui, job, status_h);
    let (dy, dx) = editor.cursor_cell(width);
    if dx < width && dy < rows {
        let at = r.rect.min + vec2((gutter + dx) as f32 * c.x, dy as f32 * c.y);
        ui.painter()
            .vline(at.x, at.y..=at.y + c.y, Stroke::new(2.0, ACCENT));
    }
    ui.label(RichText::new(editor_info(editor, &note)).color(MUTED));
    app.page = rows;
    if let Some((row, col, extend)) = editor_mouse(ui, &r, c)
        && row < rows
        && let Some(col) = col.checked_sub(gutter).filter(|&x| x < width)
    {
        app.editor_click(row, col, width, extend);
    }
}

/// Press or drag on the editor text: cell row and column (gutter included), and whether it extends the selection.
fn editor_mouse(ui: &Ui, r: &Response, c: Vec2) -> Option<(usize, usize, bool)> {
    let pressed = r.hovered() && ui.input(|i| i.pointer.primary_pressed());
    if !pressed && !r.dragged() {
        return None;
    }
    let pos = r.interact_pointer_pos().or(r.hover_pos())? - r.rect.min;
    let extend = !pressed || ui.input(|i| i.modifiers.shift);
    Some((
        (pos.y / c.y).max(0.0) as usize,
        (pos.x / c.x).max(0.0) as usize,
        extend,
    ))
}
