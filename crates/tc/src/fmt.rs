//! Frontend-neutral text: help, sizes, dates, progress.

mod help;

use crate::KeyCode;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use regex::Regex;
use tc_core::cmdline::LineEdit;
use tc_core::edit::{Editor, Eol, human};
use tc_core::find::{Hit, MAX_RESULTS};
use tc_core::panel::{CompareKind, Entry, SortKey};
use tc_core::stats::treemap::Kind as TileKind;
use tc_core::stats::{self, Node};
use tc_core::syntax::{self, Kind, Lang};
use tc_remote::Prompt;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    App, Filter, FormRow, Found, Input, ListRow, Mode, Procs, Stats, Step, Sync, Xfer, list_rows,
};
use tc_core::procs::{Criteria, Proc, ProcSort};
use tc_core::sync::Item;

pub use help::{
    HELP_CONFLICT, HELP_EDIT, HELP_FIND_FORM, HELP_FOUND, HELP_LISTS, HELP_MAIN, HELP_PROCS,
    HELP_PROPS, HELP_RENAME, HELP_SEARCH, HELP_SETTINGS, HELP_STATS, HELP_SYNC, HELP_SYNC_FORM,
    HELP_VIEW, HelpLine, help, help_filter, help_line, key_help,
};

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

/// `s` with its middle replaced by `…` when longer than `max` chars: both path ends stay.
pub fn elide(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    let head = max.saturating_sub(1) / 2;
    let tail = max.saturating_sub(1) - head;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(n - tail).collect();
    format!("{start}…{end}")
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
    let load = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);
    let current = p
        .current
        .lock()
        // a long path would widen or wrap the popup: it jumps from file to file
        .map(|c| elide(&c.display().to_string(), 80))
        .unwrap_or_default();
    let scanned = p.scanned.load(Ordering::Acquire);
    let total = |t: String| if scanned { t } else { "scanning...".to_owned() };
    let [bytes, files, dirs] = x.shown.rates.map(|r| r.unwrap_or(0.0));
    let rate = |r: Option<f64>, s: String| r.map_or("--".to_owned(), |_| s);
    let rows = [
        (
            "bytes",
            human(load(&p.done)),
            total(human(load(&p.total))),
            rate(x.shown.rates[0], format!("{}/s", human(bytes as u64))),
        ),
        (
            "files",
            load(&p.files).to_string(),
            total(load(&p.files_total).to_string()),
            rate(x.shown.rates[1], format!("{files:.1}/s")),
        ),
        (
            "dirs",
            load(&p.dirs_done).to_string(),
            total(load(&p.dirs).to_string()),
            rate(x.shown.rates[2], format!("{dirs:.1}/s")),
        ),
    ];
    let rows: Vec<String> = (rows.iter())
        .map(|(l, d, t, r)| format!("{l:<6} {:>20}   {r}", format!("{d} / {t}")))
        .collect();
    let eta = x.shown.eta.map_or("--".to_owned(), |s| hms(s as u64));
    format!("{current}\n{}\nETA {eta}\n\n[Esc] Cancel", rows.join("\n"))
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
        /// Buttons under the list: label (one glyph: an icon), tooltip, the key a click sends, enabled.
        buttons: Vec<(&'static str, &'static str, KeyCode, bool)>,
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
) -> Option<Dialog<'a>> {
    let (title, rows, sel) = list;
    let view = list_rows(rows, filter);
    Some(Dialog::List {
        title: format!("{title}{}", filter.tag()),
        sel: view.iter().position(|&i| i == sel).unwrap_or(0),
        rows: view.iter().map(|&i| rows[i].label().into_owned()).collect(),
        empty,
        buttons: Vec::new(),
    })
}

/// Bookmarks with Add (while the local panel directory is not listed), Delete (the selected
/// row) and Edit (the edit page).
fn bookmarks_dialog(app: &App, sel: usize) -> Option<Dialog<'_>> {
    let mut d = list_dialog(
        ("Bookmarks", &app.hotlist, sel),
        &app.list_filter,
        "(empty, Insert or Ctrl+D adds the current directory)",
    )?;
    if let Dialog::List { rows, buttons, .. } = &mut d {
        let add = app.can_bookmark();
        let any = !rows.is_empty();
        buttons.extend([
            (
                "✚",
                "Add the current directory (Insert)",
                KeyCode::Insert,
                add,
            ),
            (
                "➖",
                "Delete the selected bookmark (Delete)",
                KeyCode::Delete,
                any,
            ),
            (
                "Edit…",
                "Edit all: order, alias, path, add, delete (F4)",
                KeyCode::F(4),
                true,
            ),
        ]);
    }
    Some(d)
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
        Mode::Link(a) => edit(a.title(), "", &a.line, OK_CANCEL),
        Mode::NewFile(l) => edit(
            "New file (created if missing, opened in editor)",
            "",
            l,
            OK_CANCEL,
        ),
        Mode::Editor(_, l) => edit("Editor not found, enter editor command", "", l, OK_CANCEL),
        Mode::Bookmarks(sel) => bookmarks_dialog(app, *sel),
        Mode::Popular(sel) => list_dialog(
            ("Popular directories", &app.popular, *sel),
            &app.list_filter,
            "(empty, directories you visit are listed here)",
        ),
        Mode::Compare(sel) => list_dialog(
            ("Compare panels, select", &CompareKind::ALL, *sel),
            &Filter::default(),
            "",
        ),
        Mode::Drives(rows, sel, _) => list_dialog(
            ("Drives", rows, *sel),
            &Filter::default(),
            "(no drives found)",
        ),
        Mode::DirHistory(rows, sel) => {
            list_dialog(("Directory history", rows, *sel), &Filter::default(), "")
        }
        Mode::Connections(sel) => list_dialog(
            ("Connections", &app.connections, *sel),
            &app.list_filter,
            "(empty, Insert adds one)",
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
                let mut masked = LineEdit::from("*".repeat(l.text.chars().count()));
                masked.cur = l.text[..l.cur].chars().count();
                edit(label, "", &masked, OK_CANCEL)
            }
            None => text("Connecting", format!("{}\n\n[Esc] Cancel", c.url)),
        },
        Mode::Message(msg) => text("Error", format!("{msg}\n\nany key")),
        Mode::Update { tag, check } => text(
            "Update available",
            format!(
                "tc {tag} is available (running {}).\n{}\n\n{} Check for updates daily\n\n[Enter] Open release page   [Space] Toggle   [Esc] Close",
                crate::app::VERSION.unwrap_or("?"),
                crate::app::RELEASES,
                if *check { "[x]" } else { "[ ]" }
            ),
        ),
        Mode::Edit {
            error: Some(msg), ..
        } => text("Error", format!("{msg}\n\nany key")),
        Mode::Work(job) if job.visible() => {
            text(job.label, format!("{}\n\n[Esc] Cancel", job.done_text()))
        }
        Mode::Xfer(x) => xfer_dialog(x),
        Mode::AskViewer { text: t, .. } => text("Edit", t.clone()),
        Mode::AskSave { editor, step, .. } => {
            let t = match step {
                Step::Changed => {
                    return text(
                        "File changed on disk",
                        "Another program changed the file.\n\n[y] Overwrite it with mine   [r] Reload from disk (drops my edits)\n[n/Esc] Keep editing".to_owned(),
                    );
                }
                Step::Encoding => format!("File is {}. Convert to UTF-8?", editor.encoding_name()),
                Step::Eol => "Convert line endings to LF?".to_owned(),
            };
            text(
                "Save",
                format!("{t}\n\n[y/Enter] Yes   [n] No   [Esc] Cancel"),
            )
        }
        Mode::AskExit(_) => text(
            "Edit",
            "Save changes?\n\n[y/Enter] Yes   [n] No   [Esc] Cancel".to_owned(),
        ),
        _ => None,
    }
}

fn xfer_dialog(x: &Xfer) -> Option<Dialog<'_>> {
    if let Some(l) = &x.rename {
        edit("Rename", "", l, OK_CANCEL)
    } else if let Some(msg) = &x.error {
        text(
            "Error",
            format!("{msg}\n\n[r/Enter] Retry   [s] Skip   [S] Skip all\n[Esc] Cancel"),
        )
    } else if let Some((_, target)) = &x.ask {
        text(
            "File exists",
            format!(
                "{}\n\n[o/Enter] Overwrite   [s] Skip   [n] Newer   [r] Rename\n[O] Overwrite all   [S] Skip all   [N] Newer all\n[Esc] Cancel",
                target.display()
            ),
        )
    } else if x.visible() {
        text(x.label, progress_text(x))
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
/// `sort_tag`: append the non-default sort key; GUI shows it as the header arrow instead.
pub fn panel_title(app: &App, i: usize, sort_tag: bool) -> String {
    let panel = &app.panels[i];
    let mut title = panel.title();
    let tabs = app.tabs[i].len() + 1;
    if tabs > 1 {
        title = format!("[{}/{tabs}] {title}", app.tab[i] + 1);
    }
    if panel.is_virtual() {
        title += " [found]";
    }
    if app.sync_browse {
        title += " [sync]";
    }
    if let Some(dir) = panel.loading() {
        title += &format!(" [loading {}, Esc cancels]", dir.display());
    }
    match panel.sort() {
        (SortKey::Name, false) => {}
        _ if !sort_tag => {}
        (key, rev) => {
            let key = format!("{key:?}").to_lowercase();
            title += &format!(" [{key}{}]", if rev { " desc" } else { "" });
        }
    }
    let active = i == app.active;
    match &app.mode {
        Mode::Search(t) if active => title += &format!(" [search: {t}]"),
        _ if !panel.filter().is_empty() => title += &format!(" [filter: {}]", panel.filter().label),
        _ => {}
    }
    title
}

/// Status line of panel `i`: selection sum (cached in the panel), else the cursor entry in full.
pub fn panel_info(app: &App, i: usize) -> String {
    let panel = &app.panels[i];
    let (count, bytes) = panel.selection_info();
    let free = match app.free[i].1.load(std::sync::atomic::Ordering::Relaxed) {
        0 => String::new(),
        f => format!(" {} free ", human(f - 1)),
    };
    let info = match panel.current().filter(|e| !e.is_parent()) {
        _ if count > 0 => {
            // ".." is only ever the first entry: O(1) per frame
            let list = panel.entries();
            let all = list.len() - usize::from(list.first().is_some_and(Entry::is_parent));
            format!(" {count} of {all} selected, {}", human(bytes))
        }
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
            format!(" {}  {size}  {date}{link}", e.name.to_string_lossy())
        }
        None => String::new(),
    };
    format!("{free}{info}")
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

/// Find result cells: size (blank for directories), full path and the matching line if any.
pub fn found_cols(h: &Hit) -> [String; 2] {
    let e = &h.entry;
    let size = if e.is_dir {
        String::new()
    } else {
        human(e.size)
    };
    let name = e.name.to_string_lossy();
    let text = match &h.line {
        Some((n, text)) => format!("{name}:{n}: {text}"),
        None => name.into_owned(),
    };
    [size, text]
}

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

/// Sync row cells: left size and date, action, right size and date, path.
pub fn sync_cols(it: &Item, today: jiff::civil::Date) -> [String; 6] {
    let side = |e: &Option<Entry>| match e {
        Some(e) if e.is_dir => ["<DIR>".to_string(), date_of(e, today)],
        Some(e) => [human(e.size), date_of(e, today)],
        None => [String::new(), String::new()],
    };
    let slash = if it.is_dir() { "/" } else { "" };
    let [ls, ld] = side(&it.left);
    let [rs, rd] = side(&it.right);
    let path = format!("{}{slash}", it.rel.display());
    [ls, ld, it.action.symbol().to_string(), rs, rd, path]
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

/// Preview rows under the multi-rename form; PgUp/PgDn scroll by this.
pub const RENAME_PREVIEW_ROWS: usize = 12;

/// Placeholder legend under the multi-rename form.
/// Tab titles of the multi-rename form (rows per tab: `app::multi_rename::TAB_ROWS`).
pub const RENAME_TABS: [&str; 5] = ["Name", "Replace", "Placeholder", "Clean up", "Editor"];

/// Name-tab buttons (placeholder, hover text): each inserts into the focused field.
pub const RENAME_CHIPS: [(&str, &str); 6] = [
    ("[N]", "name without the extension"),
    ("[E]", "extension"),
    ("[C]", "counter: start, step and digits are set below"),
    ("[Y]", "year of the modified date"),
    ("[M]", "month of the modified date, 01-12"),
    ("[D]", "day of the modified date, 01-31"),
];

/// Placeholder mode, shown under the Find fields: pattern, replacement, example result.
pub const RENAME_PLACEHOLDER_EXAMPLES: [(&str, &str, &str); 2] = [
    (
        "%a_%b_%c_%d",
        "%d-%c-%b-%a",
        "one_two_three_four -> four-three-two-one",
    ),
    ("IMG_%n", "Photo %n", "IMG_12 -> Photo 12"),
];

/// Hover text of a multi-rename option: what it does, with an example.
pub const RENAME_TIPS: [(&str, &str); 9] = [
    ("Find case-sensitive", "Plain only: off, 'x' finds 'X' too"),
    (
        "Regex",
        "Find is a regex: $1 or ${1} in Replace is a group; (?-i) makes it case-sensitive",
    ),
    ("Underscore to space", "'two_words' -> 'two words'"),
    ("Remove multiple spaces", "'long    space' -> 'long space'"),
    ("Space before capitals", "'OneLongWord' -> 'One Long Word'"),
    (
        "Remove dots",
        "'A.B.C.' -> 'ABC', 'Mr.' -> 'Mr', '...' -> ''",
    ),
    (
        "Add to first number",
        "'Picture 1' with 10 -> 'Picture 11'; empty: off",
    ),
    (
        "Case",
        "Every Word, small kept: 'the end of a dream' -> 'The End of a Dream'",
    ),
    (
        "Counter digits",
        "3 digits: the counter [C] shows 001, 002 ...",
    ),
];

/// Hover text for a multi-rename option by its label.
pub fn rename_tip(label: &str) -> Option<&'static str> {
    RENAME_TIPS
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, t)| *t)
}

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
pub fn rename_cell<'a>(old: &str, new: &'a Result<String, String>) -> RenameCell<'a> {
    match new {
        Ok(n) if n == old => RenameCell::Same,
        Ok(n) => RenameCell::New(n),
        Err(e) if e.is_empty() => RenameCell::Blank,
        Err(e) => RenameCell::Bad(e),
    }
}

/// The new-name column of a preview row.
pub enum RenameCell<'a> {
    Same,
    New(&'a str),
    Blank,
    Bad(&'a str),
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

/// File Stats row cells.
pub struct StatsCols {
    pub marked: bool,
    pub value: String,
    /// Share of the browsed directory, 0..=1.
    pub share: f64,
    pub name: String,
}

pub fn stats_cols(st: &Stats, n: &Node) -> StatsCols {
    let value = |n: &Node| if st.by_files { n.files } else { n.size };
    let total = value(st.current()).max(1);
    let slash = if n.is_dir { "/" } else { "" };
    StatsCols {
        marked: st.is_marked(n),
        value: if st.by_files {
            thousands(value(n))
        } else {
            human(value(n))
        },
        share: value(n) as f64 / total as f64,
        name: format!("{}{slash}", n.name.to_string_lossy()),
    }
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
    let key = if st.by_files { "count" } else { "size" };
    format!("Stats: {} [{key}]{}", st.path().display(), st.filter.tag())
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
    format!("{}, {} files", human(dir.size), thousands(dir.files))
}

/// Process monitor title: source, sort and filter.
pub fn procs_title(p: &Procs) -> String {
    let src = p.remote.as_ref().map_or("local", |r| r.url());
    let key = match p.sort {
        ProcSort::Cpu => "cpu",
        ProcSort::Mem => "mem",
        ProcSort::Run => "run",
    };
    let t = format!(
        "Processes: {src} [{key}] [every {}s]",
        p.every.as_secs_f64()
    );
    t + &proc_filter_tag(&p.crit)
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
pub const PROC_COLS: &str = "    PID USER        CPU%      MEM      Min  COMMAND";

/// Sort key ('c', 'm') of the [`PROC_COLS`] title at char column `col`; other columns do not sort.
pub fn proc_col_key(col: usize) -> Option<char> {
    match col {
        18..=24 => Some('c'),
        25..=33 => Some('m'),
        34..=42 => Some('r'),
        _ => None,
    }
}

/// Open prompt of the process monitor: label and text being typed.
pub fn procs_prompt(p: &Procs) -> Option<(&'static str, &LineEdit)> {
    p.every_edit
        .as_ref()
        .map(|l| ("update every (seconds)> ", l))
}

/// Filter part of the process monitor title: ` [filter: fire, mem>=100MB]`, empty if none.
pub fn proc_filter_tag(c: &Criteria) -> String {
    if !c.active() {
        return String::new();
    }
    let mut parts = Vec::new();
    if !c.text.is_empty() {
        parts.push(c.text.clone());
    }
    if c.min_mem > 0 {
        parts.push(format!("mem>={}MB", c.min_mem as f64 / 1_048_576.0));
    }
    if c.min_cpu > 0.0 {
        parts.push(format!("cpu>={}%", c.min_cpu));
    }
    if c.min_run > 0 {
        parts.push(format!("run>={}min", c.min_run as f64 / 60.0));
    }
    format!(" [filter: {}]", parts.join(", "))
}

/// Process row: pid, user, CPU%, memory, name and command line.
pub fn proc_row(p: &Proc) -> String {
    let cmd = if p.cmd.is_empty() || p.cmd == p.name {
        p.name.clone()
    } else {
        format!("{}  {}", p.name, p.cmd)
    };
    format!(
        "{:>7} {} {:>5.1} {:>8} {:>8}  {cmd}",
        p.pid,
        fit(&p.user, 10),
        p.cpu,
        human(p.mem),
        p.run / 60
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
    String::new()
}

/// Info popup of the process monitor: title and label / value rows.
pub struct ProcInfo {
    pub title: String,
    pub rows: Vec<(&'static str, String)>,
}

/// Info popup rows of the process under the info cursor; `None` while the popup is closed.
pub fn procs_info(p: &Procs) -> Option<ProcInfo> {
    let pid = p.info?;
    let title = format!(" Process {pid} ");
    let Some(q) = p.snap.procs.iter().find(|q| q.pid == pid) else {
        return Some(ProcInfo {
            title,
            rows: vec![("Status", "ended".into())],
        });
    };
    let s = &p.snap;
    let of_cores = match s.cores {
        0 => String::new(),
        n => format!(" ({:.1} % of all {n} cores)", q.cpu / n as f32),
    };
    let of_mem = match s.mem_total {
        0 => String::new(),
        t => format!(
            " ({:.1} % of {})",
            q.mem as f64 * 100.0 / t as f64,
            human(t)
        ),
    };
    let or_dash = |v: &str| {
        if v.is_empty() {
            "-".to_owned()
        } else {
            v.to_owned()
        }
    };
    let rows = vec![
        ("PID", q.pid.to_string()),
        ("Name", q.name.clone()),
        ("Command", or_dash(&q.cmd)),
        ("User", or_dash(&q.user)),
        ("CPU", format!("{:.1} % of one core{of_cores}", q.cpu)),
        (
            "Memory",
            format!("{} ({} bytes){of_mem}", human(q.mem), thousands(q.mem)),
        ),
        ("Runtime", format!("{} ({} min)", clock(q.run), q.run / 60)),
    ];
    Some(ProcInfo { title, rows })
}

/// `[d d ]hh:mm:ss` of a runtime in seconds.
fn clock(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d} d {h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
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
    use super::{PROC_COLS, UnicodeWidthStr, elide, fit, fmt_date, proc_col_key, thousands};

    #[test]
    fn elide_keeps_both_ends() {
        assert_eq!(elide("abc", 3), "abc");
        assert_eq!(elide("abcdefgh", 5), "ab…gh");
        assert_eq!(elide("äöüßxyz", 4), "ä…yz");
    }

    /// Every column title maps to its sort key, the user column to none.
    #[test]
    fn proc_title_columns_sort() {
        let at = |t: &str| proc_col_key(PROC_COLS.find(t).unwrap());
        assert_eq!(
            ["PID", "USER", "CPU%", "MEM", "COMMAND"].map(at),
            [None, None, Some('c'), Some('m'), None]
        );
        assert_eq!(at("Min"), Some('r'));
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
