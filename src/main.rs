mod discover;
mod history;
mod lang;
mod metrics;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "scry", version, about = "Find the parts of a codebase most likely to need refactoring or to hide bugs")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
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
        /// Git --since window
        #[arg(long, default_value = "6 months ago")]
        since: String,
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
    match cli.cmd {
        Cmd::Files { path, json, top } => {
            let files = discover::walk(&path)?;
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
            let files = discover::walk(&path)?;
            let tracked: std::collections::HashSet<String> = files
                .iter()
                .filter(|f| f.kind == discover::FileKind::Source)
                .map(|f| f.path.clone())
                .collect();
            let hist = history::collect(&path, &since, &tracked)?;
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
        Cmd::Metrics { path, json, top } => {
            let files = discover::walk(&path)?;
            let src: Vec<discover::SourceFile> =
                files.into_iter().filter(|f| f.kind == discover::FileKind::Source).collect();
            let (file_metrics, mut funcs) = metrics::analyze_all(&src);
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"files": file_metrics, "functions": funcs}))?);
                return Ok(());
            }
            funcs.sort_by_key(|f| std::cmp::Reverse((f.cognitive, f.lines)));
            println!("{} functions in {} files; {} with cognitive > {}\n",
                funcs.len(), file_metrics.len(),
                funcs.iter().filter(|f| f.cognitive > metrics::COGNITIVE_HARD).count(),
                metrics::COGNITIVE_HARD);
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
