# tc

The app: library `lib.rs` (`app`, `fmt`: state, keys, dialogs, help; no drawing) and egui binary `main.rs` (`draw.rs` + `draw/<screen>.rs`, `keys.rs`: drawing, mouse, toolbar). Root rules in [../../AGENTS.md](../../AGENTS.md) apply.

## Structure

- Behavior lives in `app`; never put key handling or dialog logic into `draw`.
- `on_key` only dispatches on `Mode`; each mode's keys live in its own `*_key` method (Sonar S3776).
- `app.rs`: `Mode`, `App` struct, `on_key` dispatch, `tick`, panel keys. Features live in `app/<feature>.rs` as `impl App` blocks plus their types; new features get their own file, keep `app.rs` small.
- Module map: `editor` (viewer F3, editor F4, prompts), `xfer` (copy/move/delete, Ctrl+C/X/V), `find_files` (Alt+F7, results), `multi_rename` (Ctrl+M), `file_stats`, `proc_mon` (Ctrl+P), `sync`, `bookmarks` (Ctrl+D), `connect` (Ctrl+N, remote), `tabs` (tabs, mouse), `menu` (footers, F9 menu, toolbar), `help` (F1 popup), `widgets` (form rows, `/` filter, list helpers), `config` (load/save), `tests` (App-level tests).
- Items shared across modules are `pub(super)` and imported in `app.rs`; siblings use `super::Name`.
- Text fields: `draw/chrome.rs` `text_box` is an egui `TextEdit` with keyboard events hidden (the app owns keys); a mouse caret goes back through `App::set_caret` (`apply_caret`; no key replay). Read egui's caret from `out.state`, not `out.cursor_range` (computed before the mouse). Never put a click sense on top of a text field.
- Render only visible rows; no I/O, sorting or allocation per frame. Slow I/O runs in a worker with progress and cancel.
- Table cells: wrap header and body cells in `draw/chrome.rs` `cell` (vertically centered, `right` for numbers); a plain `ui.label` sits at the top and misaligns next to a right-aligned cell. New tables use `draw/lists.rs` `click_table` / `head_row`, new headers get a line in `draw/sim.rs` `table_headers_share_one_line`.
- Popup sizes: inside, use `ui.available_height()`, never `min_rect()` after `set_min_size` (already the full size). `set_max_size` does not stop children from widening a popup: an unclipped label (`TextWrapMode::Extend`, `clip(false)` column) grows it past the window and egui shoves it off the left edge, only while that row is visible. Text meant to overflow a cell is painted (`painter().galley`), not added. Test popups by their `area_rect` across frames, not only by painted texts.
- F1 help is its own OS window (`show_viewport_immediate`), tc stays usable: its keys go to `App::help_key`, never through `on_key`. Tests have no native windows (embedded): there the help must not read input (tc's events, handled twice).
- Scroll behavior (no rubber-band, no coasting) is set once for every scroll area in `draw.rs` `setup_fonts` (test `scrolling_has_no_rubber_band`); never per widget. Look and feel that should hold app-wide belongs in the global style, not in one screen's code.
- Row math against egui tables: a row's pitch is its height plus `spacing().item_spacing.y`. Visible rows come from the output's `inner_rect`, not the space reserved. To force an offset, let egui clamp it, and read the offset back only in a frame that didn't force one (that frame still reports the old offset). Test scrolling with End / wheel to the last line (`help_scrolls_to_the_last_line`).
- Scrolling: set a scroll offset or `scroll_to_*` only when the app's row changed, never every frame (undoes the wheel's smooth steps; jitters and repaints forever on fractional DPI). Thousands of shapes (treemap): build one cached `Mesh`, not per-frame stroked rects.

## UI feature checklist

A user-facing feature is done when all apply:

- Key binding works (logic in `app`, egui translation in `keys.rs`).
- Mouse works too: every clickable-looking control (checkbox, choice, list row, button) reacts to a left click, same effect as its key. Logic in `app` (e.g. forms: `App::form_click`; `draw` reports the clicked row). Use native egui widgets (TextEdit, checkbox, ComboBox, Button, selectable rows), no text-drawn controls.
- F1 help (`fmt/help.rs` `HELP_*`) lists every key and its behavior; same layout (`== Section ==`, key column), ends with the close key.
- F-key footer: the most important actions of a screen are buttons there. Every screen starts with `F1 Help` and ends with `F10 Close` (panels `F10 Quit`); button keys are F1–F12. A Shift action of a button is no extra button: add it to `SHIFT_FOOTER`, the footer swaps the label while Shift is held. Every footer key, Shift alternates included, is in its help; the tooltip is that help line (test enforces).
- F9 menu entry; toolbar icon if it is a tool (same icon font, tooltip shows the key).
- List screens follow conventions: `/` filter, Ctrl+R refresh, Esc / `q` / F10 close.
- README: one overview line under the matching feature section, limits included; no key details.
- Looked at: `scripts/ui_shot.sh` screenshot of the new screen, read it as an image (layout, clipping, footer).
