//! Lazy text viewer: keeps only the byte offset of the top line, reads visible lines on demand.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes searched per step when scanning backwards for a line start.
const CHUNK: usize = 8 * 1024;
/// Longest line part shown; the rest of the line is skipped.
const MAX_LINE: u64 = 4 * 1024;

pub struct Viewer {
    file: File,
    len: u64,
    /// Byte offset of the first visible line, always a line start.
    top: u64,
}

impl Viewer {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len, top: 0 })
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
            } else {
                if self.top == 0 {
                    break;
                }
                self.top = self.prev_line_start(self.top)?;
            }
        }
        Ok(())
    }

    pub fn home(&mut self) {
        self.top = 0;
    }

    /// Show the last `page` lines.
    pub fn end(&mut self, page: usize) -> io::Result<()> {
        self.top = self.prev_line_start(self.len)?;
        self.scroll(1 - page.max(1) as isize)
    }

    /// Up to `count` lines from the top, lossily decoded, tabs expanded.
    pub fn lines(&mut self, count: usize) -> io::Result<Vec<String>> {
        self.file.seek(SeekFrom::Start(self.top))?;
        let mut reader = BufReader::new(&self.file);
        let mut lines = Vec::with_capacity(count);
        let mut buf = Vec::new();
        while lines.len() < count {
            buf.clear();
            if (&mut reader).take(MAX_LINE).read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            if buf.last() == Some(&b'\n') {
                buf.pop();
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
            } else {
                reader.skip_until(b'\n')?;
            }
            lines.push(String::from_utf8_lossy(&buf).replace('\t', "    "));
        }
        Ok(lines)
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
        assert_eq!(v.lines(10).unwrap(), ["a", "b    x", "c"]);
        v.scroll(1).unwrap();
        assert_eq!(v.lines(1).unwrap(), ["b    x"]);
        v.scroll(10).unwrap();
        assert_eq!(v.lines(10).unwrap(), ["c"]);
        v.scroll(-10).unwrap();
        assert_eq!(v.lines(1).unwrap(), ["a"]);
        v.end(2).unwrap();
        assert_eq!(v.lines(10).unwrap(), ["b    x", "c"]);
        v.home();
        assert_eq!(v.lines(1).unwrap(), ["a"]);
    }

    #[test]
    fn edge_files() {
        let (_t, mut v) = viewer(b"");
        v.end(5).unwrap();
        assert!(v.lines(5).unwrap().is_empty());

        let (_t, mut v) = viewer(b"a\r\nb");
        v.end(1).unwrap();
        assert_eq!(v.lines(5).unwrap(), ["b"]);
        v.scroll(-1).unwrap();
        assert_eq!(v.lines(5).unwrap(), ["a", "b"]);

        let (_t, mut v) = viewer(b"\xff\xfe\n");
        assert_eq!(v.lines(5).unwrap(), ["\u{fffd}\u{fffd}"]);
    }

    #[test]
    fn long_lines_cross_chunks() {
        let long = "x".repeat(CHUNK * 2 + 3);
        let (_t, mut v) = viewer(format!("a\n{long}\nb\n").as_bytes());
        v.scroll(1).unwrap();
        assert_eq!(v.lines(1).unwrap()[0].len(), MAX_LINE as usize);
        v.scroll(1).unwrap();
        assert_eq!(v.lines(1).unwrap(), ["b"]);
        v.scroll(-2).unwrap();
        assert_eq!(v.lines(3).unwrap().len(), 3);
        assert_eq!(v.lines(1).unwrap(), ["a"]);
    }
}
