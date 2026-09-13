//! Near-exact clone detection.
//!
//! Tokens come from tree-sitter leaves with identifiers, strings and numbers
//! collapsed to classes, so renamed copies still match. Every k-token window is
//! hashed; winnowing keeps one fingerprint per w-window, which guarantees any
//! shared run of at least k+w-1 tokens is found while indexing a fraction of
//! the hashes. Matching fingerprints on the same diagonal are merged into runs.

use crate::discover::SourceFile;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use tree_sitter::Node;

/// Tokens per fingerprint window. ~4–6 lines of typical code.
pub const K: usize = 30;
/// Winnowing window: one fingerprint kept per W consecutive k-grams.
pub const W: usize = 20;
/// Shortest run worth reporting, in tokens.
pub const MIN_TOKENS: usize = 70;
/// Fingerprints seen in more locations than this are boilerplate, not clones.
const MAX_LOCATIONS: usize = 40;

#[derive(Debug, Clone, Serialize)]
pub struct Loc {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClonePair {
    pub a: Loc,
    pub b: Loc,
    pub tokens: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FileClones {
    /// Lines covered by at least one clone range (union).
    pub clone_lines: usize,
    pub clone_ratio: f64,
    pub pairs: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct CloneReport {
    pub pairs: Vec<ClonePair>,
    pub files: HashMap<String, FileClones>,
}

struct Tokens {
    hashes: Vec<u64>,
    lines: Vec<usize>,
}

pub fn detect(files: &[SourceFile]) -> CloneReport {
    let toks: Vec<Tokens> = files.par_iter().map(tokenize).collect();

    // fingerprint hash -> (file, token position)
    let mut index: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
    for (fi, t) in toks.iter().enumerate() {
        for (pos, h) in winnow(&t.hashes) {
            index.entry(h).or_default().push((fi, pos));
        }
    }

    // Candidate matches grouped by unordered file pair.
    let mut by_pair: HashMap<(usize, usize), Vec<(usize, usize)>> = HashMap::new();
    for locs in index.values() {
        if locs.len() < 2 || locs.len() > MAX_LOCATIONS {
            continue;
        }
        for i in 0..locs.len() {
            for j in i + 1..locs.len() {
                let (mut x, mut y) = (locs[i], locs[j]);
                if x.0 > y.0 || (x.0 == y.0 && x.1 > y.1) {
                    std::mem::swap(&mut x, &mut y);
                }
                if x.0 == y.0 && y.1 - x.1 < K {
                    continue; // overlapping with itself
                }
                by_pair.entry((x.0, y.0)).or_default().push((x.1, y.1));
            }
        }
    }

    let mut pairs: Vec<ClonePair> = by_pair
        .into_par_iter()
        .flat_map_iter(|((fa, fb), mut matches)| {
            // Same diagonal = same offset between the two positions.
            matches.sort_by_key(|(a, b)| (*a as i64 - *b as i64, *a));
            let mut runs: Vec<(usize, usize, usize)> = Vec::new(); // (startA, startB, len)
            let mut cur: Option<(usize, usize, usize)> = None;
            for (a, b) in matches {
                match cur {
                    Some((sa, sb, len)) if a as i64 - b as i64 == sa as i64 - sb as i64 && a <= sa + len => {
                        cur = Some((sa, sb, (a + K) - sa));
                    }
                    _ => {
                        if let Some(r) = cur.take() {
                            runs.push(r);
                        }
                        cur = Some((a, b, K));
                    }
                }
            }
            if let Some(r) = cur {
                runs.push(r);
            }
            let ta = &toks[fa];
            let tb = &toks[fb];
            // Fingerprints sample the k-grams, so a run's ends are up to W tokens
            // short on each side. Extend while the underlying tokens still match.
            for r in runs.iter_mut() {
                let (mut sa, mut sb, mut len) = *r;
                while sa > 0 && sb > 0 && ta.hashes[sa - 1] == tb.hashes[sb - 1] {
                    sa -= 1;
                    sb -= 1;
                    len += 1;
                }
                while sa + len < ta.hashes.len() && sb + len < tb.hashes.len() && ta.hashes[sa + len] == tb.hashes[sb + len] {
                    len += 1;
                }
                *r = (sa, sb, len);
            }
            // Repetitive code matches itself on every shifted diagonal. Keep the
            // longest run and drop any run whose A-range *and* B-range both
            // overlap an accepted run: that is the same clone seen at an offset.
            runs.retain(|(_, _, len)| *len >= MIN_TOKENS);
            // Within one file, ranges that overlap each other are a repeating
            // pattern (a table, an unrolled loop), not a copy.
            if fa == fb {
                runs.retain(|(sa, sb, len)| !overlaps(*sa, *len, *sb, *len));
            }
            runs.sort_by_key(|(_, _, len)| std::cmp::Reverse(*len));
            let mut kept: Vec<(usize, usize, usize)> = Vec::new();
            for r in runs {
                let dup = kept.iter().any(|k| overlaps(r.0, r.2, k.0, k.2) && overlaps(r.1, r.2, k.1, k.2));
                if !dup {
                    kept.push(r);
                }
            }
            kept.into_iter()
                .map(|(sa, sb, len)| ClonePair {
                    a: loc(&files[fa].path, ta, sa, len),
                    b: loc(&files[fb].path, tb, sb, len),
                    tokens: len,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    pairs.sort_by_key(|p| std::cmp::Reverse(p.tokens));

    // Per-file union of cloned lines.
    let mut ranges: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for p in &pairs {
        ranges.entry(&p.a.file).or_default().push((p.a.start_line, p.a.end_line));
        ranges.entry(&p.b.file).or_default().push((p.b.start_line, p.b.end_line));
    }
    let mut per_file = HashMap::new();
    for f in files {
        let Some(rs) = ranges.get_mut(f.path.as_str()) else { continue };
        rs.sort();
        let mut covered = 0usize;
        let mut end = 0usize;
        for &(s, e) in rs.iter() {
            let s = s.max(end + 1);
            if e >= s {
                covered += e - s + 1;
                end = e;
            }
        }
        per_file.insert(
            f.path.clone(),
            FileClones {
                clone_lines: covered,
                clone_ratio: if f.lines == 0 { 0.0 } else { covered as f64 / f.lines as f64 },
                pairs: rs.len(),
            },
        );
    }
    CloneReport { pairs, files: per_file }
}

fn overlaps(s1: usize, l1: usize, s2: usize, l2: usize) -> bool {
    s1 < s2 + l2 && s2 < s1 + l1
}

fn loc(path: &str, t: &Tokens, start: usize, len: usize) -> Loc {
    let end = (start + len - 1).min(t.lines.len().saturating_sub(1));
    Loc { file: path.to_string(), start_line: t.lines[start], end_line: t.lines[end] }
}

/// Winnowing (Schleimer, Wilkerson, Aiken 2003): hash every k-gram, keep the
/// minimum of each w-window, rightmost on ties, de-duplicated.
fn winnow(hashes: &[u64]) -> Vec<(usize, u64)> {
    if hashes.len() < K {
        return Vec::new();
    }
    let grams: Vec<u64> = hashes.windows(K).map(hash_slice).collect();
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    for start in 0..=grams.len().saturating_sub(W) {
        let win = &grams[start..(start + W).min(grams.len())];
        let mut best = 0;
        for (i, g) in win.iter().enumerate() {
            if *g <= win[best] {
                best = i;
            }
        }
        let pos = start + best;
        if last != Some(pos) {
            out.push((pos, grams[pos]));
            last = Some(pos);
        }
        if start + W >= grams.len() {
            break;
        }
    }
    out
}

fn hash_slice(s: &[u64]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

const STRING_KINDS: &[&str] = &[
    "string", "template_string", "string_literal", "raw_string_literal", "concatenated_string",
    "jsx_text", "char_literal",
];
const NUMBER_KINDS: &[&str] = &[
    "integer", "float", "number", "integer_literal", "float_literal", "true", "false", "none",
    "null", "undefined",
];

fn tokenize(file: &SourceFile) -> Tokens {
    let mut parser = file.lang.parser();
    let src = file.content.as_bytes();
    let mut out = Tokens { hashes: Vec::new(), lines: Vec::new() };
    let Some(tree) = parser.parse(src, None) else { return out };
    let mut stack: Vec<Node> = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if kind == "comment" || kind == "line_comment" || kind == "block_comment" {
            continue;
        }
        let class = if STRING_KINDS.contains(&kind) {
            Some("STR")
        } else if NUMBER_KINDS.contains(&kind) {
            Some("NUM")
        } else if n.child_count() == 0 {
            if kind.contains("identifier") { Some("ID") } else { Some(kind) }
        } else {
            None
        };
        if let Some(c) = class {
            out.hashes.push(hash_str(c));
            out.lines.push(n.start_position().row + 1);
            continue;
        }
        let mut cur = n.walk();
        let children: Vec<Node> = n.children(&mut cur).collect();
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::FileKind;
    use crate::lang::Language;

    fn sf(path: &str, content: String) -> SourceFile {
        SourceFile {
            path: path.into(),
            lang: Language::Python,
            kind: FileKind::Source,
            lines: content.lines().count(),
            bytes: content.len(),
            content,
        }
    }

    /// Twelve structurally different lines, so the body does not repeat itself.
    fn body(p: &str) -> String {
        format!(
            "    {p}_a = compute({p}, 1) + other[2]\n\
    if {p}_a and not {p}:\n\
        raise ValueError({p}_a)\n\
    for {p}_i in range(3):\n\
        {p}_a += {p}_i * 2\n\
    while {p}_a > 10:\n\
        {p}_a -= 1\n\
    {p}_b = [{p}_x for {p}_x in {p} if {p}_x]\n\
    try:\n\
        {p}_c = {p}_b[0]\n\
    except IndexError:\n\
        {p}_c = None\n"
        )
    }

    #[test]
    fn renamed_copy_is_found_and_lines_are_right() {
        let a = format!("def alpha(x):\n{}\n    return x\n", body("aa"));
        let b = format!("import os\n\ndef beta(y):\n{}\n    return y\n", body("bb"));
        let r = detect(&[sf("a.py", a), sf("b.py", b)]);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert_eq!(p.a.file, "a.py");
        assert_eq!(p.b.file, "b.py");
        assert!(p.a.start_line <= 2 && p.a.end_line >= 13, "{p:?}");
        assert!(p.b.start_line <= 4 && p.b.end_line >= 15, "{p:?}");
        assert!(r.files["a.py"].clone_ratio > 0.7);
    }

    #[test]
    fn different_code_is_not_a_clone() {
        let a = "\
def load(path):
    with open(path) as fh:
        data = json.load(fh)
    for key, value in data.items():
        if not isinstance(value, dict):
            raise ValueError(key)
    return {k: Entry(**v) for k, v in data.items()}

class Registry:
    def __init__(self):
        self.entries = {}
    def add(self, e):
        self.entries[e.id] = e
    def find(self, pred):
        return [e for e in self.entries.values() if pred(e)]
";
        let b = "\
async def handler(request):
    body = await request.json()
    try:
        user = await lookup(body['id'])
    except KeyError:
        return web.Response(status=400)
    while user.pending:
        await asyncio.sleep(0.1)
    match user.role:
        case 'admin': return admin_view(user)
        case _: return plain_view(user)
";
        let r = detect(&[sf("a.py", a.into()), sf("b.py", b.into())]);
        assert!(r.pairs.is_empty(), "{:?}", r.pairs);
    }
}
