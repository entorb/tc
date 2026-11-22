//! F1 help texts: per-mode text, line parsing, filter and footer-key lookup.

use crate::app::Mode;

pub const HELP_MAIN: &str = "\
Type to filter this help, Up/Down select, Enter runs the key.

== Highlights ==
F9              menu with all features (when keys are eaten)
Ctrl+B          bookmarks (hotlist)
Ctrl+Space      FILE STATS (like ncdu): biggest files and dirs
                of the current tree, with % and bar
Ctrl+N          SFTP / FTP / FTPS connections
Disconnect all  (F9 Go) panels and tabs on a server go back
                to their local directory
Ctrl+P          PROCESSES (like btop / task manager), of the
                SSH server on an SFTP panel

== Navigation ==
Tab             switch panel
Enter           open directory, archive or file
Backspace       parent directory
Left            parent directory
Right           open directory or archive
Up/Down         move cursor
PgUp/PgDn       page up / down
Home/End        first / last entry
Alt+Left        history back
Alt+Right       history forward
~               home directory
Ctrl+\\          root directory
Ctrl+R          refresh
Alt+F1          drives for left panel
Alt+F2          drives for right panel
Ctrl+D          add this directory to the bookmarks
Ctrl+U          swap panels
Ctrl+T          new tab
Ctrl+W          close tab
Ctrl+=/Ctrl+-   font size (tc-gui, saved; terminals: use
                the terminal's zoom)
Ctrl+PgDn       next tab
Ctrl+Tab        next tab
Ctrl+PgUp       previous tab
Alt+=           other panel to this directory
Ctrl+F3         sort by name (Ctrl+F4 ext, F5 date, F6 size;
                again: reverse)
Esc             clear filter, leave found list
Ctrl+G          found list: go to the file's directory

== Mouse ==
Wheel           scroll the panel under the pointer, 3 entries
Click           cursor to the entry, activates the panel
Shift+click     select range from the cursor
Double click    open directory / archive, view file
Click title     sort by name / size / date (again: reverse)
Click F-bar     run the key shown on the label
Click menu bar  open the F9 menu
R-click tab     close the tab (tc-gui; not the last one)
Click cmd line  type a command, like : (tc-gui)
Shift held      F-bar shows the Shift actions (tc-gui)

== Selection ==
Insert          select entry, cursor moves down
Space           same; on a local directory also sizes it
Shift+Up/Down   select range
+               select by mask
-               deselect by mask
*               invert selection
Ctrl+Y          select differences to other panel

== Search ==
Ctrl+S          quick search: type to jump to a match, Ctrl+S
                next match, Enter / Esc stop searching
/               quick filter: type to hide other entries,
                Enter keeps the filter, Esc clears it
Ctrl+F          find files
Ctrl+E          synchronize directories (left / right panel)
:               command line (Up/Down: earlier commands)

== Files ==
F3              view (media: preview)
F4              edit (media: default application)
Shift+F4        edit in external editor (asks the name,
                creates a missing file)
F5              copy
F6              move
F2              rename; with several marked: multi-rename
Shift+F6        rename
Ctrl+M          multi-rename: masks, counter, find / replace
                (tc-cli: use F9 -> File, Enter is Ctrl+M)
F7              make directory
F8              delete: y / Enter trash, p permanent,
                n / Esc cancel (remote panels: only p)
Ctrl+Insert     copy paths to clipboard (Alt+Shift+C)
Ctrl+C          copy files
Ctrl+X          cut files
Ctrl+V          paste copied / cut files here

== Tools ==
Delete config files
                (F9 Tools) delete tc's saved files: hotlist,
                connections, histories, searches, session

== Quit ==
F10             quit (asks first), also q";
pub const HELP_STATS: &str = "\
== About ==
Scan runs in up to 8 threads; a popup shows files and
scanned / discovered directories.
Wide windows show a treemap right of the list: tiles
sized by bytes, colored by file type; click selects,
double-click opens a directory; hover shows path, size
and file count of any tile.

== Move ==
Up/Down         move cursor
PgUp/PgDn       page up / down
Home/End        first / last entry
Enter           open directory
Right           open directory
Backspace       parent directory
Left            parent directory
Tab             show size / file count
n               sort by name (again: reverse)
s               sort by size (again: reverse)
C               sort by file count (again: reverse)
F5              rescan the browsed directory, totals above update
/               filter the entries by name: Enter keeps,
                Esc clears

== Files ==
F3              view file
F4              edit file
Shift+F4        edit in external editor
i               info: path, size, files, directories, date
p               feed marked / shown entries to the panel
Insert / Space  mark entry, move down
F8              delete marked entries, else the selected one (also d),
                tree updates without rescan:
                y / Enter trash, p permanent, n / Esc cancel
:               command line in this directory
Esc / q / F10   close (while scanning: cancel)";
pub const HELP_SYNC: &str = "\
== About ==
-> left is newer or only left, <- right is newer or only
right, = equal, != differs without direction (same date,
date ignored, file vs directory), del> delete right
(asymmetric or r), <del delete left, <del> delete both,
-- skip. A directory on one side only is one row,
copied or deleted whole.
Copies keep the date on local disks; remote targets get
the copy time, so use Ignore date against SFTP / FTP.
Hidden and filtered-out rows are not synchronized. Local
deletes go to the trash, remote ones are permanent.

== Move ==
Up/Down         move cursor
PgUp/PgDn       page up / down
Home/End        first / last row
F3 / F4         view / edit the left file (the right one when
                only it has the file); local only

== Actions ==
Right           copy left -> right (overwrites the right file)
Left            copy right -> left (overwrites the left file)
l / r / b       delete left / right / both (Space undoes)
c               compare the two files of the row: equal or
                first differing byte / line
*               reverse -> and <- of all shown rows
                (only rows on both sides)
Space           no action / restore the compare result
> = ! <         show / hide rows compared ->, =, !=, <- (and del)
/               filter the rows by path: Enter keeps, Esc clears
Enter           synchronize the shown rows: y starts,
                n / Esc goes back
Ctrl+R          compare again
Esc / q / F10   close (while comparing: cancel)";
pub const HELP_SYNC_FORM: &str = "\
== Fields ==
Tab             next field
Shift+Tab       previous field
Up/Down         previous / next field
Space           toggle checkbox
Left/Right      move in text
Alt+Up/Down     previous / next used settings (saved in
                tc/sync-forms)

== Options ==
File mask: names to compare, `*` and `?`, comma or space
separated. Subdirectories: compare the whole tree.
By content: same size is compared byte by byte.
Ignore date: same size = equal.
Asymmetric: the right side becomes a copy of the left
(extra right files are deleted).

== Run ==
Enter           compare the two panel directories
Esc / F10       close";
pub const HELP_FIND_FORM: &str = "\
== Fields ==
Tab             next field
Shift+Tab       previous field
Up/Down         previous / next field
Space           toggle checkbox
Left/Right      move in text
Alt+Up/Down     earlier / later searches (history)

== Name ==
Name (masks): `*` any run, `?` one character, several
masks separated by comma or space; empty = any name.
Name is a regex: the field is a regular expression
instead (local panels only). Exclude files: masks of
files to leave out. Case-sensitive: names match exactly.
Extensions: comma separated, with or without dot.

== Content ==
Typing in Content turns on Search file contents.
Plain text, `*` matches any run within a line. Content is
a regex uses a regular expression, Whole words, Not
containing (files without the text), List every match
(one result per line). No capital letter = ignore case
unless Content case-sensitive is on.

== Limits ==
Exclude dirs: names or paths with `/` below the start
directory, comma separated. Skip hidden: dot names.
Use .gitignore (local only). Follow symlinks. Max depth:
1 = only this directory.
Min / Max size: number with K, M, G or T (1024 steps),
e.g. 10k, 2 MB. Modified after / before: YYYY-MM-DD.
Not older than: 30m, 12h, 7d, 2w (bare number = days).

== Saved searches ==
F2              save the form under the name in \"Saved search\"
F3              load the next saved search
F8              delete the saved search of that name

== Run ==
Enter           start the search
Esc / F10       close (the form is kept for next time)";
pub const HELP_FOUND: &str = "\
== Move ==
Up/Down         move cursor
PgUp/PgDn       page up / down
Home/End        first / last result
/               filter the results by path: Enter keeps,
                Esc clears

== Results ==
F3              view file at the match
F4              edit file at the match
Enter           put the shown results in the panel (virtual list)
Ctrl+F3         sort by path (again: reverse)
Ctrl+F4         sort by file name
Ctrl+F5         sort by date
Ctrl+F6         sort by size
Ctrl+R          search again
Ctrl+G          go to the file's directory (local only)
Esc / q / F10   stop a running search, then close";
pub const HELP_RENAME: &str = "\
== Fields ==
Tab             next field
Shift+Tab       previous field
Up/Down         previous / next field
Space           toggle checkbox, cycle choice
Left/Right      move in text, cycle choice
Alt+Up/Down     earlier / later settings (history)
PgUp/PgDn       scroll the preview

== Name and extension masks ==
Text stays as typed, [..] placeholders are replaced:
  [N] name, [E] extension, [N2-5] characters 2 to 5,
  [N2,3] 3 characters from 2, [N-3-] last 3 to end
  (negative counts from the end)
  [C] counter (start, step, digits below)
  [Y] [M] [D] year, month, day of the modified date
  [h] [n] [s] hour, minute, second
  [[] a literal [

== Find and replace ==
Applied to the name part. Find is a regex: `$1` or
`${1}` in Replace is the first group. Find case-sensitive
is off by default. Case: unchanged, lower, UPPER,
First letter, Every Word.

== Names from a list ==
Paste (several lines)  new names, one per line
(as many as files; edit a field to use the masks again)

== Undo ==
Ctrl+Z         undo the last multi-rename (this session)

== Edit names ==
F4              edit new names in the editor (line count must match)

== Presets ==
F2              save as \"Preset\"
F3              load next preset
F8              delete preset

== Run ==
Enter           rename (the preview shows every new name)
Esc / F10       close";
pub const HELP_LISTS: &str = "\
== Bookmarks (Ctrl+B) ==
Up/Down         move
Enter           go to the directory in the active panel
Insert / a      add the active panel's directory
F4 / e          edit the alias (empty removes it)
Ctrl+Up/Down    move the bookmark (also Shift+Up/Down), saved at once
Delete/Bksp     remove the bookmark
/               filter: Enter keeps, Esc clears
Esc / q / F10   close

== Connections (Ctrl+N) ==
Up/Down         move
Enter           connect
Insert / a      add a connection
F4 / e          edit the connection
Delete/Bksp     remove the connection
/               filter: Enter keeps, Esc clears
Esc / q / F10   close

== Drives (Alt+F1 / Alt+F2) ==
Up/Down         move
Enter           go to the mount point
Esc / q / F10   close";
pub const HELP_CONFLICT: &str = "\
== Target exists ==
o               overwrite this file
s               skip this file
n               overwrite only if the source is newer
r               rename: type a new name for the target
O               overwrite all further conflicts
S               skip all further conflicts
N               newer-only for all further conflicts
Esc / Enter     cancel the whole operation (c too)";
pub const HELP_PROCS: &str = "\
== About ==
CPU% is the average over the interval, per core (can
exceed 100). Load = average number of busy processes
over the last 1 / 5 / 15 minutes; above the core count
means overloaded. The cursor keeps its row while the
list re-sorts. On an SFTP panel the list comes from the
server's /proc and ps.

== Move ==
Up/Down         move cursor
PgUp/PgDn       page up / down
Home/End        first / last process
Mouse wheel     scroll
Click title     sort by that column (GUI)

== Keys ==
c / m / p / n   sort by CPU, memory, pid, name (again: reverse)
/ or f          filter (name, command, user, pid):
                Enter keeps, Esc clears
Ctrl+R          sample now
s               update interval in seconds (default 2):
                Enter sets, Esc keeps
F8 / k          kill: t / Enter SIGTERM, k SIGKILL, n / Esc cancel
Esc / q / F10   close";
pub const HELP_VIEW: &str = "\
== Move ==
Up/Down         scroll
PgUp/PgDn       page up / down
Home/End        start / end of file
Left/Right      scroll sideways
Mouse wheel     scroll
g / Ctrl+G      go to line or N% (hex: offset 0x.. / decimal)

== Search ==
/ f F7 Ctrl+F   find (prompt: Up/Down earlier patterns)
n / Shift+F7    next match
N               previous match
Alt+C           case sensitive / smart case
                hex: jumps to the row where the matching line starts

== View ==
h / Alt+H       hex view (offset | bytes | ASCII)
Alt+Left/Right  previous / next file of the panel (skips dirs and media); Alt+b/f same
w / Alt+W       wrap long lines
l / Alt+L       line numbers
F4              edit this file at the top line (local files)
Esc / q / F10   close
F3              close";
pub const HELP_EDIT: &str = "\
== File ==
F2 / Ctrl+S     save; Markdown first gets blank lines around
                headings, lists and ``` blocks
F5              Markdown: add those blank lines now (undo:
                Ctrl+Z)
Esc / F10       exit (asks if unsaved)

== Move ==
Up/Down         line up / down
PgUp/PgDn       page up / down
Left/Right      character left / right
Home/End        start / end of line
Ctrl+Left       word left
Ctrl+Right      word right
Ctrl+Home/End   start / end of file
Ctrl+G          go to line
Mouse wheel     scroll
Mouse           click, drag, Shift+click select

== Search ==
F7 / Ctrl+F     find (prompt: Up/Down earlier patterns)
F3 / Shift+F7   next match
Shift+F3        previous match
F4 / Ctrl+R     replace all
Alt+R           replace one by one (y / Enter, n / Space,
                a all, other key stops)
Alt+C           case sensitive / smart case

== Edit ==
Enter           new line, keeps the indentation
Tab             insert a tab
Shift+Tab       unindent line / selection
Backspace       delete left
Delete          delete right
Ctrl+Backspace  delete word left
Ctrl+Delete     delete word right
Ctrl+Z          undo
Ctrl+Y          redo
Shift+arrows    select
Ctrl+A          select all
Ctrl+C          copy (also system clipboard)
Ctrl+X          cut
Ctrl+V          paste (terminal paste works too)
Ctrl+K / F8     delete selection or line
Ctrl+D          duplicate line
Alt+W           wrap on / off
Alt+L           line numbers on / off";

/// Title and text of the F1 help for `mode`.
pub fn help(mode: &Mode) -> (&'static str, &'static str) {
    match mode {
        Mode::View { .. } => ("Help: viewer", HELP_VIEW),
        Mode::Edit { .. } => ("Help: editor", HELP_EDIT),
        Mode::Stats(_) => ("Help: file stats (ncdu)", HELP_STATS),
        Mode::Procs(_) => ("Help: processes (top)", HELP_PROCS),
        Mode::Sync(_) => ("Help: synchronize directories", HELP_SYNC),
        Mode::SyncForm(_) => ("Help: synchronize form", HELP_SYNC_FORM),
        Mode::FindForm(_) => ("Help: find files", HELP_FIND_FORM),
        Mode::Found(_) => ("Help: found files", HELP_FOUND),
        Mode::MultiRename(_) => ("Help: multi-rename", HELP_RENAME),
        Mode::Hotlist(_) | Mode::Connections(_) | Mode::Drives(..) => ("Help: lists", HELP_LISTS),
        Mode::Xfer(_) => ("Help: file exists", HELP_CONFLICT),
        _ => ("Help", HELP_MAIN),
    }
}

/// One help line split for a table: `== Section ==`, `key  info`, continuation, free text.
#[derive(Debug, PartialEq)]
pub enum HelpLine<'a> {
    Section(&'a str),
    Entry(&'a str, &'a str),
    More(&'a str),
    Text(&'a str),
}

pub fn help_line(l: &str) -> HelpLine<'_> {
    if let Some(t) = l.strip_prefix("== ").and_then(|t| t.strip_suffix(" ==")) {
        return HelpLine::Section(t);
    }
    if l.starts_with(' ') {
        return HelpLine::More(l.trim());
    }
    match l.find("  ") {
        Some(k) if k < 17 => HelpLine::Entry(&l[..k], l[k..].trim()),
        _ => HelpLine::Text(l),
    }
}

/// Help lines whose section title or entry (key line with its continuation lines) contains
/// `f` (lowercase); the section title stays above its matching entries.
pub fn help_filter<'a>(text: &'a str, f: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = text.lines().collect();
    if f.is_empty() {
        return lines;
    }
    let (mut out, mut section, mut shown) = (Vec::new(), "", "");
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let more = lines[i + 1..]
            .iter()
            .take_while(|c| c.starts_with(' '))
            .count();
        let entry = &lines[i..i + 1 + more];
        i += 1 + more;
        if l.starts_with("==") {
            section = l;
            continue;
        }
        let hit = |t: &str| t.to_lowercase().contains(f);
        if l.trim().is_empty() || !(hit(section) || entry.iter().any(|e| hit(e))) {
            continue;
        }
        if !section.is_empty() && shown != section {
            out.push(section);
            shown = section;
        }
        out.extend(entry);
    }
    out
}

/// Help lines (with their continuation lines) of the key a footer label names, e.g. "S-F4 Ext" -> Shift+F4.
pub fn key_help(help: &str, label: &str) -> String {
    let key = label
        .split(' ')
        .next()
        .unwrap_or_default()
        .replace("S-", "Shift+");
    let mut out = Vec::new();
    let mut taking = false;
    for l in help.lines() {
        let col = l.get(..16).unwrap_or(l);
        if col.trim().is_empty() && !l.trim().is_empty() {
            if taking {
                out.push(l.trim());
            }
            continue;
        }
        taking = col.split(' ').chain(col.split('/')).any(|t| t == key);
        if taking {
            out.push(l.trim());
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{HelpLine, help_filter, help_line};

    #[test]
    fn help_lines_split_and_filter_by_entry() {
        assert_eq!(help_line("== Move =="), HelpLine::Section("Move"));
        assert_eq!(
            help_line("Ctrl+B          bookmarks"),
            HelpLine::Entry("Ctrl+B", "bookmarks")
        );
        assert_eq!(help_line("                more"), HelpLine::More("more"));
        assert_eq!(
            help_line("Free text, no key"),
            HelpLine::Text("Free text, no key")
        );
        let text = "== A ==\nx               one\n                two\ny               three\n\n== B ==\nz               four";
        assert_eq!(
            help_filter(text, "two"),
            ["== A ==", "x               one", "                two"]
        );
        assert_eq!(help_filter(text, "b"), ["== B ==", "z               four"]);
        assert!(help_filter(text, "nothing").is_empty());
    }
}
