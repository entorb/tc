//! Recursive file search: name masks or regex, excluded files and directories, size, date and
//! extension filters, optional content regex. Local parallel walk, or one `find` on a server.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use ignore::{DirEntry, WalkBuilder, WalkState};
use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};
use regex::Regex;

use crate::panel::{Entry, entry_with};
use crate::search::{self, Masks};
use crate::vfs::Remote;

/// Results kept at most; the search stops there.
pub const MAX_RESULTS: usize = 100_000;
/// Longest line chunk matched; longer lines are matched in pieces, so memory stays bounded.
const MAX_LINE: u64 = 64 * 1024;
/// Characters of a matching line kept for the result list.
const PREVIEW: usize = 200;

/// What to search for. Empty, `false` or `None` fields do not filter.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Name masks (`*`, `?`, comma or space separated); empty = any.
    pub mask: String,
    /// Name regex, used instead of `mask` (local search only).
    pub name_re: Option<Regex>,
    /// `mask` and `exclude_files` match case-sensitively.
    pub mask_case: bool,
    /// Files (not directories) whose name matches these masks are left out.
    pub exclude_files: String,
    /// Content search in text files.
    pub content: Option<Content>,
    /// Directories not entered at all: exact names, or paths relative to the root (with a `/`).
    pub exclude: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Modified on or after `after`, strictly before `before`.
    pub after: Option<Timestamp>,
    pub before: Option<Timestamp>,
    /// Extensions without the dot, lowercase; empty = any.
    pub exts: Vec<String>,
    /// 1 = only the root's own entries.
    pub max_depth: Option<usize>,
    /// Dot-names (and the Windows hidden attribute) skipped, hidden directories not entered.
    pub skip_hidden: bool,
    /// `.gitignore`, `.git/info/exclude` and the global git excludes apply inside git repos;
    /// `.git` itself is not entered (local search only).
    pub gitignore: bool,
    /// Symlinked directories are entered (loops are detected and skipped).
    pub follow_links: bool,
}

/// Content search: the regex and how it was built (for `grep` on servers).
#[derive(Debug, Clone)]
pub struct Content {
    pub re: Regex,
    pub text: String,
    /// Plain text where `*` matches any run of characters within a line; else a regex.
    pub plain: bool,
    pub word: bool,
    /// Case always matters; else smart case (no uppercase = ignore case).
    pub case: bool,
    /// Files that do not contain the text (text files only).
    pub negate: bool,
    /// One result per matching line instead of per file.
    pub every: bool,
}

impl Content {
    /// `text` as a regex, or with `plain` literal except `*` (any run of characters);
    /// `word` matches whole words only.
    pub fn new(text: &str, plain: bool, word: bool, case: bool) -> Result<Self, String> {
        let pattern = if plain {
            let parts: Vec<String> = text.split('*').map(regex::escape).collect();
            parts.join(".*")
        } else {
            text.to_owned()
        };
        let pattern = if word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        };
        Ok(Content {
            re: search::compile_case(&pattern, case).map_err(|e| e.to_string())?,
            text: text.to_owned(),
            plain,
            word,
            case,
            negate: false,
            every: false,
        })
    }

    /// Smart case resolved: true when the match ignores case.
    fn ignores_case(&self) -> bool {
        !self.case && !self.text.chars().any(char::is_uppercase)
    }
}

/// A found entry (name = full path) and, from a content search, the matching line:
/// number (1-based) and text (trimmed, cut to [`PREVIEW`] chars).
#[derive(Debug, Clone)]
pub struct Hit {
    pub entry: Entry,
    pub line: Option<(u64, String)>,
}

impl Query {
    /// Size, date and extension filters only files can pass; directories then never match.
    fn file_only(&self) -> bool {
        self.min_size.is_some()
            || self.max_size.is_some()
            || self.after.is_some()
            || self.before.is_some()
            || !self.exts.is_empty()
            || self.content.is_some()
    }

    fn ext_ok(&self, name: &str) -> bool {
        self.exts.is_empty()
            || Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| self.exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
    }

    fn size_date_ok(&self, e: &Entry) -> bool {
        let modified = e.modified.and_then(|m| Timestamp::try_from(m).ok());
        self.min_size.is_none_or(|m| e.size >= m)
            && self.max_size.is_none_or(|m| e.size <= m)
            && self.after.is_none_or(|a| modified.is_some_and(|m| m >= a))
            && self.before.is_none_or(|b| modified.is_some_and(|m| m < b))
    }

    /// Why this query cannot run on a server; `None` when it can.
    pub fn remote_unsupported(&self) -> Option<&'static str> {
        if self.name_re.is_some() {
            Some("Name regex is not available on remote panels.")
        } else if self.gitignore {
            Some(".gitignore is not available on remote panels.")
        } else {
            None
        }
    }
}

/// Walks `root` with several threads and passes each match to `out` (order not stable).
/// Stops on `cancel` or after `MAX_RESULTS`; unreadable directories are skipped.
pub fn find(root: &Path, q: &Query, cancel: &AtomicBool, out: impl Fn(Hit) + Sync) {
    let m = Matcher {
        q,
        masks: Masks::with_case(&q.mask, q.mask_case),
        skip: Masks::with_case(&q.exclude_files, q.mask_case),
        cancel,
        found: AtomicUsize::new(0),
        out,
    };
    walker(root, q).run(|| {
        Box::new(|dent| {
            if m.full() {
                return WalkState::Quit;
            }
            // Err: unreadable directory or a symlink loop
            if let Ok(d) = dent
                && d.depth() > 0
            {
                m.visit(&d);
            }
            WalkState::Continue
        })
    });
}

/// Parallel walker with the query's hidden, gitignore, symlink, depth and exclude settings.
fn walker(root: &Path, q: &Query) -> ignore::WalkParallel {
    let gi = q.gitignore;
    let mut b = WalkBuilder::new(root);
    b.standard_filters(false)
        .hidden(q.skip_hidden)
        .git_ignore(gi)
        .git_exclude(gi)
        .git_global(gi)
        .parents(gi)
        .follow_links(q.follow_links)
        .max_depth(q.max_depth);
    let (root, exclude) = (root.to_owned(), q.exclude.clone());
    b.filter_entry(move |d| !excluded(&root, &exclude, gi, d));
    b.build_parallel()
}

/// Directory left out: an excluded name or relative path, or `.git` with gitignore on.
fn excluded(root: &Path, exclude: &[String], gitignore: bool, d: &DirEntry) -> bool {
    if d.depth() == 0 || !d.file_type().is_some_and(|t| t.is_dir()) {
        return false;
    }
    let name = d.file_name();
    (gitignore && name == ".git")
        || exclude.iter().any(|x| {
            if x.contains(['/', '\\']) {
                // Path equality compares components, so `a/b` also matches `a\b` on Windows
                let rel = Path::new(x.trim_matches(['/', '\\']));
                d.path().strip_prefix(root).is_ok_and(|r| r == rel)
            } else {
                name == OsStr::new(x)
            }
        })
}

/// Shared state of the walker threads.
struct Matcher<'a, F> {
    q: &'a Query,
    masks: Masks,
    skip: Masks,
    cancel: &'a AtomicBool,
    found: AtomicUsize,
    out: F,
}

impl<F: Fn(Hit)> Matcher<'_, F> {
    fn full(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.found.load(Ordering::Relaxed) >= MAX_RESULTS
    }

    /// False once the cap is reached, so a file listing every line stops too.
    fn emit(&self, entry: Entry, line: Option<(u64, String)>) -> bool {
        if self.found.fetch_add(1, Ordering::Relaxed) >= MAX_RESULTS {
            return false;
        }
        (self.out)(Hit { entry, line });
        true
    }

    fn name_ok(&self, name: &str, is_dir: bool) -> bool {
        let hit = match &self.q.name_re {
            Some(re) => re.is_match(name),
            None => self.masks.matches(name),
        };
        hit && self.q.ext_ok(name) && (is_dir || self.skip.is_empty() || !self.skip.matches(name))
    }

    fn visit(&self, d: &DirEntry) {
        let path = d.path();
        let name = d.file_name().to_string_lossy();
        let is_dir = d.file_type().is_some_and(|t| t.is_dir());
        if (is_dir && self.q.file_only()) || !self.name_ok(&name, is_dir) {
            return;
        }
        // Err: vanished since listed; metadata comes with the listing on Windows
        let Ok(meta) = d.metadata() else {
            return;
        };
        let e = entry_with(path.as_os_str().to_owned(), path, meta);
        if !self.q.size_date_ok(&e) {
            return;
        }
        let Some(c) = &self.q.content else {
            self.emit(e, None);
            return;
        };
        if c.negate {
            if scan(path, &c.re, self.cancel, |_, _| false) == Some(false) {
                self.emit(e, None);
            }
        } else if c.every {
            scan(path, &c.re, self.cancel, |n, t| {
                self.emit(e.clone(), Some((n, preview(t))))
            });
        } else {
            let mut first = None;
            scan(path, &c.re, self.cancel, |n, t| {
                first = Some((n, preview(t)));
                false
            });
            if first.is_some() {
                self.emit(e, first);
            }
        }
    }
}

fn preview(line: &str) -> String {
    line.trim().chars().take(PREVIEW).collect()
}

/// Streams the file in bounded chunks and calls `hit` (line number, text) per matching line
/// until it returns false. `Some(matched)`, or `None` when unreadable, cancelled or binary
/// (a NUL byte before the scan stopped).
fn scan(
    path: &Path,
    re: &Regex,
    cancel: &AtomicBool,
    mut hit: impl FnMut(u64, &str) -> bool,
) -> Option<bool> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let (mut buf, mut line, mut matched) = (Vec::new(), 1, false);
    loop {
        buf.clear();
        let n = reader
            .by_ref()
            .take(MAX_LINE)
            .read_until(b'\n', &mut buf)
            .ok()?;
        if n == 0 {
            return Some(matched);
        }
        if buf.contains(&0) || cancel.load(Ordering::Relaxed) {
            return None;
        }
        let text = String::from_utf8_lossy(&buf);
        if re.is_match(&text) {
            matched = true;
            if !hit(line, &text) {
                return Some(true);
            }
        }
        if buf.ends_with(b"\n") {
            line += 1;
        }
    }
}

/// One `find` on the server for `q` under `root` (see [`Query::remote_unsupported`]; content
/// runs one `grep` per file). Output records: `type size mtime relative-path`, NUL separated,
/// like the File Stats scan.
pub fn remote_command(root: &str, q: &Query) -> String {
    let follow = if q.follow_links { "-L " } else { "" };
    let mut c = format!("find {follow}{} -mindepth 1", sh_quote(root));
    if let Some(d) = q.max_depth {
        c += &format!(" -maxdepth {d}");
    }
    if q.skip_hidden {
        c += " \\( -name '.*' -prune \\) -o";
    }
    if !q.exclude.is_empty() {
        let base = root.trim_end_matches('/');
        let tests: Vec<String> = (q.exclude.iter())
            .map(|x| match x.trim_matches('/') {
                rel if x.contains('/') => format!("-path {}", sh_quote(&format!("{base}/{rel}"))),
                name => format!("-name {}", sh_quote(name)),
            })
            .collect();
        c += &format!(" \\( -type d \\( {} \\) -prune \\) -o", tests.join(" -o "));
    }
    let conds: String = remote_conds(q).iter().map(|c| format!("{c} ")).collect();
    c += &format!(" \\( {conds}-printf '%y %s %T@ %P\\0' \\)");
    c
}

/// `find` tests of the name, size, date and content filters, ANDed.
fn remote_conds(q: &Query) -> Vec<String> {
    let name = if q.mask_case { "-name" } else { "-iname" };
    let any = |masks: &str| -> Option<String> {
        let tests: Vec<String> = (masks.split([',', ' ']))
            .filter(|m| !m.is_empty())
            .map(|m| format!("{name} {}", sh_quote(m)))
            .collect();
        (!tests.is_empty()).then(|| format!("\\( {} \\)", tests.join(" -o ")))
    };
    let mut conds: Vec<String> = any(&q.mask).into_iter().collect();
    if let Some(skip) = any(&q.exclude_files) {
        conds.push(format!("\\( -type d -o ! {skip} \\)"));
    }
    let exts: Vec<String> = q.exts.iter().map(|e| format!("*.{e}")).collect();
    if let Some(t) = any(&exts.join(",")) {
        conds.push(t.replace("-name ", "-iname "));
    }
    if q.file_only() {
        conds.push("! -type d".into());
    }
    if let Some(min) = q.min_size.filter(|&m| m > 0) {
        conds.push(format!("-size +{}c", min - 1));
    }
    if let Some(max) = q.max_size {
        conds.push(format!("-size -{}c", max.saturating_add(1)));
    }
    if let Some(a) = q.after {
        // -newermt is strictly newer: 1 ns before `a` keeps files modified exactly at `a`
        conds.push(format!("-newermt @{}.999999999", a.as_second() - 1));
    }
    if let Some(b) = q.before {
        conds.push(format!("! -newermt @{}", b.as_second()));
    }
    if let Some(c) = &q.content {
        conds.push(remote_grep(c));
    }
    conds
}

/// `grep` as a `find` test: `-I` skips binary files, `-F` plain text, `-E` extended regex
/// (also for plain text with `*`, escaped for ERE).
fn remote_grep(c: &Content) -> String {
    let mut flags = String::from("-qI");
    if c.ignores_case() {
        flags.push('i');
    }
    if c.word {
        flags.push('w');
    }
    let pattern = match (c.plain, c.text.contains('*')) {
        (true, false) => {
            flags.push('F');
            c.text.clone()
        }
        (true, true) => {
            flags.push('E');
            let parts: Vec<String> = c.text.split('*').map(ere_escape).collect();
            parts.join(".*")
        }
        (false, _) => {
            flags.push('E');
            c.text.clone()
        }
    };
    let grep = format!(r"-exec grep {flags} -e {} {{}} \;", sh_quote(&pattern));
    if c.negate {
        // the empty pattern matches any non-empty text file, so binary files stay out
        format!(r"-type f -exec grep -qI '' {{}} \; ! {grep}")
    } else {
        format!("-type f {grep}")
    }
}

/// POSIX extended regex matching `s` literally.
fn ere_escape(s: &str) -> String {
    s.chars().fold(String::new(), |mut out, c| {
        if r".[]()*+?{}|^$\".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

/// Runs [`remote_command`] and passes each hit to `out` (name = full remote path).
/// Stops on `cancel` or after `MAX_RESULTS`; an error when the server cannot run commands.
pub fn find_remote(
    r: &dyn Remote,
    root: &str,
    q: &Query,
    cancel: &AtomicBool,
    mut out: impl FnMut(Entry),
) -> crate::Result<()> {
    let mut found = 0;
    r.exec(&remote_command(root, q), &mut |stream| {
        let mut reader = BufReader::new(stream);
        let mut rec = Vec::new();
        loop {
            rec.clear();
            // bounded: a record longer than a path is garbage
            let n = reader
                .by_ref()
                .take(MAX_LINE)
                .read_until(0, &mut rec)
                .map_err(|e| crate::Error::Remote(e.to_string()))?;
            if n == 0 || cancel.load(Ordering::Relaxed) || found >= MAX_RESULTS {
                return Ok(());
            }
            let text = String::from_utf8_lossy(rec.strip_suffix(&[0]).unwrap_or(&rec));
            if let Some(e) = parse_record(root, &text) {
                found += 1;
                out(e);
            }
        }
    })
}

/// One `type size mtime relative-path` record of [`remote_command`]; `None` when malformed.
fn parse_record(root: &str, text: &str) -> Option<Entry> {
    let mut parts = text.splitn(4, ' ');
    let (kind, size, mtime, rel) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let modified = mtime
        .parse::<f64>()
        .ok()
        .and_then(|s| Duration::try_from_secs_f64(s).ok())
        .map(|d| UNIX_EPOCH + d);
    Some(Entry {
        name: format!("{}/{rel}", root.trim_end_matches('/')).into(),
        is_dir: kind == "d",
        is_symlink: kind == "l",
        size: size.parse().unwrap_or(0),
        modified,
        link: None,
    })
}

/// Single-quoted shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Whole number with an optional K, M, G or T suffix (1024 steps) and optional B: `500`, `10k`, `2 MB`.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim().trim_end_matches(['b', 'B']);
    let (digits, mult) = match t.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&t[..t.len() - 1], 1u64 << 10),
        Some('M') => (&t[..t.len() - 1], 1 << 20),
        Some('G') => (&t[..t.len() - 1], 1 << 30),
        Some('T') => (&t[..t.len() - 1], 1 << 40),
        _ => (t, 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .ok_or_else(|| format!("not a size: {}", s.trim()))
}

/// Date `YYYY-MM-DD` in local time: its start, or with `end` the start of the next day
/// (an exclusive bound, so the whole last second of that day is included).
pub fn parse_date(s: &str, end: bool) -> Result<Timestamp, String> {
    let bad = || format!("not a date (YYYY-MM-DD): {}", s.trim());
    let date: Date = s.trim().parse().map_err(|_| bad())?;
    let start = date
        .to_zoned_datetime(TimeZone::system())
        .map_err(|_| bad())?
        .timestamp();
    if !end {
        return Ok(start);
    }
    let next = date.tomorrow().map_err(|_| bad())?;
    next.to_zoned_datetime(TimeZone::system())
        .map(|z| z.timestamp())
        .map_err(|_| bad())
}

/// Age like `30m`, `12h`, `7d` or `2w` (a bare number = days): the time that long ago.
pub fn parse_age(s: &str) -> Result<Timestamp, String> {
    let bad = || format!("not an age (30m, 12h, 7d, 2w): {}", s.trim());
    let t = s.trim().to_ascii_lowercase();
    let unit = |n: usize| &t[..t.len() - n];
    let (digits, secs) = match t.chars().last() {
        Some('m') => (unit(1), 60),
        Some('h') => (unit(1), 3600),
        Some('d') => (unit(1), 86_400),
        Some('w') => (unit(1), 604_800),
        _ => (t.as_str(), 86_400),
    };
    // u32 days fit i64 seconds, and negative ages are refused
    let n: u32 = digits.trim().parse().map_err(|_| bad())?;
    Timestamp::now()
        .checked_sub(SignedDuration::from_secs(i64::from(n) * secs))
        .map_err(|_| bad())
}

/// Comma separated names, trimmed; empty entries dropped.
pub fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Extensions as typed (`.rs, toml`): lowercase, without dots.
pub fn parse_exts(s: &str) -> Vec<String> {
    parse_list(s)
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        AtomicBool, Content, File, Hit, Path, Query, SignedDuration, Timestamp, UNIX_EPOCH, find,
        parse_age, parse_date, parse_exts, parse_list, parse_record, parse_size, remote_command,
        remote_grep,
    };
    use regex::Regex;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn hits(root: &Path, q: &Query) -> Vec<Hit> {
        let v = Mutex::new(Vec::new());
        find(root, q, &AtomicBool::new(false), |h| {
            v.lock().unwrap().push(h)
        });
        let mut v = v.into_inner().unwrap();
        v.sort_by(|a, b| (&a.entry.name, &a.line).cmp(&(&b.entry.name, &b.line)));
        v
    }

    fn run(root: &Path, q: &Query) -> Vec<PathBuf> {
        hits(root, q)
            .into_iter()
            .map(|h| PathBuf::from(h.entry.name))
            .collect()
    }

    fn text(t: &str) -> Option<Content> {
        Some(Content::new(t, false, false, false).unwrap())
    }

    fn mask(m: &str) -> Query {
        Query {
            mask: m.into(),
            ..Query::default()
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "NTFS is not sparse by default, set_len reserves 1 TiB"
    )]
    fn mask_and_content_with_huge_sparse_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(tmp.path().join("a.txt"), "x\nNeedle here\n").unwrap();
        fs::write(sub.join("b.txt"), "nothing\n").unwrap();
        fs::write(sub.join("c.rs"), "needle\n").unwrap();
        fs::write(sub.join("bin.txt"), b"needle\0").unwrap();
        // 1 TiB of zeros: binary, so skipped after one chunk instead of read
        let big = File::create(sub.join("big.txt")).unwrap();
        big.set_len(1 << 40).unwrap();
        let (a, b, c) = (
            tmp.path().join("a.txt"),
            sub.join("b.txt"),
            sub.join("c.rs"),
        );
        assert_eq!(
            run(tmp.path(), &mask("*.txt")),
            [a.clone(), b, sub.join("big.txt"), sub.join("bin.txt")]
        );
        let content = |m: &str, re: &str| Query {
            content: text(re),
            ..mask(m)
        };
        assert_eq!(run(tmp.path(), &content("*", "needle")), [a.clone(), c]);
        assert_eq!(run(tmp.path(), &content("*.txt", "Needle")), [a]); // smart case
        assert_eq!(run(tmp.path(), &mask("sub")), [sub]);
    }

    #[test]
    fn excludes_size_and_extension_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let nm = tmp.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join("dep.js"), "x").unwrap();
        fs::write(tmp.path().join("small.rs"), "x").unwrap();
        fs::write(tmp.path().join("big.RS"), "x".repeat(100)).unwrap();
        fs::write(tmp.path().join("app.js"), "x").unwrap();
        let q = Query {
            exclude: vec!["node_modules".into()],
            exts: vec!["rs".into()],
            min_size: Some(2),
            ..Query::default()
        };
        assert_eq!(run(tmp.path(), &q), [tmp.path().join("big.RS")]);
        // directories never match once a file-only filter is set
        let q = Query {
            exts: vec!["rs".into(), "js".into()],
            ..Query::default()
        };
        assert!(!run(tmp.path(), &q).contains(&nm));
    }

    #[test]
    fn date_bounds_and_parsers() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("new.txt"), "x").unwrap();
        let now = Timestamp::now();
        let q = Query {
            after: Some(now - SignedDuration::from_secs(3600)),
            ..Query::default()
        };
        assert_eq!(run(tmp.path(), &q), [tmp.path().join("new.txt")]);
        let q = Query {
            before: Some(now - SignedDuration::from_secs(3600)),
            ..Query::default()
        };
        assert!(run(tmp.path(), &q).is_empty());
        // "before" a day includes its last, fractional second
        let end = parse_date("2024-01-01", true).unwrap();
        let late = tmp.path().join("late.txt");
        fs::write(&late, "x").unwrap();
        File::options()
            .write(true)
            .open(&late)
            .unwrap()
            .set_modified((end - SignedDuration::from_millis(500)).try_into().unwrap())
            .unwrap();
        let q = Query {
            before: Some(end),
            ..Query::default()
        };
        assert_eq!(run(tmp.path(), &q), [late]);

        assert_eq!(parse_size("10k"), Ok(10 << 10));
        assert_eq!(parse_size(" 2 MB "), Ok(2 << 20));
        assert_eq!(parse_size("1G"), Ok(1 << 30));
        assert!(parse_size("1.5M").is_err() && parse_size("x").is_err());
        assert!(parse_date("2024-02-30", false).is_err());
        let day = parse_date("2024-01-01", false).unwrap();
        assert!(parse_date("2024-01-01", true).unwrap() > day);
        assert_eq!(
            parse_list(" node_modules , .venv ,, "),
            ["node_modules", ".venv"]
        );
        assert_eq!(parse_exts(".RS, toml"), ["rs", "toml"]);
    }

    #[test]
    fn remote_command_quotes_and_prunes() {
        let q = Query {
            mask: "*.rs, *.toml".into(),
            exclude: vec!["node_modules".into(), ".venv".into()],
            exts: vec!["rs".into()],
            min_size: Some(1),
            max_size: Some(10),
            ..Query::default()
        };
        let c = remote_command("/home/o'neil", &q);
        assert!(c.starts_with("find '/home/o'\\''neil' -mindepth 1 \\( -type d"));
        assert!(c.contains("-name 'node_modules' -o -name '.venv'"));
        assert!(c.contains("-iname '*.rs' -o -iname '*.toml'"));
        assert!(c.contains("-size +0c") && c.contains("-size -11c"));
        assert!(c.ends_with("-printf '%y %s %T@ %P\\0' \\)"));
        let after = Query {
            after: Some(Timestamp::from_second(100).unwrap()),
            ..Query::default()
        };
        assert!(remote_command("/", &after).contains("-newermt @99.999999999"));
    }

    #[test]
    fn remote_records_parse() {
        let e = parse_record("/srv/", "f 12 1700000000.5000000000 a b.txt").unwrap();
        assert_eq!(e.name, "/srv/a b.txt");
        assert_eq!(e.size, 12);
        let secs = e.modified.unwrap().duration_since(UNIX_EPOCH).unwrap();
        assert_eq!(secs.as_millis(), 1_700_000_000_500);
        assert!(parse_record("/", "f 12 1700000000").is_none());
    }

    #[test]
    fn name_regex_case_exclusions_hidden_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        for d in ["src/gen", "src/lib", ".cache", "a/b/c"] {
            fs::create_dir_all(t.join(d)).unwrap();
        }
        for f in [
            "Main.rs",
            "main.rs.bak",
            "src/gen/x.rs",
            "src/lib/y.rs",
            ".cache/z.rs",
            "a/b/c/deep.rs",
        ] {
            fs::write(t.join(f), "x").unwrap();
        }
        let q = |f: fn(&mut Query)| {
            let mut q = Query {
                exts: vec!["rs".into()],
                ..Query::default()
            };
            f(&mut q);
            run(t, &q)
        };
        assert_eq!(q(|_| {}).len(), 5);
        assert_eq!(
            q(|q| q.name_re = Some(Regex::new("^de").unwrap())),
            [t.join("a/b/c/deep.rs")]
        );
        assert_eq!(
            q(|q| (q.mask, q.mask_case) = ("main*".into(), true)),
            Vec::<PathBuf>::new()
        );
        assert_eq!(q(|q| q.mask = "main*".into()), [t.join("Main.rs")]);
        assert_eq!(q(|q| q.exclude_files = "x.*, Y.RS".into()).len(), 3);
        assert_eq!(q(|q| q.exclude = vec!["src/gen".into()]).len(), 4);
        assert_eq!(q(|q| q.exclude = vec!["gen".into(), "lib".into()]).len(), 3);
        assert!(!q(|q| q.skip_hidden = true).contains(&t.join(".cache/z.rs")));
        assert_eq!(q(|q| q.max_depth = Some(1)), [t.join("Main.rs")]);
    }

    #[test]
    fn gitignore_applies_in_repos_and_skips_dot_git() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        fs::create_dir_all(t.join(".git")).unwrap();
        fs::create_dir_all(t.join("target")).unwrap();
        fs::write(t.join(".gitignore"), "target/\n*.log\n").unwrap();
        for f in [".git/config", "target/out", "a.log", "keep.txt"] {
            fs::write(t.join(f), "x").unwrap();
        }
        let q = Query {
            gitignore: true,
            exclude_files: ".gitignore".into(),
            ..Query::default()
        };
        assert_eq!(run(t, &q), [t.join("keep.txt")]);
        assert_eq!(run(t, &Query::default()).len(), 7);
    }

    #[test]
    fn content_lines_negate_plain_and_words() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        let long = "y".repeat(100_000); // a line longer than one chunk counts once
        fs::write(t.join("a.txt"), format!("{long}\n foo bar \nfoobar\nfoo\n")).unwrap();
        fs::write(t.join("b.txt"), "a.b\n").unwrap();
        fs::write(t.join("bin"), b"x\0").unwrap();
        let first = hits(
            t,
            &Query {
                content: text("foo"),
                ..Query::default()
            },
        );
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].line, Some((2, "foo bar".into())));
        let every = Content {
            every: true,
            ..Content::new("foo", false, true, false).unwrap()
        };
        let lines: Vec<_> = hits(
            t,
            &Query {
                content: Some(every),
                ..Query::default()
            },
        )
        .into_iter()
        .filter_map(|h| h.line.map(|l| l.0))
        .collect();
        assert_eq!(lines, [2, 4]); // whole words: not "foobar"
        let not = Content {
            negate: true,
            ..Content::new("foo", false, false, false).unwrap()
        };
        let q = Query {
            content: Some(not),
            ..Query::default()
        };
        assert_eq!(run(t, &q), [t.join("b.txt")]); // binary stays out
        let plain = |p| Query {
            content: Some(Content::new("a.b", p, false, false).unwrap()),
            mask: "a.txt".into(),
            ..Query::default()
        };
        assert_eq!(run(t, &plain(false)).len(), 0); // no "a?b" in a.txt
        fs::write(t.join("a.txt"), "axb\n").unwrap();
        assert_eq!(
            (run(t, &plain(false)).len(), run(t, &plain(true)).len()),
            (1, 0)
        );
        // plain text: `*` is a wildcard within a line, other regex chars are literal
        fs::write(t.join("c.txt"), "load data_01.dat now\ndata_\n.dat\n").unwrap();
        let star = |text| Query {
            content: Some(Content {
                every: true,
                ..Content::new(text, true, false, false).unwrap()
            }),
            mask: "c.txt".into(),
            ..Query::default()
        };
        let lines = |q| {
            hits(t, &q)
                .into_iter()
                .map(|h| h.line.unwrap().0)
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(star("data_*.dat")), [1]);
        assert_eq!(lines(star("data_0?.dat")), Vec::<u64>::new());
        assert!(
            Content::new("Foo", false, false, true)
                .unwrap()
                .re
                .is_match("Foo")
        );
        assert!(
            !Content::new("foo", false, false, true)
                .unwrap()
                .re
                .is_match("FOO")
        );
    }

    #[cfg(unix)]
    #[test]
    fn follow_links_enters_linked_dirs_without_loops() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        fs::create_dir(t.join("real")).unwrap();
        fs::write(t.join("real/f.txt"), "x").unwrap();
        std::os::unix::fs::symlink(t.join("real"), t.join("link")).unwrap();
        std::os::unix::fs::symlink(t, t.join("real/loop")).unwrap();
        let q = |follow_links| Query {
            mask: "f.txt".into(),
            follow_links,
            ..Query::default()
        };
        assert_eq!(run(t, &q(false)), [t.join("real/f.txt")]);
        assert_eq!(
            run(t, &q(true)),
            [t.join("link/f.txt"), t.join("real/f.txt")]
        );
    }

    #[test]
    fn remote_command_new_options_and_age() {
        let mut c = Content::new("it's", true, true, false).unwrap();
        let q = Query {
            mask: "*.RS".into(),
            mask_case: true,
            exclude_files: "*.bak".into(),
            exclude: vec!["src/gen".into()],
            max_depth: Some(3),
            skip_hidden: true,
            follow_links: true,
            content: Some(c.clone()),
            ..Query::default()
        };
        let cmd = remote_command("/srv/", &q);
        assert!(
            cmd.starts_with("find -L '/srv/' -mindepth 1 -maxdepth 3 \\( -name '.*' -prune \\) -o")
        );
        assert!(cmd.contains("-path '/srv/src/gen'") && cmd.contains("-name '*.RS'"));
        assert!(cmd.contains("\\( -type d -o ! \\( -name '*.bak' \\) \\)"));
        assert!(cmd.contains(r"-type f -exec grep -qIiwF -e 'it'\''s' {} \;"));
        c.text = "data_*.dat".into();
        assert!(remote_grep(&c).contains(r"-qIiwE -e 'data_.*\.dat'"));
        c.negate = true;
        let q = Query {
            content: Some(c),
            ..Query::default()
        };
        assert!(remote_command("/", &q).contains(r"-exec grep -qI '' {} \; ! -exec grep"));
        assert!(q.remote_unsupported().is_none());
        let q = Query {
            gitignore: true,
            ..Query::default()
        };
        assert!(q.remote_unsupported().is_some());

        let now = Timestamp::now();
        let week = parse_age("1w").unwrap();
        assert!((now - SignedDuration::from_hours(169)..now).contains(&week));
        assert!(parse_age("7").unwrap() < parse_age("12h").unwrap());
        assert!(parse_age("-1d").is_err() && parse_age("x").is_err());
    }
}
