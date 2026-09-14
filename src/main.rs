mod clones;
mod config;
mod deps;
mod discover;
mod history;
mod lang;
mod metrics;
mod report;

use anyhow::Result;
use clap::{Parser, Subcommand};
use discover::{FileKind, SourceFile};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// What the three tree-sitter passes produce from one parse per file.
struct Parsed {
    file_metrics: Vec<metrics::FileMetrics>,
    functions: Vec<metrics::FunctionMetrics>,
    deps: deps::DepGraph,
    clones: clones::CloneReport,
}

/// Parse each file once and run metrics, deps and clones over the same tree.
/// The tree is dropped before the next file starts, so peak memory is one
/// tree per worker thread, not one per file. `source` must be the `Source`
/// files of `files`, in the same order (the standalone subcommands each
/// parse for themselves and are unaffected).
fn parse_once(files: &[SourceFile], source: &[SourceFile], cfg: &config::Config, ts: &deps::TsConfigs) -> Parsed {
    struct PerFile {
        metrics: Option<(metrics::FileMetrics, Vec<metrics::FunctionMetrics>)>,
        imports: Vec<deps::RawImport>,
        tokens: Option<clones::Tokens>,
    }
    let per_file: Vec<PerFile> = files
        .par_iter()
        .map(|f| {
            let ranked = f.kind == FileKind::Source;
            let tree = if ranked || deps::walks_imports(f) { f.lang.parse(&f.content) } else { None };
            let tree = tree.as_ref();
            PerFile {
                metrics: ranked.then(|| metrics::analyze_tree(f, tree, &cfg.metrics)),
                imports: deps::imports(f, tree),
                tokens: ranked.then(|| clones::tokenize(tree)),
            }
        })
        .collect();
    let mut per_metrics = Vec::with_capacity(source.len());
    let mut imports = Vec::with_capacity(files.len());
    let mut tokens = Vec::with_capacity(source.len());
    for p in per_file {
        per_metrics.extend(p.metrics);
        imports.push(p.imports);
        tokens.extend(p.tokens);
    }
    let (file_metrics, functions) = metrics::collect(per_metrics);
    Parsed {
        file_metrics,
        functions,
        deps: deps::build_from(files, &imports, &cfg.deps, ts),
        clones: clones::detect_from(source, tokens, &cfg.clones),
    }
}

#[derive(Parser)]
#[command(name = "scry", version, about = "Find the parts of a codebase most likely to need refactoring or to hide bugs")]
struct Cli {
    /// Extra config file, layered over <root>/scry.toml (see `scry config`)
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run every pass and rank the files most worth attention, with reasons
    Scan {
        /// Repository root (must be the git root for history to join)
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit the full report as JSON
        #[arg(long)]
        json: bool,
        /// Entries per section
        #[arg(long, default_value_t = 15)]
        top: usize,
        /// Git --since window for churn (overrides [history].since)
        #[arg(long)]
        since: Option<String>,
        /// Skip git history (rank on static signals only)
        #[arg(long)]
        no_history: bool,
    },
    /// Walk a repository and report what was found, by language and kind
    Files {
        /// Repository root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit JSON instead of the human table
        #[arg(long)]
        json: bool,
        /// List the N largest source files
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Git churn, fix commits, authors and co-change pairs for source files
    History {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Git --since window (overrides [history].since)
        #[arg(long)]
        since: Option<String>,
    },
    /// Near-exact clone pairs across source files
    Clones {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Import graph: cycles, fan-in/out, instability
    Deps {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Per-function cognitive/cyclomatic complexity for source files
    Metrics {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Print the effective configuration as TOML (defaults + <root>/scry.toml + --config)
    Config {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Dump the tree-sitter S-expression of one file (debugging aid)
    Ast {
        path: PathBuf,
        /// Only print ERROR/MISSING nodes with their line and text
        #[arg(long)]
        errors: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = match &cli.cmd {
        Cmd::Scan { path, .. } | Cmd::Files { path, .. } | Cmd::History { path, .. } | Cmd::Clones { path, .. }
        | Cmd::Deps { path, .. } | Cmd::Metrics { path, .. } | Cmd::Config { path } => path.clone(),
        Cmd::Ast { .. } => PathBuf::from("."),
    };
    let mut cfg = config::Config::load(&root, cli.config.as_deref())?;
    match cli.cmd {
        Cmd::Config { .. } => {
            print!("{}", cfg.to_toml());
        }
        Cmd::Scan { path, json, top, since, no_history } => {
            if let Some(s) = since {
                cfg.history.since = s;
            }
            let files = discover::walk(&path, &cfg.discover)?;
            let source: Vec<SourceFile> = files.iter().filter(|f| f.kind == FileKind::Source).cloned().collect();
            let tracked: std::collections::HashSet<String> = source.iter().map(|f| f.path.clone()).collect();
            let history = if no_history {
                None
            } else {
                match history::collect(&path, &cfg.history, &tracked) {
                    Ok(h) => {
                        if h.commits_scanned > 0 && h.files.is_empty() {
                            eprintln!("warning: {} commits scanned but none touched a discovered source file; ranking on static signals", h.commits_scanned);
                        }
                        Some(h)
                    }
                    Err(e) => {
                        eprintln!("warning: history unavailable: {e:#}");
                        None
                    }
                }
            };
            let ts = deps::TsConfigs::load(&path);
            let parsed = parse_once(&files, &source, &cfg, &ts);
            let report = report::build(
                report::Inputs {
                    root: path.canonicalize()?.display().to_string(),
                    files: &files,
                    history: history.as_ref(),
                    file_metrics: &parsed.file_metrics,
                    functions: &parsed.functions,
                    deps: &parsed.deps,
                    clones: &parsed.clones,
                    cognitive_hard: cfg.metrics.cognitive_hard,
                },
                top,
                &cfg.report,
                &cfg.discover.test_dirs,
            );
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report::render(&report, top));
            }
        }
        Cmd::Files { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&files)?);
                return Ok(());
            }
            let mut by_kind: BTreeMap<String, (usize, usize)> = BTreeMap::new();
            for f in &files {
                let e = by_kind.entry(format!("{:?}/{}", f.kind, f.lang.name())).or_default();
                e.0 += 1;
                e.1 += f.lines;
            }
            println!("{:<24} {:>6} {:>9}", "kind/lang", "files", "lines");
            for (k, (n, l)) in &by_kind {
                println!("{k:<24} {n:>6} {l:>9}");
            }
            let mut src: Vec<&discover::SourceFile> =
                files.iter().filter(|f| f.kind == discover::FileKind::Source).collect();
            src.sort_by_key(|f| std::cmp::Reverse(f.lines));
            println!("\nlargest source files:");
            for f in src.iter().take(top) {
                println!("{:>7}  {}", f.lines, f.path);
            }
        }
        Cmd::History { path, json, top, since } => {
            if let Some(s) = since {
                cfg.history.since = s;
            }
            let files = discover::walk(&path, &cfg.discover)?;
            let tracked: std::collections::HashSet<String> = files
                .iter()
                .filter(|f| f.kind == discover::FileKind::Source)
                .map(|f| f.path.clone())
                .collect();
            let hist = history::collect(&path, &cfg.history, &tracked)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hist)?);
                return Ok(());
            }
            println!("{} commits since {}\n", hist.commits_scanned, hist.window);
            let mut rows: Vec<(&String, &history::FileHistory)> = hist.files.iter().collect();
            rows.sort_by_key(|(_, h)| std::cmp::Reverse((h.commits, h.fix_commits)));
            println!("{:>7} {:>5} {:>7}  path", "commits", "fixes", "authors");
            for (p, h) in rows.iter().take(top) {
                println!("{:>7} {:>5} {:>7}  {}", h.commits, h.fix_commits, h.authors, p);
            }
            println!("\nco-change pairs (together / strength):");
            for c in hist.co_changes.iter().take(top) {
                println!("{:>3}  {:.2}  {}  <->  {}", c.together, c.strength, c.a, c.b);
            }
        }
        Cmd::Clones { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let src: Vec<discover::SourceFile> =
                files.into_iter().filter(|f| f.kind == discover::FileKind::Source).collect();
            let r = clones::detect(&src, &cfg.clones);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            println!("{} clone pairs (>= {} tokens) across {} files\n", r.pairs.len(), cfg.clones.min_tokens, r.files.len());
            for p in r.pairs.iter().take(top) {
                println!("{:>5} tok  {}:{}-{}  <->  {}:{}-{}", p.tokens, p.a.file, p.a.start_line, p.a.end_line, p.b.file, p.b.start_line, p.b.end_line);
            }
            let mut rows: Vec<(&String, &clones::FileClones)> = r.files.iter().collect();
            rows.sort_by(|x, y| y.1.clone_ratio.partial_cmp(&x.1.clone_ratio).unwrap());
            println!("\nmost duplicated files (cloned lines / ratio):");
            for (p, c) in rows.iter().take(top) {
                println!("{:>5} {:>5.0}%  {}", c.clone_lines, c.clone_ratio * 100.0, p);
            }
        }
        Cmd::Deps { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let g = deps::build(&files, &cfg.deps, &deps::TsConfigs::load(&path));
            if json {
                println!("{}", serde_json::to_string_pretty(&g)?);
                return Ok(());
            }
            println!("{} files, {} in-repo edges\n", g.files.len(), g.edges);
            println!("directory cycles ({}):", g.dir_cycles.len());
            for c in g.dir_cycles.iter().take(top) {
                println!("  {}", c.members.join("  <->  "));
            }
            println!("\nfile cycles ({}):", g.file_cycles.len());
            for c in g.file_cycles.iter().take(top) {
                println!("  [{}] {}", c.members.len(), c.members.join(", "));
            }
            let mut rows: Vec<(&String, &deps::FileDeps)> = g.files.iter().collect();
            rows.sort_by_key(|(_, d)| std::cmp::Reverse(d.fan_in));
            println!("\n{:>6} {:>7} {:>5} {:>5}  most depended-on", "fan_in", "fan_out", "tests", "inst");
            for (p, d) in rows.iter().take(top) {
                println!("{:>6} {:>7} {:>5} {:>5.2}  {}{}", d.fan_in, d.fan_out, d.test_refs, d.instability, p, if d.in_cycle { "  (cycle)" } else { "" });
            }
        }
        Cmd::Metrics { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let src: Vec<discover::SourceFile> =
                files.into_iter().filter(|f| f.kind == discover::FileKind::Source).collect();
            let (file_metrics, mut funcs) = metrics::analyze_all(&src, &cfg.metrics);
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"files": file_metrics, "functions": funcs}))?);
                return Ok(());
            }
            funcs.sort_by_key(|f| std::cmp::Reverse((f.cognitive, f.lines)));
            println!("{} functions in {} files; {} with cognitive > {}\n",
                funcs.len(), file_metrics.len(),
                funcs.iter().filter(|f| f.cognitive > cfg.metrics.cognitive_hard).count(),
                cfg.metrics.cognitive_hard);
            println!("{:>4} {:>4} {:>4} {:>5} {:>3}  location", "cog", "cyc", "nest", "lines", "par");
            for f in funcs.iter().take(top) {
                println!("{:>4} {:>4} {:>4} {:>5} {:>3}  {}:{}  {}", f.cognitive, f.cyclomatic, f.max_nesting, f.lines, f.params, f.file, f.start_line, f.name);
            }
            let broken: Vec<&str> = file_metrics.iter().filter(|f| f.parse_errors).map(|f| f.path.as_str()).collect();
            if !broken.is_empty() {
                println!("\nfiles with parse errors ({}): {}", broken.len(), broken.join(", "));
            }
        }
        Cmd::Ast { path, errors } => {
            let lang = lang::Language::from_path(&path).ok_or_else(|| anyhow::anyhow!("unsupported extension"))?;
            let src = std::fs::read_to_string(&path)?;
            let tree = lang.parser().parse(&src, None).ok_or_else(|| anyhow::anyhow!("parse failed"))?;
            if !errors {
                println!("{}", tree.root_node().to_sexp());
                return Ok(());
            }
            let mut stack = vec![tree.root_node()];
            while let Some(n) = stack.pop() {
                if n.is_error() || n.is_missing() {
                    let text: String = n.utf8_text(src.as_bytes()).unwrap_or("").chars().take(80).collect();
                    println!("{}:{}  {}  {:?}", path.display(), n.start_position().row + 1, n.kind(), text);
                }
                let mut c = n.walk();
                for ch in n.children(&mut c) { stack.push(ch); }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lang::Language;

    fn sf(path: &str, content: &str) -> SourceFile {
        let lang = Language::from_path(std::path::Path::new(path)).unwrap();
        let kind = discover::classify(path, content, content.lines().count(), &config::Discover::default());
        SourceFile { path: path.into(), lang, kind, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    /// Twelve structurally different lines, long enough to be a clone.
    fn body(p: &str) -> String {
        format!(
            "    {p}_a = compute({p}, 1) + other[2]\n    if {p}_a and not {p}:\n        raise ValueError({p}_a)\n\
             \x20   for {p}_i in range(3):\n        {p}_a += {p}_i * 2\n    while {p}_a > 10:\n        {p}_a -= 1\n\
             \x20   {p}_b = [{p}_x for {p}_x in {p} if {p}_x]\n    try:\n        {p}_c = {p}_b[0]\n    except IndexError:\n        {p}_c = None\n"
        )
    }

    #[test]
    fn one_parse_matches_the_standalone_passes() {
        let files = vec![
            sf("pkg/__init__.py", ""),
            sf("pkg/a.py", &format!("from pkg import b\nfrom .c import thing\n\ndef alpha(x):\n{}\n    return x\n", body("aa"))),
            sf("pkg/b.py", &format!("import pkg.a\n\ndef beta(y):\n{}\n    return y\n", body("bb"))),
            sf("pkg/c.py", "from . import a\n"),
            sf("tests/test_a.py", "from pkg.a import alpha\n"),
            sf("vendor/lib/x.js", "import {y} from './y'\n"),
            sf("web/k.ts", "import x from '@/a/x'\nclass K { m(a: number) { if (a) {} else if (a > 1) {} return a ? 1 : 2; } }\n"),
            sf("web/src/a/x.ts", "export const g = (x: number) => x && x;\n"),
        ];
        let cfg = config::Config::default();
        let source: Vec<SourceFile> = files.iter().filter(|f| f.kind == FileKind::Source).cloned().collect();
        assert!(source.len() < files.len(), "the fixture needs non-source files");
        let ts = deps::TsConfigs::default();
        let once = parse_once(&files, &source, &cfg, &ts);

        let (fm, fs) = metrics::analyze_all(&source, &cfg.metrics);
        assert_eq!(json(&once.file_metrics), json(&fm));
        assert_eq!(json(&once.functions), json(&fs));
        assert_eq!(json(&once.deps), json(&deps::build(&files, &cfg.deps, &ts)));
        assert_eq!(json(&once.clones), json(&clones::detect(&source, &cfg.clones)));
        assert_eq!(once.deps.file_cycles.len(), 1);
        assert_eq!(once.clones.pairs.len(), 1, "{:?}", once.clones.pairs);
        assert!(once.functions.iter().any(|f| f.name == "K.m"));
    }

    /// `serde_json::Value` compares maps by content, so HashMap order does not matter.
    fn json<T: serde::Serialize>(v: &T) -> serde_json::Value {
        serde_json::to_value(v).unwrap()
    }
}
