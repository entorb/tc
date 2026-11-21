# AGENTS.md

## Product

Torben's Commander (`tc`): cross-platform (Windows, macOS, Linux) two-panel file manager in Rust. Scope and roadmap: [README.md](README.md). Build the smallest version of a feature that does the core job; name skipped extras instead of building them.

Document every new feature in [README.md](README.md) in the same change (keys, behavior, limits).

File operations touch user data. Never delete, overwrite or truncate without an explicit user action; on failure leave source intact, report the error, and clean up partial targets.

## Reference Code

`templates/` holds cloned reference repos (git-ignored)

- `mc` (C, GPL-3) and `doublecmd` (Pascal, GPL-2): study UX, behavior and edge cases (encodings, VFS, SSH/FTP, panels). Re-implement idiomatically in Rust; do not transliterate. Ported code keeps license attribution; check `doublecmd` license terms before copying anything.
- `rea`: template for agent guidelines and repo tooling only.

Never edit files in `templates/`.

## Tools

### CodeGraph: find and read code

Repo is indexed by CodeGraph (`.codegraph/`, config [codegraph.json](codegraph.json)).

- Call `codegraph_explore` FIRST for any code question or before editing: one call returns verbatim source, callers and blast radius. Treat its output as already read.
- Shell fallback: `codegraph explore "<symbols or question>"`.
- Use Grep/Read only for what CodeGraph does not index (config, docs) or a detail it missed. Do not re-verify its results with grep.
- Use it on `templates/` to see how `mc`/`doublecmd` solve a problem before designing ours.

### Context7: crate and tool docs

Before using or upgrading any crate API, Rust toolchain feature, or CLI (cargo, clippy, rustfmt), fetch current docs via Context7 MCP: `resolve-library-id` → `query-docs`, one concept per query. Training data lags crate releases. Fallback: `docs.rs/<crate>/<version>`, or `cargo doc --open` for the exact locked version.

### SonarQube Cloud: static analysis

Repo is connected to SonarQube Cloud (config [.sonarcloud.properties](.sonarcloud.properties), project `entorb_tc`); every push and PR is scanned. Dashboard: <https://sonarcloud.io/project/overview?id=entorb_tc>.

- Write code that passes the quality gate: no new bugs, vulnerabilities, security hotspots or code smells, keep duplication and cognitive complexity low.
- Cognitive complexity max 30 per function (rust:S3776). Key handlers: `on_key` only dispatches on `Mode`; each mode's keys live in its own `*_key` method. Clippy enforces this locally (`clippy.toml`, `[workspace.lints]`); new crates need `[lints] workspace = true`.
- Clippy's `cognitive_complexity` counts lower than Sonar's, so a clean local lint does not prove S3776 passes. Keep functions well under 30: when a function nears ~20 branches/nesting levels, extract match arms, loops and `let`-chains into helpers (`*_key`, `*_tick`, `draw_*`) before adding more.
- No wildcard imports (rust:S2208), also in `mod tests`/`mod mem`: name each item (`use super::{A, B}`). `rustfmt` and unused-import cleanup keep the list honest.
- Pass method paths, not trivial closures: `is_some_and(JoinHandle::is_finished)`, not `|j| j.is_finished()` (rust:S1612).
- Fix reported issues at the root cause; do not silence them with `NOSONAR` unless the finding is a proven false positive (comment why).
- `templates/` is git-ignored, so it is not scanned.

## Rust Conventions

- Edition 2024, stable toolchain. Cargo workspace once more than one crate exists; keep crates small so `cargo check` stays fast.
- Layers flow inward: core (panels, file ops, VFS traits) has no UI dependency; UI and backends (local, SSH, FTP, SMB) depend on core.
- Errors: `Result` with typed errors in libraries, context-rich errors at the binary edge. No `unwrap`/`expect`/`panic!` outside tests and proven invariants (comment the invariant).
- No `unsafe` without a `// SAFETY:` comment. Prefer `std` and existing deps; add a crate only when it removes real code. Add deps with `cargo add`, commit `Cargo.lock`.
- Paths: use `Path`/`PathBuf`/`OsStr`, never assume UTF-8 file names. Keep display formatting separate from identity values.
- Platform code behind `#[cfg(...)]` in one module per concern, not scattered.
- Comment only the why the code cannot show.

## Performance

Performance is a feature: files and directories can be huge (multi-GB logs, 100k-entry dirs, slow network mounts). Cost must scale with what is on screen, not with input size.

- Never read a whole file into memory (`fs::read`, `read_to_string`, `read_to_end`) for viewing, searching, comparing or copying. Stream with `BufReader`/`io::copy`, read only the visible window via `Seek`; for viewer line navigation, index line offsets lazily as the user scrolls.
- Never block the UI thread on I/O that can be slow (big dirs, copies, hashing, network VFS). Run it in a worker thread, report progress, keep it cancellable.
- Bounded memory: no unbounded `Vec`/`String` growth from file or network input; cap buffers and line lengths.
- Avoid per-frame work: no re-reading directories, re-sorting or re-allocating on every redraw; render only visible rows.
- Use metadata from `read_dir` entries before extra `stat` calls; batch syscalls.
- Tests for such code use a large or synthetic input (e.g. sparse file via `set_len`) to prove it does not load everything.

## Development

Fast feedback loop (smallest command that answers the question):

- `cargo check` - while editing; never `cargo build` just to type-check.
- `cargo test -p <crate> <filter>` - focused tests; full `cargo test` before finishing.
- `cargo fmt` - format (fix what `chk_rs_format` reports).
- `cargo shear --fix` - remove unused deps (fix what `chk_rs_dead` reports).
- `scripts/run_checks.sh` - full gate, runs every `scripts/chk_*.sh`. Single check: `sh scripts/chk_rs_lint.sh`.

| Script | Runs | Catches |
| --- | --- | --- |
| `chk_pre-commit.sh` | `prek run --all-files` | md/yaml/shell lint, secrets, whitespace |
| `chk_rs_format.sh` | `cargo fmt --all --check` | unformatted code |
| `chk_rs_lint.sh` | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | clippy lints, dead code, unused imports |
| `chk_rs_test.sh` | `cargo test --workspace --all-features --locked` | failing unit, integration, doc tests |
| `chk_rs_dead.sh` | `cargo shear` | unused dependencies |
| `chk_rs_audit.sh` | `cargo deny check advisories` | vulnerable, unsound, unmaintained crates (RustSec) |

`chk_rs_*` skip while no `Cargo.toml` exists; shared logic in `scripts/lib_rs.sh`. Tools: `cargo install --locked cargo-shear cargo-deny`.

After each feature or fix, run `scripts/run_checks.sh` and fix all failures before reporting done. `prek` hooks may auto-fix files; re-run until clean.

Pipe long compiler output through `head`/`grep -E '^(error|warning)'`; read the first error first, later errors often cascade.

Tests: new non-trivial logic gets a small unit test next to it (`#[cfg(test)] mod tests`). File-op tests use temp dirs (`tempfile`), never real user paths. Cover non-UTF-8 names, symlinks, permission errors and cross-device moves where relevant.

## Agent Efficiency

1. Tokens: smallest sufficient answer and diff. One `codegraph_explore` beats a grep/read loop. Batch independent tool calls.
2. Time: `cargo check` / focused tests over full builds.
3. Ask before adding complexity for speculative edge cases; explain the concrete failure mode first.
4. Tool names are case-sensitive: call `Bash`, not `bash` (else "No such tool available: bash").

## Git

Commit only when asked. Subject: `<type>: <Description>` with type `feat|fix|docs|style|refactor|perf|test|chore`. Message only: no body, no `Co-Authored-By` or other trailers. Never commit `*.md` files, `target/`, `templates/`, binaries or credentials (the user commits those).
