use super::chrome::line_edit;
use super::viewer::cell;
use super::{ACCENT, MUTED, SELECT_BG, TEXT, mono};
use eframe::egui::{Align, Color32, Label, Layout, RichText, Sense, TextStyle, Ui};
use egui_extras::{Column, TableBuilder};
use tc_app::app::{App, Mode, Stats};
use tc_app::fmt::{
    self, found_title, stats_footer, stats_row, stats_title, sync_footer, sync_row, sync_title,
};
use tc_app::fmt::{procs_footer, procs_header, procs_prompt, procs_title};
use tc_app::{KeyCode, KeyEvent, KeyModifiers};
use tc_core::cmdline::LineEdit;
use tc_core::edit::human;
use tc_core::procs::ProcSort;
/// One clickable list row; `Some(double)` when clicked.
fn list_row(ui: &mut Ui, text: String, selected: bool) -> Option<bool> {
    let mut t = RichText::new(text).font(mono());
    if selected {
        t = t.background_color(SELECT_BG).color(Color32::WHITE);
    }
    let r = ui.add(Label::new(t).sense(Sense::CLICK).truncate());
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
        ui.label(RichText::new(label).font(mono()));
        line_edit(ui, l);
    });
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
    let mut clicked = None;
    let rows: Vec<String> = st
        .shown()
        .skip(start)
        .take(h)
        .map(|n| stats_row(st, n))
        .collect();
    for (k, row) in rows.into_iter().enumerate() {
        if let Some(double) = list_row(ui, row, start + k == st.sel) {
            clicked = Some((start + k, double));
        }
    }
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
    ui.label(RichText::new(procs_title(p)).strong().color(ACCENT));
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
        .column(Column::remainder().clip(true))
        .header(row_h, |mut head| {
            let cols = [
                ("PID", Some(('p', ProcSort::Pid))),
                ("User", None),
                ("CPU %", Some(('c', ProcSort::Cpu))),
                ("Memory", Some(('m', ProcSort::Mem))),
                ("Command", Some(('n', ProcSort::Name))),
            ];
            for (k, (title, key)) in cols.into_iter().enumerate() {
                let arrow = match key {
                    Some((_, s)) if s == p.sort => {
                        if p.rev {
                            " ⏷"
                        } else {
                            " ⏶"
                        }
                    }
                    _ => "",
                };
                head.col(|ui| {
                    let t = RichText::new(format!("{title}{arrow}"))
                        .strong()
                        .color(MUTED);
                    let add = |ui: &mut Ui| ui.add(Label::new(t).sense(Sense::CLICK)).clicked();
                    let hit = if matches!(k, 0 | 2 | 3) {
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
