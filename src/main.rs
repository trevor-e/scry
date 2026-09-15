mod clones;
mod clumps;
mod comments;
mod config;
mod dead;
mod declared;
mod deps;
mod discover;
mod helpers;
mod history;
mod lang;
mod mentions;
mod metrics;
mod plan;
mod regions;
mod report;
mod strings;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;

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
    /// Test units (test-file functions, inline #[cfg(test)] tests) naming each source file's symbols
    Mentions {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Exported symbols nothing outside their file uses, test-only symbols, never-read fields
    /// and never-constructed variants
    Dead {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Same-name helpers defined in several files, and helper bodies inlined instead of called
    Helpers {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Message literals spelled in several files, config literals with no shared constant,
    /// near-duplicate messages
    Strings {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Parameter tuples recurring across functions, with the slots no member reads
    Clumps {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Declared dependencies no file imports, feature flags nothing checks, config knobs no
    /// code reads (Cargo)
    Declared {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Top-level banners that cut a file into labelled sections; phase labels inside functions
    /// over the cognitive threshold
    Comments {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
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
    /// The refactor plan for one file: the scan pipeline, output restricted to that file
    Plan {
        /// The file, as a path on disk (absolute, or relative to the working directory)
        file: PathBuf,
        /// Repository root the scan runs on (the file must lie under it)
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Emit the plan as JSON
        #[arg(long)]
        json: bool,
        /// Git --since window for churn (overrides [history].since)
        #[arg(long)]
        since: Option<String>,
        /// Skip git history
        #[arg(long)]
        no_history: bool,
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
        | Cmd::Deps { path, .. } | Cmd::Mentions { path, .. } | Cmd::Dead { path, .. } | Cmd::Helpers { path, .. } | Cmd::Strings { path, .. } | Cmd::Clumps { path, .. } | Cmd::Declared { path, .. } | Cmd::Comments { path, .. } | Cmd::Metrics { path, .. } | Cmd::Config { path } => path.clone(),
        Cmd::Plan { root, .. } => root.clone(),
        Cmd::Ast { .. } => PathBuf::from("."),
    };
    let mut cfg = config::Config::load(&root, cli.config.as_deref())?;
    // A misspelt kind would silently drop every step of that kind from the plan.
    for k in cfg.plan.kind_priority.iter().filter(|k| !plan::KINDS.iter().any(|s| s.name() == k.as_str())) {
        eprintln!("warning: [plan].kind_priority names no step kind `{k}` (kinds: {})", plan::KINDS.iter().map(|s| s.name()).collect::<Vec<_>>().join(", "));
    }
    match cli.cmd {
        Cmd::Config { .. } => {
            print!("{}", cfg.to_toml());
        }
        Cmd::Scan { path, json, top, since, no_history } => {
            if let Some(s) = since {
                cfg.history.since = s;
            }
            let report = scan(&path, &cfg, top, no_history)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report::render_with(&report, top, cfg.plan.include_in_text_report));
            }
        }
        Cmd::Plan { file, root, json, since, no_history } => {
            if let Some(s) = since {
                cfg.history.since = s;
            }
            let rel = file
                .canonicalize()
                .with_context(|| format!("reading {}", file.display()))?
                .strip_prefix(root.canonicalize()?)
                .map_err(|_| anyhow::anyhow!("{} is not under {}", file.display(), root.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            // Every file's plan is built; the ranking only decides which ones `scan` prints.
            let report = scan(&root, &cfg, usize::MAX, no_history)?;
            let hot = report.hotspots.iter().find(|h| h.path == rel);
            if json {
                let (plan, more): (&[plan::Step], usize) = hot.map_or((&[], 0), |h| (&h.plan, h.plan_more));
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"path": rel, "plan": plan, "plan_more": more}))?);
                return Ok(());
            }
            match hot {
                Some(h) => print!("{}", plan::render(&rel, &h.plan, h.plan_more)),
                None => {
                    eprintln!("note: {rel} is not a ranked source file");
                    println!("no plan");
                }
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
            let n = hist.commits_scanned - hist.sweep_commits;
            println!("{} commits since {} ({} directory sweeps; lift >= {:.1} {} on {n} non-sweep commits)",
                hist.commits_scanned, hist.window, hist.sweep_commits, cfg.history.min_lift,
                if hist.lift_applied { "applied" } else { "not applied" });
            let cap = if cfg.history.fix_mass_edit_cap {
                format!("{} fix-worded commits over the {}-file mass-edit cap not counted as fixes", hist.capped_fix_commits, cfg.history.max_cochange_commit_size)
            } else {
                "fix cap off (every fix-worded commit is a fix)".to_string()
            };
            println!("{} bot commits (not authors); {cap}\n", hist.bot_commits);
            let mut rows: Vec<(&String, &history::FileHistory)> = hist.files.iter().collect();
            // Path last, so ties (the norm once the fix cap flattens fix counts) print in one order.
            rows.sort_by(|(pa, a), (pb, b)| (b.commits, b.fix_commits).cmp(&(a.commits, a.fix_commits)).then_with(|| pa.cmp(pb)));
            println!("{:>7} {:>5} {:>7} {:>6} {:>4}  path", "commits", "fixes", "authors", "sweeps", "bots");
            for (p, h) in rows.iter().take(top) {
                println!("{:>7} {:>5} {:>7} {:>6} {:>4}  {}", h.commits, h.fix_commits, h.authors, h.sweep_commits, h.bot_commits, p);
            }
            println!("\nco-change pairs (together non-sweep / raw / strength / lift):");
            for c in hist.co_changes.iter().take(top) {
                println!("{:>3} {:>3}  {:.2}  {:>5.1}x  {}  <->  {}", c.together_nonsweep, c.together, c.strength, c.lift, c.a, c.b);
            }
            if !hist.sweeps.is_empty() {
                println!("\nsweep commits (excluded from pair counts, still churn):");
                for s in hist.sweeps.iter().take(top) {
                    let dir = if s.dir.is_empty() { "." } else { s.dir.as_str() };
                    println!("  {}  {:>3} files, {}/{} of {dir}  {}", &s.hash[..s.hash.len().min(10)], s.files, s.dir_touched, s.dir_files, s.subject);
                }
            }
        }
        Cmd::Clones { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let src: Vec<discover::SourceFile> =
                files.into_iter().filter(|f| f.kind == discover::FileKind::Source).collect();
            let r = clones::detect(&src, &cfg.clones, &cfg.tests, cfg.plan.symbol_fallback);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            let tables = r.pairs.iter().filter(|p| p.kind == clones::CloneKind::Table).count();
            println!("{} clone pairs (>= {} tokens, {} tables) across {} files\n", r.pairs.len(), cfg.clones.min_tokens, tables, r.files.len());
            for p in r.pairs.iter().take(top) {
                println!("{}", report::pair_line(p));
            }
            let mut rows: Vec<(&String, &clones::FileClones)> = r.files.iter().collect();
            rows.sort_by(|x, y| y.1.clone_ratio.partial_cmp(&x.1.clone_ratio).unwrap());
            println!("\nmost duplicated files (cloned lines / ratio, table lines):");
            for (p, c) in rows.iter().take(top) {
                println!("{:>5} {:>5.0}%  {:>5}  {}", c.clone_lines, c.clone_ratio * 100.0, c.table_clone_lines, p);
            }
        }
        Cmd::Deps { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let g = deps::build(&files, &cfg.deps);
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
                println!("  {}", c.headline());
                println!("    files: {}", c.members.join(", "));
                if let Some(l) = c.cut_set_line() {
                    println!("    {l}");
                }
            }
            let mut rows: Vec<(&String, &deps::FileDeps)> = g.files.iter().collect();
            rows.sort_by_key(|(_, d)| std::cmp::Reverse(d.fan_in));
            println!("\n{:>6} {:>7} {:>5} {:>5}  most depended-on", "fan_in", "fan_out", "tests", "inst");
            for (p, d) in rows.iter().take(top) {
                println!("{:>6} {:>7} {:>5} {:>5.2}  {}{}", d.fan_in, d.fan_out, d.test_refs, d.instability, p, if d.in_cycle { "  (cycle)" } else { "" });
            }
        }
        Cmd::Mentions { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let idx = mentions::index_all(&files, &cfg.tests);
            if json {
                println!("{}", serde_json::to_string_pretty(&idx)?);
                return Ok(());
            }
            let test_files = files.iter().filter(|f| f.kind == discover::FileKind::Test).count();
            let unnamed = idx.files.values().filter(|m| m.test_units == 0).count();
            println!("{} test units indexed ({} in {test_files} test files, {} inline); symbols of {} of {} source files are named by at least one; {unnamed} named by none\n",
                idx.test_file_units + idx.inline_units, idx.test_file_units, idx.inline_units, idx.files.len() - unnamed, idx.files.len());
            let mut rows: Vec<(&String, &mentions::FileMentions)> = idx.files.iter().collect();
            // Fewest test units first: the files the suite never names are the finding.
            rows.sort_by(|(pa, a), (pb, b)| (a.test_units, std::cmp::Reverse(a.public_symbols)).cmp(&(b.test_units, std::cmp::Reverse(b.public_symbols))).then_with(|| pa.cmp(pb)));
            println!("{:>5} {:>6} {:>7} {:>6} {:>7}  path", "units", "inline", "symbols", "public", "unnamed");
            for (p, m) in rows.iter().take(top) {
                println!("{:>5} {:>6} {:>7} {:>6} {:>7}  {}", m.test_units, m.inline_units, m.symbols, m.public_symbols, m.unmentioned.len(), p);
            }
            println!("\npublic symbols named by no test unit (most first):");
            rows.sort_by(|(pa, a), (pb, b)| b.unmentioned.len().cmp(&a.unmentioned.len()).then_with(|| pa.cmp(pb)));
            for (p, m) in rows.iter().filter(|(_, m)| !m.unmentioned.is_empty()).take(top) {
                let names: Vec<String> = m.unmentioned.iter().map(|s| format!("{} {}-{}", s.name, s.start_line, s.end_line)).collect();
                println!("  {p} ({} of {}): {}", m.unmentioned.len(), m.public_symbols, names.join(", "));
            }
        }
        Cmd::Dead { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let idx = dead::index_all(&files, &cfg.dead);
            let r = dead::analyze(&idx, &path, &cfg.dead);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            print!("{}", dead::render(&r, top));
        }
        Cmd::Helpers { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let (sides, symbols) = helpers::index_all(&files, &cfg.helpers, &cfg.dead);
            let graph = deps::build(&files, &cfg.deps);
            let r = helpers::analyze(&sides, &symbols, &graph, Some(&path), &cfg.helpers);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            print!("{}", helpers::render(&r, top));
        }
        Cmd::Strings { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let r = strings::analyze(&strings::index_all(&files, &cfg.strings), &cfg.strings);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            print!("{}", strings::render(&r, top));
        }
        Cmd::Clumps { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let r = clumps::analyze(&clumps::index_all(&files, &cfg.clumps), &cfg.clumps);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            print!("{}", clumps::render(&r, top, &cfg.clumps.unused_prefix));
        }
        Cmd::Declared { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let r = declared::analyze(&declared::index_all(&files, &cfg.declared), &path, true, &cfg.declared, &cfg.discover.vendor_dirs);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            print!("{}", declared::render(&r, top));
        }
        Cmd::Comments { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let src: Vec<discover::SourceFile> =
                files.into_iter().filter(|f| f.kind == discover::FileKind::Source).collect();
            let walker = comments::Walker::new(&cfg.comments, &cfg.metrics);
            let (_, _, sides) = metrics::analyze_all_with(&src, &cfg.metrics, &cfg.tests, |root, f, regions, funcs, nodes| walker.file_side(root, f, regions, funcs, nodes));
            let r = comments::analyze(&sides, &cfg.comments);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            print!("{}", comments::render(&r, top, &cfg.comments));
        }
        Cmd::Metrics { path, json, top } => {
            let files = discover::walk(&path, &cfg.discover)?;
            let src: Vec<discover::SourceFile> =
                files.into_iter().filter(|f| f.kind == discover::FileKind::Source).collect();
            let (file_metrics, mut funcs) = metrics::analyze_all(&src, &cfg.metrics, &cfg.tests);
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"files": file_metrics, "functions": funcs}))?);
                return Ok(());
            }
            funcs.sort_by_key(|f| std::cmp::Reverse((f.cognitive, f.lines)));
            // Source units, as `scan` and `files[].functions` count them; tagged units apart.
            let in_tests = funcs.iter().filter(|f| f.in_test).count();
            let tagged = if in_tests == 0 { String::new() } else { format!(" (+{in_tests} in inline tests)") };
            println!("{} functions{tagged} in {} files; {} with cognitive > {}\n",
                funcs.len() - in_tests, file_metrics.len(),
                funcs.iter().filter(|f| !f.in_test && f.cognitive > cfg.metrics.cognitive_hard).count(),
                cfg.metrics.cognitive_hard);
            println!("{:>4} {:>4} {:>4} {:>5} {:>3}  location", "cog", "cyc", "nest", "lines", "par");
            for f in funcs.iter().take(top) {
                let tag = if f.in_test { " (in inline tests)" } else { "" };
                println!("{:>4} {:>4} {:>4} {:>5} {:>3}  {}:{}  {}{tag}", f.cognitive, f.cyclomatic, f.max_nesting, f.lines, f.params, f.file, f.start_line, f.name);
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

/// Every pass over `path`, folded into the report: what `scan` prints and `plan` reads one file of.
fn scan(path: &std::path::Path, cfg: &config::Config, top: usize, no_history: bool) -> Result<report::Report> {
    let files = discover::walk(path, &cfg.discover)?;
    let source: Vec<discover::SourceFile> =
        files.iter().filter(|f| f.kind == discover::FileKind::Source).cloned().collect();
    let tracked: std::collections::HashSet<String> = source.iter().map(|f| f.path.clone()).collect();
    let history = if no_history {
        None
    } else {
        match history::collect(path, &cfg.history, &tracked) {
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
    // The mentions, dead, helpers, strings, clumps, declared and comments passes read symbols,
    // inline test units, references, helper bodies, literals, parameter lists, crate / feature /
    // field references and banner / phase comments off the metrics trees; Test files are parsed
    // once here for the ones that need them.
    let walker = dead::Walker::new(&cfg.dead);
    let hwalker = helpers::Walker::new(&cfg.helpers);
    let swalker = strings::Walker::new(&cfg.strings);
    let cwalker = clumps::Walker::new(&cfg.clumps);
    let dwalker = declared::Walker::new(&cfg.declared);
    let mwalker = comments::Walker::new(&cfg.comments, &cfg.metrics);
    let (file_metrics, functions, per_file) =
        metrics::analyze_all_with(&source, &cfg.metrics, &cfg.tests, |root, f, regions, funcs, nodes| (mentions::source_side(root, f, regions, &cfg.tests), walker.file_index(root, f, regions), hwalker.file_side(root, f, regions), swalker.file_side(root, f, regions), cwalker.file_side(root, f, regions), dwalker.file_side(root, f, regions), mwalker.file_side(root, f, regions, funcs, nodes)));
    let (mut sides, mut indexes, mut helper_sides, mut string_sides, mut clump_sides, mut declared_sides, mut comment_sides) = (Vec::with_capacity(per_file.len()), Vec::with_capacity(per_file.len()), Vec::with_capacity(per_file.len()), Vec::with_capacity(per_file.len()), Vec::with_capacity(per_file.len()), Vec::with_capacity(per_file.len()), Vec::with_capacity(per_file.len()));
    for (m, d, h, s, c, x, k) in per_file {
        sides.push(m);
        indexes.push(d);
        helper_sides.push(h);
        string_sides.push(s);
        clump_sides.push(c);
        declared_sides.push(x);
        comment_sides.push(k);
    }
    let graph = deps::build(&files, &cfg.deps);
    let clone_report = clones::detect(&source, &cfg.clones, &cfg.tests, cfg.plan.symbol_fallback);
    let tests: Vec<(&discover::SourceFile, Option<tree_sitter::Tree>)> = {
        use rayon::prelude::*;
        files.par_iter().filter(|f| f.kind == discover::FileKind::Test).map(|f| (f, f.lang.parser().parse(f.content.as_bytes(), None))).collect()
    };
    let mentions = mentions::index_parsed(&sides, &tests, &cfg.tests);
    indexes.extend(dead::index_tests(&tests, &cfg.dead));
    let symbols = dead::SymbolIndex::build(indexes);
    let dead_report = dead::analyze(&symbols, path, &cfg.dead);
    if cfg.helpers.include_test_helpers {
        helper_sides.extend(tests.iter().map(|(f, t)| hwalker.file_side(t.as_ref().map(|t| t.root_node()), f, &[])));
    }
    let helpers_report = helpers::analyze(&helper_sides, &symbols, &graph, history.as_ref().map(|_| path), &cfg.helpers);
    let strings_report = strings::analyze(&string_sides, &cfg.strings);
    let clumps_report = clumps::analyze(&clump_sides, &cfg.clumps);
    declared_sides.extend(declared::index_tests(&tests, &cfg.declared));
    let declared_report = declared::analyze(&declared_sides, path, history.is_some(), &cfg.declared, &cfg.discover.vendor_dirs);
    let comments_report = comments::analyze(&comment_sides, &cfg.comments);
    Ok(report::build(
        report::Inputs {
            root: path.canonicalize()?.display().to_string(),
            files: &files,
            history: history.as_ref(),
            file_metrics: &file_metrics,
            functions: &functions,
            deps: &graph,
            clones: &clone_report,
            mentions: &mentions,
            dead: &dead_report,
            helpers: &helpers_report,
            helpers_weight: cfg.helpers.weight,
            strings: &strings_report,
            clumps: &clumps_report,
            clumps_weight: cfg.clumps.weight,
            clumps_prefix: &cfg.clumps.unused_prefix,
            declared: &declared_report,
            declared_weight: cfg.declared.weight,
            comments: &comments_report,
            cognitive_hard: cfg.metrics.cognitive_hard,
            tests: &cfg.tests,
            list_tables_separately: cfg.clones.list_tables_separately,
            history_cfg: &cfg.history,
            dedupe_cycle_reason: cfg.deps.dedupe_cycle_reason,
            plan: &cfg.plan,
        },
        top,
        &cfg.report,
        &cfg.discover.test_dirs,
    ))
}
