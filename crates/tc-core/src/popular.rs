//! Popular directories: visit count and last visit per directory, ranked by frecency.
//! File `popular`, lines `count<TAB>unix_secs<TAB>path`; bad lines are skipped.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::hotlist::{self, to_bytes, to_path};

/// Entries kept; the lowest scores are dropped first.
pub const MAX: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popular {
    pub count: u32,
    /// Unix seconds of the last visit.
    pub last: u64,
    pub path: PathBuf,
}

impl Popular {
    /// Visit count weighted by recency (like zoxide).
    pub fn score(&self, now: u64) -> f64 {
        let w = match now.saturating_sub(self.last) {
            0..3600 => 4.0,
            3600..86_400 => 2.0,
            86_400..604_800 => 0.5,
            _ => 0.25,
        };
        f64::from(self.count) * w
    }
}

/// Missing file means empty list.
pub fn load(file: &Path) -> Result<Vec<Popular>> {
    Ok(hotlist::load(file)?
        .iter()
        .filter_map(|l| parse(&to_bytes(l)))
        .collect())
}

fn parse(line: &[u8]) -> Option<Popular> {
    let mut f = line.splitn(3, |&b| b == b'\t');
    let num = |b: &[u8]| std::str::from_utf8(b).ok()?.parse().ok();
    let (count, last) = (num(f.next()?)?, num(f.next()?)?);
    let path = f.next().filter(|p| !p.is_empty())?;
    Some(Popular {
        count: u32::try_from(count).ok()?,
        last,
        path: to_path(path),
    })
}

/// Like [`hotlist::save`] (atomic, private).
pub fn save(file: &Path, list: &[Popular]) -> Result<()> {
    let lines: Vec<PathBuf> = list
        .iter()
        .map(|p| {
            let mut line = format!("{}\t{}\t", p.count, p.last).into_bytes();
            line.extend_from_slice(&to_bytes(&p.path));
            to_path(&line)
        })
        .collect();
    hotlist::save(file, &lines)
}

/// Count a visit of `path` at `now`; past [`MAX`] the lowest score goes.
pub fn record(list: &mut Vec<Popular>, path: &Path, now: u64) {
    match list.iter_mut().find(|p| p.path == path) {
        Some(p) => (p.count, p.last) = (p.count.saturating_add(1), now),
        None => list.push(Popular {
            count: 1,
            last: now,
            path: path.to_owned(),
        }),
    }
    if list.len() > MAX
        && let Some(i) = list
            .iter()
            .enumerate()
            .filter(|(_, p)| p.path != path)
            .min_by(|a, b| a.1.score(now).total_cmp(&b.1.score(now)))
            .map(|(i, _)| i)
    {
        list.remove(i);
    }
}

/// Best score first; equal scores keep the newer visit first.
pub fn rank(list: &mut [Popular], now: u64) {
    list.sort_by(|a, b| (b.score(now).total_cmp(&a.score(now))).then_with(|| b.last.cmp(&a.last)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(count: u32, last: u64, path: &str) -> Popular {
        Popular {
            count,
            last,
            path: path.into(),
        }
    }

    #[test]
    fn record_counts_ranks_and_caps() {
        let mut l = Vec::new();
        record(&mut l, Path::new("/a"), 10);
        record(&mut l, Path::new("/a"), 20);
        record(&mut l, Path::new("/b"), 30);
        assert_eq!(l, [p(2, 20, "/a"), p(1, 30, "/b")]);
        let now = 100_000;
        l.push(p(1, now, "/new")); // 4 beats the old /a (2 * 0.25)
        rank(&mut l, now);
        assert_eq!(l[0].path, Path::new("/new"));
        for i in 0..MAX {
            record(&mut l, Path::new(&format!("/d{i}")), now);
        }
        assert_eq!(l.len(), MAX);
        assert!(l.iter().any(|e| e.path == Path::new("/d499")));
        assert!(!l.iter().any(|e| e.path == Path::new("/b")));
    }

    #[test]
    fn save_load_skips_bad_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("cfg").join("popular");
        assert!(load(&file).unwrap().is_empty());
        let l = vec![p(3, 7, "/x"), p(1, 2, "/with\ttab")];
        save(&file, &l).unwrap();
        assert_eq!(load(&file).unwrap(), l);
        std::fs::write(&file, "x\ty\tz\n1\t2\n1\t2\t\n-1\t2\t/n\n5\t6\t/ok\n").unwrap();
        assert_eq!(load(&file).unwrap(), [p(5, 6, "/ok")]);
    }
}
