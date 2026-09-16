use super::{CloneKind, ClonePair, Loc, Tokens};
use crate::{discover::SourceFile, metrics};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, Serialize)]
pub struct CloneFamily {
    pub members: Vec<Loc>,
    pub pair_count: usize,
    /// Original normalized matching ranges; comparisons use their enclosing callables.
    pub matches: Vec<ClonePair>,
    pub max_shared_tokens: usize,
    pub comparisons: Vec<Comparison>,
    pub comparisons_omitted: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub a: Loc,
    pub b: Loc,
    /// Exact, trimmed-line matches; identifier/literal differences are deliberately preserved.
    pub shared: Vec<Change>,
    pub differences: Vec<Change>,
    pub shared_blocks_omitted: usize,
    pub differences_omitted: usize,
    /// Full comparison skipped when either scope exceeds 500 lines.
    pub skipped: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub a: Option<Excerpt>,
    pub b: Option<Excerpt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Excerpt {
    pub location: Loc,
    pub code: String,
    pub truncated: bool,
}

pub(super) fn build(pairs: &[ClonePair], files: &[SourceFile], toks: &[Tokens]) -> Vec<CloneFamily> {
    let tokens: HashMap<_, _> = files.iter().zip(toks).map(|(f, t)| (f.path.as_str(), t)).collect();
    let sources: HashMap<&str, &SourceFile> = files.iter().map(|f| (f.path.as_str(), f)).collect();
    let units: HashMap<&str, Vec<Loc>> = files
        .iter()
        .zip(toks)
        .map(|(f, t)| {
            let units = t
                .tree
                .as_ref()
                .map(|tree| {
                    metrics::unit_nodes(tree.root_node(), f.lang)
                        .into_iter()
                        .map(|u| Loc {
                            file: f.path.clone(),
                            start_line: u.start_position().row + 1,
                            end_line: u.end_position().row + 1,
                            symbol: metrics::unit_name_of(u, f.lang, f.content.as_bytes()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            (f.path.as_str(), units)
        })
        .collect();
    let mut members = Vec::<Loc>::new();
    let mut ids = BTreeMap::new();
    let mut parent = Vec::new();
    let mut edges = Vec::new();
    for (pair_index, p) in pairs.iter().enumerate().filter(|(_, p)| p.kind == CloneKind::Logic) {
        let scopes = [&p.a, &p.b].map(|side| scope(side, &units[side.file.as_str()], tokens[side.file.as_str()]));
        let same_scope =
            scopes[0].file == scopes[1].file && scopes[0].start_line == scopes[1].start_line && scopes[0].end_line == scopes[1].end_line;
        let mut endpoints = Vec::new();
        for scope in if same_scope { [&p.a, &p.b] } else { scopes } {
            let key = (scope.file.clone(), scope.start_line, scope.end_line);
            let next = members.len();
            let id = *ids.entry(key).or_insert_with(|| {
                members.push(scope.clone());
                parent.push(next);
                next
            });
            endpoints.push(id);
        }
        let (a, b) = (endpoints[0], endpoints[1]);
        if a == b {
            continue;
        }
        let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
        parent[rb] = ra;
        edges.push((a, b, p.tokens, pair_index));
    }
    let mut groups: BTreeMap<usize, Vec<(usize, usize, usize, usize)>> = BTreeMap::new();
    for edge in edges {
        groups.entry(root(&mut parent, edge.0)).or_default().push(edge);
    }
    let mut out = Vec::new();
    for edges in groups.values() {
        let member_ids: BTreeSet<usize> = edges.iter().flat_map(|(a, b, _, _)| [*a, *b]).collect();
        let mut family_members: Vec<Loc> = member_ids.into_iter().map(|i| members[i].clone()).collect();
        family_members.sort_by(|a, b| (&a.file, a.start_line, a.end_line).cmp(&(&b.file, b.start_line, b.end_line)));
        let mut seen = BTreeSet::new();
        let mut comparisons = Vec::new();
        for &(a, b, _, _) in edges {
            if !seen.insert((a.min(b), a.max(b))) || comparisons.len() >= 12 {
                continue;
            }
            comparisons.push(compare(&members[a], &members[b], sources[members[a].file.as_str()], sources[members[b].file.as_str()]));
        }
        out.push(CloneFamily {
            members: family_members,
            pair_count: edges.len(),
            matches: edges.iter().map(|e| pairs[e.3].clone()).collect(),
            max_shared_tokens: edges.iter().map(|e| e.2).max().unwrap_or(0),
            comparisons_omitted: seen.len().saturating_sub(comparisons.len()),
            comparisons,
        });
    }
    out.sort_by(|a, b| {
        b.max_shared_tokens.cmp(&a.max_shared_tokens).then_with(|| {
            let (a, b) = (&a.members[0], &b.members[0]);
            (&a.file, a.start_line).cmp(&(&b.file, b.start_line))
        })
    });
    out
}

fn scope<'a>(side: &'a Loc, units: &'a [Loc], tokens: &Tokens) -> &'a Loc {
    if let Some(unit) =
        units.iter().filter(|u| u.start_line <= side.start_line && u.end_line >= side.end_line).min_by_key(|u| u.end_line - u.start_line)
    {
        return unit;
    }
    // Matching runs can bleed into the next declaration. Compare the callable containing
    // most of the matched tokens, retaining the original ranges in CloneReport.pairs.
    let lo = tokens.lines.partition_point(|line| *line < side.start_line);
    let hi = tokens.lines.partition_point(|line| *line <= side.end_line);
    units
        .iter()
        .filter_map(|unit| {
            let start = tokens.lines.partition_point(|line| *line < unit.start_line).max(lo);
            let end = tokens.lines.partition_point(|line| *line <= unit.end_line).min(hi);
            let overlap = end.saturating_sub(start);
            (overlap > 0 && overlap * 2 > hi - lo).then_some((overlap, unit))
        })
        .max_by_key(|(overlap, u)| (*overlap, std::cmp::Reverse(u.end_line - u.start_line)))
        .map_or(side, |(_, unit)| unit)
}

fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn compare(a: &Loc, b: &Loc, fa: &SourceFile, fb: &SourceFile) -> Comparison {
    let mut out = Comparison {
        a: a.clone(),
        b: b.clone(),
        shared: Vec::new(),
        differences: Vec::new(),
        shared_blocks_omitted: 0,
        differences_omitted: 0,
        skipped: false,
    };
    if a.end_line - a.start_line >= 500 || b.end_line - b.start_line >= 500 {
        out.skipped = true;
        return out;
    }
    let lines = |f: &SourceFile, scope: &Loc| {
        f.content
            .lines()
            .enumerate()
            .skip(scope.start_line - 1)
            .take(scope.end_line - scope.start_line + 1)
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(i, line)| (i + 1, line.to_string()))
            .collect::<Vec<_>>()
    };
    let (left, right) = (lines(fa, a), lines(fb, b));
    let (n, m) = (left.len(), right.len());
    let mut lcs = vec![0u16; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * (m + 1) + j] = if left[i].1.trim() == right[j].1.trim() {
                1 + lcs[(i + 1) * (m + 1) + j + 1]
            } else {
                lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        let (si, sj) = (i, j);
        let shared = i < n && j < m && left[i].1.trim() == right[j].1.trim();
        while i < n || j < m {
            let equal = i < n && j < m && left[i].1.trim() == right[j].1.trim();
            if equal != shared {
                break;
            }
            if equal {
                i += 1;
                j += 1;
            } else if j == m || (i < n && lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + j + 1]) {
                i += 1;
            } else {
                j += 1;
            }
        }
        let change = Change { a: excerpt(a, &left[si..i], fa), b: excerpt(b, &right[sj..j], fb) };
        if shared { out.shared.push(change) } else { out.differences.push(change) }
    }
    out.shared.sort_by_key(|c| std::cmp::Reverse(c.a.as_ref().map_or(0, |e| e.location.end_line - e.location.start_line)));
    out.shared_blocks_omitted = out.shared.len().saturating_sub(3);
    out.shared.truncate(3);
    out.differences_omitted = out.differences.len().saturating_sub(8);
    out.differences.truncate(8);
    out
}

fn excerpt(scope: &Loc, lines: &[(usize, String)], source: &SourceFile) -> Option<Excerpt> {
    let first = lines.first()?;
    let end = lines.last().unwrap().0.min(first.0 + 11);
    Some(Excerpt {
        location: Loc { start_line: first.0, end_line: end, ..scope.clone() },
        code: source.content.lines().skip(first.0 - 1).take(end - first.0 + 1).collect::<Vec<_>>().join("\n"),
        truncated: lines.last().unwrap().0 > end,
    })
}

pub fn render_family(family: &CloneFamily) -> String {
    use std::fmt::Write;
    let mut out =
        format!("  {} members, {} matching runs, largest {} tokens\n", family.members.len(), family.pair_count, family.max_shared_tokens);
    for member in &family.members {
        let _ = writeln!(out, "    {}", crate::plan::side_text(member, "", false));
    }
    for c in family.comparisons.iter().take(1) {
        let _ = writeln!(out, "    compare {} <-> {} (source differences; check behavior before extracting)", c.a.symbol, c.b.symbol);
        if c.skipped {
            out.push_str("      comparison omitted: scope exceeds 500 lines\n");
            continue;
        }
        if let Some(shared) = c.shared.first().and_then(|s| s.a.as_ref()) {
            let _ = writeln!(
                out,
                "      shared example {}:{}-{}{}",
                shared.location.file,
                shared.location.start_line,
                shared.location.end_line,
                if shared.truncated { " (excerpt)" } else { "" }
            );
            for line in shared.code.lines() {
                let _ = writeln!(out, "        {}", line.trim());
            }
        }
        if c.differences.is_empty() {
            out.push_str("      identical after trimming indentation and blank lines\n");
        }
        for change in &c.differences {
            for (prefix, side) in [("-", &change.a), ("+", &change.b)] {
                if let Some(e) = side {
                    let _ = writeln!(
                        out,
                        "      {prefix} {}:{}-{}{}",
                        e.location.file,
                        e.location.start_line,
                        e.location.end_line,
                        if e.truncated { " (excerpt)" } else { "" }
                    );
                    for line in e.code.lines() {
                        let _ = writeln!(out, "        {prefix} {}", line.trim());
                    }
                }
            }
        }
        if c.differences_omitted > 0 {
            let _ = writeln!(out, "      +{} more difference blocks", c.differences_omitted);
        }
    }
    if family.comparisons.len() > 1 {
        let _ = writeln!(out, "    +{} comparisons in JSON", family.comparisons.len() - 1);
    }
    if family.comparisons_omitted > 0 {
        let _ = writeln!(out, "    +{} comparisons omitted by the analysis limit", family.comparisons_omitted);
    }
    out
}
