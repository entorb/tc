//! egui rendering of the shared `App` state; texts come from `tc::fmt`, so both front ends say the same.

use std::path::Path;

use eframe::egui::{
    self, Align, Button, Color32, Context, FontFamily, FontId, Id, Label, Layout, Modal, Response,
    RichText, ScrollArea, Sense, Stroke, TextFormat, TextStyle, Ui, Vec2, text::LayoutJob, vec2,
};
use egui_extras::{Column, TableBuilder};
use tc::app::{
    App, FindForm, FormRow, MENUS, Mode, RenameForm, Stats, SyncForm, TOOLBAR, ToolItem,
};
use tc::fmt::{
    self, Dialog, Mark, dialog, editor_info, entry_cols, found_title, input_dialog, panel_info,
    panel_title, stats_footer, stats_progress, stats_row, stats_title, sync_footer, sync_row,
    sync_title,
};
use tc::fmt::{procs_confirm, procs_footer, procs_header, procs_prompt, procs_title};
use tc::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind, Rect};
use tc_core::cmdline::LineEdit;
use tc_core::edit::human;
use tc_core::panel::SortKey;
use tc_core::procs::ProcSort;
use tc_core::syntax::{self, Kind};

const ACCENT: Color32 = Color32::from_rgb(90, 160, 255);
const MARK: Color32 = Color32::from_rgb(255, 200, 60);
const LINK: Color32 = Color32::from_rgb(200, 140, 255);
const TEXT: Color32 = Color32::from_gray(210);
const MUTED: Color32 = Color32::from_gray(130);
const SELECT_BG: Color32 = Color32::from_rgb(40, 70, 120);

fn mono() -> FontId {
    FontId::monospace(14.0)
}

/// Font family that draws every icon from `emoji-icon-font`, so toolbar glyphs share one style.
fn icons() -> FontId {
    FontId::new(16.0, FontFamily::Name("icons".into()))
}

/// Registers the `icons` family: emoji-icon-font first, the default fonts as fallback.
pub fn setup_fonts(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    let family = ["emoji-icon-font", "Hack", "Ubuntu-Light"].map(String::from);
    fonts
        .families
        .insert(FontFamily::Name("icons".into()), family.into());
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|s| {
        // a file manager, not a document: arrow pointer, no text selection on labels
        s.interaction.selectable_labels = false;
        s.interaction.tooltip_delay = 0.1;
    });
}

/// Modal without the fade-in animation.
fn modal(id: &str) -> Modal {
    let mut m = Modal::new(Id::unique(id));
    m.area = m.area.fade_in(false);
    m
}

/// Frontend state that `App` does not keep: last drawn cursor per panel (scroll only when it moved).
#[derive(Default)]
pub struct Look {
    cursor: [Option<usize>; 2],
}

pub fn draw(ui: &mut Ui, app: &mut App, look: &mut Look) {
    let today = jiff::ZonedDateTime::now().date();
    egui::Panel::bottom("footer").show(ui, |ui| footer(ui, app));
    if matches!(app.mode, Mode::View { .. }) {
        egui::CentralPanel::default().show(ui, |ui| draw_view(ui, app));
    } else if app.mode.editor_mut().is_some() {
        egui::CentralPanel::default().show(ui, |ui| draw_editor(ui, app));
    } else {
        egui::Panel::top("menu").show(ui, |ui| menu_bar(ui, app));
        egui::Panel::top("toolbar").show(ui, |ui| toolbar(ui, app));
        // File Stats and processes have their own status / prompt line
        let own = app.stats.is_some()
            || matches!(app.mode, Mode::Stats(_) | Mode::Procs(_) | Mode::Sync(_));
        if own {
            egui::Panel::bottom("status").show(ui, |ui| status_line(ui, app));
        } else {
            egui::Panel::bottom("cmd").show(ui, |ui| cmdline(ui, app));
        }
        egui::CentralPanel::default().show(ui, |ui| match &app.mode {
            Mode::Found(_) => draw_found(ui, app),
            Mode::Sync(_) => draw_sync(ui, app),
            Mode::Stats(_) => draw_stats(ui, app),
            Mode::Procs(_) => draw_procs(ui, app),
            Mode::Cmd(_) if app.stats.is_some() => draw_stats(ui, app),
            _ => ui.columns(2, |cols| {
                for (i, c) in cols.iter_mut().enumerate() {
                    draw_panel(c, app, look, i, today);
                }
            }),
        });
    }
    overlays(ui.ctx(), app);
}

enum Action {
    Sort(SortKey),
    Click(usize, bool, bool),
    Tab(usize),
    CloseTab(usize),
}

fn draw_panel(ui: &mut Ui, app: &mut App, look: &mut Look, i: usize, today: jiff::civil::Date) {
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

/// Top menu bar (F9 or click); the open group drops down below its title.
fn menu_bar(ui: &mut Ui, app: &mut App) {
    let open = if let Mode::Menu(g, _) = app.mode {
        Some(g)
    } else {
        None
    };
    let mut run = None;
    ui.horizontal(|ui| {
        for (g, (title, items)) in MENUS.iter().enumerate() {
            // not focusable: Tab stays with the app instead of walking into the menu
            let r = ui.add(Button::selectable(open == Some(g), *title).sense(Sense::CLICK));
            if r.clicked() {
                app.mode = if open == Some(g) {
                    Mode::Normal
                } else {
                    Mode::Menu(g, 0)
                };
            }
            if open != Some(g) {
                continue;
            }
            let sel = if let Mode::Menu(_, s) = app.mode {
                s
            } else {
                0
            };
            egui::Area::new(Id::unique("menu-drop"))
                .order(egui::Order::Foreground)
                .fixed_pos(r.rect.left_bottom())
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_width(260.0);
                        for (i, (label, ..)) in items.iter().enumerate() {
                            let (name, key) = label.split_once("  ").unwrap_or((label, ""));
                            let b = Button::selectable(i == sel, name)
                                .shortcut_text(key.trim())
                                .sense(Sense::CLICK)
                                .min_size(egui::vec2(ui.available_width(), 0.0));
                            if ui.add(b).clicked() {
                                run = Some((g, i));
                            }
                        }
                    });
                });
        }
    });
    if let Some((g, i)) = run {
        app.menu_run(g, i);
    }
}

/// Icon toolbar under the menu bar; clicks send the key the menu lists.
fn toolbar(ui: &mut Ui, app: &mut App) {
    // toolbar keys only act on the panels; greyed out on sub-screens, dialogs and prompts
    let enabled = matches!(app.mode, Mode::Normal) && app.idle();
    ui.horizontal(|ui| {
        if !enabled {
            ui.disable();
        }
        for (g, group) in TOOLBAR.iter().enumerate() {
            if g > 0 {
                ui.separator();
            }
            toolbar_group(ui, app, group);
        }
    });
}

fn toolbar_group(ui: &mut Ui, app: &mut App, group: &[ToolItem]) {
    for &(icon, tip, code, mods) in group {
        let clicked = ui
            .add(Button::new(RichText::new(icon).font(icons())).sense(Sense::CLICK))
            .on_hover_text(tip)
            .clicked();
        if clicked && app.idle() {
            app.on_key(KeyEvent::new(code, mods));
        }
    }
}

/// F-key bar; buttons are not focusable, so Tab and Space stay with the app.
/// Holding Shift shows the Shift alternates; the tooltip is the key's F1 help.
fn footer(ui: &mut Ui, app: &mut App) {
    let shift = ui.input(|i| i.modifiers.shift);
    let help = tc::fmt::help(&app.mode).1;
    ui.horizontal(|ui| {
        for &item in app.footer_keys() {
            let (label, code, mods) = if shift {
                app.footer_shifted(item)
            } else {
                item
            };
            let clicked = ui
                .add(Button::new(label).sense(Sense::CLICK))
                .on_hover_ui(|ui| {
                    ui.label(tc::fmt::key_help(help, label));
                })
                .clicked();
            if clicked && app.footer_active() {
                app.on_key(KeyEvent::new(code, mods));
            }
        }
    });
}

/// Prompt and command being typed; a click starts typing like `:`.
fn cmdline(ui: &mut Ui, app: &mut App) {
    let r = ui.horizontal(|ui| {
        let prompt = format!("{}> ", app.panels[app.active].path().display());
        ui.label(RichText::new(prompt).font(mono()).color(MUTED));
        if let Mode::Cmd(l) = &app.mode {
            line_edit(ui, l);
        }
        ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
    });
    let click = r.response.interact(Sense::CLICK);
    if click.on_hover_cursor(egui::CursorIcon::Text).clicked()
        && matches!(app.mode, Mode::Normal)
        && app.idle()
    {
        app.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
    }
}

/// Single-line text of `l` with a caret at its cursor.
fn line_edit(ui: &mut Ui, l: &LineEdit) {
    let galley = ui
        .painter()
        .layout_no_wrap(l.text.clone(), mono(), Color32::WHITE);
    let pad = vec2(6.0, 3.0);
    let width = ui.available_width().max(galley.size().x + 2.0 * pad.x);
    let (rect, _) =
        ui.allocate_exact_size(vec2(width, galley.size().y + 2.0 * pad.y), Sense::hover());
    let caret = galley
        .pos_from_cursor(egui::text::CCursor::new(l.text[..l.cur].chars().count()))
        .min
        .x;
    let p = ui.painter();
    p.rect_filled(rect, 4.0, Color32::from_gray(25));
    p.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, ACCENT),
        egui::StrokeKind::Inside,
    );
    let origin = rect.min + pad;
    p.galley(origin, galley, Color32::WHITE);
    let x = origin.x + caret;
    p.vline(
        x,
        (origin.y)..=(rect.max.y - pad.y),
        Stroke::new(1.5, ACCENT),
    );
}

/// Monospace cell size.
fn cell(ui: &Ui) -> Vec2 {
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
fn grid(ui: &Ui, reserve: f32) -> (usize, usize, Vec2) {
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

fn draw_view(ui: &mut Ui, app: &mut App) {
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

fn draw_editor(ui: &mut Ui, app: &mut App) {
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
    app.edit_text = Rect::new(gutter as u16, 0, width as u16, rows as u16);
    if let Some(ev) = editor_mouse(ui, &r, c) {
        app.on_mouse(ev);
    }
}

/// Press or drag on the editor text as a terminal mouse event in cell units (see `App::edit_text`).
fn editor_mouse(ui: &Ui, r: &Response, c: Vec2) -> Option<MouseEvent> {
    let pressed = r.hovered() && ui.input(|i| i.pointer.primary_pressed());
    let kind = if pressed {
        MouseEventKind::Down(MouseButton::Left)
    } else if r.dragged() {
        MouseEventKind::Drag(MouseButton::Left)
    } else {
        return None;
    };
    let pos = r.interact_pointer_pos().or(r.hover_pos())? - r.rect.min;
    let shift = ui.input(|i| i.modifiers.shift);
    Some(MouseEvent {
        kind,
        column: (pos.x / c.x).max(0.0) as u16,
        row: (pos.y / c.y).max(0.0) as u16,
        modifiers: if shift {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        },
    })
}

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

fn draw_found(ui: &mut Ui, app: &mut App) {
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
fn draw_sync(ui: &mut Ui, app: &mut App) {
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
fn status_line(ui: &mut Ui, app: &App) {
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
fn prompt(ui: &mut Ui, label: &str, l: &LineEdit) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).font(mono()));
        line_edit(ui, l);
    });
}

fn draw_stats(ui: &mut Ui, app: &mut App) {
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
fn draw_procs(ui: &mut Ui, app: &mut App) {
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

fn overlays(ctx: &Context, app: &mut App) {
    if let Some(d) = dialog(app) {
        show_dialog(ctx, "dialog", &d);
    }
    if let Mode::FindForm(form) = &app.mode {
        show_find_form(ctx, form);
    }
    if let Mode::MultiRename(form) = &app.mode {
        show_multi_rename(ctx, form);
    }
    if let Mode::SyncForm(form) = &app.mode {
        show_sync_form(ctx, form);
    }
    if let Mode::Sync(s) = &app.mode
        && let Some(text) = &s.confirm
    {
        let d = Dialog::Text {
            title: "Synchronize",
            text: text.clone(),
        };
        show_dialog(ctx, "sync-confirm", &d);
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = &st.confirm
    {
        let d = Dialog::Text {
            title: "Delete",
            text: text.clone(),
        };
        show_dialog(ctx, "stats-confirm", &d);
    }
    if let Mode::Stats(st) = &app.mode
        && let Some(text) = stats_progress(st)
    {
        let d = Dialog::Text {
            title: "Scanning",
            text,
        };
        show_dialog(ctx, "stats-progress", &d);
    }
    if let Mode::Procs(p) = &app.mode
        && let Some((pid, name)) = &p.confirm
    {
        let d = Dialog::Text {
            title: "Kill",
            text: procs_confirm(*pid, name),
        };
        show_dialog(ctx, "procs-confirm", &d);
    }
    if let Some(input) = &app.input {
        show_dialog(ctx, "input", &input_dialog(input));
    }
    if app.help {
        show_help(ctx, app);
    }
}

fn show_dialog(ctx: &Context, id: &str, d: &Dialog) {
    modal(id).show(ctx, |ui| {
        ui.set_min_width(420.0);
        ui.set_max_width(720.0);
        match d {
            Dialog::Text { title, text } => {
                ui.heading(*title);
                ui.separator();
                ui.label(text.as_str());
            }
            Dialog::Edit {
                title,
                head,
                line,
                tail,
            } => {
                ui.heading(*title);
                ui.separator();
                if !head.is_empty() {
                    ui.label(head.as_str());
                }
                line_edit(ui, line);
                ui.label(RichText::new(tail.trim_start()).color(MUTED));
            }
            Dialog::List {
                title,
                rows,
                sel,
                empty,
                keys,
            } => {
                ui.heading(title);
                ui.separator();
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (k, p) in rows.iter().enumerate() {
                        let mut t = RichText::new(p.as_str());
                        if k == *sel {
                            t = t.background_color(SELECT_BG).color(Color32::WHITE);
                        }
                        let r = ui.label(t);
                        if k == *sel {
                            r.scroll_to_me(None);
                        }
                    }
                });
                if rows.is_empty() {
                    ui.label(*empty);
                }
                ui.label(RichText::new(*keys).color(MUTED));
            }
        }
    });
}

/// Find files form: fields and checkboxes, the focused one highlighted; keys go through the app.
fn show_find_form(ctx: &Context, form: &FindForm) {
    modal("find-form").show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("Find files");
        ui.separator();
        form_rows(ui, &form.rows, form.focus, form.error.as_deref());
        ui.label(RichText::new(fmt::FIND_FORM_KEYS).color(MUTED));
    });
}

fn show_sync_form(ctx: &Context, form: &SyncForm) {
    modal("sync-form").show(ctx, |ui| {
        ui.set_width(560.0);
        ui.heading("Synchronize directories (left <-> right panel)");
        ui.separator();
        form_rows(ui, &form.rows, form.focus, form.error.as_deref());
        ui.label(RichText::new(fmt::SYNC_FORM_KEYS).color(MUTED));
    });
}

/// Multi-rename over the whole window: form, then as many old -> new rows as fit; keys go through the app.
fn show_multi_rename(ctx: &Context, form: &RenameForm) {
    let size = ctx.content_rect().shrink(24.0).size();
    modal("multi-rename").show(ctx, |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        ui.heading("Multi-rename");
        ui.separator();
        form_rows(ui, &form.rows, form.focus, form.error.as_deref());
        ui.separator();
        ui.label(format!("{} files:", form.preview.len()));
        let row_h = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;
        let keys_h = 2.0 * row_h;
        let fit = ((ui.available_height() - keys_h) / row_h).max(1.0) as usize;
        for (old, new) in form.preview.iter().skip(form.scroll).take(fit) {
            let (text, bad) = fmt::rename_row(old, new);
            let t = RichText::new(text);
            ui.label(if bad { t.color(Color32::LIGHT_RED) } else { t });
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.label(RichText::new(fmt::RENAME_FORM_KEYS).color(MUTED));
        });
    });
}

/// Form as a label | value grid, the focused row highlighted, and the error under it.
fn form_rows(ui: &mut Ui, rows: &[FormRow], focus: usize, error: Option<&str>) {
    egui::Grid::new("form")
        .num_columns(2)
        .spacing(vec2(12.0, 4.0))
        .show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                let focused = i == focus;
                let color = if focused { ACCENT } else { TEXT };
                let label = |t: &str| RichText::new(t.to_owned()).color(color);
                match row {
                    FormRow::Text(name, l) => {
                        ui.label(label(name).strong());
                        if focused {
                            line_edit(ui, l);
                        } else {
                            ui.label(RichText::new(l.text.as_str()).font(mono()));
                        }
                    }
                    FormRow::Check(name, on) => {
                        ui.label("");
                        ui.label(label(&format!(
                            "{} {name}",
                            if *on { "[x]" } else { "[ ]" }
                        )));
                    }
                    FormRow::Choice(name, opts, k) => {
                        ui.label(label(name).strong());
                        ui.label(label(&format!("< {} >", opts[*k])));
                    }
                }
                ui.end_row();
            }
        });
    if let Some(e) = error {
        ui.label(RichText::new(e).color(Color32::LIGHT_RED));
    }
}

/// Help as a table: section titles, then key | info rows (continuation lines join the info).
fn show_help(ctx: &Context, app: &mut App) {
    const KEY_W: f32 = 150.0;
    let title = fmt::help(&app.mode).0;
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
                    let r = ui.horizontal(|ui| {
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
                    });
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

#[cfg(test)]
mod sim {
    use super::{App, Look, Mode, draw};
    use crate::keys;
    use eframe::egui::{self, Event, Key, Modifiers, RawInput};
    use tc_core::panel::Panel;

    const SCREEN: egui::Rect =
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(900.0, 500.0));

    /// One frame with `events`; returns the texts painted inside the screen.
    fn frame(ctx: &egui::Context, app: &mut App, look: &mut Look, events: Vec<Event>) -> String {
        let input = RawInput {
            events: events.clone(),
            screen_rect: Some(SCREEN),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            for ev in &events {
                if let Some(keys::Input::Key(k)) = keys::translate(ev, false) {
                    app.on_key(k);
                }
            }
            app.tick();
            draw(ui, app, look);
        });
        out.textures_delta.clear();
        let mut texts = String::new();
        for c in out.shapes {
            if let egui::Shape::Text(t) = c.shape
                && SCREEN.contains_rect(t.visual_bounding_rect())
                && c.clip_rect.contains_rect(t.visual_bounding_rect())
            {
                texts += t.galley.text();
                texts.push('\n');
            }
        }
        texts
    }

    fn press(k: Key, m: Modifiers) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        }
    }

    #[test]
    /// Help table, process table headers and the multi-rename window draw their texts.
    fn tables_and_dialogs_draw() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        let mut run = |app: &mut App, ev: Vec<Event>| frame(&ctx, app, &mut look, ev);
        run(&mut app, vec![press(Key::F1, Modifiers::NONE)]);
        let shown = run(&mut app, vec![]);
        assert!(
            shown.contains("Highlights") && shown.contains("Ctrl+B"),
            "{shown}"
        );
        assert!(shown.contains("bookmarks (hotlist)"), "{shown}");
        run(&mut app, vec![press(Key::Escape, Modifiers::NONE)]);
        run(&mut app, vec![press(Key::ArrowDown, Modifiers::NONE)]);
        run(&mut app, vec![press(Key::M, Modifiers::CTRL)]);
        let shown = run(&mut app, vec![]);
        assert!(
            shown.contains("Multi-rename") && shown.contains("a.txt"),
            "{shown}"
        );
        run(&mut app, vec![press(Key::Escape, Modifiers::NONE)]);
        run(&mut app, vec![press(Key::P, Modifiers::CTRL)]);
        let shown = run(&mut app, vec![]);
        assert!(
            shown.contains("PID") && shown.contains("Command"),
            "{shown}"
        );
    }

    #[test]
    /// The process monitor gets keys through the egui event path; its footer replaces the main one.
    fn procs_keys_in_gui() {
        let tmp = tempfile::tempdir().unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        frame(
            &ctx,
            &mut app,
            &mut look,
            vec![press(Key::P, Modifiers::CTRL)],
        );
        for _ in 0..30 {
            frame(&ctx, &mut app, &mut look, vec![]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        frame(
            &ctx,
            &mut app,
            &mut look,
            vec![Event::Text("s".into()), press(Key::S, Modifiers::NONE)],
        );
        assert!(
            matches!(&app.mode, Mode::Procs(p) if p.every_edit.is_some()),
            "s"
        );
        // the prompt and the monitor's own buttons are on screen, not clipped below the rows
        let shown = frame(&ctx, &mut app, &mut look, vec![]);
        assert!(shown.contains("update every (seconds)> "), "{shown}");
        assert!(shown.contains("s Interval"), "{shown}");
        frame(
            &ctx,
            &mut app,
            &mut look,
            vec![press(Key::Escape, Modifiers::NONE)],
        );
        frame(
            &ctx,
            &mut app,
            &mut look,
            vec![press(Key::F1, Modifiers::NONE)],
        );
        assert!(app.help, "F1");
        assert_eq!(app.footer_keys()[2].0, "s Interval");
    }
}
