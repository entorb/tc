//! Text editor buffer: rope in UTF-8 with LF line ends, original encoding and EOL kept as metadata.

use std::collections::VecDeque;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use encoding_rs::{Encoding, UTF_8};
use regex::Regex;
use ropey::Rope;

use crate::{Error, Result, io_err, syntax};

mod io;
mod size;

pub use size::{SizeCheck, check_size, human};

/// Undo steps kept; older ones are dropped.
const MAX_UNDO: usize = 10_000;
/// Chars scanned for a matching bracket.
const BRACKET_SCAN: usize = 20_000;
const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
    Mixed,
}

/// One text change in char indices, enough to undo and redo it.
struct Change {
    at: usize,
    removed: String,
    inserted: String,
}

pub struct Editor {
    rope: Rope,
    path: PathBuf,
    encoding: &'static Encoding,
    bom: bool,
    eol: Eol,
    /// Line and column in chars.
    // shortcut: cursor counts chars, not graphemes or display cells; upgrade with unicode-width
    pub cursor: (usize, usize),
    /// First visible line and column.
    pub top: usize,
    pub left: usize,
    dirty: bool,
    /// Selection anchor as char index; the other end is the cursor.
    sel: Option<usize>,
    /// Steps of changes, each undone as one unit.
    undo: VecDeque<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    /// Soft wrap at the window width (cursor keys still move by logical line).
    pub wrap: bool,
    /// Line number gutter.
    pub numbers: bool,
    /// Modification time at open or last save, to warn about outside changes.
    mtime: Option<SystemTime>,
}

impl Editor {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file was modified by someone else since it was opened or saved.
    pub fn changed_on_disk(&self) -> bool {
        fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .is_ok_and(|t| Some(t) != self.mtime)
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn encoding_name(&self) -> &'static str {
        self.encoding.name()
    }

    pub fn is_utf8(&self) -> bool {
        self.encoding == UTF_8
    }

    pub fn eol(&self) -> Eol {
        self.eol
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Markdown: is `line` inside a ``` code fence.
    // shortcut: scans from the file start on every call (each frame), upgrade to a cache if big markdown files lag
    pub fn in_fence(&self, line: usize) -> bool {
        let fences = self.rope.lines().take(line);
        fences
            .filter(|l| crate::syntax::is_fence(&l.chars().take(80).collect::<String>()))
            .count()
            % 2
            == 1
    }

    fn line_len(&self, line: usize) -> usize {
        let l = self.rope.line(line);
        let n = l.len_chars();
        if n > 0 && l.char(n - 1) == '\n' {
            n - 1
        } else {
            n
        }
    }

    fn idx(&self) -> usize {
        self.rope.line_to_char(self.cursor.0) + self.cursor.1
    }

    fn line_text(&self, i: usize) -> String {
        let mut s: String = self.rope.line(i).chunks().collect();
        if s.ends_with('\n') {
            s.pop();
        }
        s
    }

    /// Replaces `remove` chars at `at` by `ins`; the caller records the returned change.
    fn splice(&mut self, at: usize, remove: usize, ins: &str) -> Change {
        let removed = self.rope.slice(at..at + remove).to_string();
        if remove > 0 {
            self.rope.remove(at..at + remove);
        }
        if !ins.is_empty() {
            self.rope.insert(at, ins);
        }
        self.dirty = true;
        Change {
            at,
            removed,
            inserted: ins.to_owned(),
        }
    }

    /// Adds an undo step; with `merge` consecutive typing or deleting joins the last step.
    fn record(&mut self, c: Change, merge: bool) {
        self.redo.clear();
        let len = |s: &str| s.chars().count();
        if merge && let Some([last]) = self.undo.back_mut().map(Vec::as_mut_slice) {
            if c.removed.is_empty()
                && last.removed.is_empty()
                && last.at + len(&last.inserted) == c.at
                && !c.inserted.contains('\n')
                && !last.inserted.contains('\n')
            {
                last.inserted.push_str(&c.inserted);
                return;
            }
            if c.inserted.is_empty() && last.inserted.is_empty() {
                if c.at + len(&c.removed) == last.at {
                    last.at = c.at;
                    last.removed.insert_str(0, &c.removed);
                    return;
                }
                if c.at == last.at {
                    last.removed.push_str(&c.removed);
                    return;
                }
            }
        }
        self.undo.push_back(vec![c]);
        if self.undo.len() > MAX_UNDO {
            self.undo.pop_front();
        }
    }

    /// Adds several changes as one undo step.
    fn record_step(&mut self, step: Vec<Change>) {
        if step.is_empty() {
            return;
        }
        self.redo.clear();
        self.undo.push_back(step);
        if self.undo.len() > MAX_UNDO {
            self.undo.pop_front();
        }
    }

    pub fn undo(&mut self) {
        let Some(step) = self.undo.pop_back() else {
            return;
        };
        let mut cursor = 0;
        for c in step.iter().rev() {
            self.splice(c.at, c.inserted.chars().count(), &c.removed);
            cursor = c.at + c.removed.chars().count();
        }
        self.redo.push(step);
        self.sel = None;
        self.goto_idx(cursor);
    }

    pub fn redo(&mut self) {
        let Some(step) = self.redo.pop() else {
            return;
        };
        let mut cursor = 0;
        for c in &step {
            self.splice(c.at, c.removed.chars().count(), &c.inserted);
            cursor = c.at + c.inserted.chars().count();
        }
        self.undo.push_back(step);
        self.sel = None;
        self.goto_idx(cursor);
    }

    /// Selected char range, `None` if empty.
    fn selection(&self) -> Option<Range<usize>> {
        let (a, b) = (self.sel?, self.idx());
        (a != b).then(|| a.min(b)..a.max(b))
    }

    /// Selected columns of `line`, line end excluded.
    pub fn selection_cols(&self, line: usize) -> Option<(usize, usize)> {
        let r = self.selection()?;
        let start = self.rope.line_to_char(line);
        let (a, b) = (r.start.max(start), r.end.min(start + self.line_len(line)));
        (a < b).then(|| (a - start, b - start))
    }

    /// Call before a cursor move: keeps or drops the selection (shift held or not).
    pub fn select(&mut self, extend: bool) {
        let idx = self.idx();
        self.sel = extend.then(|| self.sel.unwrap_or(idx));
    }

    pub fn select_all(&mut self) {
        self.sel = Some(0);
        self.goto_idx(usize::MAX);
    }

    pub fn copy(&self) -> Option<String> {
        self.selection().map(|r| self.rope.slice(r).to_string())
    }

    pub fn cut(&mut self) -> Option<String> {
        let text = self.copy();
        self.delete_selection();
        text
    }

    fn delete_selection(&mut self) -> bool {
        let Some(r) = self.selection() else {
            self.sel = None;
            return false;
        };
        let c = self.splice(r.start, r.len(), "");
        self.record(c, false);
        self.sel = None;
        self.goto_idx(r.start);
        true
    }

    pub fn insert_char(&mut self, c: char) {
        self.insert(&c.to_string(), true);
    }

    /// Insert at the cursor, replacing the selection.
    pub fn insert(&mut self, s: &str, merge: bool) {
        self.delete_selection();
        let at = self.idx();
        let c = self.splice(at, 0, s);
        self.record(c, merge);
        self.goto_idx(at + s.chars().count());
    }

    pub fn backspace(&mut self) {
        let idx = self.idx();
        if self.delete_selection() || idx == 0 {
            return;
        }
        let c = self.splice(idx - 1, 1, "");
        self.record(c, true);
        self.goto_idx(idx - 1);
    }

    pub fn delete(&mut self) {
        let idx = self.idx();
        if self.delete_selection() || idx >= self.rope.len_chars() {
            return;
        }
        let c = self.splice(idx, 1, "");
        self.record(c, true);
    }

    /// Line break that keeps the indentation of the cursor line.
    pub fn newline(&mut self) {
        let (line, col) = self.cursor;
        let indent: String = (self.rope.line(line).chars().take(col))
            .take_while(|&c| c == ' ' || c == '\t')
            .collect();
        self.insert(&format!("\n{indent}"), false);
    }

    /// Deletes the selection, else up to the next (or previous) word start.
    pub fn delete_word(&mut self, forward: bool) {
        if self.delete_selection() {
            return;
        }
        let from = self.idx();
        self.move_word(forward);
        let (a, b) = (from.min(self.idx()), from.max(self.idx()));
        if a < b {
            let c = self.splice(a, b - a, "");
            self.record(c, false);
        }
        self.goto_idx(a);
    }

    /// Copies the cursor line below itself.
    pub fn duplicate_line(&mut self) {
        let (line, col) = self.cursor;
        let at = self.rope.line_to_char(line) + self.line_len(line);
        let text = format!("\n{}", self.line_text(line));
        let c = self.splice(at, 0, &text);
        self.record(c, false);
        self.sel = None;
        self.cursor = (line + 1, col);
    }

    /// Removes one tab or up to 4 spaces in front of the cursor line or every selected line.
    pub fn unindent(&mut self) {
        let (first, last) = match self.selection() {
            Some(r) => (
                self.rope.char_to_line(r.start),
                self.rope.char_to_line(r.end.saturating_sub(1).max(r.start)),
            ),
            None => (self.cursor.0, self.cursor.0),
        };
        let (mut step, mut cursor_cut) = (Vec::new(), 0);
        for l in (first..=last).rev() {
            let head = self.rope.line(l);
            let n = match head.chars().next() {
                Some('\t') => 1,
                _ => head.chars().take(4).take_while(|&c| c == ' ').count(),
            };
            if n > 0 {
                step.push(self.splice(self.rope.line_to_char(l), n, ""));
            }
            if l == self.cursor.0 {
                cursor_cut = n;
            }
        }
        self.record_step(step);
        self.sel = None;
        self.cursor.1 = self.cursor.1.saturating_sub(cursor_cut);
    }

    /// Replaces the selected match (`$1` groups work); the cursor ends behind the new text.
    pub fn replace_selection(&mut self, re: &Regex, with: &str) {
        if let Some(text) = self.copy() {
            self.insert(&re.replace(&text, with), false);
        }
    }

    /// Positions of the bracket at (or before) the cursor and its partner, if any.
    // shortcut: brackets in strings and comments count, scan stops after BRACKET_SCAN chars
    pub fn brackets(&self) -> Option<[(usize, usize); 2]> {
        let at = |i: usize| self.rope.get_char(i);
        let idx = self.idx();
        let (i, c) = [Some(idx), idx.checked_sub(1)]
            .into_iter()
            .flatten()
            .find_map(|i| {
                at(i)
                    .filter(|c| PAIRS.iter().any(|p| p.0 == *c || p.1 == *c))
                    .map(|c| (i, c))
            })?;
        let forward = PAIRS.iter().any(|p| p.0 == c);
        let (same, other) = PAIRS
            .iter()
            .find(|p| p.0 == c || p.1 == c)
            .map(|&(o, k)| if forward { (o, k) } else { (k, o) })?;
        let (mut depth, mut j) = (0usize, i);
        for _ in 0..BRACKET_SCAN {
            j = if forward { j + 1 } else { j.checked_sub(1)? };
            let ch = at(j)?;
            if ch == same {
                depth += 1;
            } else if ch == other {
                if depth == 0 {
                    let pos = |i: usize| {
                        let l = self.rope.char_to_line(i);
                        (l, i - self.rope.line_to_char(l))
                    };
                    return Some([pos(i), pos(j)]);
                }
                depth -= 1;
            }
        }
        None
    }

    /// Deletes the selection, else the cursor line.
    pub fn delete_line(&mut self) {
        if self.delete_selection() {
            return;
        }
        let line = self.cursor.0;
        let start = self.rope.line_to_char(line);
        let end = if line + 1 < self.line_count() {
            self.rope.line_to_char(line + 1)
        } else {
            self.rope.len_chars()
        };
        let c = self.splice(start, end - start, "");
        self.record(c, false);
        self.sel = None;
        self.goto_idx(start);
    }

    /// Cursor to char index `idx`, clamped to the text.
    pub fn goto_idx(&mut self, idx: usize) {
        let idx = idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(idx);
        self.cursor = (line, idx - self.rope.line_to_char(line));
    }

    /// Cursor to the start of line `n` (1-based), clamped.
    pub fn goto_line(&mut self, n: usize) {
        let line = n.saturating_sub(1).min(self.line_count() - 1);
        self.goto_idx(self.rope.line_to_char(line));
    }

    /// Cursor to the start of the next (or previous) word.
    pub fn move_word(&mut self, forward: bool) {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let n = self.rope.len_chars();
        let mut i = self.idx();
        if forward {
            while i < n && word(self.rope.char(i)) {
                i += 1;
            }
            while i < n && !word(self.rope.char(i)) {
                i += 1;
            }
        } else {
            while i > 0 && !word(self.rope.char(i - 1)) {
                i -= 1;
            }
            while i > 0 && word(self.rope.char(i - 1)) {
                i -= 1;
            }
        }
        self.goto_idx(i);
    }

    /// Selects the next (or previous) match of `re`, wrapping around; false if none.
    /// Matches are per line. Runs on the caller's thread.
    // shortcut: scans on the UI thread, upgrade to a worker if huge buffers feel slow
    pub fn find(&mut self, re: &Regex, forward: bool, wrap: bool) -> bool {
        let n = self.line_count();
        let (line, col) = self.cursor;
        // a selected match is skipped, a bare cursor may sit right at one
        let from_col = col + usize::from(self.sel.is_some());
        // n + 1 passes: the cursor line is searched again whole after wrapping
        for step in 0..=n {
            if !wrap && step > 0 && (step > line && !forward || line + step >= n && forward) {
                break;
            }
            let l = if forward {
                (line + step) % n
            } else {
                (line + n - step % n) % n
            };
            let text = self.line_text(l);
            let m = if forward {
                let from = if step == 0 {
                    byte_of(&text, from_col)
                } else {
                    Some(0)
                };
                from.and_then(|b| re.find_at(&text, b))
            } else {
                let limit = if step == 0 {
                    byte_of(&text, col).unwrap_or(text.len())
                } else {
                    usize::MAX
                };
                re.find_iter(&text).filter(|m| m.start() < limit).last()
            };
            if let Some(m) = m {
                let base = self.rope.line_to_char(l);
                let cols = |b: usize| text[..b].chars().count();
                // anchor at the end so the cursor stays at the match start
                self.sel = Some(base + cols(m.end()));
                self.goto_idx(base + cols(m.start()));
                return true;
            }
        }
        false
    }

    /// Replaces every match (`$1` or `${1}` for groups) as one undo step; returns the count.
    // shortcut: undo keeps old and new text of every changed line, upgrade if replacing in huge files
    pub fn replace_all(&mut self, re: &Regex, with: &str) -> usize {
        let (mut step, mut count) = (Vec::new(), 0);
        // bottom up, so earlier char indices stay valid
        for l in (0..self.line_count()).rev() {
            let text = self.line_text(l);
            let k = re.find_iter(&text).count();
            if k == 0 {
                continue;
            }
            count += k;
            let at = self.rope.line_to_char(l);
            step.push(self.splice(at, text.chars().count(), &re.replace_all(&text, with)));
        }
        self.record_step(step);
        self.sel = None;
        self.move_v(0);
        count
    }

    /// Markdown files: blank lines around headings, lists and ``` blocks, as one undo step.
    /// False when the file is not Markdown or nothing changed.
    pub fn format_markdown(&mut self) -> bool {
        if !syntax::lang(&self.path).is_some_and(|l| l.markdown) {
            return false;
        }
        let text: Vec<String> = (0..self.line_count()).map(|l| self.line_text(l)).collect();
        let lines: Vec<&str> = text.iter().map(String::as_str).collect();
        let at = syntax::md_blank_lines(&lines);
        // bottom up, so earlier char indices stay valid; the rope holds `\n`, save restores CRLF
        let step: Vec<Change> = (at.iter().rev())
            .map(|&l| {
                let idx = self.rope.line_to_char(l);
                self.splice(idx, 0, "\n")
            })
            .collect();
        let changed = !step.is_empty();
        self.cursor.0 += at.iter().filter(|&&l| l <= self.cursor.0).count();
        self.record_step(step);
        self.sel = None;
        self.move_v(0);
        changed
    }

    /// Left/right across line ends.
    pub fn move_h(&mut self, delta: isize) {
        let idx = self.idx() as isize + delta;
        self.goto_idx(idx.max(0) as usize);
    }

    /// Up/down, column clamped to the new line.
    pub fn move_v(&mut self, delta: isize) {
        let line = (self.cursor.0 as isize + delta).clamp(0, self.line_count() as isize - 1);
        self.cursor = (
            line as usize,
            self.cursor.1.min(self.line_len(line as usize)),
        );
    }

    pub fn home(&mut self) {
        self.cursor.1 = 0;
    }

    pub fn end(&mut self) {
        self.cursor.1 = self.line_len(self.cursor.0);
    }

    /// Keep the cursor inside the `height` x `width` window.
    pub fn scroll_to_cursor(&mut self, height: usize, width: usize) {
        let (height, width) = (height.max(1), width.max(1));
        let (line, col) = self.cursor;
        self.top = self.top.min(line).max((line + 1).saturating_sub(height));
        if self.wrap {
            self.left = 0;
            while self.top < line
                && self.rows_between(self.top, line, width) + col / width >= height
            {
                self.top += 1;
            }
            return;
        }
        self.left = self.left.min(col).max((col + 1).saturating_sub(width));
    }

    /// Screen rows `lines` .. `end` take when wrapped to `width` (cursor row included).
    fn rows_between(&self, lines: usize, end: usize, width: usize) -> usize {
        (lines..end).map(|l| self.line_len(l) / width + 1).sum()
    }

    /// Cursor cell (row, column) in the text window of `width`.
    pub fn cursor_cell(&self, width: usize) -> (usize, usize) {
        let (line, col) = self.cursor;
        if self.wrap {
            let w = width.max(1);
            (self.rows_between(self.top, line, w) + col / w, col % w)
        } else {
            (line.saturating_sub(self.top), col.saturating_sub(self.left))
        }
    }

    /// Cursor to the clicked cell of the text window, extending the selection if `extend`.
    pub fn click(&mut self, row: usize, col: usize, width: usize, extend: bool) {
        self.select(extend);
        let last = self.line_count() - 1;
        let (line, col) = if self.wrap {
            let (w, mut row, mut line) = (width.max(1), row, self.top);
            while line < last && row > self.line_len(line) / w {
                row -= self.line_len(line) / w + 1;
                line += 1;
            }
            (line, row * w + col)
        } else {
            ((self.top + row).min(last), self.left + col)
        };
        self.cursor = (line, col.min(self.line_len(line)));
    }

    /// Visible rows from `top` as (line, first column, text); a line is cut at `left`, or wrapped
    /// over several rows. Control chars (tabs too) show as one blank cell.
    // shortcut: tabs are not expanded, upgrade when tab stops matter
    pub fn rows(&self, height: usize, width: usize) -> Vec<(usize, usize, String)> {
        let w = width.max(1);
        let mut out = Vec::new();
        for i in self.top..self.line_count() {
            let n = if self.wrap {
                self.line_len(i) / w + 1
            } else {
                1
            };
            for k in 0..n {
                if out.len() >= height {
                    return out;
                }
                let start = if self.wrap { k * w } else { self.left };
                let text = (self.rope.line(i).chars())
                    .filter(|&c| c != '\n')
                    .skip(start)
                    .take(w)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                out.push((i, start, text));
            }
        }
        out
    }

    pub fn visible(&self, height: usize, width: usize) -> Vec<String> {
        self.rows(height, width).into_iter().map(|r| r.2).collect()
    }
}

/// Byte offset of char `col`; `text.len()` for the end, `None` beyond it.
fn byte_of(text: &str, col: usize) -> Option<usize> {
    text.char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .nth(col)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use super::*;

    fn open(p: &Path) -> Result<Editor> {
        Editor::open(p, &AtomicU64::new(0), &AtomicBool::new(false))
    }

    #[test]
    fn format_markdown_adds_blank_lines_as_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        fs::write(&p, "# T\r\ntext\r\n- a\r\n").unwrap();
        let mut e = open(&p).unwrap();
        assert!(e.format_markdown());
        assert_eq!(text(&e), "# T||text||- a|");
        assert!(!e.format_markdown()); // already formatted
        e.undo();
        assert_eq!(text(&e), "# T|text|- a|");
        let txt = dir.path().join("a.txt");
        fs::write(&txt, "# T\ntext\n").unwrap();
        assert!(!open(&txt).unwrap().format_markdown());
    }

    #[test]
    fn edit_ops_and_clamping() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, b"ab\nc").unwrap();
        let mut e = open(&p).unwrap();
        e.move_v(5);
        assert_eq!(e.cursor, (1, 0));
        e.move_h(10);
        assert_eq!(e.cursor, (1, 1)); // file end
        e.backspace();
        e.backspace(); // joins lines
        assert_eq!((e.cursor, e.line_count()), ((0, 2), 1));
        e.insert_char('\n');
        e.insert_char('x');
        assert_eq!(e.visible(5, 5), ["ab", "x"]);
        e.delete(); // at file end: no-op
        e.move_v(-1);
        assert_eq!(e.cursor, (0, 1)); // clamped
        e.end();
        e.delete(); // joins "ab" and "x"
        assert_eq!(e.visible(5, 5), ["abx"]);
    }

    fn editor(text: &str) -> (tempfile::TempDir, Editor) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, text).unwrap();
        let e = open(&p).unwrap();
        (dir, e)
    }

    fn text(e: &Editor) -> String {
        e.visible(100, 100).join("|")
    }

    #[test]
    fn undo_redo_merges_typing() {
        let (_d, mut e) = editor("x");
        e.end();
        for c in "abc".chars() {
            e.insert_char(c);
        }
        e.insert_char('\n');
        e.insert_char('d');
        assert_eq!(text(&e), "xabc|d");
        e.undo();
        assert_eq!(text(&e), "xabc|");
        e.undo();
        assert_eq!(text(&e), "xabc");
        e.undo();
        assert_eq!((text(&e), e.cursor), ("x".into(), (0, 1)));
        e.undo(); // nothing left
        e.redo();
        e.redo();
        assert_eq!(text(&e), "xabc|");
        e.insert_char('z'); // new edit drops redo
        e.redo();
        assert_eq!(text(&e), "xabc|z");
    }

    #[test]
    fn backspace_run_undone_at_once() {
        let (_d, mut e) = editor("hello");
        e.end();
        for _ in 0..3 {
            e.backspace();
        }
        assert_eq!(text(&e), "he");
        e.undo();
        assert_eq!((text(&e), e.cursor), ("hello".into(), (0, 5)));
        e.home();
        e.delete();
        e.delete();
        assert_eq!(text(&e), "llo");
        e.undo();
        assert_eq!(text(&e), "hello");
    }

    #[test]
    fn selection_copy_cut_paste_replace() {
        let (_d, mut e) = editor("one two\nthree");
        e.select(true);
        e.move_word(true);
        assert_eq!(e.copy().as_deref(), Some("one "));
        assert_eq!(e.selection_cols(0), Some((0, 4)));
        assert_eq!(e.selection_cols(1), None);
        assert_eq!(e.cut().as_deref(), Some("one "));
        assert_eq!(text(&e), "two|three");
        e.select(false);
        e.goto_idx(usize::MAX);
        e.insert("X\nY", false);
        assert_eq!(text(&e), "two|threeX|Y");
        e.goto_idx(0);
        e.select(true);
        e.move_h(3);
        e.insert_char('_'); // typing replaces the selection
        assert_eq!(text(&e), "_|threeX|Y");
        e.delete_line();
        assert_eq!(text(&e), "threeX|Y");
        e.goto_line(99);
        assert_eq!(e.cursor, (1, 0));
        e.delete_line(); // last line
        assert_eq!(text(&e), "threeX|");
    }

    #[test]
    fn word_moves_and_goto() {
        let (_d, mut e) = editor("foo, bar_1\nbaz");
        e.move_word(true);
        assert_eq!(e.cursor, (0, 5));
        e.move_word(true);
        assert_eq!(e.cursor, (1, 0));
        e.move_word(false);
        assert_eq!(e.cursor, (0, 5));
        e.goto_line(2);
        assert_eq!(e.cursor, (1, 0));
        e.goto_idx(usize::MAX);
        assert_eq!(e.cursor, (1, 3));
    }

    #[test]
    fn find_wraps_both_ways() {
        let (_d, mut e) = editor("foo bar\nbaz foo\né foo");
        let re = crate::search::compile("foo").unwrap();
        assert!(e.find(&re, true, true));
        assert_eq!(e.cursor, (0, 0)); // a match right at the cursor counts
        assert!(e.find(&re, true, true));
        assert_eq!(e.cursor, (1, 4));
        assert_eq!(e.copy().as_deref(), Some("foo"));
        assert!(e.find(&re, true, true));
        assert_eq!(e.cursor, (2, 2)); // char column, not byte
        assert!(e.find(&re, true, true));
        assert_eq!(e.cursor, (0, 0)); // wrapped
        assert!(!e.find(&re, false, false)); // nothing before the start
        assert!(e.find(&re, false, true));
        assert_eq!(e.cursor, (2, 2));
        assert!(e.find(&re, false, true));
        assert_eq!(e.cursor, (1, 4));
        assert!(!e.find(&crate::search::compile("nope").unwrap(), true, true));
        let (_d, mut e) = editor("one foo");
        assert!(e.find(&re, true, true)); // single match is found again after wrapping
        assert!(e.find(&re, true, true));
    }

    #[test]
    fn indent_duplicate_word_delete_brackets() {
        let (_d, mut e) = editor("  a(b[c]) x\n\tq");
        e.goto_idx(4);
        e.newline();
        assert_eq!(text(&e), "  a(|  b[c]) x| q");
        e.undo();
        assert_eq!(text(&e), "  a(b[c]) x| q");
        e.goto_idx(3); // before `(`
        assert_eq!(e.brackets(), Some([(0, 3), (0, 8)]));
        e.goto_idx(7); // behind `]`
        assert_eq!(e.brackets(), Some([(0, 7), (0, 5)]));
        e.goto_idx(2);
        assert_eq!(e.brackets(), None);
        e.select_all();
        e.unindent();
        assert_eq!(text(&e), "a(b[c]) x|q");
        e.undo();
        assert_eq!(text(&e), "  a(b[c]) x| q");
        e.goto_idx(0);
        e.duplicate_line();
        assert_eq!(e.cursor, (1, 0));
        assert_eq!(text(&e), "  a(b[c]) x|  a(b[c]) x| q");
        e.goto_idx(usize::MAX);
        e.delete_word(false);
        assert_eq!(text(&e), "  a(b[c]) x|  a(b[c]) x| ");
        e.goto_idx(0);
        e.delete_word(true);
        assert_eq!(text(&e), "a(b[c]) x|  a(b[c]) x| ");
    }

    #[test]
    fn wrapped_rows_and_click() {
        let (_d, mut e) = editor("abcdefg\nxy");
        e.wrap = true;
        assert_eq!(e.visible(10, 3), ["abc", "def", "g", "xy"]);
        e.click(2, 1, 3, false); // row "g", past its end
        assert_eq!(e.cursor, (0, 7));
        e.click(3, 1, 3, false);
        assert_eq!(e.cursor, (1, 1));
        e.goto_idx(usize::MAX);
        e.scroll_to_cursor(2, 3);
        assert_eq!((e.top, e.cursor_cell(3)), (1, (0, 2)));
    }

    #[test]
    fn replace_all_is_one_undo_step() {
        let (_d, mut e) = editor("a1 b2\nc\nd3");
        e.goto_idx(usize::MAX);
        let re = crate::search::compile(r"([a-z])(\d)").unwrap();
        assert_eq!(e.replace_all(&re, "${2}${1}"), 3);
        assert_eq!(text(&e), "1a 2b|c|3d");
        assert_eq!(e.replace_all(&re, "x"), 0);
        e.undo();
        assert_eq!(text(&e), "a1 b2|c|d3");
        e.redo();
        assert_eq!(text(&e), "1a 2b|c|3d");
    }
}
