# tc-gui

The app, binary `tc`: library `lib.rs` (`app`, `fmt`: state, keys, dialogs, help; no drawing) and egui binary `main.rs` (`draw.rs` + `draw/<screen>.rs`, `keys.rs`: drawing, mouse, toolbar). Root rules in [../../AGENTS.md](../../AGENTS.md) apply.

## Structure

- Behavior lives in `app`; never put key handling or dialog logic into `draw`.
- `on_key` only dispatches on `Mode`; each mode's keys live in its own `*_key` method (Sonar S3776).
- `app.rs`: `Mode`, `App` struct, `on_key` dispatch, `tick`, panel keys. Features live in `app/<feature>.rs` as `impl App` blocks plus their types; full-screen tools in `app/tools/`; App-level tests in `app/tests.rs`. New features get their own file, keep `app.rs` small.
- Items shared across modules are `pub(super)` and imported in `app.rs`; siblings use `super::Name`.
- Reusable egui pieces (text field, list row, table cell, click table) live in `draw/widgets.rs`, mirroring `app/widgets.rs`: check there before writing a helper; a helper a second screen needs moves there. Dialog pieces returning `FormAct` (`title_bar`, `ok_cancel`, `list_rows`) stay in `draw/dialogs.rs`.
- Full-screen tool forms live in `draw/tools.rs` (`panel_head`, `tab_bar`, `field_grid`, `info_and_error`): multi-rename (options left, preview right; F2.. tabs, F9 cancel, F10 run), find files (tabs, full width; F10 searches). Sync is one screen (`draw/lists.rs` `draw_sync`): opening compares at once, `sync_options` checkboxes on top (always have the keys), differing files below; F5 / F6 sync one way, Esc / F10 close. App side: `app/widgets.rs` `tab_range` / `tab_of` / `tab_key` / `step_focus`. A new tool form reuses both.
- Tool screens (File Stats, Find form and results, Sync, Processes, Multi-rename: `App::sub_screen`) own the window: no menu bar or toolbar, F-key bar at the bottom. Toolbar disabled in dialogs and prompts; icons come from one loaded emoji font.

## Pitfalls

Each of these shipped as a bug once.

- eframe is built with `default-features = false`: keep its `clipboard` feature, else `ctx.copy_text` silently only fills an in-process string (copy looked broken on every OS).
- macOS may eat Ctrl+F-keys and Alt keys; Windows takes Ctrl+Insert (offer Alt+Shift+C). Hence the F9 menu entry for every main-view feature.
- Prompt text fields are native egui `TextEdit`s (`draw/widgets.rs` `text_box`): typing, selection and clipboard happen in the field; `App::set_line` takes the text back and sends `KeyCode::Null` through the prompt's key handler so its per-key side effects run. A new prompt needs a `Mode::line_mut` / `field_mut` entry and must handle `Null` in its `edit_key` fall-through. Masked text sets `LineEdit::secret`.
- Text field keys: `text_box` hides keyboard events from egui (the app owns keys); a mouse caret goes back through `App::set_caret` (`apply_caret`; no key replay). Read egui's caret from `out.state`, not `out.cursor_range` (computed before the mouse). Never put a click sense on top of a text field.
- Empty filter + Enter keeps the list; it is not "filter matches nothing".
- Form focus can sit past the last row (multi-rename Editor tab has none): `rows.get_mut(focus)`, never `rows[focus]`. A names list is only safe with one name per file: check the count where it runs, not only in the preview.
- Table cells: wrap header and body cells in `draw/widgets.rs` `cell` (vertically centered, `right` for numbers); a plain `ui.label` sits at the top and misaligns next to a right-aligned cell. New tables use `draw/widgets.rs` `click_table` / `head_row`, new headers get a line in `draw/sim.rs` `table_headers_share_one_line`.
- Popup sizes: inside, use `ui.available_height()`, never `min_rect()` after `set_min_size` (already the full size). `set_max_size` does not stop children from widening a popup: an unclipped label (`TextWrapMode::Extend`, `clip(false)` column) grows it past the window and egui shoves it off the left edge, only while that row is visible. Text meant to overflow a cell is painted (`painter().galley`), not added. Test popups by their `area_rect` across frames, not only by painted texts.
- Dialog hints: a trailing text line of `[k] Label` parts split by 3 spaces becomes buttons (`draw/dialogs.rs` `hint_line`); one part that does not parse (`[n] / [Esc] Keep`) leaves the whole line plain text. `[o/Enter]` clicks the first key.
- F1 help is its own OS window (`show_viewport_immediate`), tc stays usable: its keys go to `App::help_key`, never through `on_key`. Tests have no native windows (embedded): there the help must not read input (tc's events, handled twice).
- Scroll behavior (no rubber-band, no coasting) is set once for every scroll area in `draw.rs` `setup_fonts` (test `scrolling_has_no_rubber_band`); never per widget. Look and feel that should hold app-wide belongs in the global style, not in one screen's code.
- Row math against egui tables: a row's pitch is its height plus `spacing().item_spacing.y`. Visible rows come from the output's `inner_rect`, not the space reserved. To force an offset, let egui clamp it, and read the offset back only in a frame that didn't force one (that frame still reports the old offset). Test scrolling with End / wheel to the last line (`help_scrolls_to_the_last_line`).
- Scrolling: set a scroll offset or `scroll_to_*` only when the app's row changed, never every frame (undoes the wheel's smooth steps; jitters and repaints forever on fractional DPI). Thousands of shapes (treemap): build one cached `Mesh`, not per-frame stroked rects.

## UI checks

- Headless (Linux devbox): `scripts/ui_shot.sh STEP...` builds tc, runs it in Xvfb (1280x800), runs the steps, then Read the PNGs. Steps: `"shot F.png"`, `"key F1 ctrl+f shift+F4 Down"` (XTEST via `scripts/ui_key.py`), `"wait 2"`, or any xdotool command (`"type abc"`, `"mousemove 300 300 click 1"`). Example: `scripts/ui_shot.sh "key F9" "shot $TMP/menu.png"`. Save PNGs to the scratchpad, never the repo. tc opens in the caller's directory and uses the real user config. Never send keys through `xdotool key`: on Xvfb it adds Alt to F-keys (F1 becomes Alt+F1).
- README screenshots (800x600, `screenshots/*.png`): `scripts/screenshots.sh [panel|editor|ncdu|top|rename]`; no argument makes all, a name only that one (one tc run each). A UI change visible in a README screenshot: regenerate that one and Read it. New screenshot: add a `shot <name> STEPS...` line there and its name to the `case`.

## UI feature checklist

A user-facing feature is done when all apply:

- Key binding works (logic in `app`, egui translation in `keys.rs`).
- Mouse works too: every clickable-looking control (checkbox, choice, list row, button) reacts to a left click, same effect as its key. Logic in `app` (e.g. forms: `App::form_click`; `draw` reports the clicked row). Use native egui widgets (TextEdit, checkbox, ComboBox, Button, selectable rows), no text-drawn controls.
- F1 help (`fmt/help.rs` `HELP_*`) lists every key and its behavior; same layout (`== Section ==`, key column), ends with the close key.
- F-key footer: the most important actions of a screen are buttons there. Every screen starts with `F1 Help` and ends with `F10 Close` (panels `F10 Quit`); button keys are F1–F12. A Shift action of a button is no extra button: add it to `SHIFT_FOOTER`, the footer swaps the label while Shift is held. Every footer key, Shift alternates included, is in its help; the tooltip is that help line (test `footer_keys_are_documented_in_their_help`).
- F9 menu entry (main panel view only; tool screens have no menu); toolbar icon if it is a tool (same icon font, tooltip shows the key).
- List screens follow conventions: `/` filter, Ctrl+R refresh, Esc / `q` / F10 close.
- README: one overview line under the matching feature section, limits included; no key details.
- Looked at: `scripts/ui_shot.sh` screenshot of the new screen, read it as an image (layout, clipping, footer).
