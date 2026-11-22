# tc-cli

Library = shared app logic for both front ends (TUI binary here, `tc-gui` reuses `tc_cli::app`). Root rules in [../../AGENTS.md](../../AGENTS.md) apply.

## Structure

- Change behavior here once; never fork key handling or dialog logic into `tc-gui`.
- `on_key` only dispatches on `Mode`; each mode's keys live in its own `*_key` method (Sonar S3776).
- `app.rs` is too large for cheap agent context. New modes and features go in `src/app/<feature>.rs` (pattern: `app/config.rs`, `app/sync.rs`). When a change touches a big self-contained section of `app.rs`, move it out in a separate `refactor:` commit first.
- Render only visible rows; no I/O, sorting or allocation per frame. Slow I/O runs in a worker with progress and cancel.

## UI feature checklist

A user-facing feature is done when all apply:

- Key binding works in TUI and GUI (shared logic, check `tc-gui/src/keys.rs` for GUI-only keys).
- F1 help (`fmt.rs` `HELP_*`) lists every key and its behavior; same layout (`== Section ==`, key column), ends with the close key.
- F-key footer: screen shows its own keys, sub-screens end with `F10 Close`, panels with `F10 Quit`; every footer key is in its help (test enforces).
- F9 menu entry (terminals eat keys); `tc-gui` toolbar icon if it is a tool (same icon font, tooltip shows the key).
- List screens follow conventions: `/` filter, Ctrl+R refresh, Esc / `q` / F10 close.
- README: one overview line under the matching feature section, limits included; no key details.
