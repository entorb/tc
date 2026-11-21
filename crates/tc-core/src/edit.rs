//! Text editor buffer: rope in UTF-8 with LF line ends, original encoding and EOL kept as metadata.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use encoding_rs::{Encoder, EncoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE};
use ropey::{Rope, RopeBuilder};

use crate::{Error, Result, io_err};

/// Above this the user is asked to use the viewer instead.
const LARGE: u64 = 1 << 30;
const CHUNK: usize = 64 * 1024;
const BINARY_PROBE: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
    Mixed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SizeCheck {
    Ok,
    Large(u64),
}

pub fn human(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

/// Size checks from metadata only, nothing is read.
pub fn check_size(path: &Path) -> Result<SizeCheck> {
    let free = sysinfo::System::new().map_or(0, |mut s| {
        s.refresh_memory();
        s.available_memory()
    });
    check_size_with(path, free)
}

pub(crate) fn check_size_with(path: &Path, free: u64) -> Result<SizeCheck> {
    let len = fs::metadata(path).map_err(io_err(path))?.len();
    // rope overhead and UTF-16 -> UTF-8 growth; free == 0 means the OS reported nothing
    let need = len.saturating_mul(2);
    if free != 0 && need > free {
        return Err(Error::OutOfMemory { need, free });
    }
    Ok(if len > LARGE {
        SizeCheck::Large(len)
    } else {
        SizeCheck::Ok
    })
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
}

impl Editor {
    /// Streams the file into a rope; `progress` gets bytes read, `cancel` aborts.
    pub fn open(path: &Path, progress: &AtomicU64, cancel: &AtomicBool) -> Result<Self> {
        let mut file = File::open(path).map_err(io_err(path))?;
        let mut head = vec![0; CHUNK];
        let n = read_full(&mut file, &mut head).map_err(io_err(path))?;
        head.truncate(n);
        let (encoding, bom_len) = match Encoding::for_bom(&head) {
            Some((enc, len)) => (enc, len),
            None => {
                if head[..n.min(BINARY_PROBE)].contains(&0) {
                    return Err(Error::Binary);
                }
                let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
                det.feed(&head, n < CHUNK);
                (det.guess(None, chardetng::Utf8Detection::Allow), 0)
            }
        };
        file.seek(SeekFrom::Start(bom_len as u64))
            .map_err(io_err(path))?;
        let mut decoder = encoding.new_decoder_without_bom_handling();
        let (mut builder, mut eols) = (RopeBuilder::new(), EolScan::default());
        let (mut buf, mut text) = (vec![0; CHUNK], String::new());
        let mut done = bom_len as u64;
        loop {
            let n = file.read(&mut buf).map_err(io_err(path))?;
            let last = n == 0;
            text.clear();
            text.reserve(decoder.max_utf8_buffer_length(n).unwrap_or(n * 4));
            let (_, _, errors) = decoder.decode_to_string(&buf[..n], &mut text, last);
            // lossy decoding would corrupt the file on save
            if errors {
                return Err(Error::Binary);
            }
            builder.append(&eols.normalize(&text, last));
            done += n as u64;
            progress.store(done, Ordering::Relaxed);
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if last {
                break;
            }
        }
        Ok(Self {
            rope: builder.finish(),
            path: path.to_owned(),
            encoding,
            bom: bom_len > 0,
            eol: eols.kind(),
            cursor: (0, 0),
            top: 0,
            left: 0,
            dirty: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
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

    pub fn insert_char(&mut self, c: char) {
        self.rope.insert_char(self.idx(), c);
        self.dirty = true;
        if c == '\n' {
            self.cursor = (self.cursor.0 + 1, 0);
        } else {
            self.cursor.1 += 1;
        }
    }

    pub fn backspace(&mut self) {
        let idx = self.idx();
        if idx == 0 {
            return;
        }
        self.move_h(-1);
        self.rope.remove(idx - 1..idx);
        self.dirty = true;
    }

    pub fn delete(&mut self) {
        let idx = self.idx();
        if idx < self.rope.len_chars() {
            self.rope.remove(idx..idx + 1);
            self.dirty = true;
        }
    }

    /// Left/right across line ends.
    pub fn move_h(&mut self, delta: isize) {
        let idx = (self.idx() as isize + delta).clamp(0, self.rope.len_chars() as isize) as usize;
        let line = self.rope.char_to_line(idx);
        self.cursor = (line, idx - self.rope.line_to_char(line));
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
        self.left = self.left.min(col).max((col + 1).saturating_sub(width));
    }

    /// Visible part of the lines from `top`; control chars (tabs too) show as one blank cell.
    // shortcut: tabs are not expanded, upgrade when tab stops matter
    pub fn visible(&self, height: usize, width: usize) -> Vec<String> {
        (self.top..self.line_count().min(self.top + height))
            .map(|i| {
                self.rope
                    .line(i)
                    .chars()
                    .filter(|&c| c != '\n')
                    .skip(self.left)
                    .take(width)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect()
            })
            .collect()
    }

    /// Trimmed trailing blanks, atomic replace of the (symlink-resolved) target.
    /// On error the original is intact and the buffer stays dirty.
    pub fn save(
        &mut self,
        to_utf8: bool,
        to_lf: bool,
        progress: &AtomicU64,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let target = fs::canonicalize(&self.path).map_err(io_err(&self.path))?;
        let err = |source: std::io::Error| Error::Io {
            path: target.clone(),
            source,
        };
        let dir = target.parent().unwrap_or(Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(&err)?;
        let perms = fs::metadata(&target).map_err(&err)?.permissions();
        fs::set_permissions(tmp.path(), perms).map_err(&err)?;

        let encoding = if to_utf8 { UTF_8 } else { self.encoding };
        let bom = self.bom && (!to_utf8 || self.encoding == UTF_8);
        let eol = if to_lf || self.eol == Eol::Lf {
            "\n"
        } else {
            "\r\n"
        };
        let mut sink = Sink {
            w: BufWriter::new(tmp.as_file_mut()),
            encoding,
            encoder: encoding.new_encoder(),
            buf: Vec::new(),
        };
        if bom {
            sink.w.write_all(bom_bytes(encoding)).map_err(&err)?;
        }
        let mut line = String::new();
        for (i, l) in self.rope.lines().enumerate() {
            // shortcut: one line is copied whole, a multi-GB single line doubles memory
            line.clear();
            line.extend(l.chunks());
            let has_nl = line.ends_with('\n');
            line.truncate(
                line.trim_end_matches('\n')
                    .trim_end_matches([' ', '\t'])
                    .len(),
            );
            if has_nl {
                line.push_str(eol);
            }
            sink.put(&line, &err)?;
            progress.store(i as u64, Ordering::Relaxed);
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
        }
        sink.finish(&err)?;
        drop(sink);
        tmp.as_file().sync_all().map_err(&err)?;
        tmp.persist(&target).map_err(|e| err(e.error))?;

        self.encoding = encoding;
        self.bom = bom;
        if to_lf {
            self.eol = Eol::Lf;
        }
        self.dirty = false;
        Ok(())
    }
}

fn bom_bytes(enc: &'static Encoding) -> &'static [u8] {
    if enc == UTF_16LE {
        &[0xFF, 0xFE]
    } else if enc == UTF_16BE {
        &[0xFE, 0xFF]
    } else {
        &[0xEF, 0xBB, 0xBF]
    }
}

/// `read` until `buf` is full or EOF.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// CRLF -> LF across chunk borders, counts line end kinds.
#[derive(Default)]
struct EolScan {
    pending_cr: bool,
    lf: usize,
    crlf: usize,
}

impl EolScan {
    fn normalize(&mut self, s: &str, last: bool) -> String {
        let mut out = String::with_capacity(s.len() + 1);
        for c in s.chars() {
            if std::mem::take(&mut self.pending_cr) {
                if c == '\n' {
                    self.crlf += 1;
                    out.push('\n');
                    continue;
                }
                out.push('\r');
            }
            match c {
                '\r' => self.pending_cr = true,
                '\n' => {
                    self.lf += 1;
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        if last && std::mem::take(&mut self.pending_cr) {
            out.push('\r');
        }
        out
    }

    fn kind(&self) -> Eol {
        match (self.lf, self.crlf) {
            (_, 0) => Eol::Lf,
            (0, _) => Eol::Crlf,
            _ => Eol::Mixed,
        }
    }
}

/// Encodes UTF-8 text to the target encoding; refuses to substitute unmappable chars.
struct Sink<W: Write> {
    w: W,
    encoding: &'static Encoding,
    encoder: Encoder,
    buf: Vec<u8>,
}

impl<W: Write> Sink<W> {
    fn put(&mut self, s: &str, err: &dyn Fn(std::io::Error) -> Error) -> Result<()> {
        self.encode(s, false)?;
        self.w.write_all(&self.buf).map_err(err)?;
        self.buf.clear();
        Ok(())
    }

    fn finish(&mut self, err: &dyn Fn(std::io::Error) -> Error) -> Result<()> {
        self.encode("", true)?;
        self.w.write_all(&self.buf).map_err(err)?;
        self.w.flush().map_err(err)
    }

    fn encode(&mut self, s: &str, last: bool) -> Result<()> {
        // encoding_rs cannot encode to UTF-16
        if self.encoding == UTF_16LE || self.encoding == UTF_16BE {
            for u in s.encode_utf16() {
                let b = if self.encoding == UTF_16LE {
                    u.to_le_bytes()
                } else {
                    u.to_be_bytes()
                };
                self.buf.extend_from_slice(&b);
            }
            return Ok(());
        }
        // the _to_vec variant only fills spare capacity
        let max = self
            .encoder
            .max_buffer_length_from_utf8_without_replacement(s.len());
        self.buf.reserve(max.unwrap_or(s.len() * 4 + 16));
        let (res, _) =
            self.encoder
                .encode_from_utf8_to_vec_without_replacement(s, &mut self.buf, last);
        match res {
            EncoderResult::Unmappable(_) => Err(Error::Unrepresentable(self.encoding.name())),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(p: &Path) -> Result<Editor> {
        Editor::open(p, &AtomicU64::new(0), &AtomicBool::new(false))
    }

    fn save(e: &mut Editor, to_utf8: bool, to_lf: bool) -> Result<()> {
        e.save(to_utf8, to_lf, &AtomicU64::new(0), &AtomicBool::new(false))
    }

    /// Open, save unchanged, return resulting bytes.
    fn roundtrip(bytes: &[u8], to_utf8: bool, to_lf: bool) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, bytes).unwrap();
        let mut e = open(&p).unwrap();
        save(&mut e, to_utf8, to_lf).unwrap();
        fs::read(&p).unwrap()
    }

    #[test]
    fn roundtrip_encodings() {
        for bytes in [
            &b"h\xc3\xa9llo\nworld"[..],
            b"\xef\xbb\xbfh\xc3\xa9llo\n",
            b"\xff\xfeh\0i\0\n\0",
            b"\xfe\xff\0h\0i\0\n",
            b"caf\xe9 au lait\nund noch ein Satz mit \xe4\xf6\xfc\n",
        ] {
            assert_eq!(roundtrip(bytes, false, false), bytes);
        }
    }

    #[test]
    fn trims_and_keeps_missing_final_newline() {
        assert_eq!(roundtrip(b"a  \n\tb \t\nc  ", false, false), b"a\n\tb\nc");
    }

    #[test]
    fn eol_and_utf8_conversion() {
        assert_eq!(roundtrip(b"a\r\nb\r\n", false, true), b"a\nb\n");
        assert_eq!(roundtrip(b"a\r\nb\r\n", false, false), b"a\r\nb\r\n");
        assert_eq!(roundtrip(b"a\nb\r\n", false, false), b"a\r\nb\r\n"); // mixed -> CRLF
        assert_eq!(roundtrip(b"caf\xe9\n", true, false), "café\n".as_bytes());
    }

    #[test]
    fn crlf_split_over_chunk_border() {
        let mut bytes = vec![b'a'; CHUNK - 1];
        bytes.extend(b"\r\nb");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, &bytes).unwrap();
        let e = open(&p).unwrap();
        assert_eq!((e.eol(), e.line_count()), (Eol::Crlf, 2));
    }

    #[test]
    fn binary_and_undecodable_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, b"ab\0cd").unwrap();
        assert!(matches!(open(&p), Err(Error::Binary)));
    }

    #[test]
    fn unmappable_char_fails_save_and_keeps_original() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, b"caf\xe9\n").unwrap();
        let mut e = open(&p).unwrap();
        e.insert_char('→');
        assert!(matches!(
            save(&mut e, false, false),
            Err(Error::Unrepresentable(_))
        ));
        assert_eq!(fs::read(&p).unwrap(), b"caf\xe9\n");
        assert!(e.dirty());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1); // no temp left
        save(&mut e, true, false).unwrap();
    }

    #[test]
    fn size_checks_read_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big");
        File::create(&p).unwrap().set_len(LARGE + 1).unwrap();
        assert_eq!(
            check_size_with(&p, 1 << 40).unwrap(),
            SizeCheck::Large(LARGE + 1)
        );
        assert!(matches!(
            check_size_with(&p, 1 << 20),
            Err(Error::OutOfMemory { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn save_in_readonly_dir_fails_cleanly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, b"x").unwrap();
        let mut e = open(&p).unwrap();
        e.insert_char('y');
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let r = save(&mut e, false, false);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        // root ignores directory permissions
        if r.is_err() {
            assert_eq!(fs::read(&p).unwrap(), b"x");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_through_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let (t, l) = (dir.path().join("t"), dir.path().join("l"));
        fs::write(&t, b"a \n").unwrap();
        std::os::unix::fs::symlink(&t, &l).unwrap();
        let mut e = open(&l).unwrap();
        save(&mut e, false, false).unwrap();
        assert_eq!(fs::read(&t).unwrap(), b"a\n");
        assert!(fs::symlink_metadata(&l).unwrap().is_symlink());
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
}
