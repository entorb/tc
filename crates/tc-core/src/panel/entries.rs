use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Entry, SortKey};
use crate::vfs::{Remote, rpath};
use crate::{Result, io_err};

/// Entry named `name` for `path`; None if it cannot be stat'ed.
pub(crate) fn entry_for(name: OsString, path: &Path) -> Option<Entry> {
    Some(entry_with(name, path, fs::symlink_metadata(path).ok()?))
}

/// Entry from the not-followed metadata `link_meta`; only symlinks cost extra syscalls.
pub(crate) fn entry_with(name: OsString, path: &Path, link_meta: fs::Metadata) -> Entry {
    let is_symlink = link_meta.file_type().is_symlink();
    // dangling symlink: fall back to link metadata
    let meta = if is_symlink {
        fs::metadata(path).unwrap_or(link_meta)
    } else {
        link_meta
    };
    Entry {
        name,
        is_dir: meta.is_dir(),
        is_symlink,
        size: meta.len(),
        modified: meta.modified().ok(),
        link: if is_symlink {
            fs::read_link(path).ok()
        } else {
            None
        },
        ignored: false,
    }
}

/// Lowercase text after the last dot; none for `.name` and names without a dot.
fn ext(lower: &str) -> &str {
    match lower.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext,
        _ => "",
    }
}

/// Directories first (also when reversed), then by `sort`, ties by case-insensitive name.
/// By extension only files are ordered: directories stay by name, ascending also when reversed.
pub(super) fn sort_entries(entries: &mut [Entry], (key, reverse): (SortKey, bool)) {
    entries.sort_by_cached_key(|e| {
        let lower = e.name.to_string_lossy().to_lowercase();
        let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let (primary, lower) = match key {
            SortKey::Name => (0, lower),
            SortKey::Ext if !e.is_dir => (0, format!("{}\0{lower}", ext(&lower))),
            SortKey::Ext => (0, lower),
            SortKey::Size if !e.is_dir => (e.size, lower),
            SortKey::Size => (0, lower),
            SortKey::Date => (e.modified.map_or(0, secs), lower),
        };
        (!e.is_dir, primary, lower, e.name.clone())
    });
    if reverse {
        let dirs = entries.partition_point(|e| e.is_dir);
        if key != SortKey::Ext {
            entries[..dirs].reverse();
        }
        entries[dirs..].reverse();
    }
}

/// Remote listing; always has "..", at the root it leaves the remote.
pub(super) fn remote_entries(
    r: &dyn Remote,
    dir: &Path,
    sort: (SortKey, bool),
) -> Result<Vec<Entry>> {
    let mut entries = r.list(&rpath(dir))?;
    sort_entries(&mut entries, sort);
    entries.insert(0, Entry::parent());
    Ok(entries)
}

/// Stops early (partial list) once `cancel` is set.
pub(super) fn read_entries(
    dir: &Path,
    sort: (SortKey, bool),
    cancel: &AtomicBool,
) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir).map_err(io_err(dir))? {
        if cancel.load(Ordering::Relaxed) {
            return Ok(entries);
        }
        let item = item.map_err(io_err(dir))?;
        // DirEntry::metadata does not follow symlinks and is free on Windows (part of the listing)
        // Err: vanished between read_dir and stat
        if let Ok(meta) = item.metadata() {
            entries.push(entry_with(item.file_name(), &item.path(), meta));
        }
    }
    super::git::mark_ignored(dir, &mut entries);
    sort_entries(&mut entries, sort);
    if dir.parent().is_some() {
        entries.insert(0, Entry::parent());
    }
    Ok(entries)
}
