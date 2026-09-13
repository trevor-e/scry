mod discover;
mod history;
mod lang;

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
    }
    Ok(())
}
