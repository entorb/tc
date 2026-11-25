//! F-key footers, F9 menu bar and toolbar: tables and their clicks.

use super::{App, Mode};
use crate::{KeyCode, KeyEvent, KeyModifiers};

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

/// Synchronize, the rows focused.
pub(super) const SYNC_FOOTER: [(&str, KeyCode, KeyModifiers); 7] = [
    HELP,
    ("Enter Sync", KeyCode::Enter, KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("Ctrl+R Compare", KeyCode::Char('r'), KeyModifiers::CONTROL),
    ("Space Skip", KeyCode::Char(' '), KeyModifiers::NONE),
    ("Tab Options", KeyCode::Tab, KeyModifiers::NONE),
    CLOSE,
];

/// Find files form: F2-F5 show the tabs, F10 searches, F9 cancels (like multi-rename).
pub(super) const FIND_FORM_FOOTER: [(&str, KeyCode, KeyModifiers); 7] = [
    HELP,
    f("F2 Name", 2),
    f("F3 Content", 3),
    f("F4 Folders", 4),
    f("F5 Size/Date", 5),
    f("F9 Cancel", 9),
    f("F10 Search", 10),
];

/// Synchronize, the options focused.
pub(super) const SYNC_FORM_FOOTER: [(&str, KeyCode, KeyModifiers); 3] = [
    HELP,
    ("Enter Compare", KeyCode::Enter, KeyModifiers::NONE),
    CLOSE,
];

pub(super) const FORM_FOOTER: [(&str, KeyCode, KeyModifiers); 3] = [
    HELP,
    ("Enter Start", KeyCode::Enter, KeyModifiers::NONE),
    CLOSE,
];

/// Multi-rename: F2-F6 show the tabs, F10 runs it (and closes), F9 cancels.
pub(super) const RENAME_FOOTER: [(&str, KeyCode, KeyModifiers); 8] = [
    HELP,
    f("F2 Name", 2),
    f("F3 Replace", 3),
    f("F4 Placeholder", 4),
    f("F5 Clean up", 5),
    f("F6 Editor", 6),
    f("F9 Cancel", 9),
    f("F10 Rename", 10),
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

/// Shift alternates of footer buttons (panels, File Stats); shown while Shift is held.
pub(super) const SHIFT_FOOTER: [(&str, KeyCode, KeyModifiers); 1] =
    [("S-F4 Ext edit", KeyCode::F(4), KeyModifiers::SHIFT)];

pub(super) const C: KeyModifiers = KeyModifiers::CONTROL;

pub(super) const A: KeyModifiers = KeyModifiers::ALT;

pub(super) const S: KeyModifiers = KeyModifiers::SHIFT;

pub(super) const N: KeyModifiers = KeyModifiers::NONE;

/// One menu entry: label with its key, key sent when chosen.
pub type MenuItem = (&'static str, KeyCode, KeyModifiers);

/// F9 menu bar like mc, for systems that swallow keys (macOS): group title and its entries.
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
            // no shortcut: `Null` is never typed, only sent by these entries and F1 help
            ("Symlink", KeyCode::Null, C),
            ("Hard link", KeyCode::Null, C.union(A)),
            ("Properties          Alt+Enter", KeyCode::Enter, A),
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
            ("Directory history   Alt+Down", KeyCode::Down, A),
            ("Bookmarks           Ctrl+B", KeyCode::Char('b'), C),
            ("Add bookmark        Ctrl+D", KeyCode::Char('d'), C),
            ("Popular dirs        Ctrl+Z", KeyCode::Char('z'), C),
            ("Sync browsing       Ctrl+K", KeyCode::Char('k'), C),
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
            ("Select all          Ctrl+A", KeyCode::Char('a'), C),
            (
                "Unselect all        Ctrl+Shift+A",
                KeyCode::Char('A'),
                C.union(S),
            ),
            ("Same extension      Alt+.", KeyCode::Char('.'), A),
            (
                "Restore selection   Ctrl+Shift+R",
                KeyCode::Char('R'),
                C.union(S),
            ),
            ("Compare, select     Ctrl+Y", KeyCode::Char('y'), C),
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
            ("Filter              /", KeyCode::Char('/'), N),
            ("Command line        :", KeyCode::Char(':'), N),
            ("Terminal here       Ctrl+O", KeyCode::Char('o'), C),
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
            (
                "Reopen closed tab   Ctrl+Shift+T",
                KeyCode::Char('T'),
                C.union(S),
            ),
            (
                "Tab to other side   Ctrl+Shift+O",
                KeyCode::Char('O'),
                C.union(S),
            ),
        ],
    ),
];

/// Right-click menu of a panel entry: label with its key, key sent when chosen.
pub const ENTRY_MENU: [MenuItem; 13] = [
    ("View            F3", KeyCode::F(3), N),
    ("Edit            F4", KeyCode::F(4), N),
    ("Copy            F5", KeyCode::F(5), N),
    ("Move            F6", KeyCode::F(6), N),
    ("Rename          F2", KeyCode::F(2), N),
    ("Delete          F8", KeyCode::F(8), N),
    ("Copy files      Ctrl+C", KeyCode::Char('c'), C),
    ("Cut files       Ctrl+X", KeyCode::Char('x'), C),
    ("Paste files     Ctrl+V", KeyCode::Char('v'), C),
    ("Copy paths      Ctrl+Insert", KeyCode::Insert, C),
    ("Same extension  Alt+.", KeyCode::Char('.'), A),
    ("Properties      Alt+Enter", KeyCode::Enter, A),
    ("Terminal here   Ctrl+O", KeyCode::Char('o'), C),
];

/// Font scale one 10 % step up or down, kept within 50 %..300 %.
pub fn zoom_step(zoom: f32, dir: i8) -> f32 {
    ((zoom * 10.0).round() + f32::from(dir)).clamp(5.0, 30.0) / 10.0
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
pub const PROCS_FOOTER: [(&str, KeyCode, KeyModifiers); 8] = [
    ("F1 Help", KeyCode::F(1), KeyModifiers::NONE),
    ("Tab Sort", KeyCode::Tab, KeyModifiers::NONE),
    ("/ Filter", KeyCode::Char('/'), KeyModifiers::NONE),
    ("s Interval", KeyCode::Char('s'), KeyModifiers::NONE),
    ("i Info", KeyCode::Char('i'), KeyModifiers::NONE),
    ("F5 Sample", KeyCode::F(5), KeyModifiers::NONE),
    ("F8 Kill", KeyCode::F(8), KeyModifiers::NONE),
    ("F10 Close", KeyCode::F(10), KeyModifiers::NONE),
];

/// Toolbar button: icon, tooltip, key sent when clicked.
pub type ToolItem = (&'static str, &'static str, KeyCode, KeyModifiers);

/// Toolbar groups (navigation, tools).
/// Icons must exist in egui's `emoji-icon-font`; the rest stays in the F9 menu.
pub const TOOLBAR: [&[ToolItem]; 2] = [
    &[
        ("⏴", "History back (Alt+Left)", KeyCode::Left, A),
        ("⏵", "History forward (Alt+Right)", KeyCode::Right, A),
        ("⏶", "Parent directory (Backspace)", KeyCode::Backspace, N),
        ("🏠", "Home directory (~)", KeyCode::Char('~'), N),
        ("⟳", "Refresh (Ctrl+R)", KeyCode::Char('r'), C),
        ("🖧", "Connect to server (Ctrl+N)", KeyCode::Char('n'), C),
    ],
    &[
        ("⌨", "Run command (:)", KeyCode::Char(':'), N),
        ("🗠", "File stats, ncdu (Ctrl+Space)", KeyCode::Char(' '), C),
        ("🖥", "Process monitor (Ctrl+P)", KeyCode::Char('p'), C),
        ("🔍", "Find files (Ctrl+F)", KeyCode::Char('f'), C),
        (
            "⚖",
            "Compare panels, select newer / single / different (Ctrl+Y)",
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
                        | Mode::Settings(_)
                        | Mode::Props(_)
                ))
    }

    /// Screens that own the window (no menu bar, toolbar or panels): results, sync, File Stats,
    /// processes, multi-rename.
    pub fn sub_screen(&self) -> bool {
        self.stats.is_some()
            || matches!(
                self.mode,
                Mode::Found(_)
                    | Mode::FindForm(_)
                    | Mode::Sync(_)
                    | Mode::Stats(_)
                    | Mode::Procs(_)
                    | Mode::MultiRename(_)
            )
    }

    /// F-key bar of the current screen.
    pub fn footer_keys(&self) -> &'static Footer {
        match &self.mode {
            Mode::View { .. } => &VIEW_FOOTER,
            Mode::Edit { .. } | Mode::AskSave { .. } | Mode::AskExit(_) => &EDIT_FOOTER,
            Mode::Found(_) => &FOUND_FOOTER,
            Mode::Sync(s) if s.editing => &SYNC_FORM_FOOTER,
            Mode::Sync(_) => &SYNC_FOOTER,
            Mode::FindForm(_) => &FIND_FORM_FOOTER,
            Mode::MultiRename(_) => &RENAME_FOOTER,
            Mode::Props(_) => &FORM_FOOTER,
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
}

#[cfg(test)]
mod tests {

    use super::super::menu::{FIND_FORM_FOOTER, FOOTER, MENUS, N};

    use super::super::{App, Mode};

    use crate::{KeyCode, KeyEvent, KeyModifiers};

    use tc_core::panel::Panel;

    use super::super::tests::key;

    #[test]
    fn footer_follows_screen_and_f9_cancels_tool_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let p = || Panel::open(tmp.path()).unwrap();
        let mut app = App::new(p(), p());
        assert_eq!(app.footer_keys(), &FOOTER);
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::FindForm(_)));
        assert_eq!(app.footer_keys(), &FIND_FORM_FOOTER);
        key(&mut app, KeyCode::F(9));
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
    }
}
