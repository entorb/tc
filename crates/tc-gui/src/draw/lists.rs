use super::chrome::{line_edit, list_button};
use super::viewer::cell;
use super::{ACCENT, MUTED, TEXT};
use eframe::egui::{
    Align, Align2, Color32, FontId, Label, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    TextStyle, Ui, vec2,
};
use egui_extras::{Column, TableBuilder};
use tc_app::app::{App, Mode, Stats};
use tc_app::fmt::{
    self, found_title, stats_footer, stats_row, stats_title, sync_footer, sync_row, sync_title,
};
use tc_app::fmt::{procs_footer, procs_header, procs_prompt};
use tc_app::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::cmdline::LineEdit;
use tc_core::edit::human;
use tc_core::procs::ProcSort;
use tc_core::stats::treemap::Tile;
/// One clickable list row; `Some(double)` when clicked.
fn list_row(ui: &mut Ui, text: String, selected: bool) -> Option<bool> {
    let r = list_button(ui, text, selected, true);
    if r.double_clicked() {
        Some(true)
    } else {
        r.clicked().then_some(false)
    }
}

/// Rows of a page-wise list (like the terminal): only the page holding `sel` is built.
fn page_rows(ui: &Ui) -> usize {
    // list rows are labels, spaced apart unlike the painted text grid
    let row = cell(ui).y + ui.spacing().item_spacing.y;
    let avail = ui.available_height() + ui.spacing().item_spacing.y;
    ((avail / row) as usize).max(1)
}

pub(super) fn draw_found(ui: &mut Ui, app: &mut App) {
    let Mode::Found(f) = &mut app.mode else {
        return;
    };
    ui.label(RichText::new(found_title(f)).strong().color(ACCENT));
    let h = page_rows(ui);
    let start = f.sel / h * h;
    let mut clicked = None;
    for (k, e) in f.shown().enumerate().skip(start).take(h) {
        if let Some(double) = list_row(ui, fmt::found_row(e), k == f.sel) {
            clicked = Some((k, double));
        }
    }
    app.page = h;
    if let Some((k, double)) = clicked {
        f.sel = k;
        if double {
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
    }
}

/// Synchronize directories: compared rows, click selects.
pub(super) fn draw_sync(ui: &mut Ui, app: &mut App) {
    let Mode::Sync(s) = &mut app.mode else {
        return;
    };
    let today = jiff::ZonedDateTime::now().date();
    ui.label(RichText::new(sync_title(s)).strong().color(ACCENT));
    let h = page_rows(ui);
    let start = s.sel / h * h;
    let mut clicked = None;
    for (k, it) in s.shown().enumerate().skip(start).take(h) {
        if list_row(ui, sync_row(it, today), k == s.sel).is_some() {
            clicked = Some(k);
        }
    }
    if let Some(k) = clicked {
        s.sel = k;
    }
    app.page = h;
}

/// Status line of File Stats (totals or its `:` prompt) and processes (footer or prompt).
pub(super) fn status_line(ui: &mut Ui, app: &App) {
    match (&app.mode, &app.stats) {
        (Mode::Procs(p), _) => match procs_prompt(p) {
            Some((label, l)) => prompt(ui, label, l),
            None => {
                ui.label(RichText::new(procs_footer(p)).color(MUTED));
            }
        },
        (Mode::Stats(st), _) => {
            ui.label(RichText::new(stats_footer(st)).color(MUTED));
        }
        (Mode::Sync(s), _) => {
            ui.label(RichText::new(sync_footer(s)).color(MUTED));
        }
        (Mode::Cmd(l), Some(st)) => prompt(ui, &format!("{}> ", st.path().display()), l),
        _ => {}
    }
}

/// `label` and the line being typed.
pub(super) fn prompt(ui: &mut Ui, label: &str, l: &LineEdit) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong());
        line_edit(ui, l);
    });
}

/// Window width from which File Stats shows the treemap beside the list.
const MAP_MIN_WIDTH: f32 = 800.0;

/// Treemap of the browsed directory; `Some((row, double))` when a tile was clicked.
fn stats_chart(ui: &mut Ui, st: &mut Stats) -> Option<(usize, bool)> {
    let size = vec2(ui.available_width(), ui.available_height().max(50.0));
    let (resp, painter) = ui.allocate_painter(size, Sense::click());
    let rect = resp.rect;
    st.treemap(rect.width(), rect.height(), u8::MAX, 4.0, 2.0);
    let sel = st.selected_index();
    let at = |t: &Tile| Rect::from_min_size(rect.min + vec2(t.x, t.y), vec2(t.w, t.h));
    let outline = Stroke::new(1.0, Color32::from_black_alpha(160));
    for t in st.tiles() {
        let (r, g, b) = fmt::kind_rgb(t.kind, t.depth % 2);
        painter.rect_filled(at(t), 0.0, Color32::from_rgb(r, g, b));
        painter.rect_stroke(at(t), 0.0, outline, StrokeKind::Inside);
    }
    let dir = st.current();
    for t in st.tiles().iter().filter(|t| t.depth == 0) {
        let r = at(t);
        if r.width() >= 40.0 && r.height() >= 14.0 {
            let name = dir.children[t.top].name.to_string_lossy();
            painter.with_clip_rect(r).text(
                r.min + vec2(3.0, 1.0),
                Align2::LEFT_TOP,
                name,
                FontId::monospace(11.0),
                Color32::BLACK,
            );
        }
    }
    if let Some(t) = st
        .tiles()
        .iter()
        .find(|t| t.depth == 0 && Some(t.top) == sel)
    {
        painter.rect_stroke(at(t), 0.0, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
    }
    let row_at = |p: Pos2| st.map_row(p.x - rect.min.x, p.y - rect.min.y);
    let hit = resp
        .interact_pointer_pos()
        .and_then(row_at)
        .filter(|_| resp.clicked());
    let tip = resp
        .hover_pos()
        .and_then(|p| st.map_node(p.x - rect.min.x, p.y - rect.min.y))
        .map(|(rel, n)| fmt::stats_tip(&rel, n));
    if let Some(tip) = tip {
        resp.clone().on_hover_text_at_pointer(tip);
    }
    hit.map(|r| (r, resp.double_clicked()))
}

pub(super) fn draw_stats(ui: &mut Ui, app: &mut App) {
    let (st, cmd): (&mut Stats, Option<&LineEdit>) = match (&mut app.mode, &mut app.stats) {
        (Mode::Stats(st), _) => (st, None),
        (Mode::Cmd(l), Some(st)) => (st, Some(&*l)),
        _ => return,
    };
    ui.label(RichText::new(stats_title(st)).strong().color(ACCENT));
    let h = page_rows(ui);
    let start = st.sel / h * h;
    let rows: Vec<String> = st
        .shown()
        .skip(start)
        .take(h)
        .map(|n| stats_row(st, n))
        .collect();
    let sel = st.sel;
    let list = |ui: &mut Ui| {
        let mut clicked = None;
        for (k, row) in rows.into_iter().enumerate() {
            if let Some(double) = list_row(ui, row, start + k == sel) {
                clicked = Some((start + k, double));
            }
        }
        clicked
    };
    let width = ui.available_width();
    let clicked = if width >= MAP_MIN_WIDTH {
        let rows_w = width * 0.55;
        ui.horizontal_top(|ui| {
            let left = ui
                .vertical(|ui| {
                    ui.set_max_width(rows_w);
                    list(ui)
                })
                .inner;
            left.or(stats_chart(ui, st))
        })
        .inner
    } else {
        list(ui)
    };
    let idle = cmd.is_none() && st.confirm.is_none();
    if let Some((k, double)) = clicked.filter(|_| idle) {
        st.sel = k;
        app.page = h;
        if double {
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
    } else {
        app.page = h;
    }
}

/// Process monitor: title, summary, column titles (click sorts), page of rows (click selects).
pub(super) fn draw_procs(ui: &mut Ui, app: &mut App) {
    let Mode::Procs(p) = &mut app.mode else {
        return;
    };
    ui.label(RichText::new(procs_header(p)).color(TEXT));
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;
    let h = ((ui.available_height() / row_h) as usize)
        .saturating_sub(1)
        .max(1);
    let start = p.sel / h * h;
    let shown = p.rows.len().saturating_sub(start).min(h);
    let (mut sort, mut clicked) = (None, None);
    let right = || Layout::right_to_left(Align::Center);
    TableBuilder::new(ui)
        .striped(false)
        .vscroll(false)
        .sense(Sense::CLICK)
        .column(Column::exact(70.0))
        .column(Column::exact(110.0))
        .column(Column::exact(70.0))
        .column(Column::exact(90.0))
        .column(Column::exact(70.0))
        .column(Column::remainder().clip(true))
        .header(row_h, |mut head| {
            let cols = [
                ("PID", None),
                ("User", None),
                ("CPU %", Some(('c', ProcSort::Cpu))),
                ("Memory", Some(('m', ProcSort::Mem))),
                ("Min", Some(('r', ProcSort::Run))),
                ("Command", None),
            ];
            for (k, (title, key)) in cols.into_iter().enumerate() {
                let arrow = match key {
                    Some((_, s)) if s == p.sort => " ⏷",
                    _ => "",
                };
                head.col(|ui| {
                    let t = RichText::new(format!("{title}{arrow}"))
                        .strong()
                        .color(MUTED);
                    let add = |ui: &mut Ui| ui.add(Label::new(t).sense(Sense::CLICK)).clicked();
                    let hit = if matches!(k, 0 | 2 | 3 | 4) {
                        ui.with_layout(right(), add).inner
                    } else {
                        add(ui)
                    };
                    if hit {
                        sort = key.map(|(c, _)| c);
                    }
                });
            }
        })
        .body(|body| {
            body.rows(row_h, shown, |mut row| {
                let i = start + row.index();
                let q = &p.snap.procs[p.rows[i]];
                row.set_selected(i == p.sel);
                row.col(|ui| {
                    ui.with_layout(right(), |ui| ui.label(q.pid.to_string()));
                });
                row.col(|ui| {
                    ui.add(Label::new(q.user.as_str()).truncate());
                });
                row.col(|ui| {
                    ui.with_layout(right(), |ui| ui.label(format!("{:.1}", q.cpu)));
                });
                row.col(|ui| {
                    ui.with_layout(right(), |ui| ui.label(human(q.mem)));
                });
                row.col(|ui| {
                    ui.with_layout(right(), |ui| ui.label((q.run / 60).to_string()));
                });
                row.col(|ui| {
                    let cmd = if q.cmd.is_empty() || q.cmd == q.name {
                        q.name.clone()
                    } else {
                        format!("{}  {}", q.name, q.cmd)
                    };
                    ui.add(Label::new(cmd).truncate());
                });
                if row.response().clicked() {
                    clicked = Some(i);
                }
            });
        });
    let idle = p.idle();
    if let Some(k) = clicked.filter(|_| idle) {
        p.sel = k;
    }
    app.page = h;
    if let Some(c) = sort.filter(|_| idle) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}
