//! Git-derived signals. These are the strongest predictors of defects we have,
//! and the cheapest: one `git log` over the window, parsed once.
//!
//! - churn: commits touching the file in the window
//! - fix commits: commits whose subject reads like a bug fix
//! - authors: distinct committers (bus factor)
//! - co-change: file pairs that ship together; paired with the import graph this
//!   exposes hidden coupling no static tool can see.

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

/// Commits touching more files than this are mass edits (renames, formatters)
/// and say nothing about coupling.
const MAX_COCHANGE_COMMIT_SIZE: usize = 25;
const MIN_COCHANGE_TOGETHER: usize = 3;
const MIN_COCHANGE_STRENGTH: f64 = 0.4;

/// Whole words only: "fixtures" and "prefix" are not fixes.
const FIX_WORDS: &[&str] = &[
    "fix", "fixes", "fixed", "fixing", "bugfix", "bugfixes", "hotfix", "hotfixes",
    "bug", "bugs", "buggy", "regression", "regressions", "regress", "regressed",
    "broke", "broken", "crash", "crashes", "crashed", "crashing",
    "repair", "repairs", "repaired", "correct", "corrects", "corrected", "correction",
    "patch", "patched", "wrong", "incorrect", "flake", "flaky", "flakey",
];

pub fn subject_is_fix(subject: &str) -> bool {
    let s = subject.to_ascii_lowercase();
    s.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| FIX_WORDS.contains(&w))
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
pub fn collect(root: &Path, since: &str, tracked: &HashSet<String>) -> Result<History> {
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
        let is_fix = subject_is_fix(subject);
        let touched: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
        // Mass edits (formatters, renames) say nothing about coupling; judge that
        // on everything the commit touched, not just the files we track.
        let mass_edit = touched.len() > MAX_COCHANGE_COMMIT_SIZE;
        let files: Vec<&str> = touched
            .iter()
            .filter_map(|l| l.strip_prefix(prefix.as_str()))
            .filter(|l| tracked.contains(*l))
            .collect();
        for f in &files {
            let e = hist.files.entry((*f).to_string()).or_default();
            e.commits += 1;
            e.fix_commits += usize::from(is_fix);
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
        .filter(|(_, n)| *n >= MIN_COCHANGE_TOGETHER)
        .filter_map(|((a, b), together)| {
            let ca = hist.files.get(&a)?.commits;
            let cb = hist.files.get(&b)?.commits;
            let strength = together as f64 / ca.min(cb).max(1) as f64;
            (strength >= MIN_COCHANGE_STRENGTH).then_some(CoChange { a, b, together, strength })
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
    fn fix_detection_uses_word_starts() {
        assert!(subject_is_fix("Fix hover flicker on tile picks"));
        assert!(subject_is_fix("battle: fixes regression in LoS"));
        assert!(subject_is_fix("Bugfix: wrong damage roll"));
        assert!(!subject_is_fix("Add prefix to route names"));
        assert!(!subject_is_fix("Add test fixtures for combat"));
        assert!(!subject_is_fix("Debug logging for the patcher"));
        assert!(subject_is_fix("fix(lobby): scroll jump"));
        assert!(!subject_is_fix("Lobby: one page scroll"));
    }
}
