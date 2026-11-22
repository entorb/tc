# tc-app

Library = shared app logic for both front ends (`tc-cli` ratatui, `tc-gui` egui draw `tc_app::app`). Root rules in [../../AGENTS.md](../../AGENTS.md) apply.

## Structure

- Change behavior here once; never fork key handling or dialog logic into `tc-gui`.
- `on_key` only dispatches on `Mode`; each mode's keys live in its own `*_key` method (Sonar S3776).
- `app.rs`: `Mode`, `App` struct, `on_key` dispatch, `tick`, panel keys. Features live in `app/<feature>.rs` as `impl App` blocks plus their types; new features get their own file, keep `app.rs` small.
- Module map: `editor` (viewer F3, editor F4, prompts), `xfer` (copy/move/delete, Ctrl+C/X/V), `find_files` (Alt+F7, results), `multi_rename` (Ctrl+M), `file_stats`, `proc_mon` (Ctrl+P), `sync`, `bookmarks` (Ctrl+D), `connect` (Ctrl+N, remote), `tabs` (tabs, mouse), `menu` (footers, F9 menu, toolbar), `help` (F1 popup), `widgets` (form rows, `/` filter, list helpers), `config` (load/save), `tests` (App-level tests).
- Items shared across modules are `pub(super)` and imported in `app.rs`; siblings use `super::Name`.
- Render only visible rows; no I/O, sorting or allocation per frame. Slow I/O runs in a worker with progress and cancel.

## UI feature checklist

A user-facing feature is done when all apply:

- Key binding works in TUI and GUI (shared logic, check `tc-gui/src/keys.rs` for GUI-only keys).
- Mouse works too: every clickable-looking control (checkbox, choice, list row, button) reacts to a left click in TUI and GUI, same effect as its key. Shared logic in tc-app (e.g. forms: `App::form_click`; TUI sets the drawn area like `form_area`, GUI reports the clicked row).
- F1 help (`fmt/help.rs` `HELP_*`) lists every key and its behavior; same layout (`== Section ==`, key column), ends with the close key.
- F-key footer: the most important actions of a screen are buttons there. Every screen starts with `F1 Help` and ends with `F10 Close` (panels `F10 Quit`); button keys are F1–F12. A Shift action of a button is no extra button: add it to `SHIFT_FOOTER`, tc-gui swaps the label while Shift is held. Every footer key, Shift alternates included, is in its help; the tooltip is that help line (test enforces).
- F9 menu entry (terminals eat keys); `tc-gui` toolbar icon if it is a tool (same icon font, tooltip shows the key).
- List screens follow conventions: `/` filter, Ctrl+R refresh, Esc / `q` / F10 close.
- README: one overview line under the matching feature section, limits included; no key details.
