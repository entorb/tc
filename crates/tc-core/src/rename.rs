//! Multi-rename (Ctrl+M), modeled on Total Commander: new names from a name and an extension
//! mask, find/replace and a case style. Renames never overwrite: a name another batch item
//! still holds (swaps, cycles, case-only changes) goes through a temp name first.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use jiff::ZonedDateTime;
use regex::{Regex, RegexBuilder};

use crate::ops::Progress;
use crate::vfs::Side;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Case {
    #[default]
    Keep,
    Lower,
    Upper,
    /// First letter of the name uppercase, the rest lowercase.
    First,
    /// First letter of every word uppercase, the rest lowercase.
    Words,
    /// Like `Words`, but small words (a, an, the, ...) stay lowercase and words with inner
    /// capitals (ABC, McX) are kept.
    WordsSmall,
}

/// How the find text is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Find {
    #[default]
    Plain,
    /// A regex, case-insensitive unless `(?-i)`; `$1` / `${1}` in Replace is a group.
    Regex,
}

/// What the form describes.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name_mask: String,
    pub ext_mask: String,
    pub find: String,
    pub replace: String,
    pub find_mode: Find,
    /// Placeholder stage, applied after the find: `%a` matches any text, `%n` digits only;
    /// `%a` in `ph_replace` is the group of the first `%a` in `ph_find`.
    pub ph_find: String,
    pub ph_replace: String,
    pub case_sensitive: bool,
    pub case: Case,
    pub start: i64,
    pub step: i64,
    pub width: usize,
    /// Clean-up toggles, applied to the name part after find / replace.
    pub underscore: bool,
    pub collapse: bool,
    pub space_caps: bool,
    pub dots: bool,
    /// Trim the ends and the spaces just inside brackets.
    pub trim: bool,
    /// Added to the first number of the name; 0 is off.
    pub add: i64,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            name_mask: "[N]".into(),
            ext_mask: "[E]".into(),
            find: String::new(),
            replace: String::new(),
            find_mode: Find::Plain,
            ph_find: String::new(),
            ph_replace: String::new(),
            case_sensitive: false,
            case: Case::Keep,
            start: 1,
            step: 1,
            width: 1,
            underscore: false,
            collapse: false,
            space_caps: false,
            dots: false,
            trim: false,
            add: 0,
        }
    }
}

/// A [`Spec`] with its find pattern compiled once for the whole batch.
pub struct Rule {
    spec: Spec,
    find: Option<Regex>,
    /// Replace text as a regex template (`$` escaped unless it is a regex).
    template: String,
    /// The placeholder stage: its pattern and template.
    ph: Option<(Regex, String)>,
    /// `Mr.`, `Dr.` ...: the dot goes.
    titles: Regex,
}

impl Spec {
    pub fn compile(self) -> std::result::Result<Rule, String> {
        let (pattern, template) = match self.find_mode {
            _ if self.find.is_empty() => (None, String::new()),
            Find::Plain => (
                Some(regex::escape(&self.find)),
                self.replace.replace('$', "$$"),
            ),
            Find::Regex => (Some(self.find.clone()), self.replace.clone()),
        };
        // only Plain text has the case-sensitive option; regexes use (?-i)
        let insensitive = !(self.find_mode == Find::Plain && self.case_sensitive);
        let find = pattern
            .map(|p| build(&p, insensitive, "Find"))
            .transpose()?;
        let ph = if self.ph_find.is_empty() {
            None
        } else {
            let (p, t) = placeholders(&self.ph_find, &self.ph_replace);
            Some((build(&p, true, "Search")?, t))
        };
        let titles = Regex::new(r"\b(Mr|Miss|Mrs|No|Nr|Dr)\.").map_err(|e| e.to_string())?;
        Ok(Rule {
            spec: self,
            find,
            template,
            ph,
            titles,
        })
    }
}

/// `pattern` compiled; the error names the field it came from.
fn build(pattern: &str, insensitive: bool, field: &str) -> std::result::Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(insensitive)
        .build()
        // the last line of the regex message is the short reason
        .map_err(|e| {
            format!(
                "{field}: {}",
                e.to_string().lines().last().unwrap_or_default()
            )
        })
}

/// Find pattern and replace template for `%a` placeholders: each `%x` in Find is a group
/// (`%n` digits, else any text); each `%x` in Replace is that group, unknown ones stay as typed;
/// `%%` is a literal `%`.
fn placeholders(find: &str, replace: &str) -> (String, String) {
    let mut pattern = String::new();
    let mut letters: Vec<char> = Vec::new();
    let mut rest = find;
    while let Some(i) = rest.find('%') {
        pattern += &regex::escape(&rest[..i]);
        let mut cs = rest[i + 1..].chars();
        match cs.next() {
            Some('%') => {
                pattern += "%";
                rest = cs.as_str();
            }
            Some(l) if l.is_ascii_alphanumeric() => {
                pattern += if l == 'n' { r"(\d+)" } else { "(.+?)" };
                letters.push(l);
                rest = cs.as_str();
            }
            _ => {
                pattern += "%";
                rest = &rest[i + 1..];
            }
        }
    }
    pattern += &regex::escape(rest);
    // a placeholder before a literal stops at its first occurrence; one at the end takes the rest
    if pattern.ends_with("(.+?)") {
        let n = pattern.len();
        pattern.replace_range(n - 5..n, "(.+)");
    }

    let mut template = String::new();
    let mut rest = replace;
    while let Some(i) = rest.find('%') {
        template += &rest[..i].replace('$', "$$");
        let mut cs = rest[i + 1..].chars();
        match cs.next() {
            Some('%') => {
                template += "%";
                rest = cs.as_str();
            }
            Some(l) if l.is_ascii_alphanumeric() => {
                match letters.iter().position(|&x| x == l) {
                    Some(g) => template += &format!("${{{}}}", g + 1),
                    None => template.extend(['%', l]),
                }
                rest = cs.as_str();
            }
            _ => {
                template += "%";
                rest = &rest[i + 1..];
            }
        }
    }
    template += &rest.replace('$', "$$");
    (pattern, template)
}

/// `photo.tar.gz` -> (`photo.tar`, `gz`); `.bashrc` and `README` have no extension.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

impl Rule {
    /// New name of the `index`-th item (0-based, counter order), or why there is none.
    pub fn new_name(
        &self,
        name: &std::ffi::OsStr,
        modified: Option<SystemTime>,
        index: usize,
    ) -> std::result::Result<String, String> {
        // never rename through a lossy conversion
        let name = name.to_str().ok_or("name is not valid UTF-8")?;
        let (stem, ext) = split_ext(name);
        let time = modified.and_then(|t| ZonedDateTime::try_from(t).ok());
        let ctx = Ctx {
            stem,
            ext,
            time: time.as_ref(),
            counter: self.spec.start + self.spec.step * index as i64,
            width: self.spec.width,
        };
        let mut new_stem = expand(&self.spec.name_mask, &ctx)?;
        let mut new_ext = expand(&self.spec.ext_mask, &ctx)?;
        if let Some(re) = &self.find {
            new_stem = re
                .replace_all(&new_stem, self.template.as_str())
                .into_owned();
        }
        if let Some((re, t)) = &self.ph {
            // once: a placeholder before a literal stops at its first occurrence
            new_stem = re.replacen(&new_stem, 1, t.as_str()).into_owned();
        }
        new_stem = clean(new_stem, &self.spec, &self.titles)?;
        if self.spec.trim {
            new_stem = trim_brackets(&new_stem);
        }
        match self.spec.case {
            Case::Keep => {}
            Case::Lower => (new_stem, new_ext) = (new_stem.to_lowercase(), new_ext.to_lowercase()),
            Case::Upper => (new_stem, new_ext) = (new_stem.to_uppercase(), new_ext.to_uppercase()),
            Case::First => new_stem = capitalize(&new_stem, false),
            Case::Words => new_stem = capitalize(&new_stem, true),
            Case::WordsSmall => new_stem = small_words(&new_stem),
        }
        let new = if new_ext.is_empty() {
            new_stem
        } else {
            format!("{new_stem}.{new_ext}")
        };
        check_name(&new)?;
        Ok(new)
    }
}

/// `new` as the explicit new name of `old` (from a list), or why it cannot be one.
pub fn explicit(old: &std::ffi::OsStr, new: &str) -> std::result::Result<String, String> {
    old.to_str().ok_or("name is not valid UTF-8")?;
    let new = new.trim_end_matches('\r');
    check_name(new)?;
    Ok(new.to_owned())
}

/// Uppercase the first letter (of every word with `words`), lowercase the rest.
fn capitalize(s: &str, words: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start && c.is_alphanumeric() {
            out.extend(c.to_uppercase());
            start = false;
        } else {
            out.extend(c.to_lowercase());
            if words && !c.is_alphanumeric() {
                start = true;
            }
        }
    }
    out
}

/// Clean-up toggles on the name part, in the order of the Perl reference tool.
fn clean(mut s: String, spec: &Spec, titles: &Regex) -> std::result::Result<String, String> {
    if spec.dots {
        s = titles
            .replace_all(&drop_initial_dots(&s), "$1")
            .into_owned();
        s = drop_double_dots(&s);
    }
    if spec.space_caps {
        s = space_before_caps(&s);
    }
    if spec.underscore {
        s = s.replace('_', " ");
    }
    if spec.collapse {
        s = collapse_spaces(&s);
    }
    if spec.add != 0 {
        s = add_to_first_number(&s, spec.add)?;
    }
    Ok(s)
}

/// `A.B.C.` -> `ABC`: drop the dot after a capital when a capital, `]`, `)` or the end follows.
fn drop_initial_dots(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < cs.len() {
        out.push(cs[i]);
        if cs[i].is_uppercase() && cs.get(i + 1) == Some(&'.') && dot_is_initial(&cs[i + 2..]) {
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn dot_is_initial(rest: &[char]) -> bool {
    match rest.first() {
        None => true,
        Some(c) => c.is_uppercase() || matches!(c, ']' | ')'),
    }
}

/// `...` -> `` : a dot next to another dot goes.
fn drop_double_dots(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    cs.iter()
        .enumerate()
        .filter(|&(i, &c)| c != '.' || !(i > 0 && cs[i - 1] == '.' || cs.get(i + 1) == Some(&'.')))
        .map(|(_, &c)| c)
        .collect()
}

/// `OneLongWord` -> `One Long Word`; no space before the first character.
fn space_before_caps(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c.is_uppercase()
            && prev.is_some_and(|p| !p.is_uppercase() && !p.is_whitespace() && p != '_')
        {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// `long    space` -> `long space`.
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !(c == ' ' && out.ends_with(' ')) {
            out.push(c);
        }
    }
    out
}

/// Add `add` to the first number in `s` (`Picture 1` +10 -> `Picture 11`).
fn add_to_first_number(s: &str, add: i64) -> std::result::Result<String, String> {
    let Some(start) = s.find(|c: char| c.is_ascii_digit()) else {
        return Ok(s.to_owned());
    };
    let len = s[start..]
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(s.len() - start);
    let num: i64 = s[start..start + len]
        .parse()
        .map_err(|_| "number too large".to_owned())?;
    let sum = num.checked_add(add).ok_or("number too large")?;
    // keep the zero padding: `007` + 1 -> `008`
    Ok(format!("{}{sum:0len$}{}", &s[..start], &s[start + len..]))
}

/// Trim the ends, and spaces just inside `( [ {` and `) ] }`.
fn trim_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.trim().chars() {
        if matches!(c, ')' | ']' | '}') {
            while out.ends_with(' ') {
                out.pop();
            }
        }
        if c == ' ' && matches!(out.chars().last(), Some('(' | '[' | '{')) {
            continue;
        }
        out.push(c);
    }
    out
}

const SMALL_WORDS: [&str; 13] = [
    "a", "an", "and", "at", "by", "for", "in", "of", "on", "or", "the", "to", "with",
];

/// Every word capitalized, except small words (not the first) and words that keep their own
/// capitals (`ABC`, `McX`).
fn small_words(s: &str) -> String {
    s.split(' ')
        .enumerate()
        .map(|(i, w)| {
            let lower = w.to_lowercase();
            if keeps_capitals(w) {
                w.to_owned()
            } else if i > 0 && SMALL_WORDS.contains(&lower.as_str()) {
                lower
            } else {
                capitalize(w, true)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn keeps_capitals(w: &str) -> bool {
    w.chars()
        .zip(w.chars().skip(1))
        .any(|(a, b)| a.is_uppercase() && b.is_uppercase())
        || w.strip_prefix("Mac")
            .or_else(|| w.strip_prefix("Mc"))
            .is_some_and(|t| t.starts_with(char::is_uppercase))
}

/// A single file name component the target file system accepts.
fn check_name(name: &str) -> std::result::Result<(), String> {
    let bad: &[char] = if cfg!(windows) {
        &['/', '\\', '<', '>', ':', '"', '|', '?', '*']
    } else {
        &['/']
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err("empty name".into());
    }
    if let Some(c) = name.chars().find(|c| bad.contains(c) || c.is_control()) {
        return Err(format!("invalid character {c:?}"));
    }
    Ok(())
}

struct Ctx<'a> {
    stem: &'a str,
    ext: &'a str,
    time: Option<&'a ZonedDateTime>,
    counter: i64,
    width: usize,
}

/// Replace `[..]` placeholders in `mask`; text outside brackets is kept as typed.
fn expand(mask: &str, ctx: &Ctx) -> std::result::Result<String, String> {
    let mut out = String::new();
    let mut rest = mask;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let j = after.find(']').ok_or("unclosed [ in mask")?;
        out += &placeholder(&after[..j], ctx)?;
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn placeholder(p: &str, ctx: &Ctx) -> std::result::Result<String, String> {
    let bad = || format!("unknown placeholder [{p}]");
    let date = |fmt: &str| {
        ctx.time
            .map(|t| t.strftime(fmt).to_string())
            .ok_or_else(|| "no modification time".to_owned())
    };
    match p {
        "[" => Ok("[".into()),
        "C" => Ok(format!("{:0w$}", ctx.counter, w = ctx.width)),
        "Y" => date("%Y"),
        "M" => date("%m"),
        "D" => date("%d"),
        "h" => date("%H"),
        "n" => date("%M"),
        "s" => date("%S"),
        _ if p.starts_with('N') => range(ctx.stem, &p[1..]).ok_or_else(bad),
        _ if p.starts_with('E') => range(ctx.ext, &p[1..]).ok_or_else(bad),
        _ => Err(bad()),
    }
}

/// Leading optional `-` and digits.
fn signed(s: &str) -> Option<(i64, &str)> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let len = s.len() - digits.len() + end;
    Some((s[..len].parse().ok()?, &s[len..]))
}

/// Characters of `s` a `[N..]` range picks, 1-based, negative counts from the end:
/// `` all, `2` one, `2-5` 2 to 5, `2-` 2 to end, `2--2` 2 to second last, `2,3` 3 from 2.
fn range(s: &str, spec: &str) -> Option<String> {
    if spec.is_empty() {
        return Some(s.into());
    }
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len() as i64;
    let pos = |v: i64| if v < 0 { n + v + 1 } else { v };
    let (x, rest) = signed(spec)?;
    let from = pos(x);
    let to = match rest {
        "" => from,
        "-" => n,
        _ if rest.starts_with(',') => match signed(&rest[1..])? {
            (len, "") if len >= 0 => from + len - 1,
            _ => return None,
        },
        _ if rest.starts_with('-') => match signed(&rest[1..])? {
            (y, "") => pos(y),
            _ => return None,
        },
        _ => return None,
    };
    let (from, to) = (from.max(1), to.min(n));
    if from > to {
        return Some(String::new());
    }
    // from >= 1 and to <= n, so both are valid char indexes
    Some(chars[from as usize - 1..to as usize].iter().collect())
}

/// Pairs that change, or why the batch cannot run: two items would get the same name
/// (an unchanged item keeps its name).
pub fn plan(old: &[String], new: &[String]) -> std::result::Result<Vec<(String, String)>, String> {
    let mut seen = HashSet::new();
    for n in new {
        if !seen.insert(n) {
            return Err(format!("{n}: two files would get this name"));
        }
    }
    Ok(old
        .iter()
        .zip(new)
        .filter(|(o, n)| o != n)
        .map(|(o, n)| (o.clone(), n.clone()))
        .collect())
}

/// Rename the `pairs` of an earlier [`apply`] back. Refused before anything moves when a new
/// name is gone; an old name taken by another file is refused by [`apply`].
pub fn undo(side: &Side, dir: &Path, pairs: &[(String, String)], p: &Progress) -> Result<()> {
    let back: Vec<_> = pairs.iter().map(|(o, n)| (n.clone(), o.clone())).collect();
    for (n, _) in &back {
        if !side.exists(&dir.join(n))? {
            return Err(Error::Remote(format!("{n}: gone, cannot undo")));
        }
    }
    apply(side, dir, &back, p)
}

/// Rename `pairs` (names inside `dir`) on `side`. Targets that exist and are not held by the
/// batch are refused before anything is renamed. On error or cancel, items parked under
/// temp names are moved back; one that cannot be is named in the error.
// shortcut: exists-then-rename can race with another process creating the target, upgrade to
// renameat2(RENAME_NOREPLACE) / remote exclusive rename if that matters
pub fn apply(side: &Side, dir: &Path, pairs: &[(String, String)], p: &Progress) -> Result<()> {
    // case-insensitive, so a case-only change or a swap on such a file system goes via temp
    let held: HashSet<String> = pairs.iter().map(|(o, _)| o.to_lowercase()).collect();
    let via_temp = |n: &String| held.contains(&n.to_lowercase());
    for (_, n) in pairs.iter().filter(|(_, n)| !via_temp(n)) {
        if side.exists(&dir.join(n))? {
            return Err(Error::Exists(dir.join(n)));
        }
    }
    p.files_total.store(pairs.len() as u64, Ordering::Relaxed);
    p.scanned.store(true, Ordering::Relaxed);
    let mut parked: Vec<(std::path::PathBuf, &String, &String)> = Vec::new();
    let result = (|| {
        for (o, n) in pairs {
            if p.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let from = dir.join(o);
            if via_temp(n) {
                let tmp = side.free_temp(&from)?;
                side.rename(&from, &tmp)?;
                parked.push((tmp, o, n));
            } else {
                rename_new(side, &from, &dir.join(n))?;
                p.files.fetch_add(1, Ordering::Relaxed);
            }
        }
        while let Some((tmp, o, n)) = parked.pop() {
            if let Err(e) = rename_new(side, &tmp, &dir.join(n)) {
                parked.push((tmp, o, n));
                return Err(e);
            }
            p.files.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    })();
    let Err(e) = result else {
        return Ok(());
    };
    let stuck: Vec<String> = parked
        .into_iter()
        .filter(|(tmp, o, _)| rename_new(side, tmp, &dir.join(o)).is_err())
        .map(|(tmp, o, _)| format!("{} (was {o})", tmp.display()))
        .collect();
    if stuck.is_empty() {
        return Err(e);
    }
    Err(Error::Remote(format!(
        "{e}; left under temp names: {}",
        stuck.join(", ")
    )))
}

/// Rename to `to` only if nothing is there.
fn rename_new(side: &Side, from: &Path, to: &Path) -> Result<()> {
    if side.exists(to)? {
        return Err(Error::Exists(to.to_owned()));
    }
    side.rename(from, to)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::time::{Duration, UNIX_EPOCH};

    use super::{Case, Find, Progress, Side, Spec, apply, plan, range, undo};
    use crate::vfs::mem::Mem;

    #[test]
    fn explicit_checks_names() {
        assert!(super::explicit(OsStr::new("a"), "a/b").is_err());
        assert!(super::explicit(OsStr::new("a"), "").is_err());
        assert_eq!(super::explicit(OsStr::new("a"), "b\r"), Ok("b".into()));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert!(super::explicit(OsStr::from_bytes(b"a\xff"), "b").is_err());
        }
    }

    fn rule(f: impl FnOnce(&mut Spec)) -> super::Rule {
        let mut s = Spec::default();
        f(&mut s);
        s.compile().unwrap()
    }

    fn name(r: &super::Rule, n: &str, i: usize) -> Result<String, String> {
        r.new_name(
            OsStr::new(n),
            Some(UNIX_EPOCH + Duration::from_secs(86_400 * 400)),
            i,
        )
    }

    #[test]
    fn ranges() {
        let r = |spec| range("abcdefgh", spec).unwrap();
        assert_eq!(r(""), "abcdefgh");
        assert_eq!(r("2"), "b");
        assert_eq!(r("2-4"), "bcd");
        assert_eq!(r("6-"), "fgh");
        assert_eq!(r("-3-"), "fgh");
        assert_eq!(r("2--2"), "bcdefg");
        assert_eq!(r("2,3"), "bcd");
        assert_eq!(r("-8,2"), "ab");
        assert_eq!(r("7-20"), "gh");
        assert_eq!(r("20"), "");
        assert_eq!(range("äöü", "2").unwrap(), "ö");
        assert!(range("abc", "x").is_none());
        assert!(range("abc", "1,").is_none());
    }

    #[test]
    fn masks_counter_dates() {
        let r = rule(|s| {
            s.name_mask = "img_[C]_[N1-3]".into();
            s.start = 9;
            s.step = 2;
            s.width = 3;
        });
        assert_eq!(name(&r, "holiday.JPG", 0).unwrap(), "img_009_hol.JPG");
        assert_eq!(name(&r, "holiday.JPG", 1).unwrap(), "img_011_hol.JPG");
        let r = rule(|s| s.name_mask = "[Y]".into());
        // 400 days after the epoch is in 1971 in every time zone
        assert_eq!(name(&r, "a.txt", 0).unwrap(), "1971.txt");
        let r = rule(|s| s.ext_mask = String::new());
        assert_eq!(name(&r, "a.tar.gz", 0).unwrap(), "a.tar");
        assert_eq!(name(&r, ".bashrc", 0).unwrap(), ".bashrc");
        let r = rule(|s| s.name_mask = "[[]x]".into());
        assert_eq!(name(&r, "a", 0).unwrap(), "[x]");
        for bad in ["[Q]", "[N", "[Nx]"] {
            assert!(name(&rule(|s| s.name_mask = bad.into()), "a", 0).is_err());
        }
        assert!(name(&rule(|s| s.name_mask = "a/b".into()), "a", 0).is_err());
        assert!(name(&rule(|s| s.name_mask = String::new()), "a", 0).is_err());
    }

    #[test]
    fn find_replace_and_case() {
        let r = rule(|s| {
            s.find = "A.".into();
            s.replace = "$x".into();
        });
        // plain text: `.` is literal, case ignored, `$` kept, extension untouched
        assert_eq!(name(&r, "xa.ya.A.a.b", 0).unwrap(), "x$xy$x$xa.b");
        let r = rule(|s| {
            s.find = r"(\d+)-(\d+)".into();
            s.replace = "${2}_$1".into();
            s.find_mode = Find::Regex;
        });
        assert_eq!(name(&r, "pic 1-2.png", 0).unwrap(), "pic 2_1.png");
        let s = Spec {
            find: "(".into(),
            find_mode: Find::Regex,
            ..Spec::default()
        };
        assert!(s.compile().is_err());
        let case = |c| name(&rule(|s| s.case = c), "my FILE-name.JPG", 0).unwrap();
        assert_eq!(case(Case::Lower), "my file-name.jpg");
        assert_eq!(case(Case::Upper), "MY FILE-NAME.JPG");
        assert_eq!(case(Case::First), "My file-name.JPG");
        assert_eq!(case(Case::Words), "My File-Name.JPG");
        assert_eq!(case(Case::WordsSmall), "My FILE-name.JPG");
    }

    #[test]
    fn placeholder_find_swaps_groups() {
        let r = rule(|s| {
            s.ph_find = "%a - %b".into();
            s.ph_replace = "%b - %a".into();
        });
        assert_eq!(name(&r, "x - y.txt", 0).unwrap(), "y - x.txt");
        let r = rule(|s| {
            s.ph_find = "%a - %n - %t".into();
            s.ph_replace = "[%a] - %n - %t".into();
        });
        assert_eq!(name(&r, "Trip - 12 - Day", 0).unwrap(), "[Trip] - 12 - Day");
        // `$` and `.` are literal in placeholder mode
        let r = rule(|s| {
            s.ph_find = "%a.".into();
            s.ph_replace = "$%a".into();
        });
        assert_eq!(name(&r, "x..y", 0).unwrap(), "$x.y");
    }

    #[test]
    fn placeholder_groups_match_the_first_occurrence() {
        let ph = |find: &str, replace: &str| {
            rule(|s| {
                s.ph_find = find.into();
                s.ph_replace = replace.into();
            })
        };
        // before a literal: the first occurrence; at the end: the rest of the name
        assert_eq!(name(&ph("%a_", "%a-"), "chk_rs", 0).unwrap(), "chk-rs");
        assert_eq!(name(&ph("%a_%b", "%b-%a"), "x_y_z", 0).unwrap(), "y_z-x");
        assert_eq!(
            name(&ph("%a_%b", "%b - %a"), "tom_jerry", 0).unwrap(),
            "jerry - tom"
        );
        assert_eq!(
            name(&ph("%a_%b_%c_%d", "%d-%c-%b-%a"), "1_2_3_4_5", 0).unwrap(),
            "4_5-3-2-1"
        );
    }

    #[test]
    fn clean_up_toggles() {
        let n = |r: &super::Rule, s| name(r, s, 0).unwrap();
        let r = rule(|s| s.underscore = true);
        assert_eq!(n(&r, "two_words.txt"), "two words.txt");
        let r = rule(|s| s.collapse = true);
        assert_eq!(n(&r, "long    space.txt"), "long space.txt");
        let r = rule(|s| s.space_caps = true);
        assert_eq!(n(&r, "OneLongWord.txt"), "One Long Word.txt");
        assert_eq!(n(&r, "HTMLParser.txt"), "HTMLParser.txt");
        let r = rule(|s| s.dots = true);
        assert_eq!(n(&r, "A.B.C..txt"), "ABC.txt");
        assert_eq!(n(&r, "Mr.Smith...txt"), "MrSmith.txt");
        assert_eq!(n(&r, "a.b.c.txt"), "a.b.c.txt");
        let r = rule(|s| s.add = 10);
        assert_eq!(n(&r, "Picture 1.jpg"), "Picture 11.jpg");
        assert_eq!(n(&r, "no digits.jpg"), "no digits.jpg");
        assert_eq!(n(&rule(|s| s.add = 1), "IMG_007.jpg"), "IMG_008.jpg");
        assert!(name(&rule(|s| s.add = i64::MAX), "a1", 0).is_err());
        // trims ends and spaces inside brackets only when asked
        assert_eq!(n(&rule(|_| {}), "( a ) .txt"), "( a ) .txt");
        assert_eq!(n(&rule(|s| s.trim = true), "( a ) .txt"), "(a).txt");
        let r = rule(|s| (s.underscore, s.space_caps) = (true, true));
        assert_eq!(n(&r, "my_File.txt"), "my File.txt");
        let r = rule(|s| s.dots = true);
        assert_eq!(n(&r, "Section A.intro.v2.txt"), "Section A.intro.v2.txt");
        let r = rule(|s| s.case = Case::WordsSmall);
        assert_eq!(n(&r, "the end of a dream.txt"), "The End of a Dream.txt");
        assert_eq!(n(&r, "ABC road.txt"), "ABC Road.txt");
    }

    #[test]
    fn placeholder_examples_from_the_form() {
        let ph = |find: &str, replace: &str| {
            rule(|s| {
                s.ph_find = find.into();
                s.ph_replace = replace.into();
            })
        };
        assert_eq!(
            name(&ph("%a_%b", "%b - %a"), "tom_jerry", 0).unwrap(),
            "jerry - tom"
        );
        assert_eq!(
            name(&ph("IMG_%n", "Photo %n"), "IMG_12", 0).unwrap(),
            "Photo 12"
        );
        assert_eq!(
            name(&ph("%a_%b_%c_%d", "%d-%c-%b-%a"), "one_two_three_four", 0).unwrap(),
            "four-three-two-one"
        );
        // %n takes digits only
        assert_eq!(name(&ph("IMG_%n", "x"), "IMG_ab", 0).unwrap(), "IMG_ab");
        // once only; unknown letters and %% stay literal
        assert_eq!(
            name(&ph("%a_", "%a-"), "chk_rs_asd", 0).unwrap(),
            "chk-rs_asd"
        );
        assert_eq!(name(&ph("%a", "%a 50%off"), "x", 0).unwrap(), "x 50%off");
        assert_eq!(name(&ph("%%a%n", "%n%%a"), "%a1", 0).unwrap(), "1%a");
    }

    #[test]
    fn case_sensitive_only_for_plain_text() {
        let plain = |mode| {
            rule(|s| {
                s.find = "ab".into();
                s.replace = "X".into();
                s.find_mode = mode;
                s.case_sensitive = true;
            })
        };
        assert_eq!(
            name(&plain(Find::Plain), "AB ab.txt", 0).unwrap(),
            "AB X.txt"
        );
        assert_eq!(
            name(&plain(Find::Regex), "AB ab.txt", 0).unwrap(),
            "X X.txt"
        );
        assert_eq!(name(&plain(Find::Regex), "AB.txt", 0).unwrap(), "X.txt");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_name_refused() {
        use std::os::unix::ffi::OsStrExt;
        let r = rule(|_| {});
        assert!(r.new_name(OsStr::from_bytes(b"a\xff"), None, 0).is_err());
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn plan_drops_unchanged_and_refuses_duplicates() {
        let pairs = plan(&s(&["a", "b"]), &s(&["a", "c"])).unwrap();
        assert_eq!(pairs, vec![("b".into(), "c".into())]);
        assert!(plan(&s(&["a", "b"]), &s(&["x", "x"])).is_err());
        // b would take the name a keeps
        assert!(plan(&s(&["a", "b"]), &s(&["a", "a"])).is_err());
    }

    #[test]
    fn apply_swaps_chains_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        for (f, c) in [("a", "A"), ("b", "B"), ("c", "C"), ("keep", "K")] {
            fs::write(d.join(f), c).unwrap();
        }
        let p = Progress::default();
        // swap a <-> b, chain c -> d
        let pairs = plan(&s(&["a", "b", "c"]), &s(&["b", "a", "d"])).unwrap();
        apply(&Side::Local, d, &pairs, &p).unwrap();
        assert_eq!(fs::read_to_string(d.join("a")).unwrap(), "B");
        assert_eq!(fs::read_to_string(d.join("b")).unwrap(), "A");
        assert_eq!(fs::read_to_string(d.join("d")).unwrap(), "C");
        // a target outside the batch is refused before anything moves
        let pairs = plan(&s(&["a", "b"]), &s(&["x", "keep"])).unwrap();
        assert!(apply(&Side::Local, d, &pairs, &p).is_err());
        assert_eq!(fs::read_to_string(d.join("a")).unwrap(), "B");
        assert_eq!(fs::read_to_string(d.join("keep")).unwrap(), "K");
        let mut names: Vec<_> = fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["a", "b", "d", "keep"]);
    }

    #[test]
    fn undo_reverses_and_refuses_when_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let (p, pairs) = (
            Progress::default(),
            plan(&s(&["a", "b"]), &s(&["x", "y"])).unwrap(),
        );
        let read = |f: &str| fs::read_to_string(d.join(f)).ok();
        fs::write(d.join("a"), "A").unwrap();
        fs::write(d.join("b"), "B").unwrap();
        apply(&Side::Local, d, &pairs, &p).unwrap();
        undo(&Side::Local, d, &pairs, &p).unwrap();
        assert_eq!((read("a"), read("b")), (Some("A".into()), Some("B".into())));
        // a new name is gone: nothing moves
        apply(&Side::Local, d, &pairs, &p).unwrap();
        fs::remove_file(d.join("x")).unwrap();
        assert!(undo(&Side::Local, d, &pairs, &p).is_err());
        assert_eq!((read("y"), read("b")), (Some("B".into()), None));
        // an old name is taken: nothing moves
        fs::write(d.join("x"), "X").unwrap();
        fs::write(d.join("a"), "other").unwrap();
        assert!(undo(&Side::Local, d, &pairs, &p).is_err());
        assert_eq!((read("x"), read("y")), (Some("X".into()), Some("B".into())));
    }

    #[test]
    fn apply_remote_failure_names_parked_file() {
        let m = Mem::new();
        m.put("/a", b"A");
        m.put("/b", b"B");
        m.fail_renames_to("/b");
        let side = Side::Remote(m.clone());
        let pairs = plan(&s(&["a", "b"]), &s(&["b", "a"])).unwrap();
        let err = apply(&side, "/".as_ref(), &pairs, &Progress::default()).unwrap_err();
        // b reached a; a's content cannot go to b nor back (a is taken), so its temp is named
        assert_eq!(m.get("/a").as_deref(), Some(&b"B"[..]));
        assert!(err.to_string().contains("(was a)"), "{err}");
        assert_eq!(m.paths().len(), 3, "root, a, temp");
    }
}
