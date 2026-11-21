//! Recursive file search by name mask and optional content regex.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;

use crate::panel::{Entry, entry_with};
use crate::search::mask_match;

/// Results kept at most; the search stops there.
pub const MAX_RESULTS: usize = 100_000;
/// Longest line chunk matched; longer lines are matched in pieces, so memory stays bounded.
const MAX_LINE: u64 = 64 * 1024;

/// Walk `root` (symlinked directories are not followed) and pass each match to `out`
/// (entry name = full path). Without `re` directories match by name too; with it only
/// text files whose content matches. Stops on `cancel` or after `MAX_RESULTS`.
pub fn find(
    root: &Path,
    mask: &str,
    re: Option<&Regex>,
    cancel: &AtomicBool,
    mut out: impl FnMut(Entry),
) {
    let (mut dirs, mut found) = (vec![root.to_owned()], 0);
    while let Some(dir) = dirs.pop() {
        // unreadable directories are skipped
        let Ok(items) = fs::read_dir(&dir) else {
            continue;
        };
        for item in items.flatten() {
            if cancel.load(Ordering::Relaxed) || found >= MAX_RESULTS {
                return;
            }
            let path = item.path();
            let is_dir = item.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                dirs.push(path.clone());
            }
            let hit = mask_match(mask, &item.file_name().to_string_lossy())
                && match re {
                    None => true,
                    Some(re) => !is_dir && file_matches(&path, re, cancel),
                };
            // Err: vanished since read_dir; metadata comes with the listing on Windows
            if let Some(e) = hit
                .then(|| item.metadata().ok())
                .flatten()
                .map(|m| entry_with(path.clone().into_os_string(), &path, m))
            {
                found += 1;
                out(e);
            }
        }
    }
}

/// Streams the file in bounded chunks; a NUL byte marks it binary and it never matches.
fn file_matches(path: &Path, re: &Regex, cancel: &AtomicBool) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut buf);
        if n.unwrap_or(0) == 0 || buf.contains(&0) || cancel.load(Ordering::Relaxed) {
            return false;
        }
        if re.is_match(&String::from_utf8_lossy(&buf)) {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::compile;
    use std::path::PathBuf;

    fn run(root: &Path, mask: &str, re: Option<&str>) -> Vec<PathBuf> {
        let re = re.map(|r| compile(r).unwrap());
        let mut v = Vec::new();
        find(root, mask, re.as_ref(), &AtomicBool::new(false), |e| {
            v.push(PathBuf::from(e.name))
        });
        v.sort();
        v
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
            run(tmp.path(), "*.txt", None),
            [a.clone(), b, sub.join("big.txt"), sub.join("bin.txt")]
        );
        assert_eq!(run(tmp.path(), "*", Some("needle")), [a.clone(), c]);
        assert_eq!(run(tmp.path(), "*.txt", Some("Needle")), [a]); // smart case
        assert_eq!(run(tmp.path(), "sub", None), [sub]);
    }
}
