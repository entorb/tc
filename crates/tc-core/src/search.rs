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
        // iterative: on a mismatch only the last `*` takes one more char, so O(p * n), not exponential
        fn glob(p: &[char], n: &[char]) -> bool {
            let (mut pi, mut ni) = (0, 0);
            let mut star: Option<(usize, usize)> = None;
            while ni < n.len() {
                match p.get(pi) {
                    Some('*') => {
                        star = Some((pi, ni));
                        pi += 1;
                    }
                    Some(&c) if c == '?' || c == n[ni] => (pi, ni) = (pi + 1, ni + 1),
                    _ => match star {
                        Some((sp, sn)) => {
                            star = Some((sp, sn + 1));
                            (pi, ni) = (sp + 1, sn + 1);
                        }
                        None => return false,
                    },
                }
            }
            p[pi..].iter().all(|&c| c == '*')
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
        assert!(mask_match("*", ""));
        assert!(mask_match("a**", "a"));
        assert!(mask_match("*a*b", "xaybab"));
        assert!(mask_match("*[1080p]*", "film [1080p].mkv"));
        assert!(!mask_match("*a*b", "xaybba"));
        // many stars on a long name: exponential before
        assert!(!mask_match(
            &format!("{}*b", "*a".repeat(20)),
            &"a".repeat(200)
        ));
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
