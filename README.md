# Torben's Commander

[![Lines of Code](https://sonarcloud.io/api/project_badges/measure?project=entorb_tc&metric=ncloc)](https://sonarcloud.io/summary/overall?id=entorb_tc)
[![Security issues](https://sonarcloud.io/api/project_badges/measure?project=entorb_tc&metric=software_quality_security_issues)](https://sonarcloud.io/summary/overall?id=entorb_tc)

2-panel file manager inspired by

- [Norton Commander](https://en.wikipedia.org/wiki/Norton_Commander) (Wiki)
- [Midnight Commander](https://github.com/MidnightCommander/mc) (GitHub)
- [Total Commander](https://www.ghisler.com) (HomePage)
- [Double Commander](https://github.com/doublecmd/doublecmd) (GitHub)

## Highlights

- **File Stats (ncdu)**: Ctrl+Space scans the current tree and shows the biggest files and directories with size / count, % and bar; browse, view, edit and delete from there
- **SFTP / FTP / FTPS**: Ctrl+N opens saved connections, remote panels work like local ones (copy, move, view, edit, delete)
- **Favorites / bookmarks**: Ctrl+D bookmarks the directory, Ctrl+B jumps to a bookmark (hotlist)

## Running the macOS build

The downloaded binary is not signed or notarized, so Gatekeeper blocks it ("Apple could not verify “tc-cli” is free of malware"). Run it from Terminal after clearing the quarantine flag:

```sh
xattr -d com.apple.quarantine ./tc-cli
chmod +x ./tc-cli   # artifact zips drop the exec bit
./tc-cli
```

## Running the Windows build

The downloaded `tc-cli.exe` is not signed, so SmartScreen may show "Windows protected your PC": click **More info → Run anyway**. Or clear the download flag in PowerShell and run it from a terminal:

```powershell
Unblock-File .\tc-cli.exe
.\tc-cli.exe
```

## Features

MVP, terminal UI, local file system. F1 shows all shortcuts of the current screen.

### Panels

- Norton Commander look (fixed colors): blue panels, double borders, cyan cursor bar and F-key bar; viewer, editor and find list share it
- Two panels: arrows, PgUp/PgDn, Home/End, Enter, Backspace, Tab switches panel; mouse wheel scrolls 3 entries; click sets cursor, double click opens; footer F-key labels are clickable
- Entry list: size (`B`/`K`/`M`/`G`), date (time only for today), long names end in `…`; prefix `/` dir, `~` dir symlink, `@` file symlink; the inactive cursor is gray
- Status line: with a selection `N selected, X bytes`, else the cursor entry in full (exact size, date, `-> target`)
- Select: Insert/Space, Shift+Up/Down, Shift+click range; `+` / `-` select / deselect by glob mask (`*`, `?`, comma or space separated, case-insensitive), `*` inverts. Directories are never touched by masks
- Space on a directory also calculates its total size in a worker thread (symlinks not followed, results dropped on reload)
- Sort: Ctrl+F3 name, Ctrl+F4 extension, Ctrl+F5 date, Ctrl+F6 size, same key reverses; clicking the title row sorts too. Some terminals grab Ctrl+F-keys
- Alt+. toggles dot-files (Windows hidden attribute ignored)
- Tabs per panel: Ctrl+T opens a tab on the current directory, Ctrl+W closes it (last tab stays), Ctrl+PgDn/PgUp next/previous (also Ctrl+Tab/Ctrl+Shift+Tab where the terminal sends them); title shows `[2/3]` with more than one tab; a tab refreshes when shown; Ctrl+U swaps tabs with the panels. Not saved across restarts
- Directories are read in a worker thread: if the listing takes over 50 ms (huge dir, slow mount) the old list stays usable, the title shows `[loading <dir>, Esc cancels]`, and the new list appears when ready. A hung mount keeps its worker thread until the OS call returns
- Navigation: Ctrl+R refresh, `~` home, Ctrl+\ root, Ctrl+U swap panels, Alt+= other panel follows, Alt+F1/F2 drives/mount points, Alt+Left/Right per-panel history (100 entries)
- Hotlist: Ctrl+D adds the directory, Ctrl+B opens the list (Enter jumps, Delete removes). Stored in `$XDG_CONFIG_HOME/tc/hotlist` (`~/.config/tc/hotlist`, `%APPDATA%\tc\hotlist`), one path per line
- Quick search Ctrl+S (jump to next name containing the text, Ctrl+S again for next) and quick filter Ctrl+F (only matching entries visible, Enter keeps, Esc clears; resets on directory change; hidden entries are not acted on). Both case-insensitive plain substring
- Compare panels: Ctrl+Y selects the differences by name, size or newer mtime in both panels (non-recursive, metadata only)
- Ctrl+Insert or Alt+Shift+C copies full paths of the selection to the system clipboard
- Enter on a file opens it with the default application (`xdg-open`, `open`, `start`; needs a desktop)

### File operations

- F5 copy, F6 move: editable destination prefilled with the other panel (name included for a single item; several items or trailing `/` = directory). Runs in a worker thread; after 1 s a progress popup shows file, bytes, count, speed and ETA; Esc cancels and removes the partial target
- Conflicts per file: `o` overwrite, `s` skip, `r` rename, `n` only if newer; `O`/`S`/`N` for all; Esc/Enter/`c` cancel. Overwrite copies into a fresh `.tc-tmp-*` directory next to the target (flushed to disk), then renames over it; copies keep the modification time; a move deletes the source only after success; directories merge, a file never replaces a directory or the reverse
- Shift+F6 renames the cursor entry (ignores selection), F7 mkdir
- F8 delete: confirm names the first 3 items; `y`/Enter trash, `p` permanent, `n`/Esc cancel. If trash fails nothing is deleted
- Text prompts edit at the cursor (Left/Right/Home/End/Backspace/Delete, Ctrl+U clears, paste inserts without line breaks)

### Search and analysis

- Find files: Alt+F7 asks for a name mask and an optional content regex (smart case), searches recursively in a worker thread with live results; Esc stops, second Esc closes. Content search streams text files only (NUL byte = binary skipped); symlinked dirs not followed; capped at 100000 results; no size/date filters
- Panelize: Enter in the results shows them as a flat list (`[found]`) of real paths; F3/F4/F5/F6/F8 work; Backspace/Esc returns to the start directory
- File Stats (ncdu-style): Ctrl+Space scans the directory recursively (Esc cancels), shows entries largest first with value, % and bar; Tab toggles size / file count, footer shows total size, file count and keys, Enter/Right in, Backspace/Left up, F3 view, F4 edit, Shift+F4 external editor on the selected file (closing them returns to the stats), F1 help, Esc/`q`/F10 close. F8/`d` deletes the selected entry (`y` trash, `p` permanent) and updates the tree without a rescan; `:` runs a command in the browsed directory. Scan uses up to 8 threads, the footer shows files and scanned / discovered directories. Apparent size, symlinks not followed, whole tree in memory, no refresh (also not after `:` or a failed delete). On remote panels the scan runs one `find` on an SFTP server (GNU find, as fast as a local scan there); without it (BusyBox/BSD find, FTP) it lists 8 directories in parallel over the connection; F3/F4 download a temp copy (F4 uploads it back), delete is permanent only (`p`, worker thread), Shift+F4 is not available, and `:` runs the command in the browsed directory via the system `ssh -t` (SFTP only, POSIX shell on the server, `ssh` may ask for the password again)

### Command line

- `:` opens the row above the F-keys (`dir> `); Enter runs it in the active panel's directory with `sh -c` (`cmd /C` on Windows) on the real terminal, waits for a key, then reloads both panels. Esc leaves, Up/Down history (500 entries, saved as `tc/history`). Limits: blocks tc while running, no `cd` tracking, no completion

### Archives (read-only)

- Enter on `.zip`, `.tar`, `.tar.gz`/`.tgz`, `.tar.xz`/`.txz` or `.xz` opens it as a directory (listing read in a worker thread, Esc cancels). F3/F4 extract an entry to a temp file for the viewer; F5 copies entries out to the other panel. F6/F7/F8, copying in and the command line show a read-only message
- Zip-slip entries are skipped; symlinks and hard links listed, never extracted; entries over 1 GiB refused. Limits: tar.gz/xz decompress from the start for every access, plain `.xz` shows size 0, zip times in UTC, no encrypted zips, 7z, rar, bzip2, zstd or nested archives

### Remote (SFTP, FTP, FTPS)

- Ctrl+N opens the saved connections: Enter connects in the active panel, Insert adds, F4 edits, Delete removes. One URL per line in `tc/connections` next to the hotlist: `sftp://user@host[:port]/dir`, `ftp://...`, `ftps://...` (explicit TLS, system-independent webpki roots). Passwords are never stored, they are asked on connect (masked)
- SFTP: host key checked against `~/.ssh/known_hosts`; an unknown key shows its SHA256 fingerprint, `y` trusts and saves it, a changed key is refused. Auth: ssh-agent (Unix), unencrypted `~/.ssh/id_ed25519`, `id_ecdsa`, then password. RSA keys and RSA-only host keys are not supported (RUSTSEC-2023-0071 in the `rsa` crate). Empty user = local user name
- FTP: empty user = anonymous; MLSD listing with LIST fallback, passive mode, binary transfers; a server reply missing for 60 s fails the operation. Uploads and renames need the target directory to be listable (FTP has no exclusive create, so tc refuses rather than risk overwriting)
- Remote panels list like local ones (worker thread, Esc cancels); `..` at the remote root goes back to the local directory; Alt+= and Ctrl+T keep the connection, `~`, hotlist and drives go back to the local disk
- F5/F6 between local and remote panels and within one remote: an absolute destination (the prefilled one) is on the other panel's side, a relative one in the active panel. Same conflict dialog, progress and Esc cancel as local copies; overwrite goes via an unused `.tc-tmp-[<n>-]<name>` and rename (if the final rename fails after the old file was removed, the error names the temp file holding the new content), partial targets are removed, a moved source is deleted only after its copy is complete. Moves within one server are renames; copies within one server go through a local temp file
- F7 mkdir, Shift+F6 rename, F8 delete (no trash: only `p` permanent) run in a worker
- F3 downloads to a temp file for the viewer; F4 downloads, edits, and uploads on every save (F2); files over 1 GiB are refused (copy them with F5)
- `:` on an SFTP panel runs the command in the panel directory on the server via the system `ssh -t` (POSIX shell on the server, `ssh` may ask for the password again); FTP shows an error
- Not on remote panels: Space dir size, Enter default app, Shift+F4, find, hotlist add, archives
- Limits: SMB / Windows shares not yet (crate conflict, see Planned), no saved passwords, no passphrase prompt for encrypted keys (use the agent), no Windows ssh-agent, no reconnect after a dropped connection, symlinks are never created on remote targets: file links are copied as their content, a selected directory link is followed, directory links inside a copied tree are skipped and listed in the error (a move then keeps its source), deleting a link removes only the link, permissions and times are not copied to remote targets, FTP stat lists the parent directory (slow for many small files), no IPv6 literal hosts

### Viewer (F3) and editor (F4)

- Viewer: streams from disk, Up/Down/PgUp/PgDn/Home/End, Left/Right sideways, `w` wrap, `l` line numbers (only while the line is known), `/`, `f` or F7 find, `n`/`N` next/previous, `g` line or `N%`, wheel scrolls 3 lines, encoding detected from the first 8 KiB; Esc/`q`/F3/F10 close
- Editor: F2 save, Esc/F10 exit (asks if unsaved); guesses encoding and offers UTF-8, trims trailing spaces, offers CRLF -> LF, loads in a worker thread, files over 1 GB or free RAM offer the viewer
  - Ctrl-Z undo, Ctrl-Y redo (10000 steps); Shift+arrows select, Ctrl-C/X/V copy, cut, paste (also sets the clipboard via OSC 52, not supported by every terminal); Ctrl-K or F8 delete selection (else line), Ctrl-Left/Right to next/previous word start, Ctrl-Home/End, Ctrl-G go to line
  - F7 or Ctrl-F find, F3 / Shift-F3 next / previous, F4 or Ctrl-R replace all (one undo step)
  - Ctrl-A select all, Ctrl-D duplicate line, Ctrl-Backspace/Delete delete word, Shift-Tab unindent line or selection (Tab does not indent a selection), Enter keeps the indentation; mouse click, drag and Shift-click set cursor and selection
  - Line numbers (Alt-L toggles) and soft wrap (Alt-W toggles, wraps at the window width; Up/Down still move by line); status bar shows line:col, encoding, EOL; the bracket next to the cursor and its partner (`()[]{}`, within 20000 chars, strings and comments not skipped) are underlined
  - Alt-R replaces one by one from the cursor to the end of the file (`y` replace, `n`/Space skip, `a` all the rest, other key stops; patterns matching empty text are refused); Alt-C switches search between smart case and always case sensitive
  - Saving asks to overwrite when the file changed on disk since it was opened or saved (compares modification time)
- Search is a regex (Rust syntax, `$1` in replacements), line by line, smart case (no uppercase = ignore case), all matches highlighted yellow; empty answer repeats the last search
- Syntax colors (keywords, strings, numbers, comments) by extension: rs, c/cpp/h, js/ts, java/kt/cs, go, py, sh, rb, lua, php, sql, toml/ini/yaml, json, html/xml, md (headings, `**bold**`, `*italic*`, inline `code`, ``` fences with `#` comments; the viewer assumes no fence open above the screen). Per screen row only: comments and strings spanning lines, and wrapped or sideways-scrolled rows, may color wrong
- Shift+F4 asks for a file name (prefilled), creates it empty if missing, opens it in an external editor (`code`, else asks; console editors unsupported)

F10 or `q` quits.

Run: `scripts/run.sh`, Windows: `scripts\run.cmd` (opens in current directory)

## Planned Features

- Windows, Mac and Linux build images
- File Stats: [qdirstat](https://github.com/shundhammer/qdirstat) display
- Windows Shares (SMB): the `smb` crate pins pre-release crypto crates that conflict with russh; add once they release against `curve25519-dalek` 5.0.0

## Tech Stack

- Rust
- AI coding
