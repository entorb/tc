//! Command line: line editor, persistent history and shell command construction.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, hotlist};

/// Oldest entries are dropped beyond this.
pub const MAX_HISTORY: usize = 500;

/// Single-line editor; `cur` is a byte offset on a char boundary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineEdit {
    pub text: String,
    pub cur: usize,
    /// History index being shown (`None`: the typed draft).
    browse: Option<usize>,
    draft: String,
}

impl From<String> for LineEdit {
    fn from(text: String) -> Self {
        Self {
            cur: text.len(),
            text,
            ..Self::default()
        }
    }
}

impl LineEdit {
    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cur, c);
        self.cur += c.len_utf8();
        self.browse = None;
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cur].chars().next_back() {
            self.cur -= c.len_utf8();
            self.text.remove(self.cur);
            self.browse = None;
        }
    }

    pub fn delete(&mut self) {
        if self.cur < self.text.len() {
            self.text.remove(self.cur);
            self.browse = None;
        }
    }

    pub fn left(&mut self) {
        if let Some(c) = self.text[..self.cur].chars().next_back() {
            self.cur -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cur..].chars().next() {
            self.cur += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cur = 0;
    }

    pub fn end(&mut self) {
        self.cur = self.text.len();
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Older history entry; the first call keeps the typed text as draft.
    pub fn prev(&mut self, hist: &[String]) {
        let i = match self.browse {
            None if hist.is_empty() => return,
            None => {
                self.draft = std::mem::take(&mut self.text);
                hist.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.show(hist, Some(i));
    }

    /// Newer history entry, then the draft again.
    pub fn next(&mut self, hist: &[String]) {
        match self.browse {
            None => {}
            Some(i) if i + 1 < hist.len() => self.show(hist, Some(i + 1)),
            Some(_) => self.show(hist, None),
        }
    }

    fn show(&mut self, hist: &[String], i: Option<usize>) {
        self.text = i.map_or_else(|| std::mem::take(&mut self.draft), |i| hist[i].clone());
        self.cur = self.text.len();
        self.browse = i;
    }
}

/// Add a command: empty ones and repeats of the last one are ignored.
pub fn push_history(hist: &mut Vec<String>, cmd: &str) {
    if cmd.is_empty() || hist.last().is_some_and(|l| l == cmd) {
        return;
    }
    hist.push(cmd.to_owned());
    let extra = hist.len().saturating_sub(MAX_HISTORY);
    hist.drain(..extra);
}

/// `tc/history` next to the hotlist file.
pub fn history_path() -> Option<PathBuf> {
    hotlist::default_path().map(|p| p.with_file_name("history"))
}

// shortcut: the hotlist helper stores lines as paths, commands with a newline cannot occur (single-line editor)
pub fn load_history(file: &Path) -> Result<Vec<String>> {
    let mut list: Vec<String> = hotlist::load(file)?
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let extra = list.len().saturating_sub(MAX_HISTORY);
    list.drain(..extra);
    Ok(list)
}

pub fn save_history(file: &Path, hist: &[String]) -> Result<()> {
    let list: Vec<PathBuf> = hist.iter().map(PathBuf::from).collect();
    hotlist::save(file, &list)
}

/// The platform shell running `cmd` in `dir`.
#[cfg(unix)]
pub fn shell_command(cmd: &str, dir: &Path) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd).current_dir(dir);
    c
}

#[cfg(windows)]
pub fn shell_command(cmd: &str, dir: &Path) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(cmd).current_dir(dir);
    c
}

/// Open `path` with the desktop's default application.
#[cfg(target_os = "macos")]
pub fn open_command(path: &Path) -> Command {
    let mut c = Command::new("open");
    c.arg(path);
    c
}

#[cfg(windows)]
pub fn open_command(path: &Path) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", "start", ""]).arg(path);
    c
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn open_command(path: &Path) -> Command {
    let mut c = Command::new("xdg-open");
    c.arg(path);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_with_multibyte() {
        let mut l = LineEdit::default();
        for c in "aäb".chars() {
            l.insert(c);
        }
        l.left();
        l.left();
        l.insert('x');
        assert_eq!(l.text, "axäb");
        l.delete();
        assert_eq!(l.text, "axb");
        l.backspace();
        assert_eq!(l.text, "ab");
        l.home();
        l.backspace();
        l.end();
        l.right();
        assert_eq!((l.text.as_str(), l.cur), ("ab", 2));
        l.clear();
        assert!(l.text.is_empty());
    }

    #[test]
    fn history_walk_keeps_draft() {
        let hist = vec!["one".to_owned(), "two".to_owned()];
        let mut l = LineEdit::default();
        l.insert('d');
        l.next(&hist);
        assert_eq!(l.text, "d");
        l.prev(&hist);
        assert_eq!(l.text, "two");
        l.prev(&hist);
        l.prev(&hist);
        assert_eq!(l.text, "one");
        l.next(&hist);
        l.next(&hist);
        assert_eq!(l.text, "d");
    }

    #[test]
    fn history_dedupe_bound_and_persist() {
        let mut h = Vec::new();
        for c in ["a", "a", "", "b", "a"] {
            push_history(&mut h, c);
        }
        assert_eq!(h, ["a", "b", "a"]);
        for i in 0..MAX_HISTORY + 5 {
            push_history(&mut h, &format!("c{i}"));
        }
        assert_eq!(h.len(), MAX_HISTORY);
        assert_eq!(h[0], "c5");
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("tc").join("history");
        assert!(load_history(&file).unwrap().is_empty());
        save_history(&file, &h).unwrap();
        assert_eq!(load_history(&file).unwrap(), h);
    }

    #[test]
    fn open_command_per_platform() {
        let c = open_command(Path::new("f.txt"));
        let prog = if cfg!(windows) {
            "cmd"
        } else if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(c.get_program(), prog);
        assert_eq!(LineEdit::from("äb".to_owned()).cur, 3);
    }

    #[test]
    fn shell_command_runs_in_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let c = shell_command("exit 3", tmp.path());
        assert_eq!(c.get_current_dir(), Some(tmp.path()));
        let (prog, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        assert_eq!(c.get_program(), prog);
        assert_eq!(c.get_args().next().unwrap(), flag);
        #[cfg(unix)]
        assert_eq!(
            { c }.status().unwrap().code(),
            Some(3),
            "status is passed through"
        );
    }
}
