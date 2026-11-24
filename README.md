# Torben's Commander

[![Lines of Code](https://sonarcloud.io/api/project_badges/measure?project=entorb_tc&metric=ncloc)](https://sonarcloud.io/summary/overall?id=entorb_tc)
[![Security issues](https://sonarcloud.io/api/project_badges/measure?project=entorb_tc&metric=software_quality_security_issues)](https://sonarcloud.io/summary/overall?id=entorb_tc)

Since I use computers there has always been a two-panel filemanager at my fingertips:
[Norton Commander](https://en.wikipedia.org/wiki/Norton_Commander), [Midnight Commander](https://github.com/MidnightCommander/mc), [Krusader](https://github.com/kde/krusader), [Total Commander](https://www.ghisler.com), ...

Now there is one more, to rule them all ;-)

Written (by AI) in Rust, with focus on performance, available for MacOS, Windows, and Linux, see [Releases](https://github.com/entorb/tc/releases)

## Inspired by

The file managers
[Norton Commander](https://en.wikipedia.org/wiki/Norton_Commander),
[Midnight Commander](https://github.com/MidnightCommander/mc),
[Krusader](https://github.com/kde/krusader),
[Total Commander](https://www.ghisler.com),
[Double Commander](https://github.com/doublecmd/doublecmd),
[yazi](https://github.com/sxyazi/yazi)

Adding features of
[ncdu](https://dev.yorhel.nl/ncdu),
[top/btop](https://github.com/aristocratos/btop) and
[QDirStat](https://github.com/shundhammer/qdirstat).

## Highlights

- **Highly performant**
- **SSH / SFTP / FTP / FTPS**: Ctrl+N, remote panels work like local ones
- **File Stats (ncdu)**: Ctrl+Space shows the biggest files and directories of a tree; browse, view, edit, delete; toggle between bytes and file count
- **Processes (top/btop)**: Ctrl+P, sort, filter, kill; on an SSH/SFTP panel those of the server
- **Synchronize directories**: Ctrl+E
- **Multi-file-rename**: Ctrl+M with undo

**F1** shows every key of the current screen (type to filter, Enter runs the key). **F9** opens a menu with every feature. This README is the overview; F1 is the reference.

Desktop app (egui) with icons, mouse, toolbar, tab buttons and system clipboard.

## Build and run

Needs stable Rust (`rustup`, edition 2024). From the repo root:

```sh
scripts/run.sh/.cmd        # build and run tc
sh scripts/build.sh/.cmd   # release binaries into target/release
scripts/run_checks.sh/.cmd # same checks as CI
```

Opens in the current directory; extra arguments are passed on. On Linux `tc` needs X11 or Wayland with `libxkbcommon` and OpenGL.

Releases are zips (`tc-<OS>-<arch>.zip`) holding the plain `tc` / `tc.exe`. The binaries are not signed:

- macOS: `xattr -d com.apple.quarantine ./tc`
- Windows: SmartScreen **More info → Run anyway**, or `Unblock-File .\tc.exe`

## Features

### Panels

- Norton Commander look; two panels with tabs (Ctrl+T); local tabs are restored at start; Ctrl+Shift+T reopens a closed local tab (last 10), Ctrl+Shift+O moves a tab to the other side
- Font size (Ctrl+= / Ctrl+-, F9 → Tools → Settings) is saved with the session
- Left / Right go to the parent / into the directory under the cursor
- Tool screens (find results, sync, File Stats, processes, multi-rename) cover the whole window with their own F-key bar
- Popups close by their ✖ button or a click beside them (running copies, scans and connects only by ✖); F1 help opens in its own window (move it beside tc, which stays usable; clicking a key line runs it in tc)
- Status line shows the free space of the local filesystem (not remote, archive or found lists)
- Directories load in a worker; huge or slow directories stay usable, Esc cancels
- Select by key, mouse (Shift / Ctrl+click), glob mask (`+` / `-` / `*`), all (Ctrl+A), same extension (Alt+.); Ctrl+Shift+R restores a dropped selection (same directory only); Space sizes a directory; status line shows `n of total selected`
- Right-click menu on entries (view, edit, copy, move, rename, delete, clipboard, properties, terminal); middle click opens a directory in a new tab
- Sort by name, extension, date, size (Ctrl+F3..F6 or title click: Name, Ext, Size, Date)
- Quick search (type a name, or Ctrl+S), filter popup (`/`: name, size, age, file types of the directory; ends on leaving it or Esc); dot files are always shown; F10 quits
- Git: ignored entries (`.gitignore`, `info/exclude`, global excludes) and `.git` dirs shown dim; local panels only, re-read on reload
- History (Alt+Left/Right; Alt+Down or the 🕓 panel button lists it), Ctrl+Left/Right shows the cursor directory in the other panel, drives (Alt+F1/F2), bookmarks (Ctrl+D adds, Ctrl+B lists: Insert/`a` adds, Delete/Backspace removes, F4/`e` edits the alias, Ctrl+Up/Down or Shift+Up/Down reorders; file lines `alias<TAB>path` or `path`, older files load unchanged, a tab in an alias-less path of an old file misparses)
- Sync browsing (Ctrl+K): entering a directory or going up in one panel repeats it in the other; local directories only, switches off when the other panel cannot follow
- Popular directories (Ctrl+Z): local directories you visit, ranked by visits and recency (max 500, saved in `popular`); Enter goes there, Delete forgets; not remote, archive or found lists
- Compare panels (Ctrl+Y): per panel select newer, single (only there), different (size or date), or combinations; files only, same-named directories never differ; Enter repeats the last choice
- File clipboard (Ctrl+C / X / V), also to and from Explorer, Finder or a Linux file manager (needs `wl-copy`/`wl-paste` or `xclip`)
- Enter opens a file in the default application; F3 on media and office files shows the OS quick preview

### File operations

- F5 copy, F6 move, F8 delete in a worker; after 2 s a popup shows bytes, files, dirs done / total with rates per second and an ETA (regression on bytes, refreshed each second); Esc cancels and removes the partial target
- Conflict dialog: overwrite, skip, rename, newer only (also for all). Overwrite goes through a temp file and rename; a move deletes the source only after success
- F2 / Shift+F6 rename (F2 with several marked opens multi-rename), F7 mkdir, F8 delete (trash or permanent)
- Symlink and hard link (F9 → File): link to the cursor entry in the other panel's directory, target and link path editable; never replaces an existing path; local panels only, hard links to files only
- Properties (Alt+Enter, F9 → File): info on the marked entries or the cursor entry; Unix: mode (octal with rwx preview), owner and group, optionally recursive; elsewhere the read-only flag; one file: prefilled, only changed fields applied; several or a directory: empty fields, filled ones applied to all; symlinks keep their mode and are never followed; local panels only
- Multi-rename (Ctrl+M): name / extension masks with placeholders, regex find / replace, case, counter, live preview, presets, history, names from a pasted list or the editor, undo of the last batch. Duplicate or invalid names are refused before anything moves.

### Search and analysis

- Find files (Ctrl+F): name masks or regex, content (plain, regex, whole words, not containing), size, date, depth, `.gitignore`, hidden; saved searches and history. Parallel walk, results capped at 100000
- Find results: Ctrl+F3 path, Ctrl+F4 file name, Ctrl+F5 date, Ctrl+F6 size sort them (same key reverses, new results are sorted in as they stream); view / edit at the match, Enter feeds them to a virtual panel (`[found]`) for file operations
- Remote find runs one GNU `find` (+ `grep`) on the server; no name regex, `.gitignore` or line numbers
- File Stats (Ctrl+Space): size or file count, %, bar; mark, delete, rescan, feed to panel; treemap chart beside the list in wide windows (bytes by file type, or file count per dir 2 levels deep; GUI hover tooltip). Up to 8 threads; whole tree in memory. Remote: one `find` on SFTP servers, else parallel listing
- Synchronize directories (Ctrl+E), local or remote: mask, subdirectories, by content, ignore date, asymmetric. Per row direction, delete, skip, byte compare, `*` reverse all; filter rows; settings history. Limits: remote targets get the copy time (use Ignore date), no FAT 2 s tolerance, stops at the first error

### Processes (Ctrl+P)

- CPU, memory, swap, load; list with PID, user, CPU %, memory, runtime, command; sort by CPU or memory (largest first), filter popup (text or PID with `*` wildcards, min MEM, CPU, runtime), kill, interval 0.5 to 3600 s
- On an SFTP panel: the server's `/proc` and `ps`. Non-Linux servers show lifetime CPU %; FTP not supported
- Not included: graphs, per-core meters, temperatures, disks, network, GPU, tree view

### Command line

- `:` runs a command in the panel directory (`sh -c`, Windows `cmd /C`), history saved. On an SFTP panel via the system `ssh -t`
- No terminal: output opens in the viewer, interactive commands do not work
- Limits: blocks tc while running, no `cd` tracking, no completion
- Terminal here: opens the system terminal in the active local directory (`$TERMINAL`, else first installed emulator; macOS Terminal, Windows `cmd`); not on remote or archive panels

### Archives (read-only)

- `.zip`, `.tar`, `.tar.gz`, `.tar.xz`, `.xz` open as directories; view, edit a temp copy, copy out with F5
- Zip-slip entries skipped, links never extracted, entries over 1 GiB refused. No encrypted zip, 7z, rar, bzip2, zstd, nested archives

### Remote (SFTP, FTP, FTPS)

- Saved connections in `tc/connections`; passwords are asked on connect, never stored
- F9 → Go → Disconnect all returns every remote panel and tab to its local directory; a session closes when no panel, tab or screen uses it
- SFTP: host key checked against `~/.ssh/known_hosts`; auth via ssh-agent (Unix), unencrypted ed25519 / ecdsa keys, password. No RSA keys (RUSTSEC-2023-0071)
- FTP: anonymous by default, passive, MLSD with LIST fallback, 60 s timeout. FTPS: explicit TLS
- Copy, move, rename, mkdir, delete (permanent only), view, edit (uploads on save; files over 1 GiB refused)
- Limits: no SMB, no reconnect, no passphrase prompt, no Windows ssh-agent, no IPv6 literals; symlinks never created remotely; permissions and times not copied; no dir size, default app, external editor, archives or bookmarks on remote panels

### Viewer (F3) and editor (F4)

- Viewer streams from disk (multi-GB files): search, go to line or %, wrap, line numbers, encoding detection; `h` hex view (offset, 16 bytes, ASCII; reads only the visible rows; `g` takes an offset `0x..`/decimal/`N%`, find jumps to the row of the matching line); Alt+Left/Right (or Alt+b/f, as macOS terminals send Option+arrows) view the previous / next file of the panel it was opened from (panel cursor follows; skips directories and media files; local and find-result panels only, no wrap at the ends)
- Editor: undo / redo, selection, clipboard, find / replace in one dialog (F3; regex with `$1` groups or plain text, case, one by one; matches within a line), go to line, wrap, line numbers, bracket match, encoding and CRLF detection; warns when the file changed on disk. Files over 1 GB or free RAM offer the viewer
- Markdown: save (and F5) adds blank lines around headings, lists and ``` blocks, one undo step; never removes lines
- Syntax colors for common languages (rs, c/cpp, js/ts, java, go, py, sh, rb, lua, php, sql, toml/yaml, properties, json, html/xml, md); per row only, multi-line strings and comments may color wrong
- Shift+F4 opens the cursor file (or a new one) in an external editor (`code`)

### Config

Plain text in `$XDG_CONFIG_HOME/tc/` (`~/.config/tc/`, `%APPDATA%\tc\`), private (0600), written atomically: bookmarks, connections (no passwords), searches, presets, form and command histories (last 500), session. F9 → Tools → Delete config files removes them.

Release binaries check GitHub for a newer release on the first start of a day (silent when offline); turn it off in the "Update available" dialog or F9 → Tools → Settings. Self-built binaries never check.

## Planned

- SMB shares: the `smb` crate pins pre-release crypto crates that conflict with russh; add once they release against `curve25519-dalek` 5.0.0
