//! F1 help texts: per-mode text, line parsing, filter and footer-key lookup.

use crate::app::Mode;

pub const HELP_MAIN: &str = "\
Type to filter this help, Up/Down select, Enter runs the key.

== Highlights ==
F9              menu with all features (when keys are eaten)
Ctrl+B          bookmarks
Ctrl+Z          popular directories (most visited and recent)
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
Alt+Down        directory history of the panel (also the 🕓 button
                top right of the panel; ★ next to it: bookmarks)
Ctrl+K          sync browsing on / off: entering a directory or
                going up repeats it in the other panel (local
                only; off when it cannot follow)
~               home directory
Ctrl+\\          root directory
Ctrl+R          refresh (also the free space shown in the status line);
                local panels refresh by themselves when files are
                added, removed or renamed (checked each second;
                a file growing in place needs Ctrl+R)
Alt+F1          drives for left panel
Alt+F2          drives for right panel
Ctrl+D          add this directory to the bookmarks
Ctrl+U          swap panels
Ctrl+T          new tab (also the ✚ button left of ★)
Ctrl+W          close tab
Ctrl+Shift+T    reopen the last closed tab (local, last 10)
Ctrl+Shift+O    move the tab to the other panel (not the last one)
Ctrl+=/Ctrl+-   font size (saved)
Ctrl+PgDn       next tab
Ctrl+Tab        next tab
Ctrl+PgUp       previous tab
Alt+=           other panel to this directory
Ctrl+Left       right panel: cursor directory into the left panel
Ctrl+Right      left panel: cursor directory into the right panel
Ctrl+F3         sort by name (Ctrl+F4 ext, F5 date, F6 size;
                again: reverse)
Esc             clear filter, leave found list
Ctrl+G          found list: go to the file's directory

== Mouse ==
Wheel           scroll the panel under the pointer, 3 entries
Click           cursor to the entry, activates the panel
Shift+click     select range from the cursor
Ctrl+click      select / deselect the entry (Cmd+click on macOS)
Middle click    open the directory in a new tab
Right click     menu: view, edit, copy, move, rename, delete,
                clipboard, same extension, properties, terminal
Double click    open directory / archive, view file
Click title     sort by name / ext / size / date (again: reverse)
Click F-bar     run the key shown on the label
Click menu bar  open the F9 menu
R-click tab     close the tab (not the last one)
Click cmd line  type a command, like :
✖ / Click out   close a popup (Esc); a running copy,
                scan or connect only by ✖
Shift held      F-bar shows the Shift actions

== Colors ==
Dim entries     git-ignored (.gitignore, info/exclude, global
                excludes) and .git dirs; local panels only

== Selection ==
Insert          select entry, cursor moves down
Space           same; on a local directory also sizes it
Shift+Up/Down   mark / unmark the entry and move on
Shift+PgUp/PgDn same for a page of entries
Shift+Home/End  mark up to the first / last entry; again on a
                fully marked range: unmark it
Ctrl+A          select all, directories too
Ctrl+Shift+A    unselect all
Alt+.           select files with the cursor file's extension
Ctrl+Shift+R    restore the selection dropped by unselect all or
                by leaving the directory (same directory only)
+               select by mask
-               deselect by mask
*               invert selection
Ctrl+Y          compare with the other panel and select, per
                panel, its own: newer + single (default),
                newer, single (only here), different + single,
                different (size or date); Enter repeats the last

== Search ==
a..z, 0..9      type a name to start quick search (any letter
                or digit); more letters, digits and . _ - narrow it
Up / Down       quick search: previous / next match
                Esc stops searching; other keys stop it and act on the
                cursor file (F-keys, Enter opens)
/               filter popup: name part, min / max size (10k, 5M),
                newer / older than (30m, 12h, 7d), file types of
                this directory (checkboxes; none = all; types and
                sizes hide directories). Tab / Up / Down move,
                Space toggles a type, Enter applies, Esc cancels.
                The filter ends on leaving the directory; Esc clears it
Ctrl+F          find files
Ctrl+E          synchronize directories (left / right panel)
:               command line (Up/Down: earlier commands)
Ctrl+O          open a terminal in this directory (local panels only)

== Files ==
F3              view (media: preview)
F4              edit (media: default application)
Shift+F4        edit in external editor (asks the name,
                creates a missing file)
F5              copy (Windows without symlink rights: directory
                links become junctions, file links copies)
F6              move
F2              rename; with several marked: multi-rename
Shift+F6        rename
Ctrl+M          multi-rename: masks, counter, find / replace
F7              make directory
F8              delete: y / Enter trash, p permanent,
                n / Esc cancel (remote panels: only p); also Delete
                copy / move / delete over 1 s: progress of
                bytes, files, dirs with rates per second, ETA;
                Esc cancels
Ctrl+Insert     copy paths to clipboard (Alt+Shift+C)
Ctrl+C          copy files
Ctrl+X          cut files
Ctrl+V          paste copied / cut files here
Symlink         (F9 File) link to the cursor entry in the other
                panel's directory; asks the target, then the link
                path; never replaces; local panels only
Hard link       (F9 File) same for a hard link; files only
Alt+Enter       properties of marked entries, else the cursor
                entry (F9 File): Unix mode, owner, group;
                elsewhere read-only; local panels only

== Tools ==
Settings        (F9 Tools) daily update check on / off
Delete config files
                (F9 Tools) delete tc's saved files: bookmarks,
                popular dirs, connections, histories, searches, session

== Quit ==
F10             quit";
pub const HELP_STATS: &str = "\
== About ==
Scan runs in up to 8 threads; a popup shows files and
scanned / discovered directories.
Windows from 700 px wide show a treemap right of the list: tiles
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
Tab             show and sort by size / file count (count
                mode treemap: dirs only by file count, 2 levels)
F5              rescan the browsed directory, totals above update
/               filter the entries by name: Enter keeps,
                Esc clears; 🔍 on the Name column while active
Wheel           move cursor 3 entries

== Files ==
F3              view file
F4              edit file
Shift+F4        edit in external editor
i               info: path, size, files, directories, date
p               feed marked / shown entries to the panel
Insert / Space  mark entry, move down
F8 / Del / d    delete marked entries, else the selected one,
                tree updates without rescan:
                y / Enter trash, p permanent, n / Esc cancel
:               command line in this directory
Esc / q / F10   close (while scanning: cancel)";
pub const HELP_SYNC: &str = "\
== About ==
Opening compares the files directly in the two panel
directories (subdirectories are not compared or synced)
and lists the ones that differ; equal files are not listed.
Equal: same size and date. Status: ➡ (blue) only on the
left, ⬅ (green) only on the right, ≠ (red) size or date
differ; the comment
shows left against right: size 4 B < 7 B, date ... > ...
A sync is one way: every listed file of the source is
copied, overwriting the target's file even when that one
is newer. Copies keep the date on local disks; remote
targets get the copy time, so use Ignore date against
SFTP / FTP.

== Options ==
Ignore date: same size = equal.
Mirror: files only on the target are deleted, so it
becomes a copy (local: to the trash, remote: permanently).
Up/Down/Tab     next / previous option
Space           toggle the option (click too)

== Keys ==
F5              Left -> Right: make the right side match
                the left; y starts, Esc goes back
F6              Right -> Left: the same the other way
F2 / Ctrl+R     reload: list both directories again
PgUp/PgDn       scroll the list
Home/End        first / last row
Esc / F10       close (while comparing: cancel)";
pub const HELP_SETTINGS: &str = "\
== Fields ==
Up/Down / Tab   previous / next field
Space           toggle checkbox
Click           focus a field; toggles a checkbox, cycles a choice

== Options ==
Check for updates daily: release builds ask GitHub for a
newer release on the first start of a day and show it.
Font size: Left/Right/Space change it; also Ctrl+=
and Ctrl+-.

== Run ==
Every change is saved at once.
Esc / F10       close";
pub const HELP_PROPS: &str = "\
== Fields ==
Up/Down / Tab   previous / next field
Space           toggle checkbox
Click           focus a field; toggles a checkbox, cycles a choice
Left/Right      move in text
Ctrl+Left/Right word left / right (Alt too: Mac Option)
Ctrl+Bksp/Del   delete word left / right (Alt too)

== Options ==
One file: the fields show its values; only changed fields are
applied. Several entries or a directory: the fields start empty
(the first entry's values are shown below them); every filled
field is applied to all, inside directories too if Recursive.
An empty field is left alone.
Mode: octal, 1 to 4 digits (4755 = setuid); the line under the
fields previews it as rwxr-xr-x. Symlinks keep their mode
(chmod would change the target).
Owner / Group: name or number; the link itself, not its target.
Read-only (not Unix): the file attribute.
Recursive (directories): also everything inside; symlinked
directories are not entered.

== Run ==
Enter           apply; stops at the first error, what was done
                stays done; Esc cancels while it runs
Esc / F10       close";
pub const HELP_FIND_FORM: &str = "\
== Tabs ==
F2 F3 F4 F5     show tab 1 to 4 (Name, Content, Folders,
                Size/Date; Alt+1 .. Alt+4 too)
Ctrl+PgUp/PgDn  previous / next tab
Click           a tab title, or a field of the tab
A tab title with a dot has a field filled or a box ticked.

== Fields ==
Tab             next field (the tab follows)
Shift+Tab       previous field
Up/Down         previous / next field of the shown tab
Space           toggle checkbox
Left/Right      move in text
Ctrl+Left/Right word left / right (Alt too: Mac Option)
Ctrl+Bksp/Del   delete word left / right (Alt too)
Alt+Up/Down     earlier / later searches (history)

== Name tab ==
Name (masks): `*` any run, `?` one character, several
masks separated by comma or space; empty = any name.
Name is a regex: the field is a regular expression
instead (local panels only). Case-sensitive: names match
exactly. Extensions: comma separated, with or without
dot. Exclude files: masks of files to leave out.

== Content tab ==
Typing in Content turns on Search file contents.
Plain text, `*` matches any run within a line. Content is
a regex uses a regular expression, Whole words, Not
containing (files without the text), List every match
(one result per line). No capital letter = ignore case
unless Content case-sensitive is on.

== Folders tab ==
Exclude dirs: names or paths with `/` below the start
directory, comma separated. Skip hidden: dot names.
Use .gitignore (local only). Follow symlinks. Max depth:
1 = only this directory.

== Size/Date tab ==
Min / Max size: number with K, M, G or T (1024 steps),
e.g. 10k, 2 MB. Modified after / before: YYYY-MM-DD.
Not older than: 30m, 12h, 7d, 2w (bare number = days).

== Run ==
Enter / F10     start the search
Esc / F9        cancel (the form is kept for next time)";
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
== Tabs ==
F2 F3 F4 F5 F6 show tab 1 to 5 (Name, Replace,
                Placeholder, Clean up, Editor; Alt+1 .. Alt+5 too)
Ctrl+PgUp/PgDn  previous / next tab
Click           a tab title, or a field or button of the tab
A tab title with a dot has options set away from the defaults.

== Fields ==
Tab             next field (the tab follows)
Shift+Tab       previous field
Up/Down         previous / next field of the shown tab
Space           toggle checkbox, cycle choice
Left/Right      move in text, cycle choice
Ctrl+Left/Right word left / right (Alt too: Mac Option)
Ctrl+Bksp/Del   delete word left / right (Alt too)
Alt+Up/Down     earlier / later settings (history)
PgUp/PgDn       scroll the preview (mouse wheel too)

== Name tab ==
Text stays as typed; [..] placeholders are replaced:
  [N] name, [E] extension, [N2-5] characters 2 to 5,
  [N2,3] 3 characters from 2, [N-3-] last 3 to end
  (negative counts from the end)
  [C] counter (start, step, digits below)
  [Y] [M] [D] year, month, day of the modified date
  [h] [n] [s] hour, minute, second
  [[] a literal [
The buttons [N] [E] [C] [Y] [M] [D] insert into the
focused field.

== Replace tab ==
Search and Replace: Plain finds the text as typed;
Case-sensitive applies to Plain only.
Regex: $1 or ${1} in Replace is the first group. Use
(?-i) for case-sensitive.

== Placeholder tab ==
Search and Replace use placeholders, applied once after
the Replace tab's find. %a matches any text, %n only
digits. Each %letter in Search is a group; in Replace,
%letter puts that group back (other %letters stay as
typed, %% is a %). %a before a literal stops at its
first occurrence: %a_ matches chk_ in chk_rs_asd. A
placeholder at the end of Search takes the rest.
Examples (Search -> Replace: name in -> name out):
  %a_%b_%c_%d -> %d-%c-%b-%a: one_two_three_four ->
  four-three-two-one
  IMG_%n -> Photo %n: IMG_12 -> Photo 12

== Clean up tab ==
Underscore to space     'two_words' -> 'two words'
Remove multiple spaces  'long    space' -> 'long space'
Space before capitals   'OneLongWord' -> 'One Long Word'
Remove dots             'A.B.C.' -> 'ABC', 'Mr.' -> 'Mr'
Trim spaces             '( a ) ' -> '(a)': the ends and
                        inside ( [ { ) ] }
Add to first number     'Picture 1' with 10 -> 'Picture 11',
                        'IMG_007' with 1 -> 'IMG_008'
Case: unchanged, lower, UPPER, First letter, Every Word,
Every Word with small words kept ('the end of a dream' ->
'The End of a Dream').

== Editor tab ==
F6              show the Editor tab (the new names, one per line)
The Editor starts from the other tabs' result; edit it
freely: Enter adds a line, Ctrl+Z undoes. Rename applies
the names in the Editor; the line count must match the
files. A setting in another tab starts the Editor over.
F7              restore the edited names a setting replaced

== Names from a list ==
Paste (several lines)  new names, one per line
(as many as files; edit a field to use the masks again)

== Undo ==
Ctrl+Z         undo the last multi-rename (this session)

== Run ==
Enter / F10     rename and close (the preview shows every new
                name; the form stays open on a problem)
Esc / F9        cancel";
pub const HELP_LISTS: &str = "\
== Bookmarks (Ctrl+B) ==
Up/Down         move
Enter / Click   go to the directory in the active panel
Insert / a      add the active panel's directory (✚; greyed
                while it is in the list)
F4 / e          edit page: all bookmarks (Edit… button)
Ctrl+Up/Down    move the bookmark (also Shift+Up/Down), saved at once
Delete/Bksp     remove the selected bookmark (➖)
/               filter: Enter keeps, Esc clears
Esc / q / F10   close (also a click beside the list)

== Edit bookmarks (F4 in the bookmarks) ==
Tab             next field: alias, path, next row (Shift+Tab back)
Up/Down         previous / next row
Ctrl+Up/Down    move the row (⏶ ⏷; also Shift+Up/Down)
Insert          add the active panel's directory (✚)
F8              delete the row (➖; also Ctrl+Delete)
Enter           save all (Save; empty alias shows the path)
Esc / F10       discard changes, back to the list (Cancel)

== Popular directories (Ctrl+Z) ==
Up/Down         move, best ranked (visits, recent first) on top
Enter           go to the directory in the active panel
Delete/Bksp     forget the directory
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

== Compare (Ctrl+Y) ==
Up/Down         move
Enter           select by the row, both panels
Esc / q / F10   close

== Drives (Alt+F1 / Alt+F2) ==
Up/Down         move
Enter           go to the mount point
Esc / q / F10   close

== Directory history (Alt+Down) ==
Up/Down         move, newest on top
Enter / Click   go there (a history step: back / forward stay)
Esc / q / F10   close";
pub const HELP_CONFLICT: &str = "\
== Target exists ==
o / Enter       overwrite this file
s               skip this file
n               overwrite only if the source is newer
r               rename: type a new name for the target
O               overwrite all further conflicts
S               skip all further conflicts
N               newer-only for all further conflicts
Esc / c         cancel the whole operation

== A file failed (copy, move, delete) ==
r / Enter       retry it
s               skip it: the rest goes on; a move keeps
                its source, a delete its directory
S               skip all further failed files
Esc / c         cancel the whole operation";
pub const HELP_PROCS: &str = "\
== About ==
CPU% is the average over the interval, per core (can
exceed 100). Load = average number of busy processes
over the last 1 / 5 / 15 minutes; above the core count
means overloaded. The cursor keeps its row while the
list re-sorts. On an SFTP panel the list comes from the
server's /proc and ps. RUN is the time since the
process started, in minutes.

== Move ==
Up/Down         move cursor
PgUp/PgDn       page up / down
Home/End        first / last process
Mouse wheel     scroll
Click header    sort by CPU or memory (GUI)

== Keys ==
Tab             toggle sort: CPU, memory, runtime
                (largest first)
c / m / r       sort by CPU / memory / runtime
/ or f          filter popup, all fields optional:
                Find: id, command or name, * wildcard
                  (fire*, *--x*, 42*; no * = substring)
                Min MEM (MB), Min CPU (%), Min runtime (min)
                Tab / Up / Down move, Enter applies,
                Esc cancels; Esc with no popup clears
Ctrl+R / F5     sample now
s               update interval in seconds (default 2):
                Enter sets, Esc keeps
i               info popup of the selected process
                i / Esc / F10 or a click outside closes
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
/ f F7 Ctrl+F   search dialog (Tab / Up / Down move, Space
                checks, Enter runs, F1 regex syntax)
n / Shift+F7    next match
N               previous match
Alt+C           case sensitive / smart case
                hex: jumps to the row where the matching line starts

== View ==
h / Alt+H       hex view (offset | bytes | ASCII)
Alt+Left/Right  previous / next file of the panel (skips dirs and media)
w / Alt+W       wrap long lines
l / Alt+L       line numbers
F4              edit this file at the top line (local files)
                a file changed on disk is read again (checked
                each second, position kept)
Esc / q / F10   close
F3              close";
pub const HELP_EDIT: &str = "\
== File ==
F2 / Ctrl+S     save: trailing blanks trimmed, final line break
                added; Markdown first gets blank lines around
                headings, lists and ``` blocks, and runs of
                blank lines shrink to one
F5              Markdown: do that now (undo: Ctrl+Z)
Esc / F10       exit (asks if unsaved: y / Enter saves)
                changed on disk (checked each second, and on
                save): no edits: read again; else asks:
                y overwrite, r reload (drops edits), n keep

== Move ==
Up/Down         line up / down
PgUp/PgDn       page up / down
Left/Right      character left / right
Home/End        start / end of line (Mac also Cmd+Left/Right)
Ctrl+Left       word left (also Alt+Left: Mac Option+Left)
Ctrl+Right      word right (also Alt+Right)
Ctrl+Home/End   start / end of file
Ctrl+G          go to line
Mouse wheel     scroll
Mouse           click, drag, Shift+click select

== Search ==
F3 / F7 Ctrl+F  search and replace dialog (Tab / Up / Down
                move, Space checks, Enter runs, F1 regex
                syntax, Alt+Up/Down earlier patterns)
Shift+F7        next match
Shift+F3        previous match
Ctrl+R          dialog with Replace on
Alt+R           Replace on and Confirm each: y / Enter
                replace, n / Space skip, a all, other key
                stops
Alt+C           case sensitive / smart case

== Edit ==
Enter           new line, keeps the indentation
Tab             insert a tab
Shift+Tab       unindent line / selection
Backspace       delete left
Delete          delete right
Ctrl+Backspace  delete word left (also Alt+Backspace)
Ctrl+Delete     delete word right (also Alt+Delete)
Ctrl+Z          undo
Ctrl+Y          redo
Shift+arrows    select
Ctrl+A          select all
Ctrl+C          copy (also system clipboard); no selection:
                the whole line
Ctrl+X          cut; no selection: the whole line
Ctrl+V          paste; a whole line goes above the cursor
                line
Ctrl+K / F8     delete selection or line
Ctrl+D          duplicate line
Ctrl+L          select line, again: one more
Alt+Up/Down     move line / selected lines up / down
                (Mac Option+Up/Down)
Alt+Shift+Up    copy line above (Alt+Shift+Down: below)
Ctrl+Enter      new line below (Ctrl+Shift+Enter: above)
Alt+W           wrap on / off
Alt+L           line numbers on / off";

pub const HELP_SEARCH: &str = "\
== Fields ==
Find            text to search for; empty repeats the last
                search; Alt+Up/Down earlier patterns
Replace with    text put in place of each match (Replace on)

== Options ==
Replace         replace the matches (Enter: all at once)
Confirm each    ask at each match: y / Enter replace,
                n / Space skip, a all, other key stops
Regex           Find is a regular expression (see below);
                off: the text is matched literally, and
                `$` in Replace with is not special
Case sensitive  on: exact case; off: smart case (an upper
                case letter in Find makes it exact)

== Regex syntax ==
.               any character except line break
^ $             start / end of line
\\d \\w \\s      digit, word character, whitespace
\\b             word boundary
[abc] [^a-z]    one of the set / none of the range
a|b             a or b
( ... )         group, numbered from 1
(?: ... )       group, not numbered
(?P<name> ...)  named group
a* a+ a?        0 or more, 1 or more, 0 or 1
a{2,5}          2 to 5 times
(?i)            ignore case in this part of the pattern
\\n \\t         newline, tab

== Replace with (Regex on) ==
$0              the whole match
$1 ${1}        text of group 1 (${1}x when letters follow)
$name           text of a named group
$$              a dollar sign

A match cannot span lines: `\\n` never matches.
Esc / F10 close";

/// Title and text of the F1 help for `mode`.
pub fn help(mode: &Mode) -> (&'static str, &'static str) {
    match mode {
        Mode::View { .. } => ("Help: viewer", HELP_VIEW),
        Mode::Edit { .. } => ("Help: editor", HELP_EDIT),
        Mode::Stats(_) => ("Help: file stats (ncdu)", HELP_STATS),
        Mode::Procs(_) => ("Help: processes (top)", HELP_PROCS),
        Mode::Sync(_) => ("Help: synchronize directories", HELP_SYNC),
        Mode::FindForm(_) => ("Help: find files", HELP_FIND_FORM),
        Mode::Settings(_) => ("Help: settings", HELP_SETTINGS),
        Mode::Props(_) => ("Help: properties", HELP_PROPS),
        Mode::Found(_) => ("Help: found files", HELP_FOUND),
        Mode::MultiRename(_) => ("Help: multi-rename", HELP_RENAME),
        Mode::Bookmarks(_)
        | Mode::BookmarkEdit(_)
        | Mode::Popular(_)
        | Mode::Connections(_)
        | Mode::Drives(..)
        | Mode::DirHistory(..)
        | Mode::Compare(_) => ("Help: lists", HELP_LISTS),
        Mode::Xfer(_) => ("Help: file exists, file failed", HELP_CONFLICT),
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
