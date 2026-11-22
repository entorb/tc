//! F-key footers, F9 menu bar and tc-gui toolbar: tables and their clicks.

use super::{App, Mode};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;

/// Footer bar items of one screen: label, key sent when clicked.
pub(super) type Footer = [(&'static str, KeyCode, KeyModifiers)];

pub(super) const fn f(label: &'static str, n: u8) -> (&'static str, KeyCode, KeyModifiers) {
    (label, KeyCode::F(n), KeyModifiers::NONE)
}

pub(super) const CLOSE: (&str, KeyCode, KeyModifiers) = f("F10 Close", 10);

pub(super) const HELP: (&str, KeyCode, KeyModifiers) = f("F1 Help", 1);

pub(super) const VIEW_FOOTER: [(&str, KeyCode, KeyModifiers); 8] = [
    HELP,
    f("F4 Edit", 4),
    f("F7 Search", 7),
    ("g Goto", KeyCode::Char('g'), KeyModifiers::NONE),
    ("w Wrap", KeyCode::Char('w'), KeyModifiers::NONE),
    ("l Lines", KeyCode::Char('l'), KeyModifiers::NONE),
    ("h Hex", KeyCode::Char('h'), KeyModifiers::NONE),
    CLOSE,
];

pub(super) const EDIT_FOOTER: [(&str, KeyCode, KeyModifiers); 6] = [
    HELP,
    f("F2 Save", 2),
    f("F3 Search", 3),
    f("F5 Format", 5),
    f("F8 Del line", 8),
    CLOSE,
];

pub(super) const FOUND_FOOTER: [(&str, KeyCode, KeyModifiers); 7] = [
    HELP,
    f("F3 View", 3),
    f("F4 Edit", 4),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("Enter Panel", KeyCode::Enter, KeyModifiers::NONE),
    ("Ctrl+G Go to", KeyCode::Char('g'), KeyModifiers::CONTROL),
    CLOSE,
];

pub(super) const SYNC_FOOTER: [(&str, KeyCode, KeyModifiers); 6] = [
    HELP,
    ("Enter Sync", KeyCode::Enter, KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("Ctrl+R Compare", KeyCode::Char('r'), KeyModifiers::CONTROL),
    ("Space Skip", KeyCode::Char(' '), KeyModifiers::NONE),
    CLOSE,
];

pub(super) const FIND_FORM_FOOTER: [(&str, KeyCode, KeyModifiers); 6] = [
    HELP,
    ("Enter Start", KeyCode::Enter, KeyModifiers::NONE),
    f("F2 Save", 2),
    f("F3 Load", 3),
    f("F8 Del", 8),
    CLOSE,
];

pub(super) const FORM_FOOTER: [(&str, KeyCode, KeyModifiers); 3] = [
    HELP,
    ("Enter Start", KeyCode::Enter, KeyModifiers::NONE),
    CLOSE,
];

pub(super) const SETTINGS_FOOTER: [(&str, KeyCode, KeyModifiers); 2] = [HELP, CLOSE];

/// Footer bar items: label, key sent when clicked.
pub const FOOTER: [(&str, KeyCode, KeyModifiers); 10] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
    ("F2 Rename", KeyCode::F(2), KeyModifiers::NONE),
    ("F3 View", KeyCode::F(3), KeyModifiers::NONE),
    ("F4 Edit", KeyCode::F(4), KeyModifiers::NONE),
    ("F5 Copy", KeyCode::F(5), KeyModifiers::NONE),
    ("F6 Move", KeyCode::F(6), KeyModifiers::NONE),
    ("F7 Mkdir", KeyCode::F(7), KeyModifiers::NONE),
    ("F8 Delete", KeyCode::F(8), KeyModifiers::NONE),
    ("F9 Menu", KeyCode::F(9), KeyModifiers::NONE),
    ("F10 Quit", KeyCode::F(10), KeyModifiers::NONE),
];

/// Shift alternates of footer buttons (panels, File Stats); tc-gui shows them while Shift is held.
pub(super) const SHIFT_FOOTER: [(&str, KeyCode, KeyModifiers); 1] =
    [("S-F4 Ext edit", KeyCode::F(4), KeyModifiers::SHIFT)];

pub(super) const C: KeyModifiers = KeyModifiers::CONTROL;

pub(super) const A: KeyModifiers = KeyModifiers::ALT;

pub(super) const S: KeyModifiers = KeyModifiers::SHIFT;

pub(super) const N: KeyModifiers = KeyModifiers::NONE;

/// One menu entry: label with its key, key sent when chosen.
pub type MenuItem = (&'static str, KeyCode, KeyModifiers);

/// F9 menu bar like mc, for terminals that swallow keys (macOS): group title and its entries.
pub const MENUS: [(&str, &[MenuItem]); 5] = [
    (
        "File",
        &[
            ("View                F3", KeyCode::F(3), N),
            ("Edit                F4", KeyCode::F(4), N),
            ("Edit external       Shift+F4", KeyCode::F(4), S),
            ("Copy                F5", KeyCode::F(5), N),
            ("Move                F6", KeyCode::F(6), N),
            ("Rename              F2", KeyCode::F(2), N),
            ("Multi-rename        Ctrl+M", KeyCode::Char('m'), C),
            ("Make directory      F7", KeyCode::F(7), N),
            ("Delete              F8", KeyCode::F(8), N),
            ("Copy paths          Ctrl+Insert", KeyCode::Insert, C),
            ("Copy files          Ctrl+C", KeyCode::Char('c'), C),
            ("Cut files           Ctrl+X", KeyCode::Char('x'), C),
            ("Paste files         Ctrl+V", KeyCode::Char('v'), C),
            ("Quit                F10", KeyCode::F(10), N),
        ],
    ),
    (
        "Go",
        &[
            ("Home directory      ~", KeyCode::Char('~'), N),
            ("Root directory      Ctrl+\\", KeyCode::Char('\\'), C),
            ("Refresh             Ctrl+R", KeyCode::Char('r'), C),
            ("Drives left         Alt+F1", KeyCode::F(1), A),
            ("Drives right        Alt+F2", KeyCode::F(2), A),
            ("History back        Alt+Left", KeyCode::Left, A),
            ("History forward     Alt+Right", KeyCode::Right, A),
            ("Bookmarks           Ctrl+B", KeyCode::Char('b'), C),
            ("Add bookmark        Ctrl+D", KeyCode::Char('d'), C),
            ("Connections         Ctrl+N", KeyCode::Char('n'), C),
            // no shortcut: Shift+`Null` is never typed, only sent by this entry and F1 help
            ("Disconnect all", KeyCode::Null, S),
        ],
    ),
    (
        "Select",
        &[
            ("Select by mask      +", KeyCode::Char('+'), N),
            ("Deselect by mask    -", KeyCode::Char('-'), N),
            ("Invert selection    *", KeyCode::Char('*'), N),
            ("Select differences  Ctrl+Y", KeyCode::Char('y'), C),
        ],
    ),
    (
        "Tools",
        &[
            ("Help                F1", KeyCode::F(1), N),
            ("File stats (ncdu)   Ctrl+Space", KeyCode::Char(' '), C),
            ("Processes (top)     Ctrl+P", KeyCode::Char('p'), C),
            ("Find files          Ctrl+F", KeyCode::Char('f'), C),
            ("Synchronize dirs    Ctrl+E", KeyCode::Char('e'), C),
            ("Go to found file    Ctrl+G", KeyCode::Char('g'), C),
            ("Quick search        Ctrl+S", KeyCode::Char('s'), C),
            ("Quick filter        /", KeyCode::Char('/'), N),
            ("Command line        :", KeyCode::Char(':'), N),
            // no shortcut: `Null` is never typed, only sent by these entries and F1 help
            ("Settings", KeyCode::Null, A),
            ("Delete config files", KeyCode::Null, N),
        ],
    ),
    (
        "Panel",
        &[
            ("Swap panels         Ctrl+U", KeyCode::Char('u'), C),
            ("Other panel here    Alt+=", KeyCode::Char('='), A),
            ("New tab             Ctrl+T", KeyCode::Char('t'), C),
            ("Close tab           Ctrl+W", KeyCode::Char('w'), C),
        ],
    ),
];

/// Font scale one 10 % step up or down, kept within 50 %..300 %.
pub fn zoom_step(zoom: f32, dir: i8) -> f32 {
    ((zoom * 10.0).round() + f32::from(dir)).clamp(5.0, 30.0) / 10.0
}

pub(super) fn menu_title_width(group: usize) -> u16 {
    MENUS[group].0.len() as u16 + 2
}

/// Column of a menu title in the bar (shared by the UI and mouse clicks).
pub fn menu_x(group: usize) -> u16 {
    (0..group).map(menu_title_width).sum()
}

/// Screen area of an open menu group (below the bar, with border).
pub fn menu_drop(group: usize) -> Rect {
    let items = MENUS[group].1;
    let w = items.iter().map(|i| i.0.len()).max().unwrap_or(0) as u16 + 4;
    Rect::new(menu_x(group), 1, w, items.len() as u16 + 2)
}

/// Key a main-help line describes (its first column equals a [`MENUS`] key), if any.
pub fn help_action(line: &str) -> Option<(KeyCode, KeyModifiers)> {
    let col = line.split("  ").next()?.trim();
    MENUS
        .iter()
        .flat_map(|g| g.1)
        .find(|m| m.0.rsplit("  ").next().is_some_and(|k| k.trim() == col))
        .map(|m| (m.1, m.2))
}

/// F-key bar of File Stats (labels and the keys they send).
pub const STATS_FOOTER: [(&str, KeyCode, KeyModifiers); 9] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
    ("F3 View", KeyCode::F(3), KeyModifiers::NONE),
    ("F4 Edit", KeyCode::F(4), KeyModifiers::NONE),
    ("F5 Rescan", KeyCode::F(5), KeyModifiers::NONE),
    ("F8 Delete", KeyCode::F(8), KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("Tab Size/Count", KeyCode::Tab, KeyModifiers::NONE),
    (": Cmd", KeyCode::Char(':'), KeyModifiers::NONE),
    ("F10 Close", KeyCode::F(10), KeyModifiers::NONE),
];

/// F-key bar of the process monitor.
pub const PROCS_FOOTER: [(&str, KeyCode, KeyModifiers); 5] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("s Interval", KeyCode::Char('s'), KeyModifiers::NONE),
    ("F8 Kill", KeyCode::F(8), KeyModifiers::NONE),
    ("F10 Close", KeyCode::F(10), KeyModifiers::NONE),
];

/// Toolbar button: icon, tooltip, key sent when clicked.
pub type ToolItem = (&'static str, &'static str, KeyCode, KeyModifiers);

/// tc-gui toolbar groups (navigation, tools).
/// Icons must exist in egui's `emoji-icon-font`; the rest stays in the F9 menu.
pub const TOOLBAR: [&[ToolItem]; 2] = [
    &[
        ("⏴", "History back (Alt+Left)", KeyCode::Left, A),
        ("⏵", "History forward (Alt+Right)", KeyCode::Right, A),
        ("⏶", "Parent directory (Backspace)", KeyCode::Backspace, N),
        ("🏠", "Home directory (~)", KeyCode::Char('~'), N),
        ("⟳", "Refresh (Ctrl+R)", KeyCode::Char('r'), C),
        ("★", "Bookmarks (Ctrl+B)", KeyCode::Char('b'), C),
        ("✚", "New tab (Ctrl+T)", KeyCode::Char('t'), C),
        ("🖧", "Connect to server (Ctrl+N)", KeyCode::Char('n'), C),
    ],
    &[
        ("⌨", "Run command (:)", KeyCode::Char(':'), N),
        ("🗠", "File stats, ncdu (Ctrl+Space)", KeyCode::Char(' '), C),
        ("🖥", "Process monitor (Ctrl+P)", KeyCode::Char('p'), C),
        ("🔍", "Find files (Ctrl+F)", KeyCode::Char('f'), C),
        (
            "⚖",
            "Compare panels, select differences (Ctrl+Y)",
            KeyCode::Char('y'),
            C,
        ),
        (
            "⬌",
            "Synchronize directories (Ctrl+E)",
            KeyCode::Char('e'),
            C,
        ),
        ("🖊", "Multi-rename (Ctrl+M)", KeyCode::Char('m'), C),
    ],
];

impl App {
    /// Footer labels send keys: on idle screens, and on the find results and forms.
    pub fn footer_active(&self) -> bool {
        self.idle()
            || (self.input.is_none()
                && matches!(
                    self.mode,
                    Mode::Found(_)
                        | Mode::FindForm(_)
                        | Mode::MultiRename(_)
                        | Mode::SyncForm(_)
                        | Mode::Settings(_)
                ))
    }

    /// Click on a footer label sends its key; only where the screen takes keys that way.
    pub(super) fn footer_click(&mut self, ev: MouseEvent) -> bool {
        if !self.footer.contains((ev.column, ev.row).into()) || !self.footer_active() {
            return false;
        }
        let mut x = self.footer.x;
        for &(label, code, mods) in self.footer_keys() {
            let end = x + label.len() as u16;
            if (x..end).contains(&ev.column) {
                self.on_key(KeyEvent::new(code, mods));
                break;
            }
            x = end + 2;
        }
        true
    }

    /// F-key bar of the current screen.
    pub fn footer_keys(&self) -> &'static Footer {
        match &self.mode {
            Mode::View { .. } => &VIEW_FOOTER,
            Mode::Edit { .. } | Mode::AskSave { .. } | Mode::AskExit(_) => &EDIT_FOOTER,
            Mode::Found(_) => &FOUND_FOOTER,
            Mode::Sync(_) => &SYNC_FOOTER,
            Mode::FindForm(_) => &FIND_FORM_FOOTER,
            Mode::MultiRename(_) | Mode::SyncForm(_) => &FORM_FOOTER,
            Mode::Settings(_) => &SETTINGS_FOOTER,
            Mode::Stats(_) => &STATS_FOOTER,
            Mode::Cmd(_) if self.stats.is_some() => &STATS_FOOTER,
            Mode::Procs(_) => &PROCS_FOOTER,
            _ => &FOOTER,
        }
    }

    /// Footer button `item` as shown while Shift is held: its Shift action, if the screen has one.
    pub fn footer_shifted(
        &self,
        item: (&'static str, KeyCode, KeyModifiers),
    ) -> (&'static str, KeyCode, KeyModifiers) {
        let keys = self.footer_keys();
        let alts: &Footer =
            if std::ptr::eq(keys, &FOOTER[..]) || std::ptr::eq(keys, &STATS_FOOTER[..]) {
                &SHIFT_FOOTER
            } else {
                &[]
            };
        alts.iter().find(|a| a.1 == item.1).copied().unwrap_or(item)
    }

    pub(super) fn menu_key(&mut self, code: KeyCode, group: usize, sel: usize) {
        let n = MENUS.len();
        let len = MENUS[group].1.len();
        match code {
            KeyCode::Esc | KeyCode::F(9 | 10) => {}
            KeyCode::Enter => self.menu_run(group, sel),
            KeyCode::Left => self.mode = Mode::Menu((group + n - 1) % n, 0),
            KeyCode::Right => self.mode = Mode::Menu((group + 1) % n, 0),
            KeyCode::Up => self.mode = Mode::Menu(group, (sel + len - 1) % len),
            KeyCode::Down => self.mode = Mode::Menu(group, (sel + 1) % len),
            KeyCode::Home => self.mode = Mode::Menu(group, 0),
            KeyCode::End => self.mode = Mode::Menu(group, len - 1),
            _ => self.mode = Mode::Menu(group, sel),
        }
    }

    pub fn menu_run(&mut self, group: usize, sel: usize) {
        self.mode = Mode::Normal;
        let (_, c, m) = MENUS[group].1[sel];
        self.on_key(KeyEvent::new(c, m));
    }

    /// Left click while the menu bar is shown (row 0) or open. True if the click was used.
    pub(super) fn menu_click(&mut self, ev: MouseEvent) -> bool {
        let (col, row) = (ev.column, ev.row);
        let open = if let Mode::Menu(g, _) = self.mode {
            Some(g)
        } else {
            None
        };
        if row == 0
            && let Some(g) = (0..MENUS.len()).find(|&g| {
                let x = menu_x(g);
                (x..x + menu_title_width(g)).contains(&col)
            })
        {
            self.mode = if open == Some(g) {
                Mode::Normal
            } else {
                Mode::Menu(g, 0)
            };
            return true;
        }
        let Some(g) = open else { return false };
        let r = menu_drop(g);
        if r.contains((col, row).into()) {
            // rows inside the border are entries
            if row > r.y && row + 1 < r.y + r.height && col > r.x && col + 1 < r.x + r.width {
                self.menu_run(g, usize::from(row - r.y - 1));
            }
        } else {
            self.mode = Mode::Normal;
        }
        true
    }
}

#[cfg(test)]
mod tests {

    use super::super::menu::{FIND_FORM_FOOTER, FOOTER, MENUS, N, menu_x};

    use super::super::{App, Mode};

    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use tc_core::panel::Panel;

    use super::super::tests::key;

    #[test]
    fn footer_follows_screen_and_f10_closes_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        assert_eq!(app.footer_keys(), &FOOTER);
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::FindForm(_)));
        assert_eq!(app.footer_keys(), &FIND_FORM_FOOTER);
        key(&mut app, KeyCode::F(10));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn f9_menu_runs_the_chosen_action() {
        let tmp = tempfile::tempdir().unwrap();
        let open = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(open(), open());
        app.on_key(KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Menu(..)));
        let i = MENUS[0]
            .1
            .iter()
            .position(|m| m.1 == KeyCode::F(7) && m.2 == N)
            .unwrap();
        for _ in 0..i {
            app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Mkdir(_)));

        // click the "Go" title, then its first entry (home directory)
        app.mode = Mode::Normal;
        let click = |col, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(click(menu_x(1), 0));
        assert!(matches!(app.mode, Mode::Menu(1, _)));
        app.on_mouse(click(menu_x(1) + 2, 2));
        assert!(matches!(app.mode, Mode::Normal));
    }
}
