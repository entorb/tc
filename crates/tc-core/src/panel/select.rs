use std::path::PathBuf;

use super::{Entry, Panel, compare};

impl Panel {
    /// Select entries `a..=b` (any order), not "..", and put the cursor on `b`.
    pub fn select_range(&mut self, a: usize, b: usize) {
        let last = self.entries.len().saturating_sub(1);
        let (lo, hi) = (a.min(b).min(last), a.max(b).min(last));
        let names = self.entries.get(lo..=hi).unwrap_or_default();
        let names: Vec<_> = names
            .iter()
            .filter(|e| !e.is_parent())
            .map(|e| e.name.clone())
            .collect();
        self.selected.extend(names);
        self.sel_sum.set(None);
        self.cursor = b.min(last);
    }

    /// Active name filter (lowercased), empty if none.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Show only ".." and entries whose name contains `text` (case-insensitive); in memory, no re-read.
    pub fn apply_filter(&mut self, text: &str) {
        let keep = self.current().map(|e| e.name.clone());
        let all = self
            .all
            .take()
            .unwrap_or_else(|| std::mem::take(&mut self.entries));
        self.filter = text.to_lowercase();
        self.entries = if self.filter.is_empty() {
            all.clone()
        } else {
            let f = &self.filter;
            let hit =
                |e: &&Entry| e.is_parent() || e.name.to_string_lossy().to_lowercase().contains(f);
            all.iter().filter(hit).cloned().collect()
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
        if let Some(entry) = self.current().filter(|e| !e.is_parent()) {
            let name = entry.name.clone();
            if !self.selected.remove(&name) {
                self.selected.insert(name);
            }
            self.sel_sum.set(None);
        }
        self.move_cursor(1);
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

    /// Replace the selection of both panels with their differences (see [`compare`]); visible entries only.
    pub fn compare_with(&mut self, other: &mut Panel) {
        let (sa, sb) = compare(&self.entries, &other.entries);
        self.selected = sa;
        other.selected = sb;
        self.sel_sum.set(None);
        other.sel_sum.set(None);
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
}
