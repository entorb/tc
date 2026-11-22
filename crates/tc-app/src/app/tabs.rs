//! Panel tabs and mouse input on the panels.

use super::{App, Mode};
use crate::fmt::{DATE_WIDTH, SIZE_WIDTH};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::widgets::ListState;
use std::mem;
use std::path::Path;
use std::time::{Duration, Instant};
use tc_core::archive::Format;
use tc_core::panel::SortKey;

impl App {
    /// Tabs per side: `panels[side]` is the visible tab, rotated through the ring in `tabs[side]`.
    pub(super) fn tab_key(&mut self, code: KeyCode) {
        let side = self.active;
        let bg = &mut self.tabs[side];
        let n = bg.len() + 1;
        let next = match code {
            KeyCode::Char('t') => match self.panels[side].duplicate() {
                Ok(p) => {
                    bg.push_back(mem::replace(&mut self.panels[side], p));
                    self.tab[side] += 1;
                    self.lists[side] = ListState::default();
                    return;
                }
                Err(e) => {
                    self.mode = Mode::Message(e.to_string());
                    return;
                }
            },
            KeyCode::Char('w') => {
                let Some(p) = bg.pop_front() else { return };
                self.panels[side] = p;
                self.tab[side] %= n - 1;
                None
            }
            KeyCode::PageDown | KeyCode::Tab => bg.pop_front().map(|p| {
                self.tab[side] = (self.tab[side] + 1) % n;
                (p, false)
            }),
            _ => bg.pop_back().map(|p| {
                self.tab[side] = (self.tab[side] + n - 1) % n;
                (p, true)
            }),
        };
        if let Some((p, prev)) = next {
            let old = mem::replace(&mut self.panels[side], p);
            if prev {
                self.tabs[side].push_front(old);
            } else {
                self.tabs[side].push_back(old);
            }
        } else if code != KeyCode::Char('w') {
            return;
        }
        self.lists[side] = ListState::default();
        // the directory may have changed while the tab was hidden
        if let Err(e) = self.panels[side].reload() {
            self.mode = Mode::Message(e.to_string());
        }
    }

    /// Directory names of one side's tabs in cycle order; `self.tab[side]` is the visible one.
    pub fn tab_names(&self, side: usize) -> Vec<String> {
        let (bg, p) = (&self.tabs[side], self.tab[side]);
        let n = bg.len() + 1;
        (0..n)
            .map(|k| {
                let panel = if k == p {
                    &self.panels[side]
                } else {
                    &bg[(k + n - p - 1) % n]
                };
                panel
                    .path()
                    .file_name()
                    .map_or_else(|| panel.title(), |name| name.to_string_lossy().into_owned())
            })
            .collect()
    }

    /// Shows tab `k` of `side` (click on a tab button).
    /// Close tab `k` of `side` (tc-gui right click); the last tab of a side stays.
    pub fn close_tab(&mut self, side: usize, k: usize) {
        if !self.tabs[side].is_empty() {
            self.select_tab(side, k);
            self.tab_key(KeyCode::Char('w'));
        }
    }

    pub fn select_tab(&mut self, side: usize, k: usize) {
        self.active = side;
        let n = self.tabs[side].len() + 1;
        for _ in 0..(k + n - self.tab[side]) % n {
            self.tab_key(KeyCode::PageDown);
        }
    }

    /// Wheel / touchpad scroll in viewer, editor and active file list (editor: moves the cursor, the window follows).
    pub fn on_mouse(&mut self, ev: MouseEvent) {
        if self.help {
            if ev.kind == MouseEventKind::Down(MouseButton::Left)
                && self.help_area.contains((ev.column, ev.row).into())
            {
                let row = usize::from(ev.row - self.help_area.y);
                self.help_click(self.help_scroll + row);
            }
            return;
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left) && self.footer_click(ev) {
            return;
        }
        if let MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) =
            ev.kind
            && self.input.is_none()
            && self.replacing.is_none()
            && let Mode::Edit {
                editor,
                error: None,
            } = &mut self.mode
        {
            let a = self.edit_text;
            if a.contains((ev.column, ev.row).into()) {
                let extend = ev.kind != MouseEventKind::Down(MouseButton::Left)
                    || ev.modifiers.contains(KeyModifiers::SHIFT);
                editor.click(
                    usize::from(ev.row - a.y),
                    usize::from(ev.column - a.x),
                    usize::from(a.width),
                    extend,
                );
            }
            return;
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left) {
            if self.input.is_none()
                && matches!(self.mode, Mode::Normal | Mode::Menu(..))
                && self.menu_click(ev)
            {
                return;
            }
            return self.click(ev);
        }
        let lines = match ev.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return,
        };
        if self.input.is_some() {
            return;
        }
        match &mut self.mode {
            Mode::View { view, .. } => {
                if let Err(e) = view.scroll(lines) {
                    self.mode = Mode::Message(e.to_string());
                }
            }
            Mode::Edit {
                editor,
                error: None,
            } => editor.move_v(lines),
            Mode::Procs(p) if p.idle() => {
                let last = p.rows.len().saturating_sub(1);
                p.sel = p.sel.saturating_add_signed(lines).min(last);
            }
            // the panel under the pointer scrolls, the active panel stays
            Mode::Normal => {
                let at = (ev.column, ev.row).into();
                let i = (0..2).find(|&i| self.areas[i].contains(at));
                self.panels[i.unwrap_or(self.active)].move_cursor(lines);
            }
            _ => {}
        }
    }

    /// Left click on a file row moves the cursor there (and activates the panel); with Shift it selects the range from the old cursor.
    pub(super) fn click(&mut self, ev: MouseEvent) {
        if self.input.is_some() || !matches!(self.mode, Mode::Normal) {
            return;
        }
        let Some(i) = (0..2).find(|&i| self.areas[i].contains((ev.column, ev.row).into())) else {
            return;
        };
        let area = self.areas[i];
        if ev.row == area.y {
            // title row: columns of the size and date fields sort by them
            let col = usize::from(ev.column - area.x).saturating_sub(1);
            let w = usize::from(area.width).saturating_sub(2);
            let key = if col >= w.saturating_sub(DATE_WIDTH) {
                SortKey::Date
            } else if col >= w.saturating_sub(DATE_WIDTH + 1 + SIZE_WIDTH) {
                SortKey::Size
            } else {
                SortKey::Name
            };
            return self.sort_by(i, key);
        }
        // first row is the border
        let Some(row) = usize::from(ev.row - area.y).checked_sub(1) else {
            return;
        };
        let idx = self.lists[i].offset() + row;
        if idx >= self.panels[i].entries().len()
            || row >= usize::from(area.height.saturating_sub(2))
        {
            return;
        }
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        let now = Instant::now();
        let double = !shift
            && self.last_click.is_some_and(|(t, c, r)| {
                (c, r) == (ev.column, ev.row) && now.duration_since(t) < Duration::from_millis(400)
            });
        if !shift {
            self.last_click = (!double).then_some((now, ev.column, ev.row));
        }
        self.click_entry(i, idx, shift, double);
    }

    /// Sort panel `i` by `key` (same key again reverses) and make it active.
    pub fn sort_by(&mut self, i: usize, key: SortKey) {
        self.active = i;
        self.panels[i].set_sort(key);
    }

    /// Click on entry `idx` of panel `i`: cursor there, Shift selects the range from the old cursor, double click opens.
    pub fn click_entry(&mut self, i: usize, idx: usize, shift: bool, double: bool) {
        if self.input.is_some() || !matches!(self.mode, Mode::Normal) {
            return;
        }
        self.active = i;
        let panel = &mut self.panels[i];
        if shift {
            panel.select_range(panel.cursor(), idx);
            return;
        }
        panel.set_cursor(idx);
        if double {
            let is_dir = self.panels[i]
                .current()
                .is_some_and(|e| e.is_dir || Format::detect(Path::new(&e.name)).is_some());
            let code = if is_dir {
                KeyCode::Enter
            } else {
                KeyCode::F(3)
            };
            self.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }
}
