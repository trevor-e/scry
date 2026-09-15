//! Comments as structure: the banners and phase labels an author wrote into the source.
//!
//! An LLM narrates sub-files inside one file (`// ── The ladder ──`) where a human would
//! split, and pre-annotates the phases of a long function (`// ── step 1: verify merged ──`).
//! Both are split plans already in the source: banners at the file root partition it into
//! labelled sections (P17), and phase comments inside a function over the cognitive threshold
//! say where to cut it (P18). One walk feeds both; neither is a score input.

use crate::config::{Comments as Cfg, Metrics as MetricsCfg};
use crate::discover::SourceFile;
use crate::lang::Language;
use crate::metrics::{self, FunctionMetrics};
use crate::regions::{self, TestRegion};
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use std::collections::HashMap;
use tree_sitter::Node;

/// One labelled section of a file: the lines between its banner and the next one.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    /// The banner's title (inline on the rule, or the comment line between a rule pair).
    pub name: String,
    /// First line after the banner…
    pub start: usize,
    /// …to the line before the next banner (or EOF).
    pub end: usize,
    pub lines: usize,
    /// Metrics units whose start line lies in the section.
    pub functions: usize,
}

/// One labelled phase of a function body.
#[derive(Debug, Clone, Serialize)]
pub struct Phase {
    pub label: String,
    /// The phase comment's line…
    pub start: usize,
    /// …to the line before the next phase comment in the same block, or the block's end.
    pub end: usize,
    /// Non-blank lines of the span.
    pub lines: usize,
    /// Cognitive of the phase's statements with nesting re-based to 0 (`estimate_cognitive`).
    pub est_cognitive: Option<u32>,
    /// Locals bound before this phase that it and at least one other phase read.
    pub shared_locals: Vec<String>,
}

/// A function over the cognitive threshold whose body labels its phases.
#[derive(Debug, Clone, Serialize)]
pub struct UnitPhases {
    pub unit: String,
    pub start_line: usize,
    pub end_line: usize,
    pub cognitive: u32,
    pub phases: Vec<Phase>,
    /// Every shared local, by first binding line.
    pub shared_locals: Vec<String>,
    /// The sub-line printed under the file's metrics reason.
    pub reason: String,
}

/// What one Source file contributes, computed on the metrics tree.
#[derive(Debug, Clone, Default)]
pub struct FileSide {
    pub path: String,
    /// File lines minus inline test lines: the report's size signal.
    pub source_lines: usize,
    pub lines: usize,
    pub banners: usize,
    pub sections: Vec<Section>,
    pub phase_comments: usize,
    pub units: Vec<UnitPhases>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileComments {
    /// Top-level banner comments.
    pub banners: usize,
    /// Every labelled section, thresholds aside.
    pub sections: Vec<Section>,
    /// `lines`, `min_sections` and `min_section_lines` are met; the report prints the reason
    /// when the file is also above `min_size_percentile` or in the printed hotspot list.
    pub qualifies: bool,
    /// Percentile (0-100) of source lines among Source files.
    pub size_percentile: f64,
    pub above_size_percentile: bool,
    /// The banner annotation line, set when `qualifies`.
    pub banner_reason: Option<String>,
    pub phase_comments: usize,
    /// Units over the cognitive threshold with enough labelled phases.
    pub units: Vec<UnitPhases>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub files: usize,
    pub banners: usize,
    pub files_with_banners: usize,
    /// Files whose sections meet every banner threshold, percentile included.
    pub banner_hits: usize,
    /// Files meeting the file-local thresholds but not the size percentile (the report still
    /// prints them when they are in the hotspot list).
    pub banner_below_percentile: usize,
    pub phase_comments: usize,
    pub phase_hits: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CommentsReport {
    pub totals: Totals,
    pub files: HashMap<String, FileComments>,
}

/// The compiled patterns, built once per scan and shared by every file.
pub struct Walker<'c> {
    cfg: &'c Cfg,
    cognitive_min: u32,
    /// `^\s*[rule]{n,}\s*(\S.*?)?\s*[rule]*$`: a rule with an optional inline title.
    rule: Option<Regex>,
    /// `^\s*(──|--|==)\s*(\S.*)$`: a short rule followed by text.
    short: Regex,
    phase: Vec<Regex>,
}

impl<'c> Walker<'c> {
    pub fn new(cfg: &'c Cfg, metrics: &MetricsCfg) -> Self {
        let b = &cfg.banners;
        let class = rule_class(&b.rule_chars);
        let rule = (!b.rule_chars.is_empty())
            .then(|| Regex::new(&format!(r"^\s*{class}{{{},}}\s*(\S.*?)?\s*{class}*$", b.min_rule_len.max(1))).ok())
            .flatten();
        let phase = cfg.phases.patterns.iter().filter_map(|p| RegexBuilder::new(p).case_insensitive(true).build().ok()).collect();
        Walker {
            cfg,
            cognitive_min: cfg.phases.cross_with_cognitive_min.unwrap_or(metrics.cognitive_hard),
            rule,
            short: Regex::new(r"^\s*(──|--|==)\s*(\S.*)$").expect("static regex"),
            phase,
        }
    }

    /// Banners at the file root and phase comments inside the over-threshold units, on the
    /// metrics tree: `funcs` and `nodes` are the file's units in the same order.
    pub fn file_side(&self, root: Option<Node>, f: &SourceFile, regions: &[TestRegion], funcs: &[FunctionMetrics], nodes: &[Node]) -> FileSide {
        let mut side = FileSide { path: f.path.clone(), lines: f.lines, source_lines: f.lines.saturating_sub(regions::inline_lines(regions)), ..FileSide::default() };
        let Some(root) = root else { return side };
        let src = f.content.as_bytes();
        let lines = LineIndex::new(src);
        if self.cfg.banners.enabled {
            self.banners(root, f.lang, src, &lines, funcs, &mut side);
        }
        for (fm, node) in funcs.iter().zip(nodes) {
            if fm.cognitive < self.cognitive_min || fm.lines < self.cfg.phases.min_span_lines {
                continue;
            }
            if self.cfg.phases.skip_test_modules && regions::contains(regions, node.start_byte()) {
                continue;
            }
            if let Some(u) = self.phases(*node, fm, f.lang, src, &lines, &mut side.phase_comments) {
                side.units.push(u);
            }
        }
        side
    }

    // ---------- P17: banners ----------

    fn banners(&self, root: Node, lang: Language, src: &[u8], lines: &LineIndex, funcs: &[FunctionMetrics], side: &mut FileSide) {
        let b = &self.cfg.banners;
        let comment_kinds = comment_kinds(lang);
        // Top-level comments in order, banner or not, plus the first line of every other item.
        let mut comments: Vec<TopComment> = Vec::new();
        let mut items: Vec<(usize, bool)> = Vec::new(); // (start line, is import)
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            let kind = child.kind();
            if comment_kinds.contains(&kind) {
                if is_doc_comment(child) {
                    continue;
                }
                let text = strip_comment(text(child, src));
                let banner = self.banner_title(text);
                comments.push(TopComment { start: lines.line_of(child.start_byte()), end: lines.line_of(child.end_byte().saturating_sub(1)), text: text.to_string(), banner });
            } else if child.is_named() {
                items.push((lines.line_of(child.start_byte()), import_kinds(lang).contains(&kind)));
            }
        }
        side.banners = comments.iter().filter(|c| c.banner.is_some()).count();
        if side.banners == 0 {
            return;
        }
        // Banners within `pair_gap` lines merge into one boundary when only comments lie
        // between them (a pair encloses its title, never code).
        let mut boundaries: Vec<Boundary> = Vec::new();
        for (i, c) in comments.iter().enumerate() {
            let Some(inline) = &c.banner else { continue };
            match boundaries.last_mut() {
                Some(last) if c.start <= last.last_line + b.pair_gap && !items.iter().any(|(l, _)| last.last_line < *l && *l < c.start) => {
                    last.last_line = c.end;
                    last.last_index = i;
                    if last.title.is_none() {
                        last.title = inline.clone();
                    }
                }
                _ => boundaries.push(Boundary { first_line: c.start, last_line: c.end, first_index: i, last_index: i, title: inline.clone() }),
            }
        }
        for bd in &mut boundaries {
            if bd.title.is_none() {
                // The first non-banner comment line inside the pair.
                bd.title = comments[bd.first_index..=bd.last_index].iter().filter(|c| c.banner.is_none()).map(|c| c.text.trim()).find(|t| has_word(t) && !self.skipped(t)).map(str::to_string);
            }
        }
        if b.require_title {
            boundaries.retain(|bd| bd.title.is_some());
        }
        // A boundary at the top of the file followed by an import is a license / file header;
        // its closing rule (the next boundary with no item between them) goes with it.
        if b.skip_top_of_file {
            let next_item = |line: usize| items.iter().find(|(l, _)| *l > line).copied();
            let mut drop = vec![false; boundaries.len()];
            for i in 0..boundaries.len() {
                if drop[i] {
                    continue;
                }
                let bd = &boundaries[i];
                if bd.first_line <= 3 && next_item(bd.last_line).is_some_and(|(_, imp)| imp) {
                    drop[i] = true;
                    if let Some(nb) = boundaries.get(i + 1)
                        && next_item(bd.last_line).is_some_and(|(l, _)| l > nb.last_line)
                    {
                        drop[i + 1] = true;
                    }
                }
            }
            let mut it = drop.into_iter();
            boundaries.retain(|_| !it.next().unwrap_or(false));
        }
        let total = lines.count();
        side.sections = boundaries
            .iter()
            .enumerate()
            .map(|(i, bd)| {
                let start = bd.last_line + 1;
                let end = boundaries.get(i + 1).map_or(total, |n| n.first_line - 1).max(bd.last_line);
                Section {
                    name: bd.title.clone().unwrap_or_default(),
                    start,
                    end,
                    lines: (end + 1).saturating_sub(start),
                    functions: funcs.iter().filter(|f| !f.in_test && start <= f.start_line && f.start_line <= end).count(),
                }
            })
            .collect();
    }

    /// `Some(title)` when `text` is a banner: `Some(Some(t))` with an inline title, `Some(None)`
    /// for a bare rule. Editor folding markers are never banners.
    fn banner_title(&self, text: &str) -> Option<Option<String>> {
        let b = &self.cfg.banners;
        if self.skipped(text) {
            return None;
        }
        let strip = |t: &str| t.trim_matches(|c: char| c.is_whitespace() || b.rule_chars.contains(c)).to_string();
        if let Some(m) = self.rule.as_ref().and_then(|r| r.captures(text)) {
            let title = m.get(1).map(|t| strip(t.as_str())).filter(|t| has_word(t));
            if title.as_deref().is_some_and(|t| self.skipped(t)) {
                return None;
            }
            return Some(title);
        }
        if let Some(m) = self.short.captures(text) {
            let title = strip(&m[2]);
            if has_word(&title) && !self.skipped(&title) {
                return Some(Some(title));
            }
        }
        None
    }

    fn skipped(&self, text: &str) -> bool {
        let t = text.trim_start_matches(|c: char| c == '/' || c == '*' || c.is_whitespace());
        self.cfg.banners.skip_markers.iter().any(|m| t.starts_with(m.as_str()) || t.split_whitespace().next() == Some(m.trim_start_matches('#')))
    }

    // ---------- P18: phases ----------

    fn phases(&self, unit: Node, fm: &FunctionMetrics, lang: Language, src: &[u8], lines: &LineIndex, phase_comments: &mut usize) -> Option<UnitPhases> {
        let p = &self.cfg.phases;
        let blocks = block_kinds(lang);
        let comment_kinds = comment_kinds(lang);
        let body = unit.child_by_field_name("body").filter(|b| blocks.contains(&b.kind()))?;
        // Phase comments: non-doc comments whose parent is the body or a block at most
        // `max_depth` levels below it; nested callables are their own scope.
        let mut found: Vec<(Node, Node)> = Vec::new(); // (comment, parent block)
        let mut stack: Vec<(Node, usize)> = vec![(body, 0)];
        while let Some((n, depth)) = stack.pop() {
            let mut cursor = n.walk();
            for child in n.children(&mut cursor) {
                let kind = child.kind();
                if comment_kinds.contains(&kind) {
                    if blocks.contains(&n.kind()) && !is_doc_comment(child) && self.phase.iter().any(|r| r.is_match(strip_comment(text(child, src)))) {
                        found.push((child, n));
                    }
                } else if blocks.contains(&kind) {
                    if depth < p.max_depth {
                        stack.push((child, depth + 1));
                    }
                } else if !metrics::is_callable_kind(lang, kind) {
                    stack.push((child, depth));
                }
            }
        }
        *phase_comments += found.len();
        if found.len() < p.min_phases {
            return None;
        }
        found.sort_by_key(|(c, _)| c.start_byte());
        // Spans: to the line before the next phase comment in the same block, else its end.
        let mut phases: Vec<(Phase, Node)> = Vec::new();
        for (i, (c, parent)) in found.iter().enumerate() {
            let start = lines.line_of(c.start_byte());
            let next = found[i + 1..].iter().find(|(_, q)| q.id() == parent.id()).map(|(n, _)| lines.line_of(n.start_byte()) - 1);
            let end = next.unwrap_or_else(|| lines.line_of(parent.end_byte().saturating_sub(1))).max(start);
            let label = self.phase_label(strip_comment(text(*c, src)));
            phases.push((Phase { label, start, end, lines: lines.non_blank(src, start, end), est_cognitive: None, shared_locals: Vec::new() }, *parent));
        }
        phases.sort_by_key(|(ph, _)| ph.start);
        if phases.iter().any(|(ph, _)| ph.lines < p.min_phase_lines) {
            return None;
        }
        if p.estimate_cognitive {
            for (ph, parent) in &mut phases {
                let mut cursor = parent.walk();
                let stmts: Vec<Node> = parent
                    .named_children(&mut cursor)
                    .filter(|s| !comment_kinds.contains(&s.kind()))
                    .filter(|s| { let l = lines.line_of(s.start_byte()); ph.start <= l && l <= ph.end })
                    .collect();
                ph.est_cognitive = Some(metrics::cognitive_rebased(stmts, lang, src));
            }
        }
        let mut phases: Vec<Phase> = phases.into_iter().map(|(ph, _)| ph).collect();
        let shared = self.shared_locals(unit, lang, src, lines, &mut phases);
        let reason = self.phase_reason(fm, &phases, &shared);
        Some(UnitPhases { unit: fm.name.clone(), start_line: fm.start_line, end_line: fm.end_line, cognitive: fm.cognitive, phases, shared_locals: shared, reason })
    }

    /// Names bound (parameter, `let`, assignment) before a phase and read in at least two
    /// phases that do not bind them themselves. Fills each phase's `shared_locals`.
    fn shared_locals(&self, unit: Node, lang: Language, src: &[u8], lines: &LineIndex, phases: &mut [Phase]) -> Vec<String> {
        let binders = binder_kinds(lang);
        let ident_kinds = ident_kinds(lang);
        let mut bound: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut reads: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut order: Vec<&str> = Vec::new();
        let mut stack: Vec<Node> = vec![unit];
        while let Some(n) = stack.pop() {
            let kind = n.kind();
            if ident_kinds.contains(&kind) {
                reads.entry(text(n, src)).or_default().push(lines.line_of(n.start_byte()));
            }
            if let Some(field) = binders.iter().find(|(k, _)| *k == kind).map(|(_, f)| *f)
                && let Some(pat) = if field.is_empty() { Some(n) } else { n.child_by_field_name(field) }
            {
                // Every identifier of the pattern is a binding (a tuple, a destructuring);
                // a capitalised one is a constructor or type (`Some(x)`, `Point { x }`).
                let mut ps = vec![pat];
                while let Some(q) = ps.pop() {
                    if (ident_kinds.contains(&q.kind()) || q.kind() == "shorthand_property_identifier_pattern")
                        && !text(q, src).starts_with(|c: char| c.is_uppercase())
                    {
                        let name = text(q, src);
                        let e = bound.entry(name).or_default();
                        if e.is_empty() {
                            order.push(name);
                        }
                        e.push(lines.line_of(q.start_byte()));
                    }
                    let mut c = q.walk();
                    let kids: Vec<Node> = q.children(&mut c).collect();
                    ps.extend(kids.into_iter().rev());
                }
            }
            let mut c = n.walk();
            let children: Vec<Node> = n.children(&mut c).collect();
            stack.extend(children.into_iter().rev());
        }
        let mut shared = Vec::new();
        for name in order {
            let binds = &bound[name];
            let rs = reads.get(name).map_or(&[][..], Vec::as_slice);
            let uses: Vec<usize> = phases
                .iter()
                .enumerate()
                .filter(|(_, ph)| {
                    let own = binds.iter().any(|&l| ph.start <= l && l <= ph.end);
                    let before = binds.iter().any(|&l| l < ph.start);
                    !own && before && rs.iter().any(|&l| ph.start <= l && l <= ph.end)
                })
                .map(|(i, _)| i)
                .collect();
            if uses.len() >= 2 {
                shared.push(name.to_string());
                for i in uses {
                    phases[i].shared_locals.push(name.to_string());
                }
            }
        }
        shared
    }

    fn phase_label(&self, text: &str) -> String {
        let rule_chars = &self.cfg.banners.rule_chars;
        let t = text.trim_matches(|c: char| c.is_whitespace() || rule_chars.contains(c));
        let head = t.split_once(':').map(|(h, _)| h.trim()).filter(|h| has_word(h)).unwrap_or(t);
        let head = head.trim_matches(|c: char| c.is_whitespace() || rule_chars.contains(c));
        let label = if head.is_empty() { t } else { head };
        if label.chars().count() > 40 {
            let cut: String = label.chars().take(39).collect();
            format!("{}…", cut.trim_end())
        } else {
            label.to_string()
        }
    }

    fn phase_reason(&self, fm: &FunctionMetrics, phases: &[Phase], shared: &[String]) -> String {
        let p = &self.cfg.phases;
        let spans: Vec<String> = phases.iter().map(|ph| format!("{} {}-{}", ph.label, ph.start, ph.end)).collect();
        let mut notes = Vec::new();
        if p.estimate_cognitive {
            notes.push(format!("est. cognitive {}", phases.iter().map(|ph| ph.est_cognitive.unwrap_or(0).to_string()).collect::<Vec<_>>().join(" / ")));
        }
        notes.push(match shared.len() {
            0 => "no locals shared".to_string(),
            n => {
                let named: Vec<&str> = shared.iter().take(p.max_shared_locals_named).map(String::as_str).collect();
                let more = if n > named.len() { format!(", +{} more", n - named.len()) } else { String::new() };
                format!("{n} local{} shared: {}{more}", if n == 1 { "" } else { "s" }, named.join(", "))
            }
        });
        format!("{} (lines {}-{}, cognitive {}) already labels {} phases: {} — extract each as a helper ({})", fm.name, fm.start_line, fm.end_line, fm.cognitive, phases.len(), spans.join(", "), notes.join("; "))
    }
}

/// The per-file findings with the size percentile applied and the banner reason worded.
pub fn analyze(sides: &[FileSide], cfg: &Cfg) -> CommentsReport {
    let b = &cfg.banners;
    let pct = crate::report::percentiles(&sides.iter().map(|s| s.source_lines).collect::<Vec<_>>());
    let mut totals = Totals { files: sides.len(), ..Totals::default() };
    let mut files = HashMap::with_capacity(sides.len());
    for (i, s) in sides.iter().enumerate() {
        let largest = s.sections.iter().max_by_key(|x| x.lines);
        let qualifies = b.enabled && s.lines >= b.min_file_lines && s.sections.len() >= b.min_sections && largest.is_some_and(|l| l.lines >= b.min_section_lines);
        let size_percentile = pct[i] * 100.0;
        let above = size_percentile >= b.min_size_percentile;
        totals.banners += s.banners;
        totals.files_with_banners += usize::from(s.banners > 0);
        totals.banner_hits += usize::from(qualifies && above);
        totals.banner_below_percentile += usize::from(qualifies && !above);
        totals.phase_comments += s.phase_comments;
        totals.phase_hits += s.units.len();
        files.insert(s.path.clone(), FileComments {
            banners: s.banners,
            sections: s.sections.clone(),
            qualifies,
            size_percentile,
            above_size_percentile: above,
            banner_reason: qualifies.then(|| banner_reason(&s.path, &s.sections)),
            phase_comments: s.phase_comments,
            units: s.units.clone(),
        });
    }
    CommentsReport { totals, files }
}

/// `scan.rs is cut into 4 labelled sections: 'The seals' 62-232, …, 'The ladder' 314-978 (665
/// lines, 12 fns), … — extract the largest as ladder.rs`.
fn banner_reason(path: &str, sections: &[Section]) -> String {
    // `dead/mod.rs`, `x/index.ts`, `pkg/__init__.py`: the directory is the name.
    let mut parts = path.rsplit('/');
    let base = parts.next().unwrap_or(path);
    let base = match (base.rsplit_once('.').map(|(s, _)| s), parts.next()) {
        (Some("mod" | "index" | "__init__"), Some(dir)) => format!("{dir}/{base}"),
        _ => base.to_string(),
    };
    let ext = base.rsplit_once('.').map_or("", |(_, e)| e);
    let largest = sections.iter().enumerate().max_by_key(|(_, s)| s.lines).map(|(i, _)| i).unwrap_or(0);
    let names: Vec<String> = sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mark = if i == largest { format!(" ({} lines, {} fn{})", s.lines, s.functions, if s.functions == 1 { "" } else { "s" }) } else { String::new() };
            format!("'{}' {}-{}{mark}", s.name, s.start, s.end)
        })
        .collect();
    let target = if ext.is_empty() { slug(&sections[largest].name) } else { format!("{}.{ext}", slug(&sections[largest].name)) };
    format!("{base} is cut into {} labelled sections: {} — extract the largest as {target}", sections.len(), names.join(", "))
}

/// A file stem for a section title: the first two words after a leading article, lower-case
/// and alphanumeric (`The ladder's value types` → `ladder_value`).
fn slug(title: &str) -> String {
    let words: Vec<String> = title.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    let mut it = words.iter().map(String::as_str).peekable();
    if words.len() > 1 && matches!(it.peek().copied(), Some("the" | "a" | "an")) {
        it.next();
    }
    let s: Vec<&str> = it.filter(|w| w.len() > 1).take(2).collect();
    if s.is_empty() { words.first().cloned().unwrap_or_else(|| "section".to_string()) } else { s.join("_") }
}

// ---------- shared helpers ----------

struct TopComment {
    start: usize,
    end: usize,
    text: String,
    /// `Some(inline title)` when the comment is a banner.
    banner: Option<Option<String>>,
}

struct Boundary {
    first_line: usize,
    last_line: usize,
    first_index: usize,
    last_index: usize,
    title: Option<String>,
}

/// Line numbers from byte offsets: tree-sitter rows on comment nodes holding multi-byte rule
/// characters have been wrong in the past, so this module never reads `start_position`.
pub struct LineIndex {
    /// Byte offset of every line start; `starts[0] == 0`.
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(src: &[u8]) -> Self {
        let mut starts = vec![0];
        starts.extend(src.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i + 1));
        LineIndex { starts }
    }

    /// 1-based line holding `byte`.
    pub fn line_of(&self, byte: usize) -> usize {
        self.starts.partition_point(|s| *s <= byte).max(1)
    }

    /// Lines in the file, the way `SourceFile.lines` counts them (a trailing newline ends
    /// the last line rather than starting a new one).
    pub fn count(&self) -> usize {
        self.starts.len() - 1
    }

    /// Non-blank lines in `start..=end` (1-based, inclusive).
    pub fn non_blank(&self, src: &[u8], start: usize, end: usize) -> usize {
        (start..=end)
            .filter(|&l| {
                let s = self.starts.get(l - 1).copied().unwrap_or(src.len());
                let e = self.starts.get(l).copied().unwrap_or(src.len() + 1).saturating_sub(1).max(s).min(src.len());
                src[s..e].iter().any(|b| !b.is_ascii_whitespace())
            })
            .count()
    }
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn has_word(t: &str) -> bool {
    t.chars().any(char::is_alphanumeric)
}

/// Rust doc comments (`///`, `//!`, `/** */`) carry a `*_doc_comment_marker` child.
fn is_doc_comment(c: Node) -> bool {
    let mut cursor = c.walk();
    c.children(&mut cursor).any(|k| k.kind().ends_with("doc_comment_marker"))
}

/// The comment's text without its marker: the first non-empty line of a block comment.
fn strip_comment(raw: &str) -> &str {
    let t = raw.trim();
    let t = if let Some(r) = t.strip_prefix("/*") {
        r.strip_suffix("*/").unwrap_or(r)
    } else if let Some(r) = t.strip_prefix("//") {
        r.trim_start_matches('/')
    } else if let Some(r) = t.strip_prefix('#') {
        r
    } else {
        t
    };
    t.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// A character class of `chars`, with the class metacharacters escaped.
fn rule_class(chars: &str) -> String {
    let mut s = String::from("[");
    for c in chars.chars() {
        if matches!(c, '\\' | ']' | '[' | '^' | '-' | '&' | '~') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push(']');
    s
}

fn comment_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &["line_comment", "block_comment"],
        _ => &["comment"],
    }
}

fn import_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &["use_declaration", "extern_crate_declaration"],
        Language::Python => &["import_statement", "import_from_statement", "future_import_statement"],
        _ => &["import_statement"],
    }
}

/// Nodes that hold statements: a phase comment's parent must be one.
fn block_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &["block", "match_block"],
        Language::Python => &["block"],
        _ => &["statement_block", "switch_body", "switch_case", "switch_default"],
    }
}

fn ident_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust | Language::Python => &["identifier"],
        _ => &["identifier", "shorthand_property_identifier"],
    }
}

/// `(node kind, pattern field)` of the binders; an empty field means the node itself.
fn binder_kinds(lang: Language) -> &'static [(&'static str, &'static str)] {
    match lang {
        Language::Rust => &[("parameters", ""), ("let_declaration", "pattern"), ("for_expression", "pattern"), ("assignment_expression", "left")],
        Language::Python => &[("parameters", ""), ("assignment", "left"), ("for_statement", "left")],
        _ => &[("formal_parameters", ""), ("variable_declarator", "name"), ("for_in_statement", "left"), ("assignment_expression", "left")],
    }
}

/// The standalone `scry comments` text.
pub fn render(r: &CommentsReport, top: usize, cfg: &Cfg) -> String {
    use std::fmt::Write;
    let t = &r.totals;
    let mut o = String::new();
    let _ = writeln!(o, "{} top-level banners in {} of {} source files; {} cut into >= {} labelled sections (largest >= {} lines, file >= {} lines, size percentile >= {}){}",
        t.banners, t.files_with_banners, t.files, t.banner_hits, cfg.banners.min_sections, cfg.banners.min_section_lines, cfg.banners.min_file_lines, cfg.banners.min_size_percentile,
        if t.banner_below_percentile > 0 { format!(" (+{} below the percentile, printed only as hotspots)", t.banner_below_percentile) } else { String::new() });
    let _ = writeln!(o, "{} phase comments in functions over the cognitive floor; {} functions with >= {} phases of >= {} lines spanning >= {} lines\n",
        t.phase_comments, t.phase_hits, cfg.phases.min_phases, cfg.phases.min_phase_lines, cfg.phases.min_span_lines);
    let mut rows: Vec<(&String, &FileComments)> = r.files.iter().collect();
    rows.sort_by(|(pa, _), (pb, _)| pa.cmp(pb));
    let _ = writeln!(o, "BANNERS  (files cut into labelled sections; the largest is the split)");
    let banners: Vec<&(&String, &FileComments)> = rows.iter().filter(|(_, f)| f.qualifies).collect();
    if banners.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for (p, f) in banners.iter().take(top) {
        let note = if f.above_size_percentile { String::new() } else { format!("  (size percentile {:.0}: hotspot list only)", f.size_percentile) };
        let _ = writeln!(o, "  {p}: {}{note}", f.banner_reason.as_deref().unwrap_or(""));
    }
    let _ = writeln!(o, "\nPHASES  (functions over the cognitive floor whose body labels its phases)");
    let mut units: Vec<(&String, &UnitPhases)> = rows.iter().flat_map(|(p, f)| f.units.iter().map(move |u| (*p, u))).collect();
    units.sort_by(|(pa, a), (pb, b)| b.cognitive.cmp(&a.cognitive).then_with(|| pa.cmp(pb)).then_with(|| a.start_line.cmp(&b.start_line)));
    if units.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for (p, u) in units.iter().take(top) {
        let _ = writeln!(o, "  {p}: {}", u.reason);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Metrics as MetricsCfg, Tests as TestsCfg};
    use crate::discover::FileKind;

    fn file(path: &str, lang: Language, content: &str) -> SourceFile {
        SourceFile { path: path.into(), lang, kind: FileKind::Source, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn side_with(f: &SourceFile, cfg: &Cfg, metrics: &MetricsCfg) -> FileSide {
        let w = Walker::new(cfg, metrics);
        let (_, _, mut out) = metrics::analyze_all_with(std::slice::from_ref(f), metrics, &TestsCfg::default(), &crate::config::Naming::default(), |root, f, regions, funcs, nodes| w.file_side(root, f, regions, funcs, nodes));
        out.remove(0)
    }

    fn side(f: &SourceFile) -> FileSide {
        side_with(f, &Cfg::default(), &MetricsCfg::default())
    }

    fn rule() -> &'static str {
        "─────────────────────────────────"
    }

    #[test]
    fn rust_rule_title_rule_triples_partition_the_file_and_doc_comments_never_count() {
        let src = format!(
            "use std::io;\n\n// {r}\n// The seals\n// {r}\n\npub struct A;\nfn a() {{}}\n\n/// {r}\nfn doc_rule() {{}}\n\n// ── The ladder ──{r}\n\nfn b() {{}}\nfn c() {{}}\n\n// {r}\n// The override\n// {r}\n\nfn d() {{}}\n#[cfg(test)]\nmod tests {{\n    // {r}\n    // not a banner: inside mod tests\n    // {r}\n    fn t() {{}}\n}}\n",
            r = rule()
        );
        let s = side(&file("scan.rs", Language::Rust, &src));
        assert_eq!(s.banners, 5, "{:?}", s.sections);
        let got: Vec<(&str, usize, usize, usize, usize)> = s.sections.iter().map(|x| (x.name.as_str(), x.start, x.end, x.lines, x.functions)).collect();
        assert_eq!(got, vec![("The seals", 6, 12, 7, 2), ("The ladder", 14, 17, 4, 2), ("The override", 21, 29, 9, 1)], "{got:?}");
        // The unit inside `mod tests` is not counted; the doc-comment rule is not a boundary.
    }

    #[test]
    fn rule_characters_are_counted_as_characters_and_short_rules_need_text() {
        let s = side(&file("a.rs", Language::Rust, "// ──\n// ────── \nfn a() {}\n// ─────\nfn b() {}\n// ── Title\nfn c() {}\n// -- Also\nfn d() {}\n// ------\nfn e() {}\n"));
        // `──` (2 chars, 6 bytes) and `─────` (5) are no rule; `──────` (6) is a bare rule;
        // the short forms carry a title; the bare `------` has none.
        let names: Vec<&str> = s.sections.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["Title", "Also"], "{:?}", s.sections);
        assert_eq!(s.banners, 4, "{:?}", s.sections);
    }

    #[test]
    fn editor_markers_and_untitled_rules_are_not_boundaries_unless_asked() {
        let src = "// #region Foo\nfunction a() {}\n// #endregion\n// %% cell\n// ==========\nfunction b() {}\n// ====== #endregion ======\nfunction c() {}\n";
        let s = side(&file("a.ts", Language::TypeScript, src));
        assert!(s.sections.is_empty(), "{:?}", s.sections);
        let mut cfg = Cfg::default();
        cfg.banners.require_title = false;
        let s = side_with(&file("a.ts", Language::TypeScript, src), &cfg, &MetricsCfg::default());
        assert_eq!(s.sections.iter().map(|x| (x.name.as_str(), x.start)).collect::<Vec<_>>(), vec![("", 6)], "{:?}", s.sections);
    }

    #[test]
    fn a_top_of_file_header_pair_before_the_imports_is_skipped() {
        let src = "// ==========\n// Copyright\n// ==========\nimport os\n# ---------- helpers ----------\ndef a():\n    pass\n# ---------- main ----------\ndef b():\n    pass\n".replace("//", "#");
        let s = side(&file("m.py", Language::Python, &src));
        assert_eq!(s.sections.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["helpers", "main"], "{:?}", s.sections);
        let mut cfg = Cfg::default();
        cfg.banners.skip_top_of_file = false;
        let s = side_with(&file("m.py", Language::Python, &src), &cfg, &MetricsCfg::default());
        assert_eq!(s.sections.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["Copyright", "helpers", "main"]);
        // A long header: rule, many lines, rule, then the import: both rules go.
        let long = "# ===== LICENSE =====\n# a\n# b\n# c\n# d\n# ===== END =====\nimport os\n# ---------- helpers ----------\ndef a():\n    pass\n";
        let s = side(&file("l.py", Language::Python, long));
        assert_eq!(s.sections.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["helpers"], "{:?}", s.sections);
    }

    #[test]
    fn rows_come_from_byte_offsets_on_multibyte_rules() {
        let src = format!("fn a() {{}}\n// {r} Título ünïcode {r}\nfn b() {{}}\n", r = rule());
        let s = side(&file("u.rs", Language::Rust, &src));
        assert_eq!(s.sections.iter().map(|x| (x.name.as_str(), x.start, x.end)).collect::<Vec<_>>(), vec![("Título ünïcode", 3, 3)], "{:?}", s.sections);
        let li = LineIndex::new(src.as_bytes());
        assert_eq!((li.count(), li.line_of(0), li.line_of(src.len() - 1)), (3, 1, 3));
    }

    #[test]
    fn analyze_applies_lines_sections_largest_and_percentile_thresholds() {
        let mut cfg = Cfg::default();
        cfg.banners.min_file_lines = 10;
        cfg.banners.min_section_lines = 3;
        cfg.banners.min_sections = 2;
        cfg.banners.min_size_percentile = 60.0;
        let body = "fn x() {}\n".repeat(4);
        let big = format!("// ==== one ====\n{body}{body}// ==== two ====\n{body}// ==== three ====\n{body}");
        let files = [file("big.rs", Language::Rust, &big), file("small.rs", Language::Rust, "// ==== one ====\nfn a() {}\n// ==== two ====\nfn b() {}\n"), file("mid.rs", Language::Rust, &"fn y() {}\n".repeat(20))];
        let w = Walker::new(&cfg, &MetricsCfg::default());
        let (_, _, sides) = metrics::analyze_all_with(&files, &MetricsCfg::default(), &TestsCfg::default(), &crate::config::Naming::default(), |root, f, regions, funcs, nodes| w.file_side(root, f, regions, funcs, nodes));
        let r = analyze(&sides, &cfg);
        let big = &r.files["big.rs"];
        assert!(big.qualifies && !big.above_size_percentile, "{big:?}");
        assert_eq!(big.banner_reason.as_deref(), Some("big.rs is cut into 3 labelled sections: 'one' 2-9 (8 lines, 8 fns), 'two' 11-14, 'three' 16-19 — extract the largest as one.rs"));
        assert!(!r.files["small.rs"].qualifies);
        assert_eq!((r.totals.banners, r.totals.files_with_banners, r.totals.banner_hits, r.totals.banner_below_percentile), (5, 2, 0, 1));
        cfg.banners.min_size_percentile = 50.0;
        assert_eq!(analyze(&sides, &cfg).totals.banner_hits, 1);
        cfg.banners.min_section_lines = 9;
        assert!(!analyze(&sides, &cfg).files["big.rs"].qualifies);
    }

    fn phased_rust() -> String {
        let pad = "        if a > 1 { b += 1; }\n".repeat(8);
        format!(
            "fn work(a: u8, mut b: u8) -> u8 {{\n    let mut out = 0;\n    let tmp = 3;\n    // ── step 1: gather ──\n    let seen = a;\n{pad}    out += seen;\n    // ── step 2: reduce ──\n    let seen = b;\n{pad}    out += seen + tmp;\n    match a {{\n        // ── arm banner: depth 1 ──\n        1 => {{ out += 1; }}\n        _ => {{}}\n    }}\n    let cb = |z: u8| {{\n        // ── inside a closure: another scope ──\n        z\n    }};\n    // ── step 3: finish ──\n{pad}    out + tmp + cb(b)\n}}\n"
        )
    }

    #[test]
    fn rust_phases_span_to_the_next_label_estimate_cognitive_and_find_shared_locals() {
        let m = MetricsCfg { cognitive_hard: 5, ..MetricsCfg::default() };
        let mut cfg = Cfg::default();
        cfg.phases.min_span_lines = 10;
        cfg.phases.min_phase_lines = 3;
        let f = file("w.rs", Language::Rust, &phased_rust());
        let s = side_with(&f, &cfg, &m);
        assert_eq!(s.units.len(), 1, "{:?}", s.units);
        let u = &s.units[0];
        let got: Vec<(&str, usize, usize, usize, Option<u32>)> = u.phases.iter().map(|p| (p.label.as_str(), p.start, p.end, p.lines, p.est_cognitive)).collect();
        // 4 phases: three at the body and the match-arm banner one level below (the closure's
        // banner is another scope). Each `if` is +1 at nesting 0.
        // step 2 runs to the line before step 3 (the arm banner is in the match block, not the
        // body); the arm banner runs to its block's end; step 3 to the body's closing brace.
        assert_eq!(got, vec![("step 1", 4, 14, 11, Some(8)), ("step 2", 15, 34, 20, Some(9)), ("arm banner", 27, 30, 4, Some(0)), ("step 3", 35, 45, 11, Some(8))], "{got:?}");
        // `out` and `tmp` are bound before every phase and read in >= 2; `seen` is re-bound in
        // each phase that reads it; `a`/`b` are parameters read in several phases.
        assert_eq!(u.shared_locals, vec!["a", "b", "out", "tmp"], "{:?}", u.shared_locals);
        assert_eq!(u.phases[0].shared_locals, vec!["a", "b", "out"]);
        assert_eq!(u.reason, "work (lines 1-45, cognitive 25) already labels 4 phases: step 1 4-14, step 2 15-34, arm banner 27-30, step 3 35-45 — extract each as a helper (est. cognitive 8 / 9 / 0 / 8; 4 locals shared: a, b, out, tmp)");
        assert_eq!(s.phase_comments, 4);
    }

    #[test]
    fn phase_floors_depth_and_the_cognitive_gate_drop_units() {
        let f = file("w.rs", Language::Rust, &phased_rust());
        let m = MetricsCfg { cognitive_hard: 5, ..MetricsCfg::default() };
        // The arm banner is 4 lines: min_phase_lines = 8 drops the unit; max_depth = 0 hides it.
        let mut cfg = Cfg::default();
        cfg.phases.min_span_lines = 10;
        cfg.phases.min_phase_lines = 8;
        assert!(side_with(&f, &cfg, &m).units.is_empty());
        cfg.phases.max_depth = 0;
        let s = side_with(&f, &cfg, &m);
        assert_eq!(s.units[0].phases.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), vec!["step 1", "step 2", "step 3"]);
        assert_eq!(s.units[0].phases[1].end, 34, "step 2 runs to the line before step 3");
        // Unit cognitive below the floor: nothing; the floor follows cognitive_hard when unset.
        cfg.phases.cross_with_cognitive_min = Some(40);
        assert!(side_with(&f, &cfg, &m).units.is_empty());
        cfg.phases.cross_with_cognitive_min = None;
        assert!(side_with(&f, &cfg, &MetricsCfg { cognitive_hard: 30, ..MetricsCfg::default() }).units.is_empty());
        // Span floor and phase count floor.
        cfg.phases.min_span_lines = 100;
        assert!(side_with(&f, &cfg, &m).units.is_empty());
        cfg.phases.min_span_lines = 10;
        cfg.phases.min_phases = 4;
        assert!(side_with(&f, &cfg, &m).units.is_empty());
        // No estimate when the knob is off.
        cfg.phases.min_phases = 2;
        cfg.phases.estimate_cognitive = false;
        let s = side_with(&f, &cfg, &m);
        assert!(s.units[0].phases.iter().all(|p| p.est_cognitive.is_none()));
        assert!(s.units[0].reason.ends_with("— extract each as a helper (4 locals shared: a, b, out, tmp)"), "{}", s.units[0].reason);
        cfg.phases.max_shared_locals_named = 2;
        assert!(side_with(&f, &cfg, &m).units[0].reason.ends_with("(4 locals shared: a, b, +2 more)"));
    }

    #[test]
    fn prose_dashes_and_ordinals_are_not_phases_and_test_modules_are_skipped() {
        let pad = "        if a > 1 { b += 1; }\n".repeat(8);
        let body = format!("    // --flag prose is not a phase\n    // == neither is this\n    // Finally, nor this\n    // 1) numbered\n{pad}    // Step 2 numbered word\n{pad}    b\n");
        let src = format!("fn f(a: u8, mut b: u8) -> u8 {{\n{body}}}\n#[cfg(test)]\nmod tests {{\n    fn t(a: u8, mut b: u8) -> u8 {{\n{body}    }}\n}}\n");
        let f = file("p.rs", Language::Rust, &src);
        let m = MetricsCfg { cognitive_hard: 5, ..MetricsCfg::default() };
        let mut cfg = Cfg::default();
        cfg.phases.min_span_lines = 10;
        let s = side_with(&f, &cfg, &m);
        assert_eq!(s.units.iter().map(|u| u.unit.as_str()).collect::<Vec<_>>(), vec!["f"], "{:?}", s.units);
        assert_eq!(s.units[0].phases.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), vec!["1) numbered", "Step 2 numbered word"]);
        cfg.phases.skip_test_modules = false;
        assert_eq!(side_with(&f, &cfg, &m).units.len(), 2);
    }

    #[test]
    fn python_and_typescript_phases_use_their_own_blocks_and_binders() {
        let pad = "        if a > 1:\n            b += 1\n".repeat(4);
        let py = format!("def work(a, b):\n    out = 0\n    # ── phase 1 ──\n{pad}    out += a\n    # phase 2: second\n{pad}    out += b\n    return out\n");
        let m = MetricsCfg { cognitive_hard: 4, ..MetricsCfg::default() };
        let mut cfg = Cfg::default();
        cfg.phases.min_span_lines = 10;
        cfg.phases.min_phase_lines = 3;
        let s = side_with(&file("w.py", Language::Python, &py), &cfg, &m);
        assert_eq!(s.units.len(), 1, "{:?}", s.units);
        assert_eq!(s.units[0].phases.iter().map(|p| (p.label.as_str(), p.start, p.end, p.est_cognitive)).collect::<Vec<_>>(), vec![("phase 1", 3, 12, Some(4)), ("phase 2", 13, 23, Some(4))]);
        assert_eq!(s.units[0].shared_locals, vec!["a", "b", "out"]);
        let pad = "    if (a > 1) { b += 1; }\n".repeat(4);
        let ts = format!("function work(a: number, b: number) {{\n  let out = 0;\n  // step 1\n{pad}  out += a;\n  switch (a) {{\n    // 2. in the switch body\n    case 1: out += 1; break;\n{pad}  }}\n  return out;\n}}\n");
        let s = side_with(&file("w.ts", Language::TypeScript, &ts), &cfg, &m);
        assert_eq!(s.units.len(), 1, "{:?}", s.units);
        // The switch-body phase sits one level below the body and ends with the switch.
        assert_eq!(s.units[0].phases.iter().map(|p| (p.label.as_str(), p.start, p.end)).collect::<Vec<_>>(), vec![("step 1", 3, 18), ("2. in the switch body", 10, 16)]);
    }

    #[test]
    fn render_lists_hits_and_says_none() {
        let cfg = Cfg::default();
        let text = render(&analyze(&[], &cfg), 10, &cfg);
        assert!(text.contains("BANNERS") && text.contains("PHASES") && text.matches("  none\n").count() == 2, "{text}");
    }

    #[test]
    fn a_mod_file_is_named_by_its_directory_and_constructors_are_not_locals() {
        let secs = vec![Section { name: "The walk".into(), start: 2, end: 300, lines: 299, functions: 3 }, Section { name: "x".into(), start: 302, end: 310, lines: 9, functions: 0 }];
        assert!(banner_reason("src/dead/mod.rs", &secs).starts_with("dead/mod.rs is cut into 2 labelled sections: 'The walk' 2-300 (299 lines, 3 fns), 'x' 302-310 — extract the largest as walk.rs"), "{}", banner_reason("src/dead/mod.rs", &secs));
        assert!(banner_reason("pkg/__init__.py", &secs).starts_with("pkg/__init__.py is cut"));
        assert!(banner_reason("a/b/index.ts", &secs).ends_with("walk.ts"));
        let pad = "        if a > 1 { b += 1; }\n".repeat(8);
        let src = format!("fn f(a: u8, mut b: u8) -> u8 {{\n    let Some(fact) = Some(a) else {{ return 0 }};\n    // ── step 1 ──\n{pad}    b += fact;\n    // ── step 2 ──\n{pad}    b + fact\n}}\n");
        let mut cfg = Cfg::default();
        cfg.phases.min_span_lines = 10;
        let s = side_with(&file("c.rs", Language::Rust, &src), &cfg, &MetricsCfg { cognitive_hard: 5, ..MetricsCfg::default() });
        assert_eq!(s.units[0].shared_locals, vec!["a", "b", "fact"], "{:?}", s.units[0].shared_locals);
    }

    #[test]
    fn slugs_drop_the_article_and_keep_two_words() {
        assert_eq!(slug("The ladder"), "ladder");
        assert_eq!(slug("The ladder's value types"), "ladder_value");
        assert_eq!(slug("the clock, read from the snapshot"), "clock_read");
        assert_eq!(slug("drop"), "drop");
        assert_eq!(slug("A"), "a");
        assert_eq!(slug("---"), "section");
    }
}
