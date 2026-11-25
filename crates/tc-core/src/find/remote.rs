use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use super::{Content, MAX_LINE, MAX_RESULTS, Query};
use crate::panel::Entry;
use crate::vfs::Remote;

/// One `find` on the server for `q` under `root` (see [`Query::remote_unsupported`]; content
/// runs one `grep` per file). Output records: `type size mtime relative-path`, NUL separated,
/// like the Dir Size scan.
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
pub(super) fn parse_record(root: &str, text: &str) -> Option<Entry> {
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
        ignored: false,
    })
}

/// Single-quoted shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::find::parse_age;

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
