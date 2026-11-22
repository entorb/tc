# tc-core

Core logic shared by all front ends and backends. Root rules in [../../AGENTS.md](../../AGENTS.md) apply.

## Layering

- No UI or terminal deps (no ratatui, crossterm, egui). Front ends call in, core never calls out.
- Typed errors (`thiserror`); no `anyhow` here.

## Performance

- Never `fs::read`, `read_to_string` or `read_to_end` for viewing, searching, comparing or copying. Stream with `BufReader` / `io::copy`; read only the visible window via `Seek`.
- Viewer line navigation: index line offsets lazily as the user scrolls.
- Cap buffers and line lengths; no unbounded `Vec` / `String` from file or network input.
- Use metadata from `read_dir` entries before extra `stat` calls.
- Slow operations (copy, hash, scan, network VFS) expose progress and cancel so callers can run them in a worker.

## File-op safety

- Overwrite through a temp file + rename; on failure remove the temp, leave source and target intact (pattern: `ops.rs` tests `skip_and_overwrite`, `move_overwrite_*`).
- Moves across devices fall back to copy + delete only after the copy succeeded.

## Tests

- Temp dirs via `tempfile`, never real user paths.
- Cover non-UTF-8 names, symlinks, permission errors and cross-device moves where relevant.
- Prove streaming with large or synthetic input (sparse file via `set_len`).
- Feature `mem`: in-memory `Remote` for tests of dependent crates.
