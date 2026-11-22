use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Clear, List};

use super::popup::popup;
use super::{BAR, PANEL, draw_edit, themed_block};
use ratatui::widgets::ListState;
use tc_app::app::{App, Mode, Stats};
use tc_app::fmt::{
    PROC_COLS, proc_row, procs_confirm, procs_footer, procs_header, procs_prompt, procs_title,
};
use tc_app::fmt::{stats_footer, stats_progress, stats_row, stats_title};
use tc_core::cmdline::LineEdit;
/// File Stats: entries of the browsed directory, largest first, with share and bar.
/// `cmd`: command line typed from the stats, shown in the footer row.
pub(super) fn draw_stats(frame: &mut Frame, st: &Stats, cmd: Option<&LineEdit>, area: Rect) {
    let h = usize::from(area.height.saturating_sub(3)).max(1);
    let start = st.sel / h * h;
    let items = st.shown().skip(start).take(h).map(|n| stats_row(st, n));
    let footer = stats_footer(st);
    let list = List::new(items)
        .style(PANEL)
        .block(themed_block().title(format!(" {} ", stats_title(st))))
        .highlight_style(BAR);
    let mut state = ListState::default().with_selected(Some(st.sel.saturating_sub(start)));
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, main, &mut state);
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
    frame.render_widget(Line::raw(PROC_COLS).style(PANEL.bold()), cols);
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
}
