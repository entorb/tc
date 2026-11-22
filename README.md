# Torben's Commander

[![Lines of Code](https://sonarcloud.io/api/project_badges/measure?project=entorb_tc&metric=ncloc)](https://sonarcloud.io/summary/overall?id=entorb_tc)
[![Security issues](https://sonarcloud.io/api/project_badges/measure?project=entorb_tc&metric=software_quality_security_issues)](https://sonarcloud.io/summary/overall?id=entorb_tc)

Cross-platform (Windows, macOS, Linux) two-panel file manager in Rust, inspired by
[Norton Commander](https://en.wikipedia.org/wiki/Norton_Commander),
[Midnight Commander](https://github.com/MidnightCommander/mc),
[Total Commander](https://www.ghisler.com),
[Double Commander](https://github.com/doublecmd/doublecmd),
[Krusader](https://github.com/kde/krusader),
[yazi](https://github.com/sxyazi/yazi),
[ncdu](https://dev.yorhel.nl/ncdu),
[btop](https://github.com/aristocratos/btop) and
[QDirStat](https://github.com/shundhammer/qdirstat).

## Highlights

- **File Stats (ncdu)**: Ctrl+Space shows the biggest files and directories of a tree; browse, view, edit, delete
- **SFTP / FTP / FTPS**: Ctrl+N, remote panels work like local ones
- **Processes (btop)**: Ctrl+P, sort, filter, kill; on an SFTP panel those of the server
- **Synchronize directories**: Ctrl+E, like Total Commander
- **Multi-rename**: Ctrl+M, like Total Commander, with undo

**F1** shows every key of the current screen (type to filter, Enter runs the key). **F9** opens a menu with every feature, for terminals that swallow keys. This README is the overview; F1 is the reference.

## Front ends

- `tc-cli`: terminal UI (ratatui)
- `tc-gui`: desktop window (egui) with mouse, toolbar, tab buttons and system clipboard

Both share the app logic (`crates/tc-cli` library): keys, dialogs and F1 help are identical.

## Build and run

Needs stable Rust (`rustup`, edition 2024). From the repo root:

```sh
scripts/run-cli.sh    # terminal UI, Windows: scripts\run-cli.cmd
scripts/run-gui.sh    # desktop window, Windows: scripts\run-gui.cmd
sh scripts/build.sh   # release binaries into target/release/, Windows: scripts\build.cmd
scripts/run_checks.sh # same checks as CI
```

Both open in the current directory; extra arguments are passed on. On Linux `tc-gui` needs X11 or Wayland with `libxkbcommon` and OpenGL.

Downloaded binaries are not signed:

- macOS: `xattr -d com.apple.quarantine ./tc-cli && chmod +x ./tc-cli` (same for `tc-gui`)
- Windows: SmartScreen **More info → Run anyway**, or `Unblock-File .\tc-cli.exe`

## Features

### Panels

- Norton Commander look; two panels with tabs (Ctrl+T); local tabs are restored at start
- tc-gui font size (Ctrl+= / Ctrl+-, F9 → Panel) is saved with the session
- Left / Right go to the parent / into the directory under the cursor
- Directories load in a worker; huge or slow directories stay usable, Esc cancels
- Select by key, mouse, glob mask (`+` / `-` / `*`); Space sizes a directory
- Sort by name, extension, date, size (Ctrl+F3..F6 or title click)
- Quick search (Ctrl+S), quick filter (`/`); dot files are always shown
- History (Alt+Left/Right), drives (Alt+F1/F2), bookmarks (Ctrl+D adds, Ctrl+B lists: Insert/`a` adds, Delete/Backspace removes, F4/`e` edits the alias, Ctrl+Up/Down or Shift+Up/Down reorders; file lines `alias<TAB>path` or `path`, older files load unchanged, a tab in an alias-less path of an old file misparses)
- Compare panels (Ctrl+Y) selects differences by name, size, date
- File clipboard (Ctrl+C / X / V), also to and from Explorer, Finder or a Linux file manager (needs `wl-copy`/`wl-paste` or `xclip`)
- Enter opens a file in the default application; F3 on media and office files shows the OS quick preview

### File operations

- F5 copy, F6 move in a worker with progress, speed, ETA; Esc cancels and removes the partial target
- Conflict dialog: overwrite, skip, rename, newer only (also for all). Overwrite goes through a temp file and rename; a move deletes the source only after success
- F2 / Shift+F6 rename (F2 with several marked opens multi-rename), F7 mkdir, F8 delete (trash or permanent)
- Multi-rename (Ctrl+M): name / extension masks with placeholders, regex find / replace, case, counter, live preview, presets, history, names from a pasted list or the editor, undo of the last batch. Duplicate or invalid names are refused before anything moves. `tc-cli`: terminals send Ctrl+M as Enter, use F9 → File

### Search and analysis

- Find files (Ctrl+F): name masks or regex, content (plain, regex, whole words, not containing), size, date, depth, `.gitignore`, hidden; saved searches and history. Parallel walk, results capped at 100000
- Find results: Ctrl+F3 path, Ctrl+F4 file name, Ctrl+F5 date, Ctrl+F6 size sort them (same key reverses, new results are sorted in as they stream); view / edit at the match, Enter feeds them to a virtual panel (`[found]`) for file operations
- Remote find runs one GNU `find` (+ `grep`) on the server; no name regex, `.gitignore` or line numbers
- File Stats (Ctrl+Space): size or file count, %, bar; mark, delete, rescan, feed to panel. Up to 8 threads; whole tree in memory. Remote: one `find` on SFTP servers, else parallel listing
- Synchronize directories (Ctrl+E), local or remote: mask, subdirectories, by content, ignore date, asymmetric. Per row direction, delete, skip, byte compare, `*` reverse all; filter rows; settings history. Limits: remote targets get the copy time (use Ignore date), no FAT 2 s tolerance, stops at the first error

### Processes (Ctrl+P)

- CPU, memory, swap, load; list with PID, user, CPU %, memory, command; sort, filter, kill, interval 0.5 to 3600 s
- On an SFTP panel: the server's `/proc` and `ps`. Non-Linux servers show lifetime CPU %; FTP not supported
- Not included: graphs, per-core meters, temperatures, disks, network, GPU, tree view

### Command line

- `:` runs a command in the panel directory (`sh -c`, Windows `cmd /C`), history saved. On an SFTP panel via the system `ssh -t`
- `tc-gui` has no terminal: output opens in the viewer, interactive commands do not work
- Limits: blocks tc while running, no `cd` tracking, no completion

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
- Editor: undo / redo, selection, clipboard (OSC 52 in terminals), find / replace (regex, one by one), go to line, wrap, line numbers, bracket match, encoding and CRLF detection; warns when the file changed on disk. Files over 1 GB or free RAM offer the viewer
- Markdown: save (and F5) adds blank lines around headings, lists and ``` blocks, one undo step; never removes lines
- Syntax colors for common languages (rs, c/cpp, js/ts, java, go, py, sh, rb, lua, php, sql, toml/yaml, properties, json, html/xml, md); per row only, multi-line strings and comments may color wrong
- Shift+F4 opens the cursor file (or a new one) in an external editor (`code`)

### Config

Plain text in `$XDG_CONFIG_HOME/tc/` (`~/.config/tc/`, `%APPDATA%\tc\`), private (0600), written atomically: bookmarks, connections (no passwords), searches, presets, form and command histories (last 500), session. F9 → Tools → Delete config files removes them.

## Planned

- Windows, macOS and Linux build images
- File Stats: [QDirStat](https://github.com/shundhammer/qdirstat) display
- SMB shares: the `smb` crate pins pre-release crypto crates that conflict with russh; add once they release against `curve25519-dalek` 5.0.0

## Tech stack

Rust, AI coding.
