//! Git-derived signals. These are the strongest predictors of defects we have,
//! and the cheapest: one `git log` over the window, parsed once.
//!
//! - churn: commits touching the file in the window
//! - fix commits: commits whose subject reads like a bug fix
//! - authors: distinct committers (bus factor)
//! - co-change: file pairs that ship together; paired with the import graph this
//!   exposes hidden coupling no static tool can see.

use crate::config::History as Cfg;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Default, Clone, Serialize)]
pub struct FileHistory {
    pub commits: usize,
    pub fix_commits: usize,
    pub authors: usize,
    /// Unix seconds of the newest commit touching the file.
    pub last_touched: i64,
    #[serde(skip)]
    author_set: HashSet<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoChange {
    pub a: String,
    pub b: String,
    /// Commits containing both files.
    pub together: usize,
    /// together / min(commits_a, commits_b): 1.0 means one never ships without the other.
    pub strength: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct History {
    pub window: String,
    pub commits_scanned: usize,
    pub files: HashMap<String, FileHistory>,
    pub co_changes: Vec<CoChange>,
}

pub fn subject_is_fix(subject: &str, fix_words: &[String]) -> bool {
    let s = subject.to_ascii_lowercase();
    s.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| fix_words.iter().any(|f| f == w))
}

/// Path of `root` inside its git work tree (`""` at the top level, `pkg/` below it).
/// `git log --name-only` prints paths from the work-tree root, so this is what
/// joins them back to the scanned root.
fn git_prefix(root: &Path) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(root).args(["rev-parse", "--show-prefix"]).output().context("running git rev-parse")?;
    if !out.status.success() {
        bail!("not a git work tree: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `tracked` limits the result to paths we care about (the discovered source set),
/// which keeps the co-change pair matrix small.
pub fn collect(root: &Path, cfg: &Cfg, tracked: &HashSet<String>) -> Result<History> {
    let since = cfg.since.as_str();
    let prefix = git_prefix(root)?;
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            // Otherwise non-ASCII paths arrive quoted and octal-escaped and never join.
            "-c",
            "core.quotePath=off",
            "log",
            "--no-merges",
            "--no-renames",
            "--name-only",
            "--format=%x01%H%x02%an%x02%ct%x02%s",
        ])
        .arg(format!("--since={since}"))
        .output()
        .context("running git log")?;
    if !out.status.success() {
        bail!("git log failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut hist = History { window: since.to_string(), ..Default::default() };
    let mut pairs: HashMap<(String, String), usize> = HashMap::new();

    for chunk in text.split('\u{1}').skip(1) {
        let mut lines = chunk.lines();
        let header = lines.next().unwrap_or("");
        let mut fields = header.split('\u{2}');
        let (_hash, author, ts, subject) = (
            fields.next().unwrap_or(""),
            fields.next().unwrap_or(""),
            fields.next().and_then(|t| t.parse::<i64>().ok()).unwrap_or(0),
            fields.next().unwrap_or(""),
        );
        hist.commits_scanned += 1;
        let is_fix = subject_is_fix(subject, &cfg.fix_words);
        let touched: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
        // Mass edits (formatters, lint sweeps, renames) say nothing about
        // coupling and are not fixes *of this file* whatever the subject says
        // (`fix(eslint): import/no-duplicates` over 66 files). Judge size on
        // everything the commit touched, not just the files we track.
        let mass_edit = touched.len() > cfg.max_cochange_commit_size;
        let files: Vec<&str> = touched
            .iter()
            .filter_map(|l| l.strip_prefix(prefix.as_str()))
            .filter(|l| tracked.contains(*l))
            .collect();
        for f in &files {
            let e = hist.files.entry((*f).to_string()).or_default();
            e.commits += 1;
            e.fix_commits += usize::from(is_fix && !mass_edit);
            e.author_set.insert(author.to_string());
            e.last_touched = e.last_touched.max(ts);
        }
        if files.len() >= 2 && !mass_edit {
            for i in 0..files.len() {
                for j in i + 1..files.len() {
                    let (a, b) = if files[i] < files[j] { (files[i], files[j]) } else { (files[j], files[i]) };
                    *pairs.entry((a.to_string(), b.to_string())).or_default() += 1;
                }
            }
        }
    }

    for e in hist.files.values_mut() {
        e.authors = e.author_set.len();
    }

    let mut co: Vec<CoChange> = pairs
        .into_iter()
        .filter(|(_, n)| *n >= cfg.min_cochange_together)
        .filter_map(|((a, b), together)| {
            let ca = hist.files.get(&a)?.commits;
            let cb = hist.files.get(&b)?.commits;
            let strength = together as f64 / ca.min(cb).max(1) as f64;
            (strength >= cfg.min_cochange_strength).then_some(CoChange { a, b, together, strength })
        })
        .collect();
    co.sort_by(|x, y| {
        y.together
            .cmp(&x.together)
            .then(y.strength.partial_cmp(&x.strength).unwrap())
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    hist.co_changes = co;
    Ok(hist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_detection_uses_whole_words() {
        let w = Cfg::default().fix_words;
        assert!(subject_is_fix("Fix hover flicker on tile picks", &w));
        assert!(subject_is_fix("battle: fixes regression in LoS", &w));
        assert!(subject_is_fix("Bugfix: wrong damage roll", &w));
        assert!(!subject_is_fix("Add prefix to route names", &w));
        assert!(!subject_is_fix("Add test fixtures for combat", &w));
        assert!(!subject_is_fix("Debug logging for the patcher", &w));
        assert!(subject_is_fix("fix(lobby): scroll jump", &w));
        assert!(!subject_is_fix("Lobby: one page scroll", &w));
        assert!(subject_is_fix("Lobby: one page scroll", &["lobby".to_string()]));
    }

    #[test]
    fn mass_edits_count_as_churn_but_not_as_fixes() {
        let dir = std::env::temp_dir().join(format!("scry-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "root"]);
        let commit = |subject: &str, n: usize| {
            for i in 0..n {
                std::fs::write(dir.join(format!("f{i}.py")), format!("{subject}{i}\n")).unwrap();
            }
            git(&["add", "-A"]);
            git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", subject]);
        };
        commit("fix(eslint): sweep", 30); // touches 30 files: churn, not a fix
        commit("fix: real bug", 1); // touches f0.py only
        let cfg = Cfg { since: "10 years ago".into(), ..Cfg::default() };
        let tracked: HashSet<String> = ["f0.py".to_string(), "f1.py".to_string()].into();
        let h = collect(&dir, &cfg, &tracked).unwrap();
        assert_eq!((h.files["f0.py"].commits, h.files["f0.py"].fix_commits), (2, 1), "{:?}", h.files["f0.py"]);
        assert_eq!((h.files["f1.py"].commits, h.files["f1.py"].fix_commits), (1, 0), "{:?}", h.files["f1.py"]);
        assert!(h.co_changes.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
