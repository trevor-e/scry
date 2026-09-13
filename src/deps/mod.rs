//! Import graph: who depends on whom, and where the graph is tangled.
//!
//! Imports are pulled from tree-sitter nodes, resolved against the discovered
//! file set (never the filesystem, so the graph matches what other passes see),
//! and fed to Tarjan's SCC. Cycles are reported at two granularities: file
//! cycles are the concrete tangle, directory cycles are the architectural one.

use crate::discover::{FileKind, SourceFile};
use crate::lang::Language;
use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use tree_sitter::Node;

#[derive(Debug, Default, Clone, Serialize)]
pub struct FileDeps {
    /// Distinct non-test files importing this one.
    pub fan_in: usize,
    /// Distinct in-repo files this one imports.
    pub fan_out: usize,
    /// Distinct test files importing this one (test proximity signal).
    pub test_refs: usize,
    /// Bare specifiers / third-party modules that did not resolve in-repo.
    pub external: usize,
    pub in_cycle: bool,
    /// fan_out / (fan_in + fan_out); 1.0 = depends on everything, nothing depends on it.
    pub instability: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cycle {
    pub members: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct DepGraph {
    pub files: HashMap<String, FileDeps>,
    pub file_cycles: Vec<Cycle>,
    pub dir_cycles: Vec<Cycle>,
    pub edges: usize,
    #[serde(skip)]
    edge_set: HashSet<(String, String)>,
}

impl DepGraph {
    /// True when either file imports the other, directly.
    pub fn connected(&self, a: &str, b: &str) -> bool {
        self.edge_set.contains(&(a.to_string(), b.to_string()))
            || self.edge_set.contains(&(b.to_string(), a.to_string()))
    }
}

#[derive(Debug)]
struct RawImport {
    /// Module spec as written: `a.b.c`, `./x`, `crate::y`.
    spec: String,
    /// Python: `from a.b import c` – `c` may itself be a submodule.
    names: Vec<String>,
    /// Python relative-import level (number of leading dots).
    level: usize,
}

pub fn build(files: &[SourceFile]) -> DepGraph {
    let known: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let kind_of: HashMap<&str, FileKind> = files.iter().map(|f| (f.path.as_str(), f.kind)).collect();

    let per_file: Vec<(usize, Vec<String>, usize)> = files
        .par_iter()
        .enumerate()
        .map(|(i, f)| {
            let raws = extract(f);
            let mut targets = BTreeSet::new();
            let mut external = 0usize;
            for r in raws {
                match resolve(&f.path, f.lang, &r, &known) {
                    Some(t) => {
                        for t in t {
                            if t != f.path {
                                targets.insert(t);
                            }
                        }
                    }
                    None => external += 1,
                }
            }
            (i, targets.into_iter().collect(), external)
        })
        .collect();

    let mut graph: DiGraph<usize, ()> = DiGraph::new();
    let idx: HashMap<&str, NodeIndex> =
        files.iter().enumerate().map(|(i, f)| (f.path.as_str(), graph.add_node(i))).collect();
    let mut out = DepGraph::default();
    let mut fan_in: HashMap<&str, usize> = HashMap::new();
    let mut test_refs: HashMap<&str, usize> = HashMap::new();

    for (i, targets, external) in &per_file {
        let src = files[*i].path.as_str();
        let is_test = kind_of[src] == FileKind::Test;
        for t in targets {
            let (Some(&a), Some(&b)) = (idx.get(src), idx.get(t.as_str())) else { continue };
            graph.add_edge(a, b, ());
            out.edge_set.insert((src.to_string(), t.clone()));
            if is_test {
                *test_refs.entry(t.as_str()).or_default() += 1;
            } else {
                *fan_in.entry(t.as_str()).or_default() += 1;
            }
        }
        out.files.insert(
            src.to_string(),
            FileDeps { fan_out: targets.len(), external: *external, ..Default::default() },
        );
    }
    out.edges = out.edge_set.len();

    for (p, d) in out.files.iter_mut() {
        d.fan_in = fan_in.get(p.as_str()).copied().unwrap_or(0);
        d.test_refs = test_refs.get(p.as_str()).copied().unwrap_or(0);
        let total = d.fan_in + d.fan_out;
        d.instability = if total == 0 { 0.0 } else { d.fan_out as f64 / total as f64 };
    }

    // File-level cycles.
    for scc in tarjan_scc(&graph) {
        if scc.len() < 2 {
            continue;
        }
        let mut members: Vec<String> = scc.iter().map(|n| files[graph[*n]].path.clone()).collect();
        members.sort();
        for m in &members {
            if let Some(d) = out.files.get_mut(m) {
                d.in_cycle = true;
            }
        }
        out.file_cycles.push(Cycle { members });
    }
    out.file_cycles.sort_by_key(|c| std::cmp::Reverse(c.members.len()));

    // Directory-level cycles: collapse files to their directory, drop self-edges.
    let mut dgraph: DiGraph<String, ()> = DiGraph::new();
    let mut didx: HashMap<String, NodeIndex> = HashMap::new();
    let mut dedges: HashSet<(String, String)> = HashSet::new();
    for (a, b) in &out.edge_set {
        let (da, db) = (dir_of(a), dir_of(b));
        if da != db && kind_of[a.as_str()] != FileKind::Test {
            dedges.insert((da.to_string(), db.to_string()));
        }
    }
    for (a, b) in &dedges {
        let na = *didx.entry(a.clone()).or_insert_with(|| dgraph.add_node(a.clone()));
        let nb = *didx.entry(b.clone()).or_insert_with(|| dgraph.add_node(b.clone()));
        dgraph.add_edge(na, nb, ());
    }
    for scc in tarjan_scc(&dgraph) {
        if scc.len() < 2 {
            continue;
        }
        let mut members: Vec<String> = scc.iter().map(|n| dgraph[*n].clone()).collect();
        members.sort();
        out.dir_cycles.push(Cycle { members });
    }
    out.dir_cycles.sort_by_key(|c| std::cmp::Reverse(c.members.len()));
    out
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map(|i| &path[..i]).unwrap_or("")
}

// ---------- extraction ----------

fn extract(file: &SourceFile) -> Vec<RawImport> {
    let mut parser = file.lang.parser();
    let src = file.content.as_bytes();
    let Some(tree) = parser.parse(src, None) else { return Vec::new() };
    let mut raws = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match file.lang {
            Language::Python => extract_python(n, src, &mut raws),
            Language::Rust => extract_rust(n, src, &mut raws),
            _ => extract_js(n, src, &mut raws),
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    raws
}

fn t<'a>(n: Node, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

fn extract_python(n: Node, src: &[u8], out: &mut Vec<RawImport>) {
    match n.kind() {
        "import_statement" => {
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                let name = match ch.kind() {
                    "dotted_name" => t(ch, src),
                    "aliased_import" => ch.child_by_field_name("name").map(|x| t(x, src)).unwrap_or(""),
                    _ => continue,
                };
                out.push(RawImport { spec: name.to_string(), names: vec![], level: 0 });
            }
        }
        "import_from_statement" => {
            let Some(module) = n.child_by_field_name("module_name") else { return };
            let (spec, level) = match module.kind() {
                "relative_import" => {
                    let text = t(module, src);
                    let dots = text.chars().take_while(|c| *c == '.').count();
                    (text[dots..].to_string(), dots)
                }
                _ => (t(module, src).to_string(), 0),
            };
            let mut names = Vec::new();
            let mut c = n.walk();
            for ch in n.children_by_field_name("name", &mut c) {
                let nm = match ch.kind() {
                    "aliased_import" => ch.child_by_field_name("name").map(|x| t(x, src)).unwrap_or(""),
                    _ => t(ch, src),
                };
                if !nm.is_empty() {
                    names.push(nm.to_string());
                }
            }
            out.push(RawImport { spec, names, level });
        }
        _ => {}
    }
}

fn extract_js(n: Node, src: &[u8], out: &mut Vec<RawImport>) {
    match n.kind() {
        "import_statement" | "export_statement" => {
            if let Some(s) = n.child_by_field_name("source") {
                out.push(RawImport { spec: unquote(t(s, src)), names: vec![], level: 0 });
            }
        }
        "call_expression" => {
            let Some(f) = n.child_by_field_name("function") else { return };
            let callee = t(f, src);
            if callee != "require" && callee != "import" {
                return;
            }
            if let Some(args) = n.child_by_field_name("arguments") {
                if let Some(a) = args.named_child(0).filter(|a| a.kind() == "string") {
                    out.push(RawImport { spec: unquote(t(a, src)), names: vec![], level: 0 });
                }
            }
        }
        _ => {}
    }
}

fn extract_rust(n: Node, src: &[u8], out: &mut Vec<RawImport>) {
    match n.kind() {
        "mod_item" if n.child_by_field_name("body").is_none() => {
            if let Some(name) = n.child_by_field_name("name") {
                out.push(RawImport { spec: format!("mod:{}", t(name, src)), names: vec![], level: 0 });
            }
        }
        "use_declaration" => {
            if let Some(arg) = n.child_by_field_name("argument") {
                // Take the leading path up to the first `{` or `*`; good enough to
                // find the module file.
                let text = t(arg, src);
                let head = text.split(['{', '*']).next().unwrap_or("").trim().trim_end_matches("::");
                if head.starts_with("crate::") || head.starts_with("super::") || head.starts_with("self::") {
                    out.push(RawImport { spec: head.to_string(), names: vec![], level: 0 });
                }
            }
        }
        _ => {}
    }
}

fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string()
}

// ---------- resolution ----------

fn resolve(from: &str, lang: Language, imp: &RawImport, known: &HashSet<&str>) -> Option<Vec<String>> {
    match lang {
        Language::Python => resolve_python(from, imp, known),
        Language::Rust => resolve_rust(from, imp, known),
        _ => resolve_js(from, imp, known).map(|p| vec![p]),
    }
}

fn join(dir: &str, rest: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn ancestors(dir: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut d = dir.to_string();
    loop {
        v.push(d.clone());
        if d.is_empty() {
            break;
        }
        d = dir_of(&d).to_string();
    }
    v
}

fn py_candidates(base: &str, module: &str) -> [String; 2] {
    let rel = module.replace('.', "/");
    let p = join(base, &rel);
    [format!("{p}.py"), format!("{p}/__init__.py")]
}

fn resolve_python(from: &str, imp: &RawImport, known: &HashSet<&str>) -> Option<Vec<String>> {
    let dir = dir_of(from);
    let roots: Vec<String> = if imp.level > 0 {
        let mut d = dir.to_string();
        for _ in 1..imp.level {
            d = dir_of(&d).to_string();
        }
        vec![d]
    } else {
        ancestors(dir)
    };
    let mut hits = Vec::new();
    for root in &roots {
        let mut found = false;
        if imp.spec.is_empty() {
            // `from . import x` – x are submodules of the package at root.
            for nm in &imp.names {
                for c in py_candidates(root, nm) {
                    if known.contains(c.as_str()) {
                        hits.push(c);
                        found = true;
                        break;
                    }
                }
            }
        } else {
            for c in py_candidates(root, &imp.spec) {
                if known.contains(c.as_str()) {
                    hits.push(c);
                    found = true;
                    break;
                }
            }
            if found {
                for nm in &imp.names {
                    let sub = format!("{}.{}", imp.spec, nm);
                    for c in py_candidates(root, &sub) {
                        if known.contains(c.as_str()) {
                            hits.push(c);
                            break;
                        }
                    }
                }
            }
        }
        if found {
            return Some(hits);
        }
    }
    None
}

const JS_EXTS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts"];

fn resolve_js(from: &str, imp: &RawImport, known: &HashSet<&str>) -> Option<String> {
    let dir = dir_of(from);
    let spec = imp.spec.as_str();
    let bases: Vec<String> = if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
        vec![join(dir, spec)]
    } else if let Some(rest) = spec.strip_prefix("@/").or_else(|| spec.strip_prefix("~/")) {
        // Common alias: `@/x` -> `<nearest src>/x`.
        ancestors(dir).into_iter().map(|a| join(&a, &format!("src/{rest}"))).collect()
    } else {
        return None;
    };
    for base in bases {
        if known.contains(base.as_str()) {
            return Some(base);
        }
        // `./x.js` written for ESM output usually means `./x.ts` in source.
        let stem = base.strip_suffix(".js").or_else(|| base.strip_suffix(".jsx")).unwrap_or(&base);
        for ext in JS_EXTS {
            let c = format!("{stem}{ext}");
            if known.contains(c.as_str()) {
                return Some(c);
            }
        }
        for ext in JS_EXTS {
            let c = format!("{base}/index{ext}");
            if known.contains(c.as_str()) {
                return Some(c);
            }
        }
    }
    None
}

fn resolve_rust(from: &str, imp: &RawImport, known: &HashSet<&str>) -> Option<Vec<String>> {
    let dir = dir_of(from);
    let file = from.rsplit('/').next().unwrap_or(from);
    // Where this file's child modules live.
    let child_dir = if matches!(file, "main.rs" | "lib.rs" | "mod.rs") {
        dir.to_string()
    } else {
        join(dir, file.trim_end_matches(".rs"))
    };
    if let Some(name) = imp.spec.strip_prefix("mod:") {
        for c in [format!("{}/{name}.rs", child_dir), format!("{}/{name}/mod.rs", child_dir)] {
            let c = c.trim_start_matches('/').to_string();
            if known.contains(c.as_str()) {
                return Some(vec![c]);
            }
        }
        return None;
    }
    let (base, path) = if let Some(p) = imp.spec.strip_prefix("crate::") {
        // Crate root = nearest ancestor containing lib.rs or main.rs.
        let root = ancestors(dir).into_iter().find(|a| {
            known.contains(join(a, "lib.rs").as_str()) || known.contains(join(a, "main.rs").as_str())
        })?;
        (root, p)
    } else if let Some(p) = imp.spec.strip_prefix("super::") {
        (dir_of(&child_dir).to_string(), p)
    } else {
        (child_dir.clone(), imp.spec.trim_start_matches("self::"))
    };
    let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
    // Longest prefix that names a file wins.
    for n in (1..=segs.len()).rev() {
        let rel = segs[..n].join("/");
        for c in [join(&base, &format!("{rel}.rs")), join(&base, &format!("{rel}/mod.rs"))] {
            if known.contains(c.as_str()) {
                return Some(vec![c]);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(path: &str, content: &str) -> SourceFile {
        let lang = Language::from_path(std::path::Path::new(path)).unwrap();
        let kind = crate::discover::classify(path, content, 1);
        SourceFile { path: path.into(), lang, kind, lines: 1, bytes: content.len(), content: content.into() }
    }

    #[test]
    fn python_cycle_and_relative_imports() {
        let files = vec![
            sf("backend/engine/__init__.py", ""),
            sf("backend/engine/a.py", "from engine import b\nfrom .c import thing\n"),
            sf("backend/engine/b.py", "import engine.a\n"),
            sf("backend/engine/c.py", "from . import a\n"),
            sf("backend/tests/test_a.py", "from engine.a import x\n"),
        ];
        let g = build(&files);
        assert_eq!(g.file_cycles.len(), 1);
        assert_eq!(g.file_cycles[0].members, vec!["backend/engine/a.py", "backend/engine/b.py", "backend/engine/c.py"]);
        let a = &g.files["backend/engine/a.py"];
        assert_eq!(a.test_refs, 1);
        assert_eq!(a.fan_in, 2); // b and c
        assert!(g.dir_cycles.is_empty());
    }

    #[test]
    fn ts_resolution_and_dir_cycle() {
        let files = vec![
            sf("src/a/x.ts", "import { y } from '../b/y'\nexport * from './z.js'\n"),
            sf("src/a/z.ts", ""),
            sf("src/b/y.tsx", "import x from '@/a/x'\nimport React from 'react'\n"),
            sf("src/b/index.ts", "export const q = require('./y')\n"),
        ];
        let g = build(&files);
        assert_eq!(g.files["src/a/x.ts"].fan_out, 2);
        assert_eq!(g.files["src/b/y.tsx"].external, 1);
        assert_eq!(g.file_cycles.len(), 1);
        assert_eq!(g.dir_cycles.len(), 1);
        assert_eq!(g.dir_cycles[0].members, vec!["src/a", "src/b"]);
        assert!(g.connected("src/b/index.ts", "src/b/y.tsx"));
    }

    #[test]
    fn rust_mod_and_crate_paths() {
        let files = vec![
            sf("src/main.rs", "mod deps;\nmod lang;\n"),
            sf("src/deps/mod.rs", "use crate::lang::Language;\n"),
            sf("src/lang/mod.rs", "use super::deps::Cycle;\n"),
        ];
        let g = build(&files);
        assert_eq!(g.files["src/main.rs"].fan_out, 2);
        assert!(g.connected("src/deps/mod.rs", "src/lang/mod.rs"));
        assert_eq!(g.file_cycles.len(), 1);
    }
}
