//! Panel filter (`/` popup): name part, extensions, size and age; ".." always stays.

use super::Entry;
use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

/// What the panel shows; the default keeps everything.
#[derive(Debug, Clone, Default)]
pub struct EntryFilter {
    /// Lowercased part of the name.
    pub name: String,
    /// Lowercased extensions without dot (`""` = none); empty = any. Files only.
    pub exts: Vec<String>,
    /// Size bounds in bytes, inclusive. Files only.
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Modified after `newer` / before `older`.
    pub newer: Option<SystemTime>,
    pub older: Option<SystemTime>,
    /// Shown in the panel title, as the user typed it.
    pub label: String,
}

impl EntryFilter {
    /// Filter on the name part only (lowercased).
    pub fn by_name(text: &str) -> Self {
        Self {
            name: text.to_lowercase(),
            label: text.to_owned(),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
            && self.exts.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.newer.is_none()
            && self.older.is_none()
    }

    /// File criteria set (extension or size): directories are hidden then.
    fn files_only(&self) -> bool {
        !self.exts.is_empty() || self.min_size.is_some() || self.max_size.is_some()
    }

    pub fn matches(&self, e: &Entry) -> bool {
        if e.is_parent() {
            return true;
        }
        if e.is_dir && self.files_only() {
            return false;
        }
        let name_ok =
            self.name.is_empty() || (e.name.to_string_lossy().to_lowercase()).contains(&self.name);
        let ext_ok = self.exts.is_empty() || self.exts.contains(&ext_of(e));
        let size_ok =
            self.min_size.is_none_or(|m| e.size >= m) && self.max_size.is_none_or(|m| e.size <= m);
        // no known time: an age bound cannot hold
        let time_ok = (self.newer.is_none_or(|t| e.modified.is_some_and(|m| m > t)))
            && (self.older.is_none_or(|t| e.modified.is_some_and(|m| m < t)));
        name_ok && ext_ok && size_ok && time_ok
    }
}

/// Lowercased extension of a file name, `""` when it has none (also `.bashrc`).
pub fn ext_of(e: &Entry) -> String {
    Path::new(&e.name)
        .extension()
        .map_or_else(String::new, |x| x.to_string_lossy().to_lowercase())
}

/// Extensions of the files in `entries` with their counts, most common first, then by name.
pub fn ext_counts(entries: &[Entry]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for e in entries.iter().filter(|e| !e.is_dir) {
        *counts.entry(ext_of(e)).or_default() += 1;
    }
    let mut out: Vec<_> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::{EntryFilter, ext_counts};
    use crate::panel::Panel;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn names(p: &Panel) -> Vec<String> {
        let list = p.entries().iter();
        list.map(|e| e.name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn filters_by_ext_size_and_age() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.rs"), "12345").unwrap();
        fs::write(tmp.path().join("b.RS"), "1").unwrap();
        fs::write(tmp.path().join("c.txt"), "123").unwrap();
        fs::write(tmp.path().join("noext"), "").unwrap();
        fs::create_dir(tmp.path().join("dir.rs")).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        assert_eq!(
            ext_counts(p.entries()),
            [("rs".into(), 2), (String::new(), 1), ("txt".into(), 1)]
        );
        p.apply_filter(EntryFilter {
            exts: vec!["rs".into(), String::new()],
            ..EntryFilter::default()
        });
        assert_eq!(names(&p), ["..", "a.rs", "b.RS", "noext"]); // no directories
        p.apply_filter(EntryFilter {
            min_size: Some(2),
            max_size: Some(4),
            ..EntryFilter::default()
        });
        assert_eq!(names(&p), ["..", "c.txt"]);
        let hour = Duration::from_secs(3600);
        p.apply_filter(EntryFilter {
            newer: SystemTime::now().checked_sub(hour),
            ..EntryFilter::default()
        });
        assert_eq!(p.entries().len(), 6); // all new, the directory too
        p.apply_filter(EntryFilter {
            older: SystemTime::now().checked_sub(hour),
            ..EntryFilter::default()
        });
        assert_eq!(names(&p), [".."]);
        assert_eq!(ext_counts(p.all_entries()).len(), 3); // the popup lists the unfiltered files
        p.apply_filter(EntryFilter::default());
        assert_eq!(p.entries().len(), 6);
    }
}
