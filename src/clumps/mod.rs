//! Parameter tuple clumps: one group of parameter names recurring across functions, with the
//! slots no member reads.
//!
//! Agents extend a family by copying the last sibling's signature, so five `plan_*` functions
//! carry `(s, f, a, _m)` and every one of them leaves `_m` unread. Per function the pass keeps
//! the named parameters (receivers out) with their type text; every `min_group`..`max_group`
//! name combination is grouped repo-wide, a group is a clump at `min_functions` members or
//! `min_files` files when enough slots agree on type, and each slot says how many members leave
//! it unused (`unused_prefix`-named or unreferenced in the body). A lead, not a defect
//! predictor: weight 0 by default.

use crate::config::Clumps as Cfg;
use crate::discover::SourceFile;
use crate::lang::Language;
use crate::metrics;
use crate::regions::{self, TestRegion};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tree_sitter::Node;

// ---------- per-file collection ----------

/// One named parameter of a function.
#[derive(Debug, Clone, Serialize)]
pub struct Param {
    pub name: String,
    /// Declared type text, whitespace-normalized (`&Snapshot`); None when unannotated.
    pub ty: Option<String>,
    /// Named with `unused_prefix`, or referenced nowhere in the body.
    pub unused: bool,
}

/// A function of a Source file: name as metrics reports it, its named parameters in declared
/// order, and every identifier its body references (for the unused slots and the single-caller
/// note; text match only).
#[derive(Debug, Clone)]
pub struct FnSite {
    pub name: String,
    pub line: usize,
    pub params: Vec<Param>,
    pub refs: HashSet<String>,
}

/// A file's functions, collected on the metrics pass's tree (`scan`) or one parse (`scry clumps`).
#[derive(Debug, Clone, Default)]
pub struct FileSide {
    pub path: String,
    pub functions: Vec<FnSite>,
    /// Functions left out: trait / override methods and callbacks, protocol tuples, overloads /
    /// dunders / stubs, same-name duplicates.
    pub skipped_trait_impls: usize,
    pub skipped_protocol: usize,
    pub skipped_other: usize,
    pub skipped_duplicates: usize,
    /// Identifiers referenced outside the analysed functions: the bodies of skipped units and
    /// the file's statements outside any unit (a dispatch table, a module-level call), imports
    /// and test regions apart. A member named here has a caller the single-caller note cannot
    /// name, so the note is withheld.
    pub other_refs: HashSet<String>,
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_receiver(name: &str) -> bool {
    matches!(name, "self" | "cls" | "this")
}

/// `{name}` / `{name:` captures of a Rust 2021 format string: each name is a reference.
fn captures(s: &str, out: &mut HashSet<String>) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if b.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            let start = i + 1;
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > start && j < b.len() && (b[j] == b'}' || b[j] == b':') {
                out.insert(s[start..j].to_string());
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
}

/// Every identifier-like leaf under `body`: `identifier`, shorthand field / property names, and
/// `{name}` captures inside string literals under a macro `token_tree`.
fn body_refs(body: Node, src: &[u8], lang: Language) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack: Vec<(Node, bool)> = vec![(body, false)];
    while let Some((n, in_tt)) = stack.pop() {
        let kind = n.kind();
        match kind {
            "identifier" | "shorthand_field_identifier" | "shorthand_property_identifier" => {
                out.insert(text(n, src).to_string());
                continue;
            }
            "comment" | "line_comment" | "block_comment" => continue,
            "string_literal" | "raw_string_literal" if lang == Language::Rust && in_tt => {
                captures(text(n, src), &mut out);
                continue;
            }
            _ => {}
        }
        let tt = in_tt || kind == "token_tree";
        let mut c = n.walk();
        let children: Vec<Node> = n.children(&mut c).collect();
        for ch in children.into_iter().rev() {
            stack.push((ch, tt));
        }
    }
    out
}

/// The named parameters of a unit node with their type text, receivers left out, in declared
/// order, at most `max` of them. Destructured and variadic parameters have no name and no slot.
fn params_of(node: Node, src: &[u8], lang: Language, max: usize) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let Some(params) = node.child_by_field_name("parameters") else {
        // `x => x`: a TS arrow with one bare parameter.
        if let Some(p) = node.child_by_field_name("parameter") && p.kind() == "identifier" {
            let name = text(p, src);
            if !is_receiver(name) {
                out.push((name.to_string(), None));
            }
        }
        return out;
    };
    let mut c = params.walk();
    for p in params.named_children(&mut c) {
        if out.len() >= max {
            break;
        }
        let (name, ty) = match (lang, p.kind()) {
            (Language::Rust, "parameter") => {
                let Some(pat) = p.child_by_field_name("pattern") else { continue };
                if pat.kind() != "identifier" {
                    continue;
                }
                (text(pat, src), p.child_by_field_name("type").map(|t| norm_ws(text(t, src))))
            }
            (Language::Python, "identifier") => (text(p, src), None),
            (Language::Python, "typed_parameter") => {
                let mut cc = p.walk();
                let Some(id) = p.named_children(&mut cc).find(|n| n.kind() == "identifier") else { continue };
                (text(id, src), p.child_by_field_name("type").map(|t| norm_ws(text(t, src))))
            }
            (Language::Python, "default_parameter" | "typed_default_parameter") => {
                let Some(id) = p.child_by_field_name("name").filter(|n| n.kind() == "identifier") else { continue };
                (text(id, src), p.child_by_field_name("type").map(|t| norm_ws(text(t, src))))
            }
            (Language::Python | Language::Rust, _) => continue,
            (_, "required_parameter" | "optional_parameter") => {
                let Some(pat) = p.child_by_field_name("pattern") else { continue };
                if pat.kind() != "identifier" {
                    continue;
                }
                let ty = p.child_by_field_name("type").map(|t| norm_ws(text(t, src).trim_start().trim_start_matches(':')));
                (text(pat, src), ty)
            }
            (_, "identifier") => (text(p, src), None),
            _ => continue,
        };
        if is_receiver(name) {
            continue;
        }
        out.push((name.to_string(), ty));
    }
    out
}

/// Why a unit does not own its signature: a trait / interface / override method, a callback
/// passed as an argument, an `@overload`, a dunder, or a stub body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Skip {
    TraitImpl,
    Other,
}

fn python_stub(body: Node) -> bool {
    let mut c = body.walk();
    body.named_children(&mut c).all(|s| match s.kind() {
        "pass_statement" | "comment" => true,
        "expression_statement" => s.named_child(0).is_some_and(|e| matches!(e.kind(), "string" | "ellipsis")),
        _ => false,
    })
}

fn skip_reason(node: Node, body: Node, name: &str, src: &[u8], lang: Language, cfg: &Cfg) -> Option<Skip> {
    match lang {
        Language::Rust => {
            let mut cur = node.parent();
            while let Some(p) = cur {
                match p.kind() {
                    "impl_item" => return (cfg.skip_trait_impls && p.child_by_field_name("trait").is_some()).then_some(Skip::TraitImpl),
                    "trait_item" => return cfg.skip_trait_impls.then_some(Skip::TraitImpl),
                    "function_item" | "source_file" => return None,
                    _ => cur = p.parent(),
                }
            }
            None
        }
        Language::Python => {
            if cfg.skip_dunder && name.len() > 4 && name.starts_with("__") && name.ends_with("__") {
                return Some(Skip::Other);
            }
            if cfg.skip_overloads
                && let Some(d) = node.parent().filter(|p| p.kind() == "decorated_definition")
            {
                let mut c = d.walk();
                if d.named_children(&mut c).any(|ch| ch.kind() == "decorator" && text(ch, src).trim_start_matches('@').trim().ends_with("overload")) {
                    return Some(Skip::Other);
                }
            }
            if python_stub(body) {
                return Some(Skip::Other);
            }
            if cfg.skip_trait_impls {
                // A method: (decorated_definition?) -> block -> class_definition with superclasses.
                let holder = node.parent().filter(|p| p.kind() == "decorated_definition").unwrap_or(node);
                if let Some(block) = holder.parent().filter(|p| p.kind() == "block")
                    && let Some(class) = block.parent().filter(|p| p.kind() == "class_definition")
                    && class.child_by_field_name("superclasses").is_some()
                {
                    return Some(Skip::TraitImpl);
                }
            }
            None
        }
        _ => {
            if !cfg.skip_trait_impls {
                return None;
            }
            // A callback passed as an argument: an arrow directly in `arguments`, or a method /
            // function inside an object literal that is itself an argument (Node's
            // `new Writable({ write(chunk, encoding, callback) {} })` protocol).
            let mut cur = node.parent();
            while let Some(p) = cur {
                match p.kind() {
                    "arguments" => return Some(Skip::TraitImpl),
                    "object" | "pair" | "method_definition" | "parenthesized_expression" | "as_expression" | "satisfies_expression" | "type_assertion" => cur = p.parent(),
                    _ => break,
                }
            }
            if node.kind() == "method_definition"
                && let Some(body) = node.parent().filter(|p| p.kind() == "class_body")
                && let Some(class) = body.parent()
            {
                let mut c = class.walk();
                if class.children(&mut c).any(|ch| ch.kind() == "class_heritage") {
                    return Some(Skip::TraitImpl);
                }
            }
            None
        }
    }
}

pub struct Walker {
    cfg: Cfg,
    /// `protocol_tuples` as name sets: a function whose parameter names cover one is a callback.
    protocols: Vec<HashSet<String>>,
}

impl Walker {
    pub fn new(cfg: &Cfg) -> Self {
        let protocols = cfg.protocol_tuples.iter().filter(|t| !t.is_empty()).map(|t| t.iter().cloned().collect()).collect();
        Self { cfg: cfg.clone(), protocols }
    }

    /// The functions of one file, on an already-parsed tree (`None` when parsing failed).
    pub fn file_side(&self, root: Option<Node>, file: &SourceFile, regions: &[TestRegion]) -> FileSide {
        let mut side = FileSide { path: file.path.clone(), ..FileSide::default() };
        let Some(root) = root else { return side };
        let src = file.content.as_bytes();
        let lang = file.lang;
        let mut seen: HashSet<String> = HashSet::new();
        let mut units: HashSet<usize> = HashSet::new();
        for node in metrics::unit_nodes(root, lang) {
            units.insert(node.id());
            if lang == Language::Rust && regions::contains(regions, node.start_byte()) {
                continue;
            }
            let Some(body) = node.child_by_field_name("body") else { continue };
            let name = metrics::unit_name_of(node, lang, src);
            match skip_reason(node, body, name.rsplit('.').next().unwrap_or(&name), src, lang, &self.cfg) {
                Some(Skip::TraitImpl) => {
                    side.skipped_trait_impls += 1;
                    side.other_refs.extend(body_refs(body, src, lang));
                    continue;
                }
                Some(Skip::Other) => {
                    side.skipped_other += 1;
                    side.other_refs.extend(body_refs(body, src, lang));
                    continue;
                }
                None => {}
            }
            if self.cfg.dedupe_same_name_in_file && !seen.insert(name.clone()) {
                side.skipped_duplicates += 1;
                side.other_refs.extend(body_refs(body, src, lang));
                continue;
            }
            let params = params_of(node, src, lang, self.cfg.max_params);
            if self.protocols.iter().any(|p| p.iter().all(|n| params.iter().any(|(pn, _)| pn == n))) {
                side.skipped_protocol += 1;
                side.other_refs.extend(body_refs(body, src, lang));
                continue;
            }
            let refs = body_refs(body, src, lang);
            let params = params
                .into_iter()
                .map(|(name, ty)| {
                    let unused = (!self.cfg.unused_prefix.is_empty() && name.starts_with(&self.cfg.unused_prefix)) || !refs.contains(&name);
                    Param { name, ty, unused }
                })
                .collect();
            side.functions.push(FnSite { name, line: node.start_position().row + 1, params, refs });
        }
        // Statements outside every unit (a dispatch table, a module-level call), imports,
        // comments and test regions left out.
        let mut stack: Vec<Node> = vec![root];
        while let Some(n) = stack.pop() {
            let kind = n.kind();
            if units.contains(&n.id()) {
                continue;
            }
            if matches!(kind, "comment" | "line_comment" | "block_comment" | "use_declaration" | "extern_crate_declaration" | "import_statement" | "import_from_statement" | "future_import_statement") {
                continue;
            }
            if lang == Language::Rust && regions::contains(regions, n.start_byte()) {
                continue;
            }
            if matches!(kind, "identifier" | "shorthand_field_identifier" | "shorthand_property_identifier") {
                side.other_refs.insert(text(n, src).to_string());
                continue;
            }
            let mut c = n.walk();
            let children: Vec<Node> = n.children(&mut c).collect();
            stack.extend(children.into_iter().rev());
        }
        side
    }

    /// One parse per file, for `scry clumps`.
    pub fn parse_side(&self, file: &SourceFile) -> FileSide {
        let src = file.content.as_bytes();
        let tree = file.lang.parser().parse(src, None);
        let root = tree.as_ref().map(|t| t.root_node());
        let regions = match (root, file.lang) {
            (Some(root), Language::Rust) => regions::test_regions(root, src),
            _ => Vec::new(),
        };
        self.file_side(root, file, &regions)
    }
}

pub fn index_all(files: &[SourceFile], cfg: &Cfg) -> Vec<FileSide> {
    let w = Walker::new(cfg);
    files.par_iter().filter(|f| f.kind == crate::discover::FileKind::Source).map(|f| w.parse_side(f)).collect()
}

// ---------- clumps ----------

/// `(file index, function index)` into the sides.
type Site = (usize, usize);

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub file: String,
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Clump {
    /// Slot names in the first member's declared order.
    pub params: Vec<String>,
    /// Per slot, the type text every annotated member agrees on; null when none is annotated
    /// or they differ.
    pub types: Vec<Option<String>>,
    /// Varying slots -> their distinct type texts, first seen first.
    pub variants: BTreeMap<String, Vec<String>>,
    /// Members in file, line order.
    pub functions: Vec<Member>,
    pub files: usize,
    /// Slot -> members that leave it unused (`unused_prefix`-named or unreferenced); slots
    /// every member uses are absent.
    pub unused_slots: BTreeMap<String, usize>,
    /// The one function every member's name is referenced from, when there is exactly one.
    pub single_caller: Option<String>,
    /// The section line.
    pub line: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileClumps {
    /// This file's functions that are members of a reported clump.
    pub members: usize,
    /// `(tuple text, line)` per member function of the file.
    pub clumps: Vec<(String, usize)>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    /// Functions analysed (after the skips).
    pub functions: usize,
    pub in_clumps: usize,
    pub clumps: usize,
    /// Files with a member function.
    pub files: usize,
    /// Functions carrying an `unused_prefix`-named parameter.
    pub silenced_functions: usize,
    pub skipped_trait_impls: usize,
    pub skipped_protocol: usize,
    pub skipped_other: usize,
    pub skipped_duplicates: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClumpsReport {
    pub totals: Totals,
    pub notes: Vec<String>,
    /// Members desc, files desc, then tuple text.
    pub clumps: Vec<Clump>,
    pub files: BTreeMap<String, FileClumps>,
}

fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// `plan_repair src/cmd/repair.rs:79, plan_start src/cmd/flow.rs:300, plan_ship :853`: the path
/// is elided when it repeats the previous site's; at most `listed`, then `+N more`. Names are
/// as metrics reports them (`ReadByLine.new`), so two types' constructors read apart.
fn sites_text(members: &[Member], listed: usize) -> String {
    let mut parts = Vec::new();
    let mut prev = "";
    for m in members.iter().take(listed) {
        if m.file == prev {
            parts.push(format!("{} :{}", m.name, m.line));
        } else {
            parts.push(format!("{} {}:{}", m.name, m.file, m.line));
        }
        prev = &m.file;
    }
    if members.len() > listed {
        parts.push(format!("+{} more", members.len() - listed));
    }
    parts.join(", ")
}

/// Name pieces: `plan_repair` -> [plan, repair], `printEntryFormat` -> [print, entry, format].
fn pieces(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in name.chars() {
        if c == '_' || c == '-' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else if c.is_uppercase() && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur.push(c.to_ascii_lowercase());
        } else {
            cur.push(c.to_ascii_lowercase());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `PlanInput` from members named `plan_*`; None when their names share no leading piece.
fn struct_name(members: &[Member]) -> Option<String> {
    let first = pieces(bare(&members.first()?.name));
    let mut common = first.len();
    for m in &members[1..] {
        let p = pieces(bare(&m.name));
        common = common.min(first.iter().zip(&p).take_while(|(a, b)| a == b).count());
    }
    if common == 0 {
        return None;
    }
    let mut s = String::new();
    for piece in &first[..common] {
        let mut cs = piece.chars();
        if let Some(c) = cs.next() {
            s.extend(c.to_uppercase());
            s.push_str(cs.as_str());
        }
    }
    s.push_str("Input");
    Some(s)
}

pub fn analyze(sides: &[FileSide], cfg: &Cfg) -> ClumpsReport {
    let min_group = cfg.min_group.max(1);
    let max_group = cfg.max_group.max(min_group);
    let mut totals = Totals::default();
    let mut groups: HashMap<Vec<String>, Vec<Site>> = HashMap::new();
    for (fi, side) in sides.iter().enumerate() {
        totals.functions += side.functions.len();
        totals.skipped_trait_impls += side.skipped_trait_impls;
        totals.skipped_protocol += side.skipped_protocol;
        totals.skipped_other += side.skipped_other;
        totals.skipped_duplicates += side.skipped_duplicates;
        for (fni, f) in side.functions.iter().enumerate() {
            if !cfg.unused_prefix.is_empty() && f.params.iter().any(|p| p.name.starts_with(&cfg.unused_prefix)) {
                totals.silenced_functions += 1;
            }
            let n = f.params.len().min(cfg.max_params).min(31);
            if n < min_group {
                continue;
            }
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|a, b| f.params[*a].name.cmp(&f.params[*b].name));
            for mask in 1u32..(1u32 << n) {
                let k = mask.count_ones() as usize;
                if k < min_group || k > max_group {
                    continue;
                }
                let names: Vec<String> = order.iter().enumerate().filter(|(i, _)| mask >> i & 1 == 1).map(|(_, p)| f.params[*p].name.clone()).collect();
                groups.entry(names).or_default().push((fi, fni));
            }
        }
    }
    let param_of = |site: Site, name: &str| sides[site.0].functions[site.1].params.iter().find(|p| p.name == name);
    // Per slot: the distinct annotated type texts, first seen first (members in site order).
    let slot_types = |name: &str, sites: &[Site]| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in sites {
            if let Some(t) = param_of(*s, name).and_then(|p| p.ty.clone()) && !out.contains(&t) {
                out.push(t);
            }
        }
        out
    };
    let passes = |names: &[String], sites: &[Site]| -> bool {
        let files = sites.iter().map(|s| s.0).collect::<HashSet<_>>().len();
        let enough = if cfg.report_if_either { sites.len() >= cfg.min_functions || files >= cfg.min_files } else { sites.len() >= cfg.min_functions && files >= cfg.min_files };
        if !enough {
            return false;
        }
        let per_slot: Vec<Vec<String>> = names.iter().map(|n| slot_types(n, sites)).collect();
        let agreeing = per_slot.iter().filter(|t| t.len() == 1).count();
        if agreeing >= cfg.min_typed_slots {
            return true;
        }
        if per_slot.iter().any(|t| !t.is_empty()) {
            return false;
        }
        sites.len() > cfg.min_functions && names.iter().map(|n| n.chars().count()).sum::<usize>() >= cfg.min_name_len
    };
    // Groups with one member set collapse into the union of their names: the largest tuple
    // those functions share.
    let mut merged: HashMap<Vec<Site>, BTreeSet<String>> = HashMap::new();
    for (names, sites) in &groups {
        if !passes(names, sites) {
            continue;
        }
        let mut key = sites.clone();
        key.sort_unstable();
        key.dedup();
        merged.entry(key).or_default().extend(names.iter().cloned());
    }
    let site_key = |s: &Site| (sides[s.0].path.as_str(), sides[s.0].functions[s.1].line);
    let mut clumps: Vec<Clump> = Vec::new();
    for (mut sites, names) in merged {
        sites.sort_by_key(|s| site_key(s));
        let first = &sides[sites[0].0].functions[sites[0].1];
        let params: Vec<String> = first.params.iter().filter(|p| names.contains(&p.name)).map(|p| p.name.clone()).collect();
        let functions: Vec<Member> = sites.iter().map(|s| Member { file: sides[s.0].path.clone(), name: sides[s.0].functions[s.1].name.clone(), line: sides[s.0].functions[s.1].line }).collect();
        let files = sites.iter().map(|s| s.0).collect::<HashSet<_>>().len();
        let mut types = Vec::new();
        let mut variants = BTreeMap::new();
        let mut unused_slots = BTreeMap::new();
        for name in &params {
            let ts = slot_types(name, &sites);
            match ts.len() {
                1 => types.push(ts.into_iter().next()),
                0 => types.push(None),
                _ => {
                    types.push(None);
                    variants.insert(name.clone(), ts);
                }
            }
            let k = sites.iter().filter(|s| param_of(**s, name).is_some_and(|p| p.unused)).count();
            if k > 0 {
                unused_slots.insert(name.clone(), k);
            }
        }
        let single_caller = if cfg.note_single_caller {
            let mut caller: Option<Site> = None;
            let mut ok = true;
            for s in &sites {
                let me = bare(&sides[s.0].functions[s.1].name);
                // A reference from outside the analysed functions (a skipped trait method, a
                // dispatch table) is a caller the note cannot name.
                if sides.iter().any(|side| side.other_refs.contains(me)) {
                    ok = false;
                    break;
                }
                let callers: Vec<Site> = sides
                    .iter()
                    .enumerate()
                    .flat_map(|(fi, side)| side.functions.iter().enumerate().map(move |(fni, f)| ((fi, fni), f)))
                    .filter(|(site, f)| site != s && bare(&f.name) != me && f.refs.contains(me))
                    .map(|(site, _)| site)
                    .collect();
                if callers.len() != 1 || caller.is_some_and(|c| c != callers[0]) {
                    ok = false;
                    break;
                }
                caller = Some(callers[0]);
            }
            caller.filter(|_| ok).map(|c| bare(&sides[c.0].functions[c.1].name).to_string())
        } else {
            None
        };
        let n = sites.len();
        let typed: Vec<String> = params.iter().zip(&types).map(|(p, t)| match t { Some(t) => format!("{p}: {t}"), None => p.clone() }).collect();
        let mut clauses: Vec<String> = params.iter().filter_map(|p| unused_slots.get(p).map(|k| format!("{p} unused in {k}/{n}"))).collect();
        clauses.extend(params.iter().filter_map(|p| variants.get(p).map(|v| format!("{p} varies ({})", v.join(", ")))));
        if let Some(c) = &single_caller {
            clauses.push(format!("all called from {c}"));
        }
        let tail = if clauses.is_empty() { String::new() } else { format!("; {}", clauses.join("; ")) };
        let line = format!("{n:>2} fns  {files:>2} files  ({})  {}{tail}", typed.join(", "), sites_text(&functions, cfg.max_sites_listed));
        clumps.push(Clump { params, types, variants, functions, files, unused_slots, single_caller, line });
    }
    clumps.sort_by(|a, b| b.functions.len().cmp(&a.functions.len()).then_with(|| b.files.cmp(&a.files)).then_with(|| a.params.cmp(&b.params)));

    let mut files: BTreeMap<String, FileClumps> = BTreeMap::new();
    let mut member_fns: HashSet<(String, String)> = HashSet::new();
    for c in &clumps {
        let n = c.functions.len();
        let tuple = format!("({})", c.params.join(", "));
        let mut clauses: Vec<String> = c.params.iter().filter_map(|p| c.unused_slots.get(p).map(|k| if *k == n { format!("`{p}` is unused in all {n}") } else { format!("`{p}` is unused in {k} of {n}") })).collect();
        clauses.extend(c.params.iter().filter_map(|p| c.variants.get(p).map(|v| format!("`{p}` varies ({})", v.join(", ")))));
        if let Some(s) = &c.single_caller {
            clauses.push(format!("all called from `{s}`"));
        }
        let fully_unused = c.unused_slots.values().filter(|k| **k == n).count();
        let what = struct_name(&c.functions).unwrap_or_else(|| "shared input".to_string());
        let article = if what.starts_with(['A', 'E', 'I', 'O', 'U']) { "an" } else { "a" };
        let advice = format!("introduce {article} {what} struct{}", match fully_unused { 0 => "", 1 => " or drop the slot", _ => " or drop the slots" });
        let scope = if c.files == 1 { "in this file".to_string() } else { format!("across {} files", c.files) };
        let clauses = if clauses.is_empty() { String::new() } else { format!("; {}", clauses.join("; ")) };
        let reason = format!("parameters {tuple} recur in {n} functions {scope} ({}){clauses} - {advice}", sites_text(&c.functions, cfg.max_sites_listed));
        let mut done: HashSet<&str> = HashSet::new();
        for m in &c.functions {
            let e = files.entry(m.file.clone()).or_default();
            e.clumps.push((tuple.clone(), m.line));
            if member_fns.insert((m.file.clone(), m.name.clone())) {
                e.members += 1;
            }
            if done.insert(m.file.as_str()) {
                e.reasons.push(reason.clone());
            }
        }
    }
    for fc in files.values_mut() {
        fc.clumps.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let total = fc.reasons.len();
        if total > cfg.max_reported_per_file {
            fc.reasons.truncate(cfg.max_reported_per_file);
            fc.reasons.push(format!("(+{} more parameter clumps in clumps)", total - cfg.max_reported_per_file));
        }
    }
    totals.clumps = clumps.len();
    totals.in_clumps = member_fns.len();
    totals.files = files.len();
    let mut notes = Vec::new();
    let skipped = totals.skipped_trait_impls + totals.skipped_protocol + totals.skipped_other + totals.skipped_duplicates;
    if skipped > 0 {
        notes.push(format!(
            "{skipped} functions not analysed: {} trait / override methods and callbacks, {} protocol tuples, {} overloads / dunders / stubs, {} same-name duplicates",
            totals.skipped_trait_impls, totals.skipped_protocol, totals.skipped_other, totals.skipped_duplicates
        ));
    }
    ClumpsReport { totals, notes, clumps, files }
}

/// `11 clumps over 27 of 774 functions (3.5%) in 9 files; 11 functions (1.4%) carry a _-prefixed parameter`.
pub fn totals_line(r: &ClumpsReport, prefix: &str) -> String {
    let t = &r.totals;
    let pct = |n: usize| if t.functions == 0 { 0.0 } else { 100.0 * n as f64 / t.functions as f64 };
    let silenced = if prefix.is_empty() { String::new() } else { format!("; {} functions ({:.1}%) carry a {prefix}-prefixed parameter", t.silenced_functions, pct(t.silenced_functions)) };
    format!("{} clumps over {} of {} functions ({:.1}%) in {} files{silenced}", t.clumps, t.in_clumps, t.functions, pct(t.in_clumps), t.files)
}

pub fn render(r: &ClumpsReport, top: usize, prefix: &str) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    if r.totals.functions == 0 {
        let _ = writeln!(o, "none");
    } else {
        let _ = writeln!(o, "{}", totals_line(r, prefix));
    }
    for n in &r.notes {
        let _ = writeln!(o, "note  {n}");
    }
    let _ = writeln!(o, "\nclumps (functions, files):");
    if r.clumps.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for c in r.clumps.iter().take(top) {
        let _ = writeln!(o, "  {}", c.line);
    }
    let _ = writeln!(o, "\nfiles (functions in clumps):");
    let mut rows: Vec<(&String, &FileClumps)> = r.files.iter().collect();
    rows.sort_by(|a, b| b.1.members.cmp(&a.1.members).then_with(|| a.0.cmp(b.0)));
    if rows.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for (p, fc) in rows.iter().take(top) {
        let _ = writeln!(o, "  {:>5}  {p}", fc.members);
        for r in &fc.reasons {
            let _ = writeln!(o, "         - {r}");
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::FileKind;
    use std::path::Path;

    fn src(path: &str, content: &str) -> SourceFile {
        let lang = Language::from_path(Path::new(path)).unwrap();
        SourceFile { path: path.into(), lang, kind: FileKind::Source, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn side(path: &str, content: &str, cfg: &Cfg) -> FileSide {
        Walker::new(cfg).parse_side(&src(path, content))
    }

    fn sig(f: &FnSite) -> String {
        format!("{}({})", f.name, f.params.iter().map(|p| format!("{}{}{}", p.name, p.ty.as_ref().map(|t| format!(": {t}")).unwrap_or_default(), if p.unused { "!" } else { "" })).collect::<Vec<_>>().join(", "))
    }

    fn sigs(path: &str, content: &str, cfg: &Cfg) -> Vec<String> {
        side(path, content, cfg).functions.iter().map(sig).collect()
    }

    fn run(files: &[SourceFile], cfg: &Cfg) -> ClumpsReport {
        analyze(&index_all(files, cfg), cfg)
    }

    const FLOW: &str = "\
pub fn plan_start(s: &Snapshot, f: &StartFacts, a: &StartArgs, _m: &Minter) -> Plan { go(s, f, a) }
pub fn plan_ship(s: &Snapshot, f: &ShipFacts, a: &ShipArgs, _m: &Minter) -> Plan { go(s, f, a) }
pub fn plan_park(s: &Snapshot, f: &Facts, a: &ParkArgs, _m: &Minter) -> Plan { go(s, f, a) }
pub fn plan_drop(s: &Snapshot, f: &Facts, a: &DropArgs, _m: &Minter) -> Plan { go(s, f, a) }
pub fn run(s: &Snapshot) { plan_start(); plan_ship(); plan_park(); plan_drop(); plan_repair(); }
";
    const REPAIR: &str = "pub fn plan_repair(s: &Snapshot, f: &Facts, a: &RepairArgs, _m: &Minter) -> Plan { go(s, f, a) }\n";

    #[test]
    fn parameters_are_named_typed_and_receivers_and_patterns_are_left_out() {
        let cfg = Cfg::default();
        let rs = "impl S { fn m(&self, a: u8, mut b: u8, (c, d): (u8, u8), _e: T) -> u8 { S { b }; println!(\"{a:?}\"); a } fn n(self: Box<Self>, x: &'a  str) { x; } }\nfn f(q: u8, r: u8, s: u8, t: u8, u: u8, v: u8, w: u8, x: u8, y: u8) { q + r + s + t + u + v + w + x + y }\n";
        assert_eq!(sigs("a.rs", rs, &cfg), vec!["S.m(a: u8, b: u8, _e: T!)", "S.n(x: &'a str)", "f(q: u8, r: u8, s: u8, t: u8, u: u8, v: u8, w: u8, x: u8)"]);
        assert_eq!(sigs("a.rs", rs, &Cfg { max_params: 3, ..cfg.clone() })[2], "f(q: u8, r: u8, s: u8)");
        let py = "def m(self, a, b: int, c=1, d: int = 2, *args, **kw): return a + b + c\ndef n(cls, x): return x\n";
        assert_eq!(sigs("b.py", py, &cfg), vec!["m(a, b: int, c, d: int!)", "n(x)"]);
        let ts = "function f(a: number, b?: string, { c }: Opts, ...rest: T[]) { return a; }\nconst g = x => x;\nfunction h(this: W, a, b) { return b; }\n";
        assert_eq!(sigs("c.ts", ts, &cfg), vec!["f(a: number, b: string!)", "g(x)", "h(a!, b)"]);
    }

    #[test]
    fn unused_is_the_prefix_or_no_reference_and_format_captures_count() {
        let cfg = Cfg::default();
        let rs = "fn f(a: u8, b: u8, c: u8, d: u8, e: u8, _f: u8) { println!(\"{a} {b:?} {{c}} {}\", d); S { e }; _f; }\n";
        assert_eq!(sigs("a.rs", rs, &cfg), vec!["f(a: u8, b: u8, c: u8!, d: u8, e: u8, _f: u8!)"]);
        // A capture outside a macro token tree is plain text; the prefix knob can be emptied.
        assert_eq!(sigs("a.rs", "fn f(a: u8) { let s = \"{a}\"; }\n", &cfg), vec!["f(a: u8!)"]);
        assert_eq!(sigs("a.rs", rs, &Cfg { unused_prefix: String::new(), ..cfg.clone() })[0], "f(a: u8, b: u8, c: u8!, d: u8, e: u8, _f: u8)");
        let ts = "function f(a: number, b: number, c: number) { const o = { a, x: b }; return `${c}`; }\n";
        assert_eq!(sigs("c.ts", ts, &cfg), vec!["f(a: number, b: number, c: number)"]);
        let py = "def f(a, b, c):\n    return f\"{a} {b!r}\"\n";
        assert_eq!(sigs("b.py", py, &cfg), vec!["f(a, b, c!)"]);
    }

    #[test]
    fn owned_signatures_only_and_same_names_count_once() {
        let cfg = Cfg::default();
        let rs = "\
impl Tr for S { fn t(&self, a: u8, b: u8, c: u8) { a + b + c } }
trait Tr2 { fn sig(a: u8, b: u8, c: u8); fn dflt(a: u8, b: u8, c: u8) { a + b + c } }
impl S { fn inherent(&self, a: u8, b: u8, c: u8) { a + b + c } }
#[cfg(unix)] fn twin(a: u8, b: u8, c: u8) { a + b + c }
#[cfg(windows)] fn twin(a: u8, b: u8, c: u8) { a }
#[test] fn t(a: u8, b: u8, c: u8) { a + b + c }
#[cfg(test)] mod tests { fn helper(a: u8, b: u8, c: u8) { a + b + c } }
";
        let s = side("a.rs", rs, &cfg);
        assert_eq!(s.functions.iter().map(sig).collect::<Vec<_>>(), vec!["S.inherent(a: u8, b: u8, c: u8)", "twin(a: u8, b: u8, c: u8)"]);
        assert_eq!((s.skipped_trait_impls, s.skipped_duplicates), (2, 1));
        let all = side("a.rs", rs, &Cfg { skip_trait_impls: false, dedupe_same_name_in_file: false, ..cfg.clone() });
        assert_eq!(all.functions.len(), 5, "{:?}", all.functions.iter().map(sig).collect::<Vec<_>>());
        let py = "\
class C(Base):
    def m(self, a, b, c): return a + b + c
class D:
    def m(self, a, b, c): return a + b + c
@overload
def f(a: int, b: int, c: int) -> int: ...
def f(a, b, c): return a + b + c
def g(a, b, c):
    \"\"\"doc\"\"\"
    pass
def __eq__(a, b, c): return a
def h(a, b, c): return a
";
        let s = side("b.py", py, &cfg);
        assert_eq!(s.functions.iter().map(sig).collect::<Vec<_>>(), vec!["D.m(a, b, c)", "f(a, b, c)", "h(a, b!, c!)"]);
        assert_eq!((s.skipped_trait_impls, s.skipped_other), (1, 3));
        let s = side("b.py", py, &Cfg { skip_dunder: false, skip_overloads: false, ..cfg.clone() });
        assert_eq!(s.functions.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["D.m", "f", "__eq__", "h"]);
        let ts = "\
class K extends B { m(a: number, b: number, c: number) { return a + b + c; } }
class L { m(a: number, b: number, c: number) { return a + b + c; } }
abstract class A { abstract n(a: number, b: number, c: number): void; }
interface I { s(a: number, b: number, c: number): void; }
function f(a: number, b: number, c: number): void;
function f(a: number, b: number, c: number) { return a; }
arr.map((a: number, b: number, c: number) => a);
const g = (a: number, b: number, c: number) => a;
const w = new Writable({ write(chunk: Buffer, _encoding: string, callback: () => void) { callback() }, final: function (a: number, b: number, c: number) { return a } });
run(({ a, b }) => a, { handle: (a: number, b: number, c: number) => a } as Opts);
";
        let s = side("c.ts", ts, &cfg);
        // Methods and functions inside an object literal passed as an argument (`new Writable({
        // write(...) {} })`) are protocol callbacks too.
        assert_eq!(s.functions.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["L.m", "f", "g"]);
        assert_eq!(s.skipped_trait_impls, 6);
    }

    #[test]
    fn protocol_tuples_are_never_analysed() {
        let cfg = Cfg::default();
        let py = "def show_help(ctx: Context, param: Parameter, value: bool): return value\ndef other(ctx: Context, param: Parameter, incomplete: str): return incomplete\n";
        let s = side("b.py", py, &cfg);
        assert_eq!(s.functions.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["other"]);
        assert_eq!(s.skipped_protocol, 1);
        let ts = "app.use(async (c, next, extra) => { await next(); });\nexport const mw = async (c: Context, next: Next, opts: O) => { await next(); };\n";
        let s = side("c.ts", ts, &cfg);
        assert!(s.functions.is_empty(), "{:?}", s.functions.iter().map(sig).collect::<Vec<_>>());
        assert_eq!((s.skipped_trait_impls, s.skipped_protocol), (1, 1));
        assert_eq!(side("b.py", py, &Cfg { protocol_tuples: vec![], ..cfg.clone() }).functions.len(), 2);
    }

    #[test]
    fn flagship_clump_reports_unused_varying_and_single_caller_with_the_reason_wording() {
        let cfg = Cfg::default();
        let r = run(&[src("src/cmd/flow.rs", FLOW), src("src/cmd/repair.rs", REPAIR)], &cfg);
        assert_eq!(r.clumps.len(), 1, "{:?}", r.clumps.iter().map(|c| &c.line).collect::<Vec<_>>());
        let c = &r.clumps[0];
        assert_eq!(c.params, vec!["s", "f", "a", "_m"]);
        assert_eq!(c.types, vec![Some("&Snapshot".to_string()), None, None, Some("&Minter".to_string())]);
        assert_eq!(c.variants["f"], vec!["&StartFacts", "&ShipFacts", "&Facts"]);
        assert_eq!((c.functions.len(), c.files), (5, 2));
        assert_eq!(c.unused_slots, BTreeMap::from([("_m".to_string(), 5)]));
        assert_eq!(c.single_caller.as_deref(), Some("run"));
        assert_eq!(c.line, " 5 fns   2 files  (s: &Snapshot, f, a, _m: &Minter)  plan_start src/cmd/flow.rs:1, plan_ship :2, plan_park :3, plan_drop :4, plan_repair src/cmd/repair.rs:1; _m unused in 5/5; f varies (&StartFacts, &ShipFacts, &Facts); a varies (&StartArgs, &ShipArgs, &ParkArgs, &DropArgs, &RepairArgs); all called from run");
        let reason = "parameters (s, f, a, _m) recur in 5 functions across 2 files (plan_start src/cmd/flow.rs:1, plan_ship :2, plan_park :3, plan_drop :4, plan_repair src/cmd/repair.rs:1); `_m` is unused in all 5; `f` varies (&StartFacts, &ShipFacts, &Facts); `a` varies (&StartArgs, &ShipArgs, &ParkArgs, &DropArgs, &RepairArgs); all called from `run` - introduce a PlanInput struct or drop the slot";
        assert_eq!(r.files["src/cmd/flow.rs"].reasons, vec![reason]);
        assert_eq!(r.files["src/cmd/repair.rs"].reasons, vec![reason]);
        assert_eq!(r.files["src/cmd/flow.rs"].members, 4);
        assert_eq!(r.files["src/cmd/flow.rs"].clumps, vec![("(s, f, a, _m)".to_string(), 1), ("(s, f, a, _m)".to_string(), 2), ("(s, f, a, _m)".to_string(), 3), ("(s, f, a, _m)".to_string(), 4)]);
        assert_eq!((r.totals.functions, r.totals.in_clumps, r.totals.clumps, r.totals.files, r.totals.silenced_functions), (6, 5, 1, 2, 5));
        assert_eq!(totals_line(&r, "_"), "1 clumps over 5 of 6 functions (83.3%) in 2 files; 5 functions (83.3%) carry a _-prefixed parameter");
        // A slot unused in some members is `k of n`; a used one recommends only the struct.
        let repair2 = REPAIR.replace("go(s, f, a)", "go(s, f, a, _m)");
        let r = run(&[src("src/cmd/flow.rs", FLOW), src("src/cmd/repair.rs", &repair2)], &Cfg { unused_prefix: String::new(), ..cfg.clone() });
        assert_eq!(r.clumps.len(), 1, "{:?}", r.clumps.iter().map(|c| &c.line).collect::<Vec<_>>());
        assert!(r.files["src/cmd/repair.rs"].reasons[0].contains("`_m` is unused in 4 of 5"), "{:?}", r.files["src/cmd/repair.rs"].reasons);
        assert!(r.files["src/cmd/repair.rs"].reasons[0].ends_with("- introduce a PlanInput struct"), "{:?}", r.files["src/cmd/repair.rs"].reasons);
        // Two callers, or none, drop the note; the knob turns it off.
        let flow2 = format!("{FLOW}pub fn run2() {{ plan_start(); }}\n");
        assert_eq!(run(&[src("src/cmd/flow.rs", &flow2), src("src/cmd/repair.rs", REPAIR)], &cfg).clumps[0].single_caller, None);
        // A caller the pass does not analyse (a trait method, a dispatch table) is a caller too.
        let trait_caller = format!("{FLOW}impl Command for X {{ fn execute(&self) {{ plan_ship(); }} }}\n");
        assert_eq!(run(&[src("src/cmd/flow.rs", &trait_caller), src("src/cmd/repair.rs", REPAIR)], &cfg).clumps[0].single_caller, None);
        let table = format!("{FLOW}static TABLE: &[fn()] = &[plan_park];\n");
        assert_eq!(run(&[src("src/cmd/flow.rs", &table), src("src/cmd/repair.rs", REPAIR)], &cfg).clumps[0].single_caller, None);
        // An import of the name, or a test, is not.
        let imported = format!("use crate::cmd::flow::plan_start;\n{FLOW}#[cfg(test)]\nmod tests {{ fn t() {{ plan_drop(); }} }}\n");
        assert_eq!(run(&[src("src/cmd/flow.rs", &imported), src("src/cmd/repair.rs", REPAIR)], &cfg).clumps[0].single_caller.as_deref(), Some("run"));
        assert_eq!(run(&[src("src/cmd/flow.rs", FLOW), src("src/cmd/repair.rs", REPAIR)], &Cfg { note_single_caller: false, ..cfg.clone() }).clumps[0].single_caller, None);
    }

    #[test]
    fn thresholds_typed_gate_and_untyped_fallback() {
        let cfg = Cfg::default();
        let f = |name: &str, body: &str| format!("fn {name}(a: &A, b: &B, c: &C) -> u8 {{ {body} }}\n");
        let four: String = ["p", "q", "r", "s"].iter().map(|n| f(n, "a + b + c")).collect();
        // Four members in one file report; three do not; two files with two members do.
        assert_eq!(run(&[src("x.rs", &four)], &cfg).clumps.len(), 1);
        assert_eq!(run(&[src("x.rs", &four.lines().take(3).collect::<Vec<_>>().join("\n"))], &cfg).clumps.len(), 0);
        let two = run(&[src("x.rs", &f("p", "a")), src("y.rs", &f("q", "b"))], &cfg);
        assert_eq!(two.clumps.len(), 1);
        assert_eq!(two.files["x.rs"].reasons[0], "parameters (a, b, c) recur in 2 functions across 2 files (p x.rs:1, q y.rs:1); `a` is unused in 1 of 2; `b` is unused in 1 of 2; `c` is unused in all 2 - introduce a shared input struct or drop the slot");
        assert_eq!(run(&[src("x.rs", &f("p", "a")), src("y.rs", &f("q", "b"))], &Cfg { report_if_either: false, ..cfg.clone() }).clumps.len(), 0);
        assert_eq!(run(&[src("x.rs", &four)], &Cfg { min_functions: 5, ..cfg.clone() }).clumps.len(), 0);
        // One agreeing typed slot is below the bar; the knob lowers it.
        let mixed = "fn p(a: &A, b: &B1, c: &C1) { a } fn q(a: &A, b: &B2, c: &C2) { a } fn r(a: &A, b: &B3, c: &C3) { a } fn s(a: &A, b: &B4, c: &C4) { a }\n";
        assert_eq!(run(&[src("x.rs", mixed)], &cfg).clumps.len(), 0);
        assert_eq!(run(&[src("x.rs", mixed)], &Cfg { min_typed_slots: 1, ..cfg.clone() }).clumps.len(), 1);
        // Untyped: min_functions + 1 members and the joined-name floor; typed one-letter tuples pass.
        let py = |n: usize, names: &str| (0..n).map(|i| format!("def f{i}({names}): return 1\n")).collect::<String>();
        assert_eq!(run(&[src("b.py", &py(4, "a, b, c"))], &cfg).clumps.len(), 0);
        assert_eq!(run(&[src("b.py", &py(5, "a, b, c"))], &cfg).clumps.len(), 1);
        assert_eq!(run(&[src("b.py", &py(5, "a, b, c"))], &Cfg { min_name_len: 4, ..cfg.clone() }).clumps.len(), 0);
        assert_eq!(run(&[src("b.py", &py(5, "ab, b, c"))], &Cfg { min_name_len: 4, ..cfg.clone() }).clumps.len(), 1);
        assert_eq!(run(&[src("x.rs", &four)], &Cfg { min_name_len: 4, ..cfg.clone() }).clumps.len(), 1);
        // Partly annotated Python below the typed bar is not a clump.
        assert_eq!(run(&[src("b.py", &py(5, "a: int, b, c"))], &cfg).clumps.len(), 0);
    }

    #[test]
    fn groups_with_one_member_set_collapse_and_wider_groups_stay() {
        let cfg = Cfg::default();
        let f = |name: &str, params: &str| format!("fn {name}({params}) -> u8 {{ 0 }}\n");
        let five = "a: &A, b: &B, c: &C, d: &D, e: &E";
        let files = [
            src("x.rs", &[f("p", five), f("q", five), f("r", five), f("s", five), f("t", "a: &A, b: &B, c: &C")].concat()),
        ];
        let r = run(&files, &cfg);
        let lines: Vec<(Vec<String>, usize)> = r.clumps.iter().map(|c| (c.params.clone(), c.functions.len())).collect();
        assert_eq!(lines, vec![(vec!["a".into(), "b".into(), "c".into()], 5), (vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()], 4)]);
        assert_eq!(r.files["x.rs"].members, 5);
        assert_eq!(r.files["x.rs"].reasons.len(), 2);
        // The per-file cap, then the remainder line; `max_group` bounds what is grouped.
        let r = run(&files, &Cfg { max_reported_per_file: 1, ..cfg.clone() });
        assert_eq!(r.files["x.rs"].reasons[1], "(+1 more parameter clumps in clumps)");
        assert_eq!(run(&files, &Cfg { min_group: 5, max_group: 5, ..cfg.clone() }).clumps.len(), 1);
        assert!(run(&files, &Cfg { min_group: 6, max_group: 6, ..cfg.clone() }).clumps.is_empty());
    }

    #[test]
    fn render_lists_clumps_and_files() {
        let cfg = Cfg::default();
        let r = run(&[src("src/cmd/flow.rs", FLOW), src("src/cmd/repair.rs", REPAIR)], &cfg);
        let text = render(&r, 10, "_");
        assert!(text.starts_with("1 clumps over 5 of 6 functions (83.3%) in 2 files; 5 functions (83.3%) carry a _-prefixed parameter\n\nclumps (functions, files):\n   5 fns   2 files  (s: &Snapshot"), "{text}");
        assert!(text.contains("\nfiles (functions in clumps):\n      4  src/cmd/flow.rs\n         - parameters (s, f, a, _m) recur"), "{text}");
        let empty = render(&ClumpsReport::default(), 10, "_");
        assert!(empty.contains("clumps (functions, files):\n  none\n"), "{empty}");
    }
}
