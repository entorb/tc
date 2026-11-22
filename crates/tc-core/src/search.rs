//! Regex compile shared by viewer and editor.

use regex::{Regex, RegexBuilder};

use crate::{Error, Result};

/// Smart case: a pattern without uppercase chars matches case-insensitively.
// shortcut: escapes like \S count as uppercase, use (?i) there
pub fn compile(pattern: &str) -> Result<Regex> {
    compile_case(pattern, false)
}

/// Like `compile`, with `exact` the case always matters.
pub fn compile_case(pattern: &str, exact: bool) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(!exact && !pattern.chars().any(char::is_uppercase))
        .build()
        // the last line of the regex message is the short reason
        .map_err(|e| Error::Pattern(e.to_string().lines().last().unwrap_or_default().to_owned()))
}

/// Case-insensitive glob (`*`, `?`) against any of the comma or space separated masks; empty means `*`.
pub fn mask_match(masks: &str, name: &str) -> bool {
    Masks::new(masks).matches(name)
}

/// Name masks parsed once, for matching many names (find, select by mask).
pub struct Masks {
    masks: Vec<Vec<char>>,
    case: bool,
}

impl Masks {
    pub fn new(masks: &str) -> Self {
        Self::with_case(masks, false)
    }

    /// With `case` the masks match case-sensitively.
    pub fn with_case(masks: &str, case: bool) -> Self {
        let masks = if case {
            masks.to_owned()
        } else {
            masks.to_lowercase()
        };
        Masks {
            masks: masks
                .split([',', ' '])
                .filter(|m| !m.is_empty())
                .map(|m| m.chars().collect())
                .collect(),
            case,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.masks.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        fn glob(p: &[char], n: &[char]) -> bool {
            match p.split_first() {
                None => n.is_empty(),
                Some(('*', rest)) => (0..=n.len()).any(|i| glob(rest, &n[i..])),
                Some((c, rest)) => n
                    .first()
                    .is_some_and(|x| (*c == '?' || c == x) && glob(rest, &n[1..])),
            }
        }
        if self.masks.is_empty() {
            return true;
        }
        let name: Vec<char> = if self.case {
            name.chars().collect()
        } else {
            name.to_lowercase().chars().collect()
        };
        self.masks.iter().any(|m| glob(m, &name))
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, Masks, compile, mask_match};

    #[test]
    fn masks() {
        assert!(mask_match("", "a"));
        assert!(mask_match("*.RS, *.toml", "main.rs"));
        assert!(mask_match("*.rs *.toml", "Cargo.TOML"));
        assert!(mask_match("a?c*", "abc"));
        assert!(!mask_match("*.rs", "main.rs.bak"));
        assert!(!mask_match("a?c", "ac"));
        assert!(!Masks::with_case("*.RS", true).matches("main.rs"));
        assert!(Masks::with_case("*.RS", true).matches("main.RS"));
    }

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
