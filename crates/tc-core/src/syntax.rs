//! Line-local syntax highlighting: comments, strings, numbers and keywords per file type.
// shortcut: line-local, multi-line comments/strings and wrapped/h-scrolled row fragments lose state; upgrade with a per-line state cache if needed

use std::ops::Range;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keyword,
    Str,
    Number,
    Comment,
    Bold,
    Italic,
}

pub struct Lang {
    /// Markdown: `#` headings, `**bold**`, `*italic*`, ``` fences with `#` comments, no numbers.
    pub markdown: bool,
    /// Line comments only at the line start (after spaces), like `.properties`.
    comment_at_start: bool,
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str)>,
    quotes: &'static str,
    keywords: &'static [&'static str],
}

const C_LIKE: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "bool",
    "true",
    "false",
    "class",
    "namespace",
    "template",
    "typename",
    "public",
    "private",
    "protected",
    "virtual",
    "new",
    "delete",
    "this",
    "nullptr",
    "using",
    "try",
    "catch",
    "throw",
    "operator",
    "friend",
    "constexpr",
    "override",
    "final",
];

const RUST: Lang = Lang {
    line: &["//"],
    markdown: false,
    comment_at_start: false,
    block: Some(("/*", "*/")),
    quotes: "\"",
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok", "Err",
    ],
};

const C: Lang = Lang {
    line: &["//"],
    markdown: false,
    comment_at_start: false,
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: C_LIKE,
};

const JS: Lang = Lang {
    line: &["//"],
    markdown: false,
    comment_at_start: false,
    block: Some(("/*", "*/")),
    quotes: "\"'`",
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "yield",
        "enum",
        "implements",
        "private",
        "public",
        "readonly",
    ],
};

const JAVA: Lang = Lang {
    line: &["//"],
    markdown: false,
    comment_at_start: false,
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "abstract",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "float",
        "for",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "new",
        "null",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "throws",
        "true",
        "false",
        "try",
        "void",
        "while",
        "fun",
        "val",
        "var",
        "when",
        "object",
        "namespace",
        "using",
        "string",
        "bool",
    ],
};

const GO: Lang = Lang {
    line: &["//"],
    markdown: false,
    comment_at_start: false,
    block: Some(("/*", "*/")),
    quotes: "\"'`",
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
        "true",
        "false",
        "nil",
    ],
};

const PYTHON: Lang = Lang {
    line: &["#"],
    markdown: false,
    comment_at_start: false,
    block: None,
    quotes: "\"'",
    keywords: &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield", "self", "match", "case",
    ],
};

const SHELL: Lang = Lang {
    line: &["#"],
    markdown: false,
    comment_at_start: false,
    block: None,
    quotes: "\"'",
    keywords: &[
        "if", "then", "else", "elif", "fi", "case", "esac", "for", "while", "until", "do", "done",
        "in", "function", "return", "local", "export", "readonly", "set", "unset", "echo", "exit",
    ],
};

const RUBY: Lang = Lang {
    line: &["#"],
    markdown: false,
    comment_at_start: false,
    block: None,
    quotes: "\"'",
    keywords: &[
        "begin", "class", "def", "do", "else", "elsif", "end", "ensure", "false", "for", "if",
        "in", "module", "next", "nil", "require", "rescue", "return", "self", "true", "unless",
        "until", "when", "while", "yield",
    ],
};

const LUA: Lang = Lang {
    line: &["--"],
    markdown: false,
    comment_at_start: false,
    block: None,
    quotes: "\"'",
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ],
};

const PHP: Lang = Lang {
    line: &["//", "#"],
    markdown: false,
    comment_at_start: false,
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "abstract",
        "array",
        "as",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "echo",
        "else",
        "elseif",
        "extends",
        "false",
        "for",
        "foreach",
        "function",
        "if",
        "implements",
        "include",
        "namespace",
        "new",
        "null",
        "private",
        "protected",
        "public",
        "require",
        "return",
        "static",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "use",
        "while",
    ],
};

const SQL: Lang = Lang {
    line: &["--"],
    markdown: false,
    comment_at_start: false,
    block: Some(("/*", "*/")),
    quotes: "'\"",
    keywords: &[
        "select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set",
        "delete", "create", "table", "drop", "alter", "index", "join", "left", "right", "inner",
        "outer", "on", "group", "by", "order", "having", "limit", "as", "null", "is", "in",
        "primary", "key", "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "INSERT", "INTO",
        "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "DROP", "ALTER", "INDEX", "JOIN",
        "LEFT", "RIGHT", "INNER", "OUTER", "ON", "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "AS",
        "NULL", "IS", "IN", "PRIMARY", "KEY",
    ],
};

const CONF: Lang = Lang {
    line: &["#", ";"],
    markdown: false,
    comment_at_start: false,
    block: None,
    quotes: "\"'",
    keywords: &["true", "false", "yes", "no", "on", "off", "null"],
};

const PROPERTIES: Lang = Lang {
    line: &["#", "!"],
    markdown: false,
    comment_at_start: true,
    block: None,
    quotes: "",
    keywords: &["true", "false"],
};

const JSON: Lang = Lang {
    line: &[],
    markdown: false,
    comment_at_start: false,
    block: None,
    quotes: "\"",
    keywords: &["true", "false", "null"],
};

const MARKUP: Lang = Lang {
    line: &[],
    markdown: false,
    comment_at_start: false,
    block: Some(("<!--", "-->")),
    quotes: "\"'",
    keywords: &[],
};

const MARKDOWN: Lang = Lang {
    line: &[],
    markdown: true,
    comment_at_start: false,
    block: Some(("<!--", "-->")),
    quotes: "`",
    keywords: &[],
};

/// Language by file extension (case-insensitive), `None` for unknown or non-UTF-8.
pub fn lang(path: &Path) -> Option<&'static Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => &RUST,
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" => &C,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => &JS,
        "java" | "kt" | "kts" | "cs" => &JAVA,
        "go" => &GO,
        "py" | "pyw" => &PYTHON,
        "sh" | "bash" | "zsh" => &SHELL,
        "rb" => &RUBY,
        "lua" => &LUA,
        "php" => &PHP,
        "sql" => &SQL,
        "toml" | "ini" | "cfg" | "conf" | "yaml" | "yml" => &CONF,
        "properties" => &PROPERTIES,
        "json" => &JSON,
        "html" | "htm" | "xml" | "svg" => &MARKUP,
        "md" | "markdown" => &MARKDOWN,
        _ => return None,
    })
}

/// Markdown: `line` opens or closes a code fence.
pub fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// Markdown block a line belongs to, for [`md_blank_lines`].
#[derive(Clone, Copy, PartialEq)]
enum MdBlock {
    Blank,
    Heading,
    List,
    /// Lines of the n-th code fence, both fence lines included.
    Code(usize),
    Text,
}

fn md_list_item(line: &str) -> bool {
    let t = line.trim_start();
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    ["- ", "* ", "+ "].iter().any(|p| t.starts_with(p))
        || (digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")))
}

/// Lines (0-based) a blank line belongs before, so headings, lists and ``` blocks are
/// surrounded by blank lines. Insert-only: nothing inside a code block changes, no blank is doubled.
pub fn md_blank_lines(lines: &[&str]) -> Vec<usize> {
    let (mut fences, mut open) = (0, false);
    let mut prev = MdBlock::Blank;
    let mut out = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end();
        let block = if is_fence(line) {
            if !open {
                fences += 1;
            }
            open = !open;
            MdBlock::Code(fences)
        } else if open {
            MdBlock::Code(fences)
        } else if line.is_empty() {
            MdBlock::Blank
        } else if line.starts_with('#')
            && matches!(
                line.trim_start_matches('#').chars().next(),
                None | Some(' ')
            )
        {
            MdBlock::Heading
        } else if md_list_item(line) || (prev == MdBlock::List && line.starts_with(' ')) {
            MdBlock::List
        } else {
            MdBlock::Text
        };
        let apart = block != prev || block == MdBlock::Heading;
        let framed = [prev, block]
            .iter()
            .any(|b| !matches!(b, MdBlock::Text | MdBlock::Blank));
        if i > 0 && apart && framed && prev != MdBlock::Blank && block != MdBlock::Blank {
            out.push(i);
        }
        prev = block;
    }
    out
}

/// Highlighted byte ranges of `line`, in order. `fence` is the markdown code-fence
/// state, carried from row to row by the caller.
pub fn tokens(lang: &Lang, line: &str, fence: &mut bool) -> Vec<(Range<usize>, Kind)> {
    if lang.markdown
        && let Some(out) = markdown_line(line, fence)
    {
        return out;
    }
    let mut out = Vec::new();
    let mut it = line.char_indices().peekable();
    let mut prev_word = false;
    while let Some((i, c)) = it.next() {
        let rest = &line[i..];
        let at_start = !lang.comment_at_start || line[..i].trim().is_empty();
        if at_start && lang.line.iter().any(|p| rest.starts_with(p)) {
            out.push((i..line.len(), Kind::Comment));
            break;
        }
        if let Some((open, close)) = lang.block.filter(|(o, _)| rest.starts_with(o)) {
            let end = rest[open.len()..]
                .find(close)
                .map_or(line.len(), |e| i + open.len() + e + close.len());
            out.push((i..end, Kind::Comment));
            while it.next_if(|&(j, _)| j < end).is_some() {}
            prev_word = false;
            continue;
        }
        if lang.quotes.contains(c) {
            let end = quote_end(&mut it, c, line.len());
            out.push((i..end, Kind::Str));
            prev_word = false;
            continue;
        }
        if lang.markdown
            && (c == '*' || c == '_' && !prev_word)
            && let Some(end) = emphasis(rest)
        {
            let kind = if rest[1..].starts_with(c) {
                Kind::Bold
            } else {
                Kind::Italic
            };
            out.push((i..i + end, kind));
            while it.next_if(|&(j, _)| j < i + end).is_some() {}
            prev_word = false;
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let end = word_end(&mut it, i, c);
            if !prev_word {
                if c.is_ascii_digit() {
                    if !lang.markdown {
                        out.push((i..end, Kind::Number));
                    }
                } else if lang.keywords.contains(&&line[i..end]) {
                    out.push((i..end, Kind::Keyword));
                }
            }
            prev_word = true;
            continue;
        }
        prev_word = false;
    }
    out
}

/// Markdown line classified as a whole (fence, fenced text, heading); `None` means inline scan.
fn markdown_line(line: &str, fence: &mut bool) -> Option<Vec<(Range<usize>, Kind)>> {
    if is_fence(line) {
        *fence = !*fence;
        return Some(vec![(0..line.len(), Kind::Str)]);
    }
    if *fence {
        return Some(
            line.find('#')
                .map(|i| (i..line.len(), Kind::Comment))
                .into_iter()
                .collect(),
        );
    }
    line.trim_start()
        .starts_with('#')
        .then(|| vec![(0..line.len(), Kind::Keyword)])
}

/// Consume a string body after its opening quote `q`; returns the end byte (line end if unclosed).
fn quote_end(it: &mut impl Iterator<Item = (usize, char)>, q: char, len: usize) -> usize {
    let mut escaped = false;
    for (j, d) in it {
        if escaped {
            escaped = false;
        } else if d == '\\' {
            escaped = true;
        } else if d == q {
            return j + d.len_utf8();
        }
    }
    len
}

/// Consume the rest of a word/number starting with `c` at byte `i`; returns its end byte.
fn word_end(it: &mut std::iter::Peekable<std::str::CharIndices>, i: usize, c: char) -> usize {
    let mut end = i + c.len_utf8();
    while let Some((j, d)) =
        it.next_if(|&(_, d)| d.is_alphanumeric() || d == '_' || d == '.' && c.is_ascii_digit())
    {
        end = j + d.len_utf8();
    }
    end
}

/// Byte length of `**x**`/`*x*` (or `_`) at the start of `rest`, if closed on this line.
fn emphasis(rest: &str) -> Option<usize> {
    let c = rest.chars().next()?;
    let n = if rest[1..].starts_with(c) { 2 } else { 1 };
    let (mark, body) = rest.split_at(n);
    if body.starts_with(char::is_whitespace) {
        return None;
    }
    let e = body.find(mark).filter(|&e| e > 0)?;
    (!body[..e].ends_with(char::is_whitespace)).then_some(n + e + n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds<'a>(path: &str, line: &'a str) -> Vec<(&'a str, Kind)> {
        let lang = lang(Path::new(path)).unwrap();
        tokens(lang, line, &mut false)
            .into_iter()
            .map(|(r, k)| (&line[r], k))
            .collect()
    }

    #[test]
    fn rust_line() {
        assert_eq!(
            kinds("a.RS", r#"let x = "a\"//b" + 42; // done"#),
            [
                ("let", Kind::Keyword),
                (r#""a\"//b""#, Kind::Str),
                ("42", Kind::Number),
                ("// done", Kind::Comment),
            ]
        );
    }

    #[test]
    fn block_comment_numbers_and_words() {
        assert_eq!(
            kinds("a.c", "int /* x */ v2 = 1.5; /* open"),
            [
                ("int", Kind::Keyword),
                ("/* x */", Kind::Comment),
                ("1.5", Kind::Number),
                ("/* open", Kind::Comment),
            ]
        );
        assert_eq!(kinds("a.py", "'unclosed"), [("'unclosed", Kind::Str)]);
    }

    #[test]
    fn markdown_heading_and_code() {
        assert_eq!(kinds("a.md", "## Title"), [("## Title", Kind::Keyword)]);
        assert_eq!(kinds("a.md", "run `ls` 42"), [("`ls`", Kind::Str)]);
        assert_eq!(
            kinds("a.md", "**b** *i* _u_ a * b snake_case_x"),
            [
                ("**b**", Kind::Bold),
                ("*i*", Kind::Italic),
                ("_u_", Kind::Italic)
            ]
        );
    }

    #[test]
    fn markdown_blank_lines_around_blocks() {
        let md = [
            "# Title",         // 0
            "text",            // 1: blank before
            "- a",             // 2: blank before
            "  more",          // 3: list continuation
            "- b",             // 4
            "after",           // 5: blank before
            "```sh",           // 6: blank before
            "# not a heading", // 7: code, untouched
            "- not a list",    // 8
            "```",             // 9
            "```",             // 10: next fence, blank before
            "```",             // 11
            "## Next",         // 12: blank before
            "#hashtag",        // 13: not a heading, blank before (after heading)
            "",                // 14
            "1. one",          // 15
        ];
        assert_eq!(md_blank_lines(&md), [1, 2, 5, 6, 10, 12, 13]);
        assert!(md_blank_lines(&["# A", "", "b"]).is_empty());
    }

    #[test]
    fn properties_comments_only_at_line_start() {
        assert_eq!(
            kinds("a.properties", "  ! note"),
            [("! note", Kind::Comment)]
        );
        assert_eq!(kinds("a.properties", "# x"), [("# x", Kind::Comment)]);
        assert_eq!(
            kinds("a.properties", "msg=Hi! #1 on=true"),
            [("1", Kind::Number), ("true", Kind::Keyword)]
        );
    }

    #[test]
    fn markdown_fence_state() {
        let md = lang(Path::new("a.md")).unwrap();
        let mut fence = false;
        let rows = ["```sh", "ls # list", "# not a heading", "```", "# Heading"];
        let got: Vec<_> = rows.iter().map(|r| tokens(md, r, &mut fence)).collect();
        assert_eq!(got[0], [(0..5, Kind::Str)]);
        assert_eq!(got[1], [(3..9, Kind::Comment)]);
        assert_eq!(got[2], [(0..15, Kind::Comment)]);
        assert_eq!(got[3], [(0..3, Kind::Str)]);
        assert_eq!(got[4], [(0..9, Kind::Keyword)]);
        assert!(!fence);
    }

    #[test]
    fn unknown_or_non_utf8_has_no_lang() {
        assert!(lang(Path::new("a.unknown")).is_none());
        assert!(lang(Path::new("Makefile")).is_none());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::OsStr::from_bytes(b"a.\xffrs");
            assert!(lang(Path::new(name)).is_none());
        }
    }
}
