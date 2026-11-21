//! Regex compile shared by viewer and editor.

use regex::{Regex, RegexBuilder};

use crate::{Error, Result};

/// Smart case: a pattern without uppercase chars matches case-insensitively.
// shortcut: escapes like \S count as uppercase, use (?i) there
pub fn compile(pattern: &str) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(!pattern.chars().any(char::is_uppercase))
        .build()
        // the last line of the regex message is the short reason
        .map_err(|e| Error::Pattern(e.to_string().lines().last().unwrap_or_default().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_case_and_errors() {
        assert!(compile("foo").unwrap().is_match("FOO"));
        assert!(!compile("Foo").unwrap().is_match("foo"));
        let Err(Error::Pattern(msg)) = compile("a(") else {
            panic!("expected pattern error");
        };
        assert!(!msg.contains('\n') && !msg.is_empty());
    }
}
