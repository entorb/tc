//! Frontend-neutral text: help, sizes, dates, progress.

mod help;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use regex::Regex;
use tc_core::cmdline::LineEdit;
use tc_core::edit::{Editor, Eol, human};
use tc_core::find::{Hit, MAX_RESULTS};
use tc_core::panel::{Entry, SortKey};
use tc_core::stats::treemap::Kind as TileKind;
use tc_core::stats::{self, Node, Order};
use tc_core::syntax::{self, Kind, Lang};
use tc_remote::Prompt;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    App, Filter, FormRow, Found, Input, ListRow, Mode, Procs, Stats, Step, Sync, Xfer, list_rows,
};
use tc_core::procs::{Proc, ProcSort};
use tc_core::sync::Item;

pub use help::{
    HELP_CONFLICT, HELP_EDIT, HELP_FIND_FORM, HELP_FOUND, HELP_LISTS, HELP_MAIN, HELP_PROCS,
    HELP_RENAME, HELP_SETTINGS, HELP_STATS, HELP_SYNC, HELP_SYNC_FORM, HELP_VIEW, HelpLine, help,
    help_filter, help_line, key_help,
};

/// Length of "YY-MM-DD HH:MM".
pub const DATE_WIDTH: usize = 14;
/// Length of "1023.9 K".
pub const SIZE_WIDTH: usize = 8;

/// `1234567` as `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `s` cut to `w` display columns with a trailing `…`, or padded with spaces to `w`.
pub fn fit(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    if s.width() <= w {
        out.push_str(s);
        used = s.width();
    } else {
        for c in s.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > w - 1 {
                break;
            }
            out.push(c);
            used += cw;
        }
        out.push('…');
        used += 1;
    }
    out.extend(std::iter::repeat_n(' ', w - used));
    out
}

/// Time only for today's files, else short date and time.
pub fn fmt_date(z: &jiff::ZonedDateTime, today: jiff::civil::Date) -> String {
    let fmt = if z.date() == today {
        "%H:%M:%S"
    } else {
        "%y-%m-%d %H:%M"
    };
    z.strftime(fmt).to_string()
}

pub fn progress_text(x: &Xfer) -> String {
    let p = &x.progress;
    let (done, files) = (
        p.done.load(Ordering::Relaxed),
        p.files.load(Ordering::Relaxed),
    );
    let current = p
        .current
        .lock()
        .map(|c| c.display().to_string())
        .unwrap_or_default();
    let (total, of) = if p.scanned.load(Ordering::Acquire) {
        let (t, f) = (
            p.total.load(Ordering::Relaxed),
            p.files_total.load(Ordering::Relaxed),
        );
        (human(t), format!("{} / {f}", files.min(f)))
    } else {
        ("scanning...".to_owned(), files.to_string())
    };
    let remaining = p.total.load(Ordering::Relaxed).saturating_sub(done) as f64;
    let speed = x
        .eta
        .rate()
        .map_or("--".to_owned(), |r| format!("{}/s", human(r as u64)));
    let eta = match x.eta.eta(remaining) {
        Some(s) if p.scanned.load(Ordering::Acquire) => hms(s as u64),
        _ => "--".to_owned(),
    };
    format!(
        "{current}\n{} / {total}   files {of}\n{speed}   ETA {eta}\n\n[Esc] Cancel",
        human(done)
    )
}

pub fn hms(s: u64) -> String {
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, m, s) => format!("{m}:{s:02}"),
        (h, m, s) => format!("{h}:{m:02}:{s:02}"),
    }
}

/// Popup content of the current mode, rendered by each front end in its own look.
pub enum Dialog<'a> {
    Text {
        title: &'a str,
        text: String,
    },
    /// Text field `line` between `head` and `tail` lines.
    Edit {
        title: &'a str,
        head: String,
        line: LineEdit,
        tail: String,
    },
    /// List with the selected row, `empty` shown when there are no rows.
    List {
        title: String,
        rows: Vec<String>,
        /// Position in `rows`.
        sel: usize,
        empty: &'static str,
        keys: &'static str,
    },
}

const OK_CANCEL: &str = "[Enter] OK   [Esc] Cancel";

fn edit<'a>(title: &'a str, head: &str, l: &LineEdit, tail: &str) -> Option<Dialog<'a>> {
    Some(Dialog::Edit {
        title,
        head: head.to_owned(),
        line: l.clone(),
        tail: tail.to_owned(),
    })
}

/// List of paths with the rows `filter` keeps; `list.2` is a row of `list.1`.
fn list_dialog<'a>(
    list: (&str, &[impl ListRow], usize),
    filter: &Filter,
    empty: &'static str,
    keys: &'static str,
) -> Option<Dialog<'a>> {
    let (title, rows, sel) = list;
    let view = list_rows(rows, filter);
    Some(Dialog::List {
        title: format!("{title}{}", filter.tag()),
        sel: view.iter().position(|&i| i == sel).unwrap_or(0),
        rows: view.iter().map(|&i| rows[i].label().into_owned()).collect(),
        empty,
        keys,
    })
}

fn text<'a>(title: &'a str, text: String) -> Option<Dialog<'a>> {
    Some(Dialog::Text { title, text })
}

/// Popup of `app.mode`; `None` for modes without one (Found and Stats draw full screen).
pub fn dialog(app: &App) -> Option<Dialog<'_>> {
    match &app.mode {
        Mode::Confirm(op, _, t) => text(op.verb(), t.clone()),
        Mode::ResetConfig(dir) => text(
            "Delete config files",
            format!(
                "Delete tc's files in {}?\n{}\nOther files there stay.\n\n[y] Delete   [Esc] Cancel",
                dir.display(),
                tc_core::settings::FILES.join(", ")
            ),
        ),
        Mode::Transfer(op, targets, l) => edit(
            op.verb(),
            &format!(
                "{} {} item(s) to (edit name or directory):",
                op.verb(),
                targets.len()
            ),
            l,
            OK_CANCEL,
        ),
        Mode::Mkdir(l) => edit("Make directory", "", l, OK_CANCEL),
        Mode::NewFile(l) => edit(
            "New file (created if missing, opened in editor)",
            "",
            l,
            OK_CANCEL,
        ),
        Mode::Editor(_, l) => edit("Editor not found, enter editor command", "", l, OK_CANCEL),
        Mode::Hotlist(sel) => list_dialog(
            ("Hotlist", &app.hotlist, *sel),
            &app.list_filter,
            "(empty, Insert or Ctrl+D adds the current directory)",
            "Enter go  Insert add  F4 alias  Ctrl+Up/Dn move  Del/Bksp remove  / filter  Esc close",
        ),
        Mode::Drives(rows, sel, _) => list_dialog(
            ("Drives", rows, *sel),
            &Filter::default(),
            "(no drives found)",
            "Enter go  Esc close",
        ),
        Mode::Connections(sel) => list_dialog(
            ("Connections", &app.connections, *sel),
            &app.list_filter,
            "(empty, Insert adds one)",
            "Enter connect  Insert add  F4 edit  Del/Bksp remove  / filter  Esc close",
        ),
        Mode::HotlistAlias(row, l) => edit(
            "Bookmark alias",
            &format!(
                "{}\n(empty removes the alias)",
                app.hotlist[*row].path.to_string_lossy()
            ),
            l,
            "[Enter] Save   [Esc] Cancel",
        ),
        Mode::ConnEdit(_, l) => edit(
            "Connection",
            "sftp://user@host[:port]/dir, ftp://..., ftps://...\n(no passwords, they are asked on connect)",
            l,
            "[Enter] Save   [Esc] Cancel",
        ),
        Mode::Connecting(c) => match &c.ask {
            Some((Prompt::HostKey(t), _)) => text(
                "Host key",
                format!("{t}\n\n[y] Trust and save to known_hosts   [Esc] Cancel"),
            ),
            Some((Prompt::Password(label), l)) => {
                let masked = LineEdit::from("*".repeat(l.text.chars().count()));
                edit(label, "", &masked, OK_CANCEL)
            }
            None => text("Connecting", format!("{}\n\n[Esc] Cancel", c.url)),
        },
        Mode::Message(msg) => text("Error", format!("{msg}\n\nany key")),
        Mode::Update { tag, check } => text(
            "Update available",
            format!(
                "tc {tag} is available (running {}).\n\n{} Check for updates daily\n\n[Enter] Open release page   [Space] Toggle   [Esc] Close",
                crate::app::VERSION.unwrap_or("?"),
                if *check { "[x]" } else { "[ ]" }
            ),
        ),
        Mode::Edit {
            error: Some(msg), ..
        } => text("Error", format!("{msg}\n\nany key")),
        Mode::Work(job) if job.visible() => {
            text(job.label, format!("{}%\n\n[Esc] Cancel", job.percent()))
        }
        Mode::Xfer(x) => xfer_dialog(x),
        Mode::AskViewer { text: t, .. } => text("Edit", t.clone()),
        Mode::AskSave { editor, step, .. } => {
            let t = match step {
                Step::Changed => "File changed on disk. Overwrite it?".to_owned(),
                Step::Encoding => format!("File is {}. Convert to UTF-8?", editor.encoding_name()),
                Step::Eol => "Convert line endings to LF?".to_owned(),
            };
            text("Save", format!("{t}\n\n[y] Yes   [n] No   [Esc] Cancel"))
        }
        Mode::AskQuit => text("Quit", "Quit tc?\n\n[y] Quit   [Esc] Cancel".to_owned()),
        Mode::AskExit(_) => text(
            "Edit",
            "Save changes?\n\n[y] Yes   [n] No   [Esc] Cancel".to_owned(),
        ),
        _ => None,
    }
}

fn xfer_dialog(x: &Xfer) -> Option<Dialog<'_>> {
    if let Some(l) = &x.rename {
        edit("Rename", "", l, OK_CANCEL)
    } else if let Some((_, target)) = &x.ask {
        text(
            "File exists",
            format!(
                "{}\n\n[o] Overwrite   [s] Skip   [r] Rename   [n] Newer\n[O] Overwrite all   [S] Skip all   [N] Newer all\n[Esc] Cancel",
                target.display()
            ),
        )
    } else if x.visible() {
        text("Working", progress_text(x))
    } else {
        None
    }
}

/// Text prompt of the viewer, editor or panel (`app.input`).
pub fn input_dialog(input: &Input) -> Dialog<'_> {
    let error = input.error.as_deref().unwrap_or_default();
    Dialog::Edit {
        title: input.ask.title(),
        head: String::new(),
        line: input.text.clone(),
        tail: format!("{error}\n{OK_CANCEL}"),
    }
}

/// Size of a directory entry of panel `i`: calculating, calculated, or `None`.
fn dir_size(app: &App, i: usize, e: &Entry) -> Option<String> {
    let panel = &app.panels[i];
    if !e.is_dir || e.is_parent() || panel.archive().is_some() {
        return None;
    }
    match &app.dir_job {
        Some((side, p, _)) if *side == i && *p == panel.path().join(&e.name) => Some("...".into()),
        _ => panel.dir_size(e).map(human),
    }
}

/// Modification date of `e` for the panel list.
pub fn date_of(e: &Entry, today: jiff::civil::Date) -> String {
    e.modified
        .and_then(|t| jiff::ZonedDateTime::try_from(t).ok())
        .map_or_else(String::new, |z| fmt_date(&z, today))
}

/// Name with type prefix (`/` dir, `~` dir symlink, `@` file symlink), size and date of a panel row.
pub fn entry_cols(app: &App, i: usize, e: &Entry, today: jiff::civil::Date) -> [String; 3] {
    let prefix = match (e.is_symlink, e.is_dir) {
        (true, true) => '~',
        (true, false) => '@',
        (false, true) => '/',
        _ => ' ',
    };
    let size = if e.is_dir {
        dir_size(app, i, e).unwrap_or_else(|| "<DIR>".to_owned())
    } else {
        human(e.size)
    };
    // found lists hold full paths: show them below the search root
    let name = match Path::new(&e.name).strip_prefix(app.panels[i].path()) {
        Ok(rel) if app.panels[i].is_virtual() => rel.as_os_str(),
        _ => e.name.as_os_str(),
    };
    [
        format!("{prefix}{}", name.to_string_lossy()),
        size,
        date_of(e, today),
    ]
}

/// Panel title: tab, path and active flags (found, loading, sort, hidden, search, filter).
pub fn panel_title(app: &App, i: usize) -> String {
    let panel = &app.panels[i];
    let mut title = panel.title();
    let tabs = app.tabs[i].len() + 1;
    if tabs > 1 {
        title = format!("[{}/{tabs}] {title}", app.tab[i] + 1);
    }
    if panel.is_virtual() {
        title += " [found]";
    }
    if let Some(dir) = panel.loading() {
        title += &format!(" [loading {}, Esc cancels]", dir.display());
    }
    match panel.sort() {
        (SortKey::Name, false) => {}
        (key, rev) => {
            let key = format!("{key:?}").to_lowercase();
            title += &format!(" [{key}{}]", if rev { " desc" } else { "" });
        }
    }
    let active = i == app.active;
    match &app.mode {
        Mode::Search(t) if active => title += &format!(" [search: {t}]"),
        Mode::Filter(t) if active => title += &format!(" [filter: {t}]"),
        _ if !panel.filter().is_empty() => title += &format!(" [filter: {}]", panel.filter()),
        _ => {}
    }
    title
}

/// Status line of panel `i`: selection sum (cached in the panel), else the cursor entry in full.
pub fn panel_info(app: &App, i: usize) -> String {
    let panel = &app.panels[i];
    let (count, bytes) = panel.selection_info();
    match panel.current().filter(|e| !e.is_parent()) {
        _ if count > 0 => format!(" {count} selected, {} ", human(bytes)),
        Some(e) => {
            let size = match dir_size(app, i, e) {
                Some(s) => s,
                None if e.is_dir => "<DIR>".to_owned(),
                None => format!("{} B", thousands(e.size)),
            };
            let link = e
                .link
                .as_ref()
                .map_or(String::new(), |l| format!("  -> {}", l.display()));
            let date = e
                .modified
                .and_then(|t| jiff::ZonedDateTime::try_from(t).ok())
                .map_or_else(String::new, |z| z.strftime("%y-%m-%d %H:%M:%S").to_string());
            format!(" {}  {size}  {date}{link} ", e.name.to_string_lossy())
        }
        None => String::new(),
    }
}

/// Title of the find results list.
pub fn found_title(f: &Found) -> String {
    let state = if f.running { "searching" } else { "done" };
    let cap = if f.list.len() >= MAX_RESULTS {
        ", capped"
    } else {
        ""
    };
    let err = match f.error.lock().ok().and_then(|e| e.clone()) {
        Some(e) => format!(", {e}"),
        None => String::new(),
    };
    let shown = if f.filter.active() || f.filter.editing {
        format!("{} of ", f.shown_len())
    } else {
        String::new()
    };
    let sort = match f.sort {
        Some((key, rev)) => {
            let key = format!("{key:?}").to_lowercase();
            format!(" [{key}{}]", if rev { " desc" } else { "" })
        }
        None => String::new(),
    };
    format!(
        " Find: {shown}{} found, {state}{cap}{err}{sort}{} ",
        f.list.len(),
        f.filter.tag()
    )
}

/// Find result row: size (blank for directories), full path and the matching line if any.
pub fn found_row(h: &Hit) -> String {
    let e = &h.entry;
    let size = if e.is_dir {
        String::new()
    } else {
        human(e.size)
    };
    let name = e.name.to_string_lossy();
    match &h.line {
        Some((n, text)) => format!("{size:>9}  {name}:{n}: {text}"),
        None => format!("{size:>9}  {name}"),
    }
}

/// Keys shown under the settings form.
pub const SETTINGS_FORM_KEYS: &str = "Space checkbox  Esc close";

/// Keys shown under the sync form.
pub const SYNC_FORM_KEYS: &str =
    "Tab/Up/Down field  Alt+Up/Down history  Space checkbox  Enter compare  Esc cancel";

/// Sync screen title: both directories and the options.
pub fn sync_title(s: &Sync) -> String {
    let side = |i: usize| {
        let url = s.sides[i].remote().map_or("", |r| r.url());
        format!("{url}{}", s.roots[i].display())
    };
    let hidden: String = (["->", "=", "!=", "<-"].iter().zip(s.show))
        .filter(|(_, on)| !on)
        .map(|(k, _)| format!(" {k}"))
        .collect();
    let hidden = if hidden.is_empty() {
        String::new()
    } else {
        format!("  hidden:{hidden}")
    };
    format!(
        " Sync: {}  <->  {}{hidden}{} ",
        side(0),
        side(1),
        s.filter.tag()
    )
}

/// Sync row: left size and date, action, right size and date, path.
pub fn sync_row(it: &Item, today: jiff::civil::Date) -> String {
    let side = |e: &Option<Entry>| match e {
        Some(e) if e.is_dir => {
            format!("{:>SIZE_WIDTH$} {:DATE_WIDTH$}", "<DIR>", date_of(e, today))
        }
        Some(e) => format!(
            "{:>SIZE_WIDTH$} {:DATE_WIDTH$}",
            human(e.size),
            date_of(e, today)
        ),
        None => " ".repeat(SIZE_WIDTH + 1 + DATE_WIDTH),
    };
    let slash = if it.is_dir() { "/" } else { "" };
    format!(
        "{} {:^5} {}  {}{slash}",
        side(&it.left),
        it.action.symbol(),
        side(&it.right),
        it.rel.display()
    )
}

/// Message of `c` on a sync row: equal, first difference, or the error.
pub fn sync_diff_text(
    rel: &Path,
    sizes: [u64; 2],
    res: &tc_core::Result<Option<(u64, u64)>>,
) -> String {
    let rel = rel.display();
    match res {
        Ok(None) => format!("Equal content: {rel} ({})", human(sizes[0])),
        Ok(Some((at, line))) => {
            let mut text = format!(
                "Different: {rel}\nFirst difference at byte {} (line {line}).",
                thousands(*at)
            );
            if sizes[0] != sizes[1] {
                text += &format!("\nLeft {}, right {}.", human(sizes[0]), human(sizes[1]));
            }
            text
        }
        Err(e) => format!("Compare: {e}"),
    }
}

/// Sync status line: compare progress, else counts and keys.
pub fn sync_footer(s: &Sync) -> String {
    if let Some(rel) = s.diffing() {
        let p = &s.progress;
        return format!(
            "comparing {}: {} of {}  [Esc] Cancel",
            rel.display(),
            human(p.done.load(Ordering::Relaxed)),
            human(p.total.load(Ordering::Relaxed))
        );
    }
    if s.comparing() {
        return format!(
            "comparing: {} names, {} dirs  [Esc] Cancel",
            thousands(s.progress.files.load(Ordering::Relaxed)),
            thousands(s.progress.dirs_done.load(Ordering::Relaxed))
        );
    }
    let mut n = [0; 8];
    for it in s.shown() {
        n[it.action as usize] += 1;
    }
    format!(
        "{} shown of {}: = {}  != {}  -> {}  <- {}  del {}  -- {}   Left/Right direction  l/r/b delete  c compare file  > = ! < show",
        s.view.len(),
        s.items.len(),
        n[0],
        n[1],
        n[2],
        n[3],
        n[4] + n[6] + n[7],
        n[5]
    )
}

/// Keys shown under the find form.
pub const FIND_FORM_KEYS: &str = "Tab/Up/Down field  Space checkbox  Enter search  Esc cancel
F2 save as \"Saved search\"  F3 load next saved  F8 delete saved
Alt+Up/Down earlier searches";

/// Preview rows under the multi-rename form; PgUp/PgDn scroll by this.
pub const RENAME_PREVIEW_ROWS: usize = 12;

/// Keys shown under the multi-rename form.
pub const RENAME_FORM_KEYS: &str = "Tab/Up/Down field  Space/Left/Right option  PgUp/PgDn list
Enter rename  Esc cancel  Alt+Up/Down earlier settings
F2 save preset  F3 load next  F8 delete  F4 edit names  Ctrl+Z undo last
[N] name [E] ext ([N2-5] [N2,3] [N-3-] chars)  [C] counter
[Y] [M] [D] [h] [n] [s] modified date/time  [[] = [";

/// Form lines as (text, focused), the caret after the focused field's text.
pub fn form_lines(rows: &[FormRow], focused: usize) -> Vec<(String, bool)> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let focus = i == focused;
            let text = match row {
                FormRow::Choice(label, opts, i) => format!("{label:<18} < {} >", opts[*i]),
                FormRow::Check(label, on) => {
                    format!("[{}] {label}", if *on { "x" } else { " " })
                }
                FormRow::Text(label, l) => {
                    let value = if focus {
                        format!("{}_{}", &l.text[..l.cur], &l.text[l.cur..])
                    } else {
                        l.text.clone()
                    };
                    format!("{label:<18} {value}")
                }
            };
            (text, focus)
        })
        .collect()
}

/// Multi-rename preview row: `old -> new`, or `old  ! why` (bool: is an error).
pub fn rename_row(old: &str, new: &Result<String, String>) -> (String, bool) {
    match new {
        Ok(n) if n == old => (format!("{old}  (unchanged)"), false),
        Ok(n) => (format!("{old}  ->  {n}"), false),
        Err(e) if e.is_empty() => (old.to_owned(), false),
        Err(e) => (format!("{old}  ! {e}"), true),
    }
}

/// Treemap tile color of a file type, darkened 18% per `shade` step (nested or alternating tiles).
pub fn kind_rgb(k: TileKind, shade: u8) -> (u8, u8, u8) {
    let (r, g, b) = match k {
        TileKind::Dir => (96, 96, 112),
        TileKind::Image => (230, 200, 60),
        TileKind::Video => (200, 90, 200),
        TileKind::Audio => (80, 190, 210),
        TileKind::Archive => (90, 180, 90),
        TileKind::Document => (80, 120, 220),
        TileKind::Code => (230, 140, 60),
        TileKind::Exec => (210, 70, 70),
        TileKind::Other => (170, 170, 170),
    };
    let f = (1.0 - 0.18 * f32::from(shade.min(4))).max(0.0);
    let d = |c: u8| (f32::from(c) * f) as u8;
    (d(r), d(g), d(b))
}

/// File Stats row: value, share, text bar and name.
pub fn stats_row(st: &Stats, n: &Node) -> String {
    const BAR_WIDTH: usize = 10;
    let value = |n: &Node| if st.by_files { n.files } else { n.size };
    let total = value(st.current()).max(1);
    let share = value(n) as f64 / total as f64;
    let filled = ((share * BAR_WIDTH as f64).round() as usize).min(BAR_WIDTH);
    let bar = "#".repeat(filled) + &" ".repeat(BAR_WIDTH - filled);
    let shown = if st.by_files {
        thousands(value(n))
    } else {
        human(value(n))
    };
    let slash = if n.is_dir { "/" } else { "" };
    let mark = if st.is_marked(n) { '*' } else { ' ' };
    format!(
        "{mark}{shown:>11} {:>5.1}% [{bar}] {}{slash}",
        share * 100.0,
        n.name.to_string_lossy()
    )
}

/// File Stats info popup text; `modified` only when known without a network round trip.
pub fn stats_info(path: &Path, n: &Node, modified: Option<SystemTime>) -> String {
    let (kind, dirs) = if n.is_dir {
        (
            "directory",
            format!("\nDirectories: {}", thousands(stats::dirs(n))),
        )
    } else {
        ("file", String::new())
    };
    let date = modified
        .and_then(|t| jiff::ZonedDateTime::try_from(t).ok())
        .map_or_else(String::new, |z| {
            format!("\nModified:    {}", z.strftime("%Y-%m-%d %H:%M"))
        });
    format!(
        "Path:        {}\nType:        {kind}\nSize:        {} ({} bytes)\nFiles:       {}{dirs}{date}",
        path.display(),
        human(n.size),
        thousands(n.size),
        thousands(n.files)
    )
}

/// File Stats treemap tooltip: path below the browsed directory, size, files of a directory.
pub fn stats_tip(rel: &Path, n: &Node) -> String {
    let size = format!("Size:  {}", human(n.size));
    if n.is_dir {
        format!("{}/\n{size}\nFiles: {}", rel.display(), thousands(n.files))
    } else {
        format!("{}\n{size}", rel.display())
    }
}

/// File Stats title: directory, sort and filter.
pub fn stats_title(st: &Stats) -> String {
    let key = match st.order {
        Order::Name => "name",
        Order::Size => "size",
        Order::Count => "count",
    };
    let rev = if st.rev { " rev" } else { "" };
    format!(
        "Stats: {} [{key}{rev}]{}",
        st.path().display(),
        st.filter.tag()
    )
}

/// File Stats footer: scan progress, else totals and keys.
/// Scan progress shown centered while File Stats reads the tree.
pub fn stats_progress(st: &Stats) -> Option<String> {
    st.scanning().then(|| {
        format!(
            "{} files\n{} / {} dirs\n\n[Esc] Cancel",
            thousands(st.progress.files.load(Ordering::Relaxed)),
            thousands(st.progress.dirs_done.load(Ordering::Relaxed)),
            thousands(st.progress.dirs.load(Ordering::Relaxed))
        )
    })
}

pub fn stats_footer(st: &Stats) -> String {
    if st.scanning() {
        return "scanning...  [Esc] Cancel".to_owned();
    }
    let dir = st.current();
    let mode = if st.by_files { "count" } else { "size" };
    format!(
        "{}, {} files  Tab size/count ({mode})  n/s/C sort  p panel  Enter open  Backspace up",
        human(dir.size),
        thousands(dir.files)
    )
}

/// Process monitor title: source, sort and filter.
pub fn procs_title(p: &Procs) -> String {
    let src = p.remote.as_ref().map_or("local", |r| r.url());
    let key = match p.sort {
        ProcSort::Pid => "pid",
        ProcSort::Name => "name",
        ProcSort::Cpu => "cpu",
        ProcSort::Mem => "mem",
    };
    let t = format!(
        "Processes: {src} [{key}{}] [every {}s]",
        if p.rev { " rev" } else { "" },
        p.every.as_secs_f64()
    );
    t + &p.filter.tag()
}

/// Summary line: CPU, memory, swap, load and process count; `-` = unknown.
pub fn procs_header(p: &Procs) -> String {
    let s = &p.snap;
    let mem = |used, total| match total {
        0 => "-".to_owned(),
        _ => format!("{} / {}", human(used), human(total)),
    };
    let cpu = s.cpu.map_or_else(|| "-".to_owned(), |c| format!("{c:.0}%"));
    let cores = if s.cores > 0 {
        format!(" of {} cores", s.cores)
    } else {
        String::new()
    };
    let load = s.load.map_or_else(
        || "-".to_owned(),
        |[a, b, c]| format!("{a:.2} {b:.2} {c:.2}"),
    );
    format!(
        "CPU {cpu}{cores}  Mem {}  Swap {}  Load {load} (1/5/15 min)  {} processes",
        mem(s.mem_used, s.mem_total),
        mem(s.swap_used, s.swap_total),
        thousands(s.procs.len() as u64)
    )
}

/// Column titles matching [`proc_row`].
pub const PROC_COLS: &str = "    PID USER        CPU%      MEM  COMMAND";

/// Sort key ('p', 'c', 'm', 'n') of the [`PROC_COLS`] title at char column `col`; user has none.
pub fn proc_col_key(col: usize) -> Option<char> {
    match col {
        0..=7 => Some('p'),
        18..=24 => Some('c'),
        25..=33 => Some('m'),
        34.. => Some('n'),
        _ => None,
    }
}

/// Open prompt of the process monitor: label and text being typed.
pub fn procs_prompt(p: &Procs) -> Option<(&'static str, &LineEdit)> {
    match &p.every_edit {
        Some(l) => Some(("update every (seconds)> ", l)),
        None if p.filter.editing => Some(("filter> ", &p.filter.line)),
        None => None,
    }
}

/// Process row: pid, user, CPU%, memory, name and command line.
pub fn proc_row(p: &Proc) -> String {
    let cmd = if p.cmd.is_empty() || p.cmd == p.name {
        p.name.clone()
    } else {
        format!("{}  {}", p.name, p.cmd)
    };
    format!(
        "{:>7} {} {:>5.1} {:>8}  {cmd}",
        p.pid,
        fit(&p.user, 10),
        p.cpu,
        human(p.mem)
    )
}

/// Footer: kill result or keys; `loading` before the first snapshot.
pub fn procs_footer(p: &Procs) -> String {
    if !p.status.is_empty() {
        return p.status.clone();
    }
    if p.snap.procs.is_empty() {
        return "loading...".into();
    }
    "c/m/p/n sort  / filter  s interval  F8 kill".into()
}

/// Kill question for the process monitor.
pub fn procs_confirm(pid: u32, name: &str) -> String {
    format!("Kill {pid} {name}?\n\n[t] Terminate (SIGTERM)   [k] Kill (SIGKILL)   [Esc] Cancel")
}

/// Editor status line: position, encoding, line ending, dirty flag, note.
pub fn editor_info(editor: &Editor, note: &str) -> String {
    let (line, col) = editor.cursor;
    let eol = match editor.eol() {
        Eol::Lf => "LF",
        Eol::Crlf => "CRLF",
        Eol::Mixed => "mixed",
    };
    let dirty = if editor.dirty() { " *" } else { "" };
    format!(
        "{}:{}  {}  {eol}{dirty}  {note}  Ctrl+Z Undo",
        line + 1,
        col + 1,
        editor.encoding_name()
    )
}

/// Look of one character of viewer / editor text; front ends map it to their colors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mark {
    pub kind: Option<Kind>,
    /// Search hit (drawn over the syntax color).
    pub hit: bool,
    pub selected: bool,
    /// Matching bracket at the cursor.
    pub bracket: bool,
}

/// `row` split into runs of equal [`Mark`]: `lang` syntax, matches of `re`, the selected columns
/// (counted from the first column of the line, minus `left`) and `brackets` columns.
// shortcut: matches the shown text, so a pattern with \t or one cut at the left edge can differ from the real match
pub fn highlight(
    row: &str,
    lang: Option<&Lang>,
    fence: &mut bool,
    re: Option<&Regex>,
    sel: Option<(usize, usize)>,
    (left, brackets): (usize, &[usize]),
) -> Vec<(String, Mark)> {
    let mut marks = vec![Mark::default(); row.chars().count()];
    let at = |byte: usize| row[..byte].chars().count();
    for (r, kind) in lang
        .map(|l| syntax::tokens(l, row, fence))
        .unwrap_or_default()
    {
        for m in &mut marks[at(r.start)..at(r.end)] {
            m.kind = Some(kind);
        }
    }
    for found in re.into_iter().flat_map(|re| re.find_iter(row)) {
        for m in &mut marks[at(found.start())..at(found.end())] {
            m.hit = true;
        }
    }
    if let Some((a, b)) = sel {
        let end = marks.len();
        for m in &mut marks[a.saturating_sub(left).min(end)..b.saturating_sub(left).min(end)] {
            m.selected = true;
        }
    }
    for &c in brackets {
        if let Some(m) = marks.get_mut(c) {
            m.bracket = true;
        }
    }
    let mut runs: Vec<(String, Mark)> = Vec::new();
    for (c, m) in row.chars().zip(marks) {
        match runs.last_mut() {
            Some((text, last)) if *last == m => text.push(c),
            _ => runs.push((c.to_string(), m)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::{PROC_COLS, UnicodeWidthStr, fit, fmt_date, proc_col_key, thousands};

    /// Every column title maps to its sort key, the user column to none.
    #[test]
    fn proc_title_columns_sort() {
        let at = |t: &str| proc_col_key(PROC_COLS.find(t).unwrap());
        assert_eq!(
            ["PID", "USER", "CPU%", "MEM", "COMMAND"].map(at),
            [Some('p'), None, Some('c'), Some('m'), Some('n')]
        );
    }

    #[test]
    fn date_today_shows_time_only() {
        let z = jiff::ZonedDateTime::now();
        let today = z.date();
        assert_eq!(fmt_date(&z, today), z.strftime("%H:%M:%S").to_string());
        let yesterday = today.yesterday().unwrap();
        assert_eq!(
            fmt_date(&z, yesterday),
            z.strftime("%y-%m-%d %H:%M").to_string()
        );
    }

    #[test]
    fn thousands_and_fit() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("ab", 4), "ab  ");
        assert_eq!(fit("x", 0), "");
        let w = fit("日本語", 5);
        assert_eq!(w.width(), 5);
        assert!(w.trim_end().ends_with('…'));
    }
}
