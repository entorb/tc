//! Git-ignore marks for local listings.

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::Entry;

/// Mark `.git` dirs and, inside a repo, entries of `dir` ignored by git.
/// Best effort: unreadable or invalid ignore files count as empty.
pub(super) fn mark_ignored(dir: &Path, entries: &mut [Entry]) {
    for e in entries.iter_mut() {
        e.ignored = e.is_dir && e.name == ".git";
    }
    let Some(root) = dir.ancestors().find(|a| a.join(".git").exists()) else {
        return;
    };
    // deepest first: a nearer .gitignore overrides, then info/exclude, then core.excludesFile
    let mut matchers: Vec<Gitignore> = dir
        .ancestors()
        .take_while(|a| a.starts_with(root))
        .map(|a| Gitignore::new(a.join(".gitignore")).0)
        .collect();
    let mut exclude = GitignoreBuilder::new(root);
    exclude.add(root.join(".git/info/exclude"));
    matchers.push(exclude.build().unwrap_or_else(|_| Gitignore::empty()));
    matchers.push(GitignoreBuilder::new(root).build_global().0);
    for e in entries.iter_mut().filter(|e| !e.ignored && !e.is_parent()) {
        let path = dir.join(&e.name);
        e.ignored = matchers
            .iter()
            .map(|m| m.matched_path_or_any_parents(&path, e.is_dir))
            .find(|m| !m.is_none())
            .is_some_and(|m| m.is_ignore());
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicBool;

    use super::super::{SortKey, read_entries};

    /// Names marked ignored in a listing of `dir`.
    fn dimmed(dir: &std::path::Path) -> Vec<String> {
        let all = read_entries(dir, (SortKey::Name, false), &AtomicBool::new(false)).unwrap();
        all.iter()
            .filter(|e| e.ignored)
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn marks_gitignored_entries_and_dot_git() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        for d in [".git", "target/debug", "src", "sub"] {
            fs::create_dir_all(t.join(d)).unwrap();
        }
        fs::write(t.join(".gitignore"), "target/\n*.log\n!keep.log\n").unwrap();
        fs::write(t.join("sub/.gitignore"), "!x.log\n").unwrap();
        for f in ["a.log", "keep.log", "target/out", "sub/x.log", "sub/y.log"] {
            fs::write(t.join(f), "x").unwrap();
        }
        assert_eq!(dimmed(t), [".git", "target", "a.log"]);
        assert_eq!(dimmed(&t.join("target")), ["debug", "out"]);
        assert_eq!(dimmed(&t.join("sub")), ["y.log"]);
    }

    #[test]
    fn outside_a_repo_only_dot_git_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        fs::create_dir_all(t.join("x/.git")).unwrap();
        fs::write(t.join(".gitignore"), "*.log\n").unwrap();
        fs::write(t.join("a.log"), "x").unwrap();
        assert!(dimmed(t).is_empty());
        assert_eq!(dimmed(&t.join("x")), [".git"]);
    }
}
