//! Multi-rename (Ctrl+M), modeled on Total Commander: new names from a name and an extension
//! mask, find/replace and a case style. Renames never overwrite: a name another batch item
//! still holds (swaps, cycles, case-only changes) goes through a temp name first.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use jiff::ZonedDateTime;
use regex::{NoExpand, Regex, RegexBuilder};

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
}

/// What the form describes.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name_mask: String,
    pub ext_mask: String,
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub case: Case,
    pub start: i64,
    pub step: i64,
    pub width: usize,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            name_mask: "[N]".into(),
            ext_mask: "[E]".into(),
            find: String::new(),
            replace: String::new(),
            regex: false,
            case_sensitive: false,
            case: Case::Keep,
            start: 1,
            step: 1,
            width: 1,
        }
    }
}

/// A [`Spec`] with its find pattern compiled once for the whole batch.
pub struct Rule {
    spec: Spec,
    find: Option<Regex>,
}

impl Spec {
    pub fn compile(self) -> std::result::Result<Rule, String> {
        let find = if self.find.is_empty() {
            None
        } else {
            let pattern = if self.regex {
                self.find.clone()
            } else {
                regex::escape(&self.find)
            };
            let re = RegexBuilder::new(&pattern)
                .case_insensitive(!self.case_sensitive)
                .build()
                // the last line of the regex message is the short reason
                .map_err(|e| {
                    format!("Find: {}", e.to_string().lines().last().unwrap_or_default())
                })?;
            Some(re)
        };
        Ok(Rule { spec: self, find })
    }
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
            new_stem = if self.spec.regex {
                re.replace_all(&new_stem, self.spec.replace.as_str())
            } else {
                re.replace_all(&new_stem, NoExpand(&self.spec.replace))
            }
            .into_owned();
        }
        match self.spec.case {
            Case::Keep => {}
            Case::Lower => (new_stem, new_ext) = (new_stem.to_lowercase(), new_ext.to_lowercase()),
            Case::Upper => (new_stem, new_ext) = (new_stem.to_uppercase(), new_ext.to_uppercase()),
            Case::First => new_stem = capitalize(&new_stem, false),
            Case::Words => new_stem = capitalize(&new_stem, true),
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

    use super::{Case, Progress, Side, Spec, apply, plan, range};
    use crate::vfs::mem::Mem;

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
            s.regex = true;
        });
        assert_eq!(name(&r, "pic 1-2.png", 0).unwrap(), "pic 2_1.png");
        let s = Spec {
            find: "(".into(),
            regex: true,
            ..Spec::default()
        };
        assert!(s.compile().is_err());
        let case = |c| name(&rule(|s| s.case = c), "my FILE-name.JPG", 0).unwrap();
        assert_eq!(case(Case::Lower), "my file-name.jpg");
        assert_eq!(case(Case::Upper), "MY FILE-NAME.JPG");
        assert_eq!(case(Case::First), "My file-name.JPG");
        assert_eq!(case(Case::Words), "My File-Name.JPG");
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
