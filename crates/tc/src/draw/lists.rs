use super::dialogs::apply;
use super::tools::{sync_hints, sync_options};
use super::widgets::{cell, click_table, left, line_edit, page_rows, right};
use super::{ACCENT, MARK, MUTED, TEXT};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Label, Pos2, ProgressBar, Rect, RichText, Sense,
    Shape, Stroke, StrokeKind, Ui, epaint::Mesh, vec2,
};
use egui_extras::{Column, TableBuilder, TableRow};
use std::sync::Arc;
use tc::app::{App, Mode, Stats};
use tc::fmt::{
    self, StatsCols, found_cols, found_title, stats_cols, stats_footer, stats_title, sync_cols,
    sync_footer, sync_title,
};
use tc::fmt::{procs_footer, procs_header, procs_prompt};
use tc::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::cmdline::LineEdit;
use tc_core::edit::human;
use tc_core::procs::ProcSort;
use tc_core::stats::treemap::Tile;
pub(super) fn draw_found(ui: &mut Ui, app: &mut App) {
    let Mode::Found(f) = &mut app.mode else {
        return;
    };
    ui.label(RichText::new(found_title(f)).strong().color(ACCENT));
    let (row_h, h) = page_rows(ui);
    let start = f.sel / h * h;
    let rows: Vec<[String; 2]> = f.shown().skip(start).take(h).map(found_cols).collect();
    let cols = [Column::exact(80.0), Column::remainder().clip(true)];
    let clicked = click_table(
        ui,
        &cols,
        &["Size", "Path"],
        &[0],
        (row_h, start, rows.len(), f.sel),
        |row, i| {
            let [size, text] = &rows[i - start];
            row.col(|ui| right(ui, size.as_str()));
            row.col(|ui| left(ui, text.as_str()));
        },
    );
    app.page = h;
    if let Some((k, double)) = clicked {
        f.sel = k;
        if double {
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
    }
}

/// Synchronize directories: the options on top, the compared rows below; a click on a row
/// selects it and gives the rows the keyboard.
pub(super) fn draw_sync(ui: &mut Ui, app: &mut App) {
    let Mode::Sync(s) = &mut app.mode else {
        return;
    };
    ui.label(RichText::new(sync_title(s)).strong().color(ACCENT));
    let act = sync_options(ui, s);
    if s.editing && s.items.is_empty() && !s.comparing() {
        sync_hints(ui);
    } else {
        app.page = sync_rows(ui, s);
    }
    apply(app, act);
}

/// The compared rows that fit, the selected one's page; returns the rows per page.
fn sync_rows(ui: &mut Ui, s: &mut tc::app::Sync) -> usize {
    let today = jiff::ZonedDateTime::now().date();
    let (row_h, h) = page_rows(ui);
    let start = s.sel / h * h;
    let rows: Vec<[String; 6]> = (s.shown().skip(start).take(h))
        .map(|it| sync_cols(it, today))
        .collect();
    let cols = [
        Column::exact(80.0),
        Column::exact(110.0),
        Column::exact(60.0),
        Column::exact(80.0),
        Column::exact(110.0),
        Column::remainder().clip(true),
    ];
    let titles = [
        "Left size",
        "Left date",
        "Action",
        "Right size",
        "Right date",
        "Path",
    ];
    let clicked = click_table(
        ui,
        &cols,
        &titles,
        &[0, 3],
        (row_h, start, rows.len(), s.sel),
        |row, i| sync_cells(row, &rows[i - start]),
    );
    if let Some((k, _)) = clicked {
        s.sel = k;
        s.editing = false;
    }
    h
}

fn sync_cells(row: &mut TableRow, c: &[String; 6]) {
    row.col(|ui| right(ui, c[0].as_str()));
    row.col(|ui| left(ui, c[1].as_str()));
    row.col(|ui| {
        ui.centered_and_justified(|ui| ui.label(c[2].as_str()));
    });
    row.col(|ui| right(ui, c[3].as_str()));
    row.col(|ui| left(ui, c[4].as_str()));
    row.col(|ui| left(ui, c[5].as_str()));
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
const MAP_MIN_WIDTH: f32 = 700.0;

/// Treemap of the browsed directory; `Some((row, double))` when a tile was clicked.
fn stats_chart(ui: &mut Ui, st: &mut Stats) -> Option<(usize, bool)> {
    let size = vec2(ui.available_width(), ui.available_height().max(50.0));
    let (resp, painter) = ui.allocate_painter(size, Sense::click());
    let rect = resp.rect;
    st.treemap(rect.width(), rect.height(), u8::MAX, 4.0, 2.0);
    let sel = st.selected_index();
    let at = |t: &Tile| Rect::from_min_size(rect.min + vec2(t.x, t.y), vec2(t.w, t.h));
    painter.add(Shape::Mesh(tiles_mesh(ui.ctx(), rect, st.tiles())));
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

/// Tiles as one mesh (outline quad, fill quad inset by 1 px), rebuilt only when the tiles or
/// the area changed: re-tessellating thousands of stroked rects every frame costs a CPU core.
fn tiles_mesh(ctx: &egui::Context, rect: Rect, tiles: &[Tile]) -> Arc<Mesh> {
    type Cached = (Rect, Vec<Tile>, Arc<Mesh>);
    let id = egui::Id::unique("stats-treemap-mesh");
    if let Some(mesh) = ctx.data(|d| {
        d.get_temp::<Cached>(id)
            .filter(|(r, t, _)| *r == rect && t.as_slice() == tiles)
            .map(|c| c.2)
    }) {
        return mesh;
    }
    let mut mesh = Mesh::default();
    let outline = Color32::from_rgb(20, 20, 20);
    for t in tiles {
        let r = Rect::from_min_size(rect.min + vec2(t.x, t.y), vec2(t.w, t.h));
        let (red, g, b) = fmt::kind_rgb(t.kind, t.depth % 2);
        let fill = Color32::from_rgb(red, g, b);
        if r.width() > 2.0 && r.height() > 2.0 {
            mesh.add_colored_rect(r, outline);
            mesh.add_colored_rect(r.shrink(1.0), fill);
        } else {
            mesh.add_colored_rect(r, fill);
        }
    }
    let mesh = Arc::new(mesh);
    ctx.data_mut(|d| d.insert_temp::<Cached>(id, (rect, tiles.to_vec(), mesh.clone())));
    mesh
}

pub(super) fn draw_stats(ui: &mut Ui, app: &mut App) {
    let (st, cmd): (&mut Stats, Option<&LineEdit>) = match (&mut app.mode, &mut app.stats) {
        (Mode::Stats(st), _) => (st, None),
        (Mode::Cmd(l), Some(st)) => (st, Some(&*l)),
        _ => return,
    };
    ui.label(RichText::new(stats_title(st)).strong().color(ACCENT));
    let (row_h, h) = page_rows(ui);
    let start = st.sel / h * h;
    let rows: Vec<StatsCols> = st
        .shown()
        .skip(start)
        .take(h)
        .map(|n| stats_cols(st, n))
        .collect();
    let sel = st.sel;
    let size = if st.by_files { "Files" } else { "Size" };
    let name = if st.filter.active() {
        "Name 🔍"
    } else {
        "Name"
    };
    let list = |ui: &mut Ui| {
        let cols = [
            Column::exact(10.0),
            Column::exact(62.0),
            Column::exact(50.0),
            Column::exact(100.0),
            Column::remainder().clip(true),
        ];
        let titles = ["", size, "Share", "", name];
        click_table(
            ui,
            &cols,
            &titles,
            &[1, 2],
            (row_h, start, rows.len(), sel),
            |row, i| stats_cells(row, &rows[i - start]),
        )
    };
    let width = ui.available_width();
    let clicked = if width >= MAP_MIN_WIDTH {
        let rows_w = width * 0.5;
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

fn stats_cells(row: &mut TableRow, c: &StatsCols) {
    row.col(|ui| {
        if c.marked {
            ui.label(RichText::new("*").color(MARK));
        }
    });
    row.col(|ui| right(ui, c.value.as_str()));
    row.col(|ui| right(ui, format!("{:.1}%", c.share * 100.0)));
    row.col(|ui| {
        ui.add(
            ProgressBar::new(c.share as f32)
                .desired_width(ui.available_width())
                .corner_radius(CornerRadius::ZERO),
        );
    });
    row.col(|ui| left(ui, c.name.as_str()));
}

/// Process monitor: title, summary, column titles (click sorts), page of rows (click selects).
pub(super) fn draw_procs(ui: &mut Ui, app: &mut App) {
    let Mode::Procs(p) = &mut app.mode else {
        return;
    };
    ui.label(RichText::new(procs_header(p)).color(TEXT));
    let (row_h, h) = page_rows(ui);
    let start = p.sel / h * h;
    let shown = p.rows.len().saturating_sub(start).min(h);
    let (mut sort, mut clicked) = (None, None);
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
            let c = &p.crit;
            // a column shows 🔍 while its filter field is set
            // the text filter matches the id, the name and the command
            let filtered = [
                !c.text.is_empty(),
                false,
                c.min_cpu > 0.0,
                c.min_mem > 0,
                c.min_run > 0,
                !c.text.is_empty(),
            ];
            for (k, (title, key)) in cols.into_iter().enumerate() {
                let arrow = match key {
                    Some((_, s)) if s == p.sort => " ⏷",
                    _ => "",
                };
                let icon = if filtered[k] { " 🔍" } else { "" };
                head.col(|ui| {
                    let t = RichText::new(format!("{title}{icon}{arrow}"))
                        .strong()
                        .color(MUTED);
                    let add = |ui: &mut Ui| ui.add(Label::new(t).sense(Sense::CLICK)).clicked();
                    let hit = cell(ui, matches!(k, 0 | 2 | 3 | 4), add);
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
                row.col(|ui| right(ui, q.pid.to_string()));
                row.col(|ui| {
                    ui.add(Label::new(q.user.as_str()).truncate());
                });
                row.col(|ui| right(ui, format!("{:.1}", q.cpu)));
                row.col(|ui| right(ui, human(q.mem)));
                row.col(|ui| right(ui, (q.run / 60).to_string()));
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
