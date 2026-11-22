//! Lazy text viewer: keeps only the byte offset of the top line, reads visible lines on demand.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use encoding_rs::Encoding;
use regex::Regex;

/// Bytes searched per step when scanning backwards for a line start.
const CHUNK: usize = 8 * 1024;
/// Longest line part shown; the rest of the line is skipped.
const MAX_LINE: u64 = 4 * 1024;
pub struct Viewer {
    path: PathBuf,
    file: File,
    len: u64,
    /// Byte offset of the first visible line, always a line start.
    top: u64,
    /// First visible column.
    left: usize,
    encoding: &'static Encoding,
    /// Wrap long lines at the view width instead of scrolling sideways.
    pub wrap: bool,
    /// 0-based number of the top line, `None` once a jump made it unknown.
    top_line: Option<u64>,
}

impl Viewer {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        // shortcut: guesses from the first chunk only, upgrade if later text differs
        let mut head = Vec::with_capacity(CHUNK);
        (&file).take(CHUNK as u64).read_to_end(&mut head)?;
        let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
        det.feed(&head, head.len() < CHUNK);
        let encoding = det.guess(None, chardetng::Utf8Detection::Allow);
        Ok(Self {
            path: path.to_owned(),
            file,
            len,
            top: 0,
            left: 0,
            encoding,
            wrap: false,
            top_line: Some(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Positive scrolls down, negative up; stops at first and last line.
    // shortcut: a file without newlines scans O(size) per keypress, upgrade to wrap-at-width offsets if binaries feel slow
    pub fn scroll(&mut self, lines: isize) -> io::Result<()> {
        for _ in 0..lines.unsigned_abs() {
            if lines > 0 {
                let next = self.next_line_start(self.top)?;
                if next >= self.len {
                    break;
                }
                self.top = next;
                self.top_line = self.top_line.map(|n| n + 1);
            } else {
                if self.top == 0 {
                    break;
                }
                self.top = self.prev_line_start(self.top)?;
                self.top_line = self.top_line.map(|n| n.saturating_sub(1));
            }
        }
        Ok(())
    }

    /// 0-based number of the top line; unknown after a jump (counting would read the file up to there).
    // shortcut: numbers stay unknown after goto percent, search and End until Home or `g`, upgrade with a lazy line index
    pub fn top_line(&self) -> Option<u64> {
        self.top_line
    }

    pub fn home(&mut self) {
        self.top_line = Some(0);
        self.top = 0;
        self.left = 0;
    }

    /// Positive scrolls right, negative left; capped at the longest line part shown.
    pub fn scroll_h(&mut self, cols: isize) {
        self.left = self.left.saturating_add_signed(cols).min(MAX_LINE as usize);
    }

    pub fn encoding_name(&self) -> &'static str {
        self.encoding.name()
    }

    /// Position in the file in percent, by byte offset of the top line.
    pub fn percent(&self) -> u64 {
        (self.top * 100).checked_div(self.len).unwrap_or(100)
    }

    /// Top line becomes line `n` (1-based), the last line if the file is shorter.
    pub fn goto_line(&mut self, n: u64) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::new(&self.file);
        let (mut pos, mut skipped) = (0, 0);
        for _ in 1..n {
            let k = reader.skip_until(b'\n')? as u64;
            if pos + k >= self.len {
                break;
            }
            pos += k;
            skipped += 1;
        }
        self.top = pos;
        self.top_line = Some(skipped);
        Ok(())
    }

    /// Top line becomes the line containing the byte at `percent` of the file size.
    pub fn goto_percent(&mut self, percent: u64) -> io::Result<()> {
        // u128: size * percent cannot overflow
        let off = (self.len as u128 * percent.min(100) as u128 / 100) as u64;
        self.top_line = (off == 0).then_some(0);
        self.top = if self.len == 0 || off == 0 {
            0
        } else {
            self.prev_line_start(off.min(self.len - 1) + 1)?
        };
        Ok(())
    }

    /// Moves the top line to the next (or previous) line matching `re`; false if none.
    /// Lines are matched one by one, so patterns cannot span lines and the file is never loaded.
    // shortcut: backward search seeks per line, upgrade to chunked reverse scan if slow
    pub fn search(&mut self, re: &Regex, forward: bool) -> io::Result<bool> {
        let mut buf = Vec::new();
        if forward {
            let mut pos = self.next_line_start(self.top)?;
            self.file.seek(SeekFrom::Start(pos))?;
            let mut reader = BufReader::new(&self.file);
            loop {
                let n = read_line(&mut reader, &mut buf)?;
                if n == 0 {
                    return Ok(false);
                }
                if re.is_match(&self.encoding.decode_without_bom_handling(&buf).0) {
                    self.top = pos;
                    self.top_line = None;
                    return Ok(true);
                }
                pos += n;
            }
        }
        let mut pos = self.top;
        while pos > 0 {
            pos = self.prev_line_start(pos)?;
            self.file.seek(SeekFrom::Start(pos))?;
            read_line(&mut BufReader::new(&self.file), &mut buf)?;
            if re.is_match(&self.encoding.decode_without_bom_handling(&buf).0) {
                self.top = pos;
                self.top_line = None;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Show the last `page` lines.
    pub fn end(&mut self, page: usize) -> io::Result<()> {
        self.top = self.prev_line_start(self.len)?;
        self.top_line = None;
        self.scroll(1 - page.max(1) as isize)
    }

    /// Up to `count` lines from the top, decoded, tabs expanded, `width` columns from `left`.
    pub fn lines(&mut self, count: usize, width: usize) -> io::Result<Vec<String>> {
        Ok(self.rows(count, width)?.into_iter().map(|r| r.1).collect())
    }

    /// Up to `count` screen rows from the top; the flag marks the first row of a line.
    /// Wrap on: lines are cut into `width` columns (`left` ignored), else one row per line.
    pub fn rows(&mut self, count: usize, width: usize) -> io::Result<Vec<(bool, String)>> {
        self.file.seek(SeekFrom::Start(self.top))?;
        let mut reader = BufReader::new(&self.file);
        let mut rows = Vec::with_capacity(count);
        let mut buf = Vec::new();
        while rows.len() < count {
            if read_line(&mut reader, &mut buf)? == 0 {
                break;
            }
            let text = self
                .encoding
                .decode_without_bom_handling(&buf)
                .0
                .replace('\t', "    ");
            if !self.wrap {
                rows.push((true, text.chars().skip(self.left).take(width).collect()));
                continue;
            }
            let chars: Vec<char> = text.chars().collect();
            let mut parts = chars.chunks(width.max(1));
            rows.push((true, parts.next().unwrap_or_default().iter().collect()));
            for part in parts.take(count - rows.len().min(count)) {
                rows.push((false, part.iter().collect()));
            }
        }
        rows.truncate(count);
        Ok(rows)
    }

    /// Start of the line after the one at `pos`, or `len` if there is none.
    fn next_line_start(&mut self, pos: u64) -> io::Result<u64> {
        self.file.seek(SeekFrom::Start(pos))?;
        Ok(pos + BufReader::new(&self.file).skip_until(b'\n')? as u64)
    }

    /// Start of the line before the line starting at `pos`.
    fn prev_line_start(&mut self, pos: u64) -> io::Result<u64> {
        // The byte before `pos` ends the previous line; skip it.
        let mut end = pos.saturating_sub(1);
        let mut buf = [0; CHUNK];
        while end > 0 {
            let start = end.saturating_sub(CHUNK as u64);
            // In range: end - start <= CHUNK.
            let chunk = &mut buf[..(end - start) as usize];
            self.file.seek(SeekFrom::Start(start))?;
            self.file.read_exact(chunk)?;
            if let Some(i) = chunk.iter().rposition(|&b| b == b'\n') {
                return Ok(start + i as u64 + 1);
            }
            end = start;
        }
        Ok(0)
    }
}

/// Reads one line, capped at `MAX_LINE`, into `buf` without its line end.
/// Returns the bytes consumed including the skipped rest, 0 at end of file.
fn read_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<u64> {
    buf.clear();
    let mut n = reader.by_ref().take(MAX_LINE).read_until(b'\n', buf)? as u64;
    if n == 0 {
        return Ok(0);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    } else {
        n += reader.skip_until(b'\n')? as u64;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer(content: &[u8]) -> (tempfile::NamedTempFile, Viewer) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp, content).unwrap();
        let v = Viewer::open(tmp.path()).unwrap();
        (tmp, v)
    }

    #[test]
    fn scrolls_and_clamps() {
        let (_t, mut v) = viewer(b"a\nb\tx\nc\n");
        assert_eq!(v.lines(10, 80).unwrap(), ["a", "b    x", "c"]);
        v.scroll(1).unwrap();
        assert_eq!(v.lines(1, 80).unwrap(), ["b    x"]);
        v.scroll(10).unwrap();
        assert_eq!(v.lines(10, 80).unwrap(), ["c"]);
        v.scroll(-10).unwrap();
        assert_eq!(v.lines(1, 80).unwrap(), ["a"]);
        v.end(2).unwrap();
        assert_eq!(v.lines(10, 80).unwrap(), ["b    x", "c"]);
        v.home();
        assert_eq!(v.lines(1, 80).unwrap(), ["a"]);
    }

    #[test]
    fn edge_files() {
        let (_t, mut v) = viewer(b"");
        v.end(5).unwrap();
        assert!(v.lines(5, 80).unwrap().is_empty());

        let (_t, mut v) = viewer(b"a\r\nb");
        v.end(1).unwrap();
        assert_eq!(v.lines(5, 80).unwrap(), ["b"]);
        v.scroll(-1).unwrap();
        assert_eq!(v.lines(5, 80).unwrap(), ["a", "b"]);

        // detected as a legacy encoding, never fails on odd bytes
        let (_t, mut v) = viewer(b"\xff\xfe\n");
        assert_eq!(v.lines(5, 80).unwrap().len(), 1);
    }

    #[test]
    fn long_lines_cross_chunks() {
        let long = "x".repeat(CHUNK * 2 + 3);
        let (_t, mut v) = viewer(format!("a\n{long}\nb\n").as_bytes());
        v.scroll(1).unwrap();
        assert_eq!(v.lines(1, usize::MAX).unwrap()[0].len(), MAX_LINE as usize);
        v.scroll(1).unwrap();
        assert_eq!(v.lines(1, 80).unwrap(), ["b"]);
        v.scroll(-2).unwrap();
        assert_eq!(v.lines(3, 80).unwrap().len(), 3);
        assert_eq!(v.lines(1, 80).unwrap(), ["a"]);
    }

    #[test]
    fn search_both_directions() {
        let (_t, mut v) = viewer(b"alpha\nBeta\ngamma\nbeta two\n");
        let re = crate::search::compile("^beta").unwrap();
        assert!(v.search(&re, true).unwrap());
        assert_eq!(v.lines(1, 80).unwrap(), ["Beta"]);
        assert!(v.search(&re, true).unwrap());
        assert_eq!(v.lines(1, 80).unwrap(), ["beta two"]);
        assert!(!v.search(&re, true).unwrap());
        assert!(v.search(&re, false).unwrap());
        assert_eq!(v.lines(1, 80).unwrap(), ["Beta"]);
        assert!(!v.search(&re, false).unwrap());
        // $ must match before the line end
        let re = crate::search::compile("gamma$").unwrap();
        assert!(v.search(&re, true).unwrap());
    }

    #[test]
    fn goto_and_view_options() {
        let (_t, mut v) = viewer(b"a\nb\nc\nd\n");
        v.goto_line(3).unwrap();
        assert_eq!(v.lines(1, 80).unwrap(), ["c"]);
        v.goto_line(99).unwrap();
        assert_eq!(v.lines(5, 80).unwrap(), ["d"]);
        v.goto_line(0).unwrap();
        assert_eq!(v.lines(1, 80).unwrap(), ["a"]);
        v.goto_percent(50).unwrap(); // byte 4 is "c"
        assert_eq!(v.lines(1, 80).unwrap(), ["c"]);
        v.goto_percent(100).unwrap();
        assert_eq!(v.lines(5, 80).unwrap(), ["d"]);
        assert_eq!(v.percent(), 75);

        let (_t, mut v) = viewer(b"abcdef\n");
        v.scroll_h(2);
        assert_eq!(v.lines(1, 3).unwrap(), ["cde"]);
        v.scroll_h(-10);
        assert_eq!(v.lines(1, 3).unwrap(), ["abc"]);

        let (_t, mut v) = viewer(b"caf\xe9\n");
        assert_eq!(
            (v.encoding_name(), v.lines(1, 80).unwrap()),
            ("windows-1252", vec!["café".to_owned()])
        );
    }

    #[test]
    fn wrap_rows_and_line_numbers() {
        let (_t, mut v) = viewer(b"abcdef\nx\ny\n");
        v.wrap = true;
        assert_eq!(
            v.rows(10, 4).unwrap(),
            [
                (true, "abcd".into()),
                (false, "ef".into()),
                (true, "x".into()),
                (true, "y".into())
            ]
        );
        assert_eq!(v.top_line(), Some(0));
        v.scroll(1).unwrap();
        assert_eq!(v.top_line(), Some(1));
        v.goto_line(3).unwrap();
        assert_eq!(v.top_line(), Some(2));
        v.goto_percent(50).unwrap();
        assert_eq!(v.top_line(), None);
        v.home();
        assert_eq!(v.top_line(), Some(0));
    }
}
