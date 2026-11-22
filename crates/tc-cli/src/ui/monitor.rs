use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Clear, List};

use super::forms::{draw_form, form_lines};
use super::popup::popup;
use super::{BAR, PANEL, draw_edit, header_line, themed_block};
use ratatui::widgets::ListState;
use tc_app::app::{App, Mode, Stats};
use tc_app::fmt::kind_rgb;
use tc_app::fmt::{
    PROC_COLS, proc_col_key, proc_row, procs_confirm, procs_footer, procs_header, procs_info,
    procs_prompt, procs_title,
};
use tc_app::fmt::{stats_footer, stats_progress, stats_row, stats_title};
use tc_core::cmdline::LineEdit;
use tc_core::procs::ProcSort;
use tc_core::stats::treemap::Tile;
/// File Stats: entries of the browsed directory, largest first, with share and bar.
/// `cmd`: command line typed from the stats, shown in the footer row.
/// Returns the treemap's inner area (empty when the window is too narrow for it).
pub(super) fn draw_stats(
    frame: &mut Frame,
    st: &mut Stats,
    cmd: Option<&LineEdit>,
    area: Rect,
) -> Rect {
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let (table, chart) = if main.width >= MAP_MIN_WIDTH {
        let [l, r] =
            Layout::horizontal([Constraint::Min(56), Constraint::Percentage(45)]).areas(main);
        (l, Some(r))
    } else {
        (main, None)
    };
    let h = usize::from(area.height.saturating_sub(3)).max(1);
    let start = st.sel / h * h;
    let items = st.shown().skip(start).take(h).map(|n| stats_row(st, n));
    let footer = stats_footer(st);
    let list = List::new(items)
        .style(PANEL)
        .block(themed_block().title(format!(" {} ", stats_title(st))))
        .highlight_style(BAR);
    let mut state = ListState::default().with_selected(Some(st.sel.saturating_sub(start)));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, table, &mut state);
    let map = chart.map_or_else(Rect::default, |r| draw_treemap(frame, st, r));
    match cmd {
        Some(l) => {
            let prompt = format!("{}> ", st.path().display());
            let style = Style::new().fg(Color::Gray).bg(Color::Black);
            draw_edit(frame, status, &prompt, l, style);
        }
        None => frame.render_widget(Line::raw(footer).style(BAR), status),
    }
    if let Some(text) = &st.confirm {
        popup(frame, "Delete", text);
    }
    if let Some(text) = stats_progress(st) {
        popup(frame, "Scanning", &text);
    }
    map
}

/// Window width from which File Stats shows the treemap beside the list.
const MAP_MIN_WIDTH: u16 = 100;

/// Cells of `t` inside `r`, `None` if it is smaller than one cell.
fn tile_cells(r: Rect, t: &Tile) -> Option<Rect> {
    // round: a cell belongs to the tile holding its centre, as `stats_click` tests it
    let edge = |v: f32| v.max(0.0).round() as u16;
    let (x0, x1) = (edge(t.x), edge(t.x + t.w).min(r.width));
    let (y0, y1) = (edge(t.y), edge(t.y + t.h).min(r.height));
    (x1 > x0 && y1 > y0).then(|| Rect::new(r.x + x0, r.y + y0, x1 - x0, y1 - y0))
}

fn fill(frame: &mut Frame, a: Rect, bg: Color) {
    let buf = frame.buffer_mut();
    for y in a.top()..a.bottom() {
        for x in a.left()..a.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char(' ').set_bg(bg);
            }
        }
    }
}

/// Treemap of the browsed directory: two levels, colored by file type, the cursor's tile has
/// a highlighted label row. Returns the inner area.
fn draw_treemap(frame: &mut Frame, st: &mut Stats, area: Rect) -> Rect {
    let block = themed_block().title(" Treemap ");
    let r = block.inner(area);
    frame.render_widget(block, area);
    st.treemap(f32::from(r.width), f32::from(r.height), 1, 1.0, 0.0);
    let sel = st.selected_index();
    let dir = st.current();
    let mut n = 0u8;
    for t in st.tiles() {
        let Some(a) = tile_cells(r, t) else { continue };
        // depth 0 alternates in layout order (mostly adjacent tiles), nested tiles are darkest
        let shade = if t.depth == 0 {
            n ^= 1;
            n
        } else {
            2
        };
        let (cr, cg, cb) = kind_rgb(t.kind, shade);
        fill(frame, a, Color::Rgb(cr, cg, cb));
    }
    for t in st.tiles().iter().filter(|t| t.depth == 0) {
        let Some(a) = tile_cells(r, t) else { continue };
        let selected = Some(t.top) == sel;
        if a.width < 3 && !selected {
            continue;
        }
        let style = if selected {
            BAR
        } else {
            Style::new().fg(Color::Black)
        };
        let name = dir.children[t.top].name.to_string_lossy();
        let label = fit(&name, usize::from(a.width));
        frame.render_widget(Line::raw(label).style(style), Rect { height: 1, ..a });
    }
    r
}

/// `s` cut to at most `w` characters.
fn fit(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}

/// Process monitor: summary, column titles, the page of processes holding the cursor, footer.
pub(super) fn draw_procs(frame: &mut Frame, app: &mut App, area: Rect) {
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let block = themed_block();
    let [head, cols, rows] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(block.inner(main));
    let h = usize::from(rows.height).max(1);
    app.page = h;
    let Mode::Procs(p) = &app.mode else {
        return;
    };
    let start = p.sel / h * h;
    let items = p
        .rows
        .iter()
        .skip(start)
        .take(h)
        .map(|&i| proc_row(&p.snap.procs[i]));
    let list = List::new(items).style(PANEL).highlight_style(BAR);
    let mut state = ListState::default().with_selected(Some(p.sel.saturating_sub(start)));
    frame.render_widget(Clear, area);
    frame.render_widget(block.title(format!(" {} ", procs_title(p))), main);
    frame.render_widget(Line::raw(procs_header(p)).style(PANEL), head);
    let key = match p.sort {
        ProcSort::Cpu => 'c',
        ProcSort::Mem => 'm',
        ProcSort::Run => 'r',
    };
    let header = header_line(PROC_COLS, |col| proc_col_key(col) == Some(key));
    frame.render_widget(header, cols);
    frame.render_stateful_widget(list, rows, &mut state);
    if let Some((label, l)) = procs_prompt(p) {
        let style = Style::new().fg(Color::Gray).bg(Color::Black);
        draw_edit(frame, status, label, l, style);
    } else {
        frame.render_widget(Line::raw(procs_footer(p)).style(BAR), status);
    }
    if let Some((pid, name)) = &p.confirm {
        popup(frame, "Kill", &procs_confirm(*pid, name));
    }
    if let Some(d) = &p.draft {
        let lines = form_lines(&d.rows, d.focus, d.error.as_deref());
        draw_form(frame, " Process filter ", lines, d.focus);
    }
    if let Some(info) = procs_info(p) {
        let text: Vec<String> = info
            .rows
            .iter()
            .map(|(k, v)| format!("{k:<9} {v}"))
            .collect();
        app.proc_info_area = popup(frame, &info.title, &text.join("\n"));
    }
}
