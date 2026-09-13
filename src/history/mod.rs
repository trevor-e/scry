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

const FIX_WORDS: &[&str] = &[
    "fix", "bug", "hotfix", "regress", "broke", "broken", "crash", "repair", "correct", "patch",
    "wrong", "incorrect", "flake", "flaky",
];

pub fn subject_is_fix(subject: &str) -> bool {
    let s = subject.to_ascii_lowercase();
    // Word-boundary-ish: avoid "prefix", "suffix" matching "fix".
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| FIX_WORDS.iter().any(|f| w.starts_with(f)))
}

/// `tracked` limits the result to paths we care about (the discovered source set),
/// which keeps the co-change pair matrix small.
pub fn collect(root: &Path, since: &str, tracked: &HashSet<String>) -> Result<History> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
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
        let files: Vec<&str> = lines
            .map(str::trim)
            .filter(|l| !l.is_empty() && tracked.contains(*l))
            .collect();
        for f in &files {
            let e = hist.files.entry((*f).to_string()).or_default();
            e.commits += 1;
            e.fix_commits += usize::from(is_fix);
            e.author_set.insert(author.to_string());
            e.last_touched = e.last_touched.max(ts);
        }
        if files.len() >= 2 && files.len() <= MAX_COCHANGE_COMMIT_SIZE {
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
    co.sort_by(|x, y| y.together.cmp(&x.together).then(y.strength.partial_cmp(&x.strength).unwrap()));
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
        assert!(!subject_is_fix("Lobby: one page scroll"));
    }
}
