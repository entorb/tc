//! Squarified treemap layout of a directory tree (Bruls, Huizing, van Wijk).

use std::ffi::OsStr;

use super::Node;

/// File type of a tile, picks its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    Image,
    Video,
    Audio,
    Archive,
    Document,
    Code,
    Exec,
    Other,
}

/// One rectangle of the treemap, in caller units (cells or pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Index in the laid-out dir's `children` of the depth-0 ancestor (= table identity).
    pub top: usize,
    /// 0 = child of the laid-out dir.
    pub depth: u8,
    /// Index of the enclosing tile in the layout, `None` at depth 0.
    pub parent: Option<usize>,
    /// Index in the enclosing node's `children`.
    pub child: usize,
    pub kind: Kind,
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct Params {
    max_depth: u8,
    min_side: f64,
    inset: f64,
    /// Dirs only, weighted by file count instead of bytes.
    by_files: bool,
}

impl Params {
    fn weight(&self, n: &Node) -> u64 {
        if self.by_files { n.files } else { n.size }
    }
}

/// Squarified treemap of `dir`'s children in `w`×`h`; dirs nest up to `max_depth`
/// (0 = no nesting), each nested level inset by `inset`. Tiles with a side < `min_side`
/// are dropped. Parents come before their children (paint order). `by_files`: only dirs,
/// sized by their file count; a dir's loose files leave its area uncovered.
pub fn layout(
    dir: &Node,
    w: f32,
    h: f32,
    max_depth: u8,
    min_side: f32,
    inset: f32,
    by_files: bool,
) -> Vec<Tile> {
    let mut out = Vec::new();
    // also rejects NaN
    if w.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
        || h.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
    {
        return out;
    }
    let p = Params {
        max_depth,
        min_side: f64::from(min_side.max(0.0)),
        inset: f64::from(inset.max(0.0)),
        by_files,
    };
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: f64::from(w),
        h: f64::from(h),
    };
    place(dir, rect, 0, None, None, &p, &mut out);
    out
}

/// Worst aspect ratio of a row of `sum` bytes (`min`..`max` per item) laid along side `s`.
fn worst(sum: f64, min: f64, max: f64, s: f64, scale: f64) -> f64 {
    let area = sum * scale;
    let (s2, a2) = (s * s, area * area);
    (s2 * max * scale / a2).max(a2 / (s2 * min * scale))
}

/// Number of leading `sizes` (descending) that form the next row of a strip along side `s`.
fn row_len(sizes: &[f64], s: f64, scale: f64) -> usize {
    let (mut sum, max) = (sizes[0], sizes[0]);
    let mut cur = worst(sum, max, max, s, scale);
    let mut n = 1;
    // sizes descend, so the candidate is the row's smallest item
    while let Some(&next) = sizes.get(n) {
        let w = worst(sum + next, next, max, s, scale);
        if w > cur {
            break;
        }
        (sum, cur) = (sum + next, w);
        n += 1;
    }
    n
}

fn place(
    n: &Node,
    rect: Rect,
    depth: u8,
    top: Option<usize>,
    parent: Option<usize>,
    p: &Params,
    out: &mut Vec<Tile>,
) {
    let ch = &n.children;
    let mut idx: Vec<usize> = (0..ch.len())
        .filter(|&i| p.weight(&ch[i]) > 0 && (ch[i].is_dir || !p.by_files))
        .collect();
    idx.sort_by(|&a, &b| p.weight(&ch[b]).cmp(&p.weight(&ch[a])).then(a.cmp(&b)));
    let sizes: Vec<f64> = idx.iter().map(|&i| p.weight(&ch[i]) as f64).collect();
    // the parent's total, so space of skipped loose files stays free
    let total = sizes.iter().sum::<f64>().max(p.weight(n) as f64);
    if sizes.is_empty() || rect.w <= 0.0 || rect.h <= 0.0 || total <= 0.0 {
        return;
    }
    let scale = rect.w * rect.h / total;
    let (mut r, mut at) = (rect, 0);
    while at < idx.len() {
        let short = r.w.min(r.h);
        let len = row_len(&sizes[at..], short, scale);
        let area = sizes[at..at + len].iter().sum::<f64>() * scale;
        let t = area / short;
        // the rest is too thin to show; bounds the tile count for huge directories
        if t < p.min_side || t.is_nan() {
            return;
        }
        let vertical = r.w >= r.h;
        let mut off = 0.0;
        for k in at..at + len {
            let len_k = sizes[k] * scale / t;
            let cell = if vertical {
                Rect {
                    x: r.x,
                    y: r.y + off,
                    w: t,
                    h: len_k,
                }
            } else {
                Rect {
                    x: r.x + off,
                    y: r.y,
                    w: len_k,
                    h: t,
                }
            };
            off += len_k;
            let i = idx[k];
            emit(n, i, cell, (depth, top, parent), p, out);
        }
        if vertical {
            (r.x, r.w) = (r.x + t, r.w - t);
        } else {
            (r.y, r.h) = (r.y + t, r.h - t);
        }
        at += len;
    }
}

fn emit(
    n: &Node,
    i: usize,
    cell: Rect,
    (depth, top, parent): (u8, Option<usize>, Option<usize>),
    p: &Params,
    out: &mut Vec<Tile>,
) {
    if cell.w < p.min_side || cell.h < p.min_side {
        return;
    }
    let c = &n.children[i];
    let top = top.unwrap_or(i);
    let at = out.len();
    out.push(Tile {
        x: cell.x as f32,
        y: cell.y as f32,
        w: cell.w as f32,
        h: cell.h as f32,
        top,
        depth,
        parent,
        child: i,
        kind: kind(&c.name, c.is_dir),
    });
    if c.is_dir && depth < p.max_depth {
        let inner = Rect {
            x: cell.x + p.inset,
            y: cell.y + p.inset,
            w: cell.w - 2.0 * p.inset,
            h: cell.h - 2.0 * p.inset,
        };
        if inner.w >= p.min_side && inner.h >= p.min_side {
            place(c, inner, depth + 1, Some(top), Some(at), p, out);
        }
    }
}

/// Tile kind from the name's extension (case-insensitive).
pub fn kind(name: &OsStr, is_dir: bool) -> Kind {
    if is_dir {
        return Kind::Dir;
    }
    let path = std::path::Path::new(name);
    let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_lowercase()) else {
        return Kind::Other;
    };
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "svg"
        | "ico" | "raw" | "cr2" | "nef" => Kind::Image,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" => {
            Kind::Video
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" | "wma" => Kind::Audio,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "iso" | "dmg"
        | "deb" | "rpm" | "jar" => Kind::Archive,
        "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp"
        | "txt" | "md" | "rtf" | "epub" => Kind::Document,
        "rs" | "c" | "h" | "cpp" | "hpp" | "py" | "js" | "ts" | "go" | "java" | "kt" | "cs"
        | "rb" | "php" | "sh" | "html" | "css" | "json" | "toml" | "yaml" | "yml" | "xml" => {
            Kind::Code
        }
        "exe" | "dll" | "so" | "dylib" | "bin" | "o" | "a" | "msi" | "appimage" => Kind::Exec,
        _ => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Tile, kind, layout};
    use crate::stats::Node;
    use std::ffi::OsStr;

    fn file(name: &str, size: u64) -> Node {
        Node {
            name: name.into(),
            size,
            files: 1,
            ..Node::default()
        }
    }

    fn dir(name: &str, children: Vec<Node>) -> Node {
        Node {
            name: name.into(),
            is_dir: true,
            size: children.iter().map(|c| c.size).sum(),
            files: children.iter().map(|c| c.files).sum(),
            children,
        }
    }

    fn overlap(a: &Tile, b: &Tile) -> bool {
        let e = 1e-3;
        a.x + a.w > b.x + e && b.x + b.w > a.x + e && a.y + a.h > b.y + e && b.y + b.h > a.y + e
    }

    fn sample() -> Node {
        dir(
            "r",
            vec![
                file("a", 50),
                file("b", 30),
                file("c", 10),
                file("d", 6),
                file("e", 4),
            ],
        )
    }

    #[test]
    fn tiles_fill_area_proportionally_without_overlap() {
        let root = sample();
        let t = layout(&root, 100.0, 60.0, 0, 0.0, 0.0, false);
        assert_eq!(t.len(), 5);
        let mut total = 0.0;
        for tile in &t {
            assert!(tile.x >= 0.0 && tile.y >= 0.0);
            assert!(tile.x + tile.w <= 100.01 && tile.y + tile.h <= 60.01);
            let want =
                f32::from(u8::try_from(root.children[tile.top].size).unwrap()) / 100.0 * 6000.0;
            assert!((tile.w * tile.h - want).abs() <= want * 0.01, "{tile:?}");
            total += tile.w * tile.h;
        }
        assert!((total - 6000.0).abs() < 60.0);
        for (i, a) in t.iter().enumerate() {
            for b in &t[i + 1..] {
                assert!(!overlap(a, b), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn child_order_does_not_matter() {
        let mut root = sample();
        let key = |root: &Node| {
            let mut v: Vec<_> = layout(root, 100.0, 60.0, 0, 0.0, 0.0, false)
                .iter()
                .map(|t| (root.children[t.top].size, t.x.to_bits(), t.y.to_bits()))
                .collect();
            v.sort_unstable();
            v
        };
        let a = key(&root);
        root.children.reverse();
        assert_eq!(a, key(&root));
    }

    #[test]
    fn empty_and_zero_sizes() {
        assert!(layout(&Node::default(), 10.0, 10.0, 1, 0.0, 0.0, false).is_empty());
        let z = dir("z", vec![file("a", 0), file("b", 0)]);
        assert!(layout(&z, 10.0, 10.0, 1, 0.0, 0.0, false).is_empty());
        let one = dir("o", vec![file("a", 0), file("b", 5)]);
        assert_eq!(layout(&one, 10.0, 10.0, 1, 0.0, 0.0, false).len(), 1);
    }

    #[test]
    fn zero_area_does_not_panic() {
        let root = sample();
        assert!(layout(&root, 0.0, 10.0, 1, 0.0, 0.0, false).is_empty());
        assert!(layout(&root, 10.0, 0.0, 1, 0.0, 0.0, false).is_empty());
        assert!(layout(&root, f32::NAN, 10.0, 1, 0.0, 0.0, false).is_empty());
    }

    #[test]
    fn min_side_drops_tiny_entries() {
        let root = dir("r", vec![file("big", 1_000_000), file("tiny", 1)]);
        assert_eq!(layout(&root, 100.0, 100.0, 0, 0.0, 0.0, false).len(), 2);
        let t = layout(&root, 100.0, 100.0, 0, 2.0, 0.0, false);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].top, 0);
    }

    #[test]
    fn nesting_follows_max_depth() {
        let root = dir(
            "r",
            vec![dir("d", vec![file("x", 3), file("y", 1)]), file("f", 4)],
        );
        let flat = layout(&root, 80.0, 40.0, 0, 0.0, 1.0, false);
        assert!(flat.iter().all(|t| t.depth == 0));
        let t = layout(&root, 80.0, 40.0, 1, 0.0, 1.0, false);
        let parent = t.iter().find(|t| t.depth == 0 && t.top == 0).unwrap();
        let inner: Vec<_> = t.iter().filter(|t| t.depth == 1).collect();
        assert_eq!(inner.len(), 2);
        for n in inner {
            assert_eq!(n.top, 0);
            assert!(n.x >= parent.x + 1.0 - 1e-3 && n.y >= parent.y + 1.0 - 1e-3);
            assert!(n.x + n.w <= parent.x + parent.w - 1.0 + 1e-3);
            assert!(n.y + n.h <= parent.y + parent.h - 1.0 + 1e-3);
        }
        // parent/child links lead from every tile back to its own node
        let names: Vec<_> = t
            .iter()
            .map(|tile| {
                let mut chain = vec![tile.child];
                let mut up = tile.parent;
                while let Some(k) = up {
                    chain.push(t[k].child);
                    up = t[k].parent;
                }
                chain
                    .iter()
                    .rev()
                    .fold(&root, |n, &i| &n.children[i])
                    .name
                    .clone()
            })
            .collect();
        let mut names: Vec<_> = names.iter().map(|n| n.to_str().unwrap()).collect();
        names.sort_unstable();
        assert_eq!(names, ["d", "f", "x", "y"]);
    }

    #[test]
    fn by_files_shows_dirs_only_sized_by_count() {
        let root = dir(
            "r",
            vec![
                dir("a", vec![file("1", 1), file("2", 1), file("3", 1)]),
                dir("b", vec![file("4", 900)]),
                dir("e", vec![]),
                file("big", 5000),
            ],
        );
        // 5 files: a 3, b 1, loose "big" 1 (its area stays free)
        let t = layout(&root, 100.0, 100.0, 0, 0.0, 0.0, true);
        assert_eq!(t.len(), 2);
        assert!(t.iter().all(|t| t.kind == Kind::Dir));
        let area = |i: usize| t.iter().find(|t| t.child == i).map(|t| t.w * t.h).unwrap();
        assert!((area(0) - 6000.0).abs() < 1.0 && (area(1) - 2000.0).abs() < 1.0);
    }

    #[test]
    fn kinds() {
        assert_eq!(kind(OsStr::new("A.JPG"), false), Kind::Image);
        assert_eq!(kind(OsStr::new("a.jpg"), true), Kind::Dir);
        assert_eq!(kind(OsStr::new("Makefile"), false), Kind::Other);
    }
}
