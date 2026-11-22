use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use regex::Regex;

use super::{BAR, MARK, PANEL, syntax_style, themed_block};
use std::path::Path;
use tc_app::app::{App, Mode};
use tc_app::fmt::{self, Mark, editor_info};
use tc_core::syntax::{self, Lang};
pub(super) fn draw_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let Mode::View { title, view } = &mut app.mode else {
        return;
    };
    let bottom = format!(
        " {}%  {}  {} ",
        view.percent(),
        if view.hex() {
            "hex"
        } else {
            view.encoding_name()
        },
        app.note
    );
    let block = themed_block().title(title.as_str()).title_bottom(bottom);
    let inner = block.inner(area);
    let height = inner.height as usize;
    app.page = height.max(1);
    // numbers only while the top line is known, see `Viewer::top_line`
    let top = view.top_line().filter(|_| app.view_numbers && !view.hex());
    let gutter = if top.is_some() { 7 } else { 0 };
    let lang = syntax::lang(Path::new(title.as_str())).filter(|_| !view.hex());
    // shortcut: viewer assumes no code fence open above the screen, upgrade with a fence index in `Viewer`
    let mut fence = false;
    let visible: Vec<Line> = match view.rows(height, (inner.width as usize).saturating_sub(gutter))
    {
        Ok(rows) => {
            let mut n = top.unwrap_or(0);
            rows.into_iter()
                .map(|(first, text)| {
                    let mut line =
                        highlight(text, lang, &mut fence, app.search.as_ref(), None, (0, &[]));
                    if top.is_some() {
                        let num = if first {
                            format!("{:>6} ", n + 1)
                        } else {
                            " ".repeat(7)
                        };
                        line.spans.insert(0, Span::raw(num));
                    }
                    n += u64::from(first);
                    line
                })
                .collect()
        }
        Err(e) => vec![Line::raw(e.to_string())],
    };
    frame.render_widget(Paragraph::new(visible).style(PANEL).block(block), area);
}

pub(super) fn draw_editor(
    frame: &mut Frame,
    editor: &mut tc_core::edit::Editor,
    (page, text_area): (&mut usize, &mut Rect),
    note: &str,
    re: Option<&Regex>,
    area: Rect,
) {
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let block = themed_block().title(editor.path().display().to_string());
    let inner = block.inner(main);
    let gutter = if editor.numbers {
        editor.line_count().to_string().len() + 1
    } else {
        0
    };
    let (height, width) = (
        inner.height as usize,
        (inner.width as usize).saturating_sub(gutter),
    );
    *page = height.max(1);
    *text_area = Rect::new(inner.x + gutter as u16, inner.y, width as u16, inner.height);
    editor.scroll_to_cursor(height, width);
    let brackets = editor.brackets();
    let lang = syntax::lang(editor.path());
    let mut fence = lang.is_some_and(|l| l.markdown) && editor.in_fence(editor.top);
    let lines: Vec<Line> = editor
        .rows(height, width)
        .into_iter()
        .map(|(l, start, row)| {
            let marks: Vec<usize> = brackets
                .iter()
                .flatten()
                .filter(|p| p.0 == l)
                .filter_map(|p| p.1.checked_sub(start))
                .collect();
            let mut line = highlight(
                row,
                lang,
                &mut fence,
                re,
                editor.selection_cols(l),
                (start, &marks),
            );
            if editor.numbers {
                // continuation rows of a wrapped line get a blank gutter
                let num = if start == 0 || !editor.wrap {
                    format!("{:>1$} ", l + 1, gutter - 1)
                } else {
                    " ".repeat(gutter)
                };
                line.spans
                    .insert(0, Span::styled(num, Style::new().fg(Color::DarkGray)));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).style(PANEL).block(block), main);
    let (dy, dx) = editor.cursor_cell(width);
    if dx < width && dy < height {
        frame.set_cursor_position((text_area.x + dx as u16, inner.y + dy as u16));
    }
    let info = editor_info(editor, note);
    frame.render_widget(Line::raw(info).style(BAR), status);
}

/// `row` in `lang` syntax colors, search hits yellow, selection reversed, see [`fmt::highlight`].
pub(super) fn highlight(
    row: String,
    lang: Option<&Lang>,
    fence: &mut bool,
    re: Option<&Regex>,
    sel: Option<(usize, usize)>,
    cols: (usize, &[usize]),
) -> Line<'static> {
    let spans: Vec<Span> = fmt::highlight(&row, lang, fence, re, sel, cols)
        .into_iter()
        .map(|(t, m)| Span::styled(t, mark_style(m)))
        .collect();
    Line::from(spans)
}

fn mark_style(m: Mark) -> Style {
    let mut st = if m.hit {
        MARK
    } else {
        m.kind.map_or_else(Style::new, syntax_style)
    };
    if m.selected {
        st = st.reversed();
    }
    if m.bracket {
        st = st.bold().underlined();
    }
    st
}
