use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{CompareKind, Entry, EntryFilter, Panel, compare};

impl Panel {
    /// Select entries `a..=b` (any order), not "..", and put the cursor on `b`.
    pub fn select_range(&mut self, a: usize, b: usize) {
        self.mark_range(a, b, true);
    }

    /// Every entry of `a..=b` (any order, ".." aside) is marked; false for ".." alone.
    pub fn range_marked(&self, a: usize, b: usize) -> bool {
        let last = self.entries.len().saturating_sub(1);
        let (lo, hi) = (a.min(b).min(last), a.max(b).min(last));
        let mut names = self.entries.get(lo..=hi).unwrap_or_default().iter();
        let mut names = names.by_ref().filter(|e| !e.is_parent()).peekable();
        names.peek().is_some() && names.all(|e| self.selected.contains(&e.name))
    }

    /// Mark (`on`) or unmark entries `a..=b` (any order), not "..", and put the cursor on `b`.
    pub fn mark_range(&mut self, a: usize, b: usize, on: bool) {
        let last = self.entries.len().saturating_sub(1);
        let (lo, hi) = (a.min(b).min(last), a.max(b).min(last));
        let names = self.entries.get(lo..=hi).unwrap_or_default();
        let names = names.iter().filter(|e| !e.is_parent()).map(|e| &e.name);
        if on {
            self.selected.extend(names.cloned());
        } else {
            for n in names {
                self.selected.remove(n);
            }
        }
        self.sel_sum.set(None);
        self.cursor = b.min(last);
    }

    /// Active filter; [`EntryFilter::is_empty`] if none.
    pub fn filter(&self) -> &EntryFilter {
        &self.filter
    }

    /// Every entry of the directory, also those the filter hides.
    pub fn all_entries(&self) -> &[Entry] {
        self.all.as_deref().unwrap_or(&self.entries)
    }

    /// Show only ".." and the entries `filter` keeps; in memory, no re-read.
    pub fn apply_filter(&mut self, filter: EntryFilter) {
        let keep = self.current().map(|e| e.name.clone());
        let all = self
            .all
            .take()
            .unwrap_or_else(|| std::mem::take(&mut self.entries));
        self.filter = filter;
        self.entries = if self.filter.is_empty() {
            all.clone()
        } else {
            all.iter()
                .filter(|e| self.filter.matches(e))
                .cloned()
                .collect()
        };
        if !self.filter.is_empty() {
            self.all = Some(all);
        }
        self.sel_sum.set(None);
        self.cursor = 0;
        if let Some(name) = keep {
            self.focus(&name);
        }
    }

    /// Quick search: move the cursor to the next entry (from `cursor + skip`, wrapping) whose name contains `text`.
    pub fn search(&mut self, text: &str, skip: usize) -> bool {
        let text = text.to_lowercase();
        let n = self.entries.len();
        let hit = (0..n).map(|k| (self.cursor + skip + k) % n).find(|&i| {
            self.entries[i]
                .name
                .to_string_lossy()
                .to_lowercase()
                .contains(&text)
        });
        hit.inspect(|&i| self.cursor = i).is_some()
    }

    /// Put the cursor on entry `i`, clamped to the list.
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.entries.len().saturating_sub(1));
    }

    /// Toggle selection of the cursor entry and move down.
    pub fn toggle_select(&mut self) {
        self.toggle_and_move(1);
    }

    /// Toggle selection of the cursor entry, then move the cursor by `delta`.
    pub fn toggle_and_move(&mut self, delta: isize) {
        if let Some(entry) = self.current().filter(|e| !e.is_parent()) {
            let name = entry.name.clone();
            if !self.selected.remove(&name) {
                self.selected.insert(name);
            }
            self.sel_sum.set(None);
        }
        self.move_cursor(delta);
    }

    /// Select (`on`) or deselect visible files matching `masks`; directories are left alone.
    pub fn select_mask(&mut self, masks: &str, on: bool) {
        self.sel_sum.set(None);
        let masks = crate::search::Masks::new(masks);
        for e in self
            .entries
            .iter()
            .filter(|e| !e.is_dir && masks.matches(&e.name.to_string_lossy()))
        {
            if on {
                self.selected.insert(e.name.clone());
            } else {
                self.selected.remove(&e.name);
            }
        }
    }

    /// Replace the selection of both panels with the entries `kind` picks (see [`compare`]); visible entries only.
    pub fn compare_with(&mut self, other: &mut Panel, kind: CompareKind) {
        let (sa, sb) = compare(&self.entries, &other.entries, kind);
        self.selected = sa;
        other.selected = sb;
        self.sel_sum.set(None);
        other.sel_sum.set(None);
    }

    /// Select every visible entry but "..", directories too.
    pub fn select_all(&mut self) {
        let names = self.entries.iter().filter(|e| !e.is_parent());
        self.selected.extend(names.map(|e| e.name.clone()));
        self.sel_sum.set(None);
    }

    /// Drop the selection; [`Panel::restore_selection`] brings it back.
    pub fn unselect_all(&mut self) {
        if !self.selected.is_empty() {
            self.prev_sel = Some((self.path.clone(), std::mem::take(&mut self.selected)));
        }
        self.sel_sum.set(None);
    }

    /// Select again the last dropped selection (unselect all, directory change); false if it was elsewhere.
    pub fn restore_selection(&mut self) -> bool {
        match &self.prev_sel {
            Some((dir, names)) if *dir == self.path => {
                self.selected.extend(names.iter().cloned());
                self.sel_sum.set(None);
                true
            }
            _ => false,
        }
    }

    /// Select the visible files with the extension of the cursor file (case-insensitive; none = no extension).
    pub fn select_same_ext(&mut self) {
        let ext = |e: &Entry| {
            Path::new(&e.name)
                .extension()
                .map(OsStr::to_ascii_lowercase)
        };
        let Some(want) = self.current().filter(|e| !e.is_dir).map(ext) else {
            return;
        };
        let names = self.entries.iter().filter(|e| !e.is_dir && ext(e) == want);
        let names: Vec<_> = names.map(|e| e.name.clone()).collect();
        self.selected.extend(names);
        self.sel_sum.set(None);
    }

    /// Invert the selection of visible files; directories are left alone.
    pub fn invert_selection(&mut self) {
        self.sel_sum.set(None);
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            if !self.selected.remove(&e.name) {
                self.selected.insert(e.name.clone());
            }
        }
    }

    /// Entries an operation acts on: the selection, else the cursor entry.
    pub fn target_entries(&self) -> Vec<&Entry> {
        if self.selected.is_empty() {
            self.current()
                .filter(|e| !e.is_parent())
                .into_iter()
                .collect()
        } else {
            self.entries
                .iter()
                .filter(|e| self.is_selected(e))
                .collect()
        }
    }

    /// Paths an operation acts on: the selection, else the cursor entry.
    pub fn targets(&self) -> Vec<PathBuf> {
        let list = self.target_entries();
        list.into_iter().map(|e| self.path.join(&e.name)).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn selection_drives_targets() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a"), "").unwrap();
        fs::write(tmp.path().join("b"), "").unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        assert!(p.targets().is_empty()); // cursor on ".."
        p.toggle_select(); // ".." not selectable, moves to "a"
        assert_eq!(p.targets(), [p.path().join("a")]);
        p.toggle_select(); // select "a"
        p.toggle_select(); // select "b"
        assert_eq!(p.targets(), [p.path().join("a"), p.path().join("b")]);
        fs::remove_file(tmp.path().join("a")).unwrap();
        p.reload().unwrap();
        assert_eq!(p.targets(), [p.path().join("b")]);
    }

    #[test]
    fn mask_and_invert() {
        let tmp = tempfile::tempdir().unwrap();
        for n in ["a.rs", "b.RS", "c.txt"] {
            fs::write(tmp.path().join(n), "").unwrap();
        }
        fs::create_dir(tmp.path().join("d.rs")).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.select_mask("*.rs", true);
        assert_eq!(p.targets(), [p.path().join("a.rs"), p.path().join("b.RS")]);
        p.invert_selection();
        assert_eq!(p.targets(), [p.path().join("c.txt")]);
        p.select_mask("", true);
        p.select_mask("c.*", false);
        assert_eq!(p.targets().len(), 2);
    }

    #[test]
    fn select_all_same_ext_and_restore() {
        let tmp = tempfile::tempdir().unwrap();
        for n in ["a.rs", "b.RS", "c.txt", "noext"] {
            fs::write(tmp.path().join(n), "").unwrap();
        }
        fs::create_dir(tmp.path().join("d.rs")).unwrap();
        let mut p = Panel::open(tmp.path()).unwrap();
        p.select_all();
        assert_eq!(p.targets().len(), 5); // dirs too, never ".."
        p.unselect_all();
        assert!(p.selection_info().0 == 0);
        assert!(p.restore_selection());
        assert_eq!(p.targets().len(), 5);
        p.unselect_all();
        assert!(p.focus(OsStr::new("a.rs")));
        p.select_same_ext();
        assert_eq!(p.targets(), [p.path().join("a.rs"), p.path().join("b.RS")]);
        p.unselect_all();
        assert!(p.focus(OsStr::new("d.rs")));
        p.select_same_ext(); // cursor on a directory: nothing
        assert_eq!(p.selection_info().0, 0);
        // a selection left behind by a directory change restores only in its directory
        p.restore_selection();
        p.go(tmp.path().join("d.rs")).unwrap();
        while p.loading().is_some() {
            if let Some(r) = p.wait(std::time::Duration::from_secs(5)) {
                r.unwrap();
            }
        }
        assert!(!p.restore_selection());
        p.parent().unwrap();
        while p.loading().is_some() {
            if let Some(r) = p.wait(std::time::Duration::from_secs(5)) {
                r.unwrap();
            }
        }
        assert!(p.restore_selection());
        assert_eq!(p.targets(), [p.path().join("a.rs"), p.path().join("b.RS")]);
    }
}
