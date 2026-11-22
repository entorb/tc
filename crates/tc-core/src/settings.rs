//! Settings file: one `key=value` per line, keys may repeat (lists), unknown keys are ignored.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::hotlist::{self, to_bytes, to_path};
use crate::{Result, io_err};

/// `tc/settings` next to the hotlist file.
pub fn path() -> Option<PathBuf> {
    hotlist::default_path().map(|p| p.with_file_name("settings"))
}

/// Missing file means no settings; lines without `=` are skipped.
pub fn load(file: &Path) -> Result<Vec<(String, OsString)>> {
    Ok(hotlist::load(file)?
        .iter()
        .filter_map(|line| {
            let b = to_bytes(line);
            let eq = b.iter().position(|&c| c == b'=')?;
            let key = String::from_utf8_lossy(&b[..eq]).into_owned();
            Some((key, to_path(&b[eq + 1..]).into_os_string()))
        })
        .collect())
}

/// Same guarantees as `hotlist::save`; values containing a newline are dropped.
pub fn save(file: &Path, kv: &[(String, OsString)]) -> Result<()> {
    let lines: Vec<PathBuf> = kv
        .iter()
        .map(|(k, v)| {
            let mut b = k.as_bytes().to_vec();
            b.push(b'=');
            b.extend_from_slice(&to_bytes(Path::new(v)));
            to_path(&b)
        })
        .collect();
    hotlist::save(file, &lines)
}

/// Every file tc keeps in its config directory.
pub const FILES: [&str; 8] = [
    "hotlist",
    "connections",
    "searches",
    "history",
    "settings",
    "find-forms",
    "find-history",
    "rename-history",
];

/// Delete tc's [`FILES`] in `dir` (other files stay), then `dir` if it is empty; returns how many.
pub fn remove_all(dir: &Path) -> Result<usize> {
    let mut n = 0;
    for name in FILES {
        let file = dir.join(name);
        match std::fs::remove_file(&file) {
            Ok(()) => n += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&file)(e)),
        }
    }
    let _ = std::fs::remove_dir(dir); // fails when not empty: other files are not ours
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::{load, remove_all, save};

    #[test]
    fn roundtrip_repeated_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings");
        assert!(load(&file).unwrap().is_empty());
        let kv = vec![
            ("left".to_owned(), "/a=b".into()),
            ("left".to_owned(), "/c".into()),
            ("active".to_owned(), "1".into()),
        ];
        save(&file, &kv).unwrap();
        assert_eq!(load(&file).unwrap(), kv);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_value() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings");
        let v = super::to_path(b"/x/\xff").into_os_string();
        let kv = vec![("left".to_owned(), v)];
        save(&file, &kv).unwrap();
        assert_eq!(load(&file).unwrap(), kv);
    }

    #[test]
    fn remove_all_keeps_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tc");
        save(&dir.join("settings"), &[]).unwrap();
        save(&dir.join("hotlist"), &[]).unwrap();
        assert_eq!(remove_all(&dir).unwrap(), 2);
        assert!(!dir.exists());
        save(&dir.join("history"), &[]).unwrap();
        std::fs::write(dir.join("other"), "x").unwrap();
        assert_eq!(remove_all(&dir).unwrap(), 1);
        assert!(dir.join("other").exists());
    }
}
