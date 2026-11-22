# AGENTS.md

## Product

Torben's Commander (`tc`): cross-platform (Windows, macOS, Linux) two-panel file manager in Rust. Scope and roadmap: [README.md](README.md). Build the smallest version of a feature that does the core job; name skipped extras instead of building them.

Docs in the same change as the feature:

- F1 help (`crates/tc-cli/src/fmt.rs` `HELP_*`) is the reference: every key and its behavior.
- [README.md](README.md) is the overview: one line per feature (key, what it does, limits). No key tables, no per-key detail.

File operations touch user data. Never delete, overwrite or truncate without an explicit user action; on failure leave source intact, report the error, and clean up partial targets.

## Architecture

Layers flow inward: front ends and backends depend on core, never the reverse.

- `crates/tc-core`: panels, file ops, VFS traits, viewer/editor, find, sync, archives. No UI deps.
- `crates/tc-remote`: SFTP / FTP / FTPS backends implementing `tc-core` VFS.
- `crates/tc-cli`: shared app logic (`app.rs` `Mode` state machine, `app/<feature>.rs`, `fmt.rs` help and rendering helpers) plus the ratatui TUI binary.
- `crates/tc-gui`: egui front end reusing `tc_cli::app`; only drawing, mouse and toolbar (`draw.rs`, `keys.rs`).

Before editing a crate, read its `AGENTS.md` (`crates/tc-core/AGENTS.md`, `crates/tc-cli/AGENTS.md`).

## Known Pitfalls

Each of these shipped as a bug once.

- Config files: write through `hotlist::save` (atomic, 0600, follows symlinks); never `fs::write`.
- Temp names: `.tc-tmp-*`; never remove a temp file tc did not create (remote overwrite spares foreign ones).
- FTP has no exclusive create: refuse writes when the target listing fails instead of risking an overwrite.
- Child processes (external editor, `ssh`, clipboard tools): wait or reap them; no zombies.
- Windows: pass shell commands raw to `cmd /C` (no Rust arg quoting); Ctrl+Insert is taken by the system, offer Alt+Shift+C.
- Terminals: Ctrl+M arrives as Enter, Ctrl+F-keys and Alt keys may be eaten (macOS). Every feature needs an F9 menu entry.
- Read state (cursor name, selection) before replacing the data it points into (File Stats rescan bug).
- Empty filter + Enter keeps the list; it is not "filter matches nothing".
- `tc-gui`: toolbar disabled on sub-screens, dialogs and prompts; icons come from one loaded emoji font.
- Footer keys must appear in their F1 help: test `footer_keys_are_documented_in_their_help` in `app.rs`.

## Reference Code

`templates/` holds cloned reference repos (git-ignored, not scanned). Never edit them.

- `mc` (C, GPL-3), `doublecmd` (Pascal, GPL-2), `krusader` (C++/Qt, GPL-2+): UX, behavior, edge cases (encodings, VFS, SSH/FTP, panels). Re-implement idiomatically; do not transliterate. Ported code keeps license attribution.
- `yazi`, `btop`, `ncdu`: UX references for panels, processes, File Stats. `templates/FEATURES-*.md` summarize each.
- `rea`: template for agent guidelines and repo tooling only.

## Tools

### CodeGraph (mandatory)

- `codegraph_explore` FIRST for any code question or before editing: verbatim source, callers, blast radius. Treat output as read; no grep re-verification. Shell: `codegraph explore "<query>"`.
- Grep/Read only for config, docs, or a detail it missed. `app.rs` is ~7000 lines: never Read it whole.
- "files changed after last sync" or stale results: `codegraph sync`. Stale lock: `codegraph unlock`. Last resort: `codegraph index`.
- Templates: `projectPath: /code/tc/templates/<repo>`; no index yet: `codegraph init templates/<repo>`. Check how `mc` / `doublecmd` / `krusader` / `yazi` solve a problem before designing ours.

### Context7 (mandatory)

Before using a crate API new to this code, upgrading a crate, or a toolchain feature / CLI flag: `resolve-library-id` → `query-docs`, one concept per query. Key crates: ratatui, crossterm, egui/eframe, russh, russh-sftp, suppaftp, tokio, ropey, regex, zip, tar, sysinfo. Fallback: `docs.rs/<crate>/<version>`.

### SonarQube Cloud

Project `entorb_tc` ([.sonarcloud.properties](.sonarcloud.properties)), scans every push. Dashboard: <https://sonarcloud.io/project/overview?id=entorb_tc>. `scripts/get_sonar_issues.sh` → `tmp/sonar.json`.

- No new bugs, vulnerabilities, hotspots or smells; low duplication.
- Cognitive complexity max 30 (rust:S3776). Clippy counts lower than Sonar: near ~20 branches/nesting, extract match arms, loops, `let`-chains into helpers. New crates need `[lints] workspace = true`.
- No wildcard imports (rust:S2208), also in `mod tests`/`mod mem`.
- Method paths, not trivial closures: `is_some_and(JoinHandle::is_finished)` (rust:S1612).
- Fix at the root cause; `NOSONAR` only for proven false positives (comment why).

## Rust Conventions

- Edition 2024, stable, Cargo workspace; small crates keep `cargo check` fast.
- Typed errors in libraries, context-rich at the binary edge. No `unwrap`/`expect`/`panic!` outside tests and commented invariants.
- No `unsafe` without `// SAFETY:`. Prefer `std` and existing deps; `cargo add`, commit `Cargo.lock`.
- `Path`/`PathBuf`/`OsStr`, never assume UTF-8 names. Display formatting separate from identity values.
- Platform code behind `#[cfg(...)]`, one module per concern.

## Performance

Files and directories can be huge (multi-GB logs, 100k-entry dirs, slow mounts). Cost scales with what is on screen.

- Never read a whole file; stream and seek (`crates/tc-core/AGENTS.md`).
- Slow I/O (big dirs, copies, hashing, network) never on the UI thread: worker, progress, cancel.
- Bounded memory from file or network input. No per-frame I/O, sorting or allocation; render visible rows only.

## Development

- `cargo check` while editing; never `cargo build` to type-check.
- `cargo test -p <crate> <filter>` focused; full suite before finishing.
- `cargo fmt`, `cargo shear --fix`.
- `scripts/run_checks.sh` runs every `scripts/chk_*.sh`: pre-commit (`prek`, may auto-fix: re-run), fmt, clippy `-D warnings`, tests, shear, `cargo deny`, cspell (add real words to `cspell-words.txt`).
- Long compiler output: `| grep -E '^(error|warning)' | head`; fix the first error first.

Tests:

- Unit tests for logic a later change could silently break (parsers, sorting, names/paths, file ops, undo, state transitions), next to the code. Behavior, not implementation; skip getters and drawing.
- Remote backends: every new or changed operation gets an `#[ignore]` `live_*` E2E test in `crates/tc-remote/src/lib.rs` (CI job `e2e`; locally `TC_TEST_SFTP`/`TC_TEST_FTP`/`TC_TEST_PASS`). Cover symlinks (file, dir, dangling, loop), non-UTF-8 and special names, empty and large files, permissions, overwrite, failure cleanup, dropped sessions. `mem` `Remote` is for dependent-crate logic only.

## Definition of Done

1. `scripts/run_checks.sh` passes.
2. Non-trivial logic tested; remote changes have `live_*` tests; file ops follow `crates/tc-core/AGENTS.md`.
3. Context7 consulted for new crate APIs.
4. User-facing change: `crates/tc-cli/AGENTS.md` checklist done (F1 help, footer, menu, README line).

## Agent Efficiency

1. Smallest sufficient answer and diff. One `codegraph_explore` beats a grep/read loop. Batch independent tool calls.
2. Ask before adding complexity for speculative edge cases; name the concrete failure first.
3. Tool names are case-sensitive: `Bash`, not `bash`.
4. Sub-agents: cheapest model that fits, never above `opus`, never `fable`:
   - `haiku`: mechanical edits (labels, help/README text, renames, clear one-file fixes)
   - `sonnet`: routine multi-file changes following existing patterns
   - `opus`: new non-trivial logic (file ops, workers, parsers, undo), cross-crate design, hard bugs
   Same files (e.g. `app.rs`): sequential agents; parallel only for disjoint files.
   Non-trivial features: `opus` `Plan` agent writes a short spec (files, edge cases, tests), `sonnet` implements, `opus` reviews the diff once (bugs, data safety, help/README drift). Skip for mechanical changes.
   Every prompt says: `codegraph_explore` first, Context7 for crate APIs, read the crate's `AGENTS.md`.

## Git

Commit after each new feature. Subject `<type>: <Description>`, type `feat|fix|docs|style|refactor|perf|test|chore`; no body, no trailers. Never commit `*.md`, `cspell-words.txt`, `target/`, `templates/`, binaries or credentials (the user commits those).
