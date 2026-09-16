//! Git-derived signals. These are the strongest predictors of defects we have,
//! and the cheapest: one `git log` over the window, parsed once.
//!
//! - churn: commits touching the file in the window
//! - fix commits: commits whose subject reads like a bug fix (a mass edit never is)
//! - authors: distinct non-bot committers (bus factor)
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
    /// Commits among `commits` that were directory sweeps: churn, but never pair evidence.
    pub sweep_commits: usize,
    /// Commits among `commits` by a `bot_patterns` author: churn, but never an author.
    pub bot_commits: usize,
    /// Fix-worded commits among `commits` that were over the mass-edit cap and so not counted in `fix_commits`.
    pub capped_fix_commits: usize,
    /// Unix seconds of the newest commit touching the file.
    pub last_touched: i64,
    #[serde(skip)]
    author_set: HashSet<String>,
}

impl FileHistory {
    /// Commits the pair rule and lift are judged on.
    pub fn nonsweep(&self) -> usize {
        self.commits - self.sweep_commits
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CoChange {
    pub a: String,
    pub b: String,
    /// Commits containing both files (sweeps included, mass edits excluded).
    pub together: usize,
    /// `together` without sweep commits: what the pair rule and lift are judged on.
    pub together_nonsweep: usize,
    /// together_nonsweep / min(non-sweep commits_a, commits_b): 1.0 means one never ships without the other.
    pub strength: f64,
    /// together_nonsweep / (commits_a x commits_b / N), with the per-file `commits` (sweeps
    /// included) and N = non-sweep commits in the window: how many times more often than chance
    /// the two ship together. A file that rides every sweep earns a lower lift, deliberately.
    pub lift: f64,
}

/// A commit that touched at least `sweep_fraction` (and `sweep_min_files`) of one directory's tracked files.
#[derive(Debug, Clone, Serialize)]
pub struct SweepCommit {
    pub hash: String,
    pub subject: String,
    /// Tracked files the commit touched in total.
    pub files: usize,
    /// The directory that qualified it (the one with the most touched files when several do).
    pub dir: String,
    /// Touched tracked files in `dir` / tracked files in `dir`.
    pub dir_touched: usize,
    pub dir_files: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct History {
    pub window: String,
    pub commits_scanned: usize,
    /// Directory-sweep commits in the window: excluded from pair counts, still churn.
    pub sweep_commits: usize,
    /// True when non-sweep commits reached `min_commits_for_lift`, so pairs under `min_lift` were dropped.
    pub lift_applied: bool,
    /// Commits whose author name matched `bot_patterns`: still churn, never an author.
    pub bot_commits: usize,
    /// Fix-worded commits over `max_cochange_commit_size` that `fix_mass_edit_cap` kept out of every file's fix count.
    pub capped_fix_commits: usize,
    pub sweeps: Vec<SweepCommit>,
    pub files: HashMap<String, FileHistory>,
    pub co_changes: Vec<CoChange>,
    /// Tracked files of every commit that fed `together_nonsweep` (non-sweep, non-mass-edit, >= 2
    /// tracked files), so the report can recount which shared import changed alongside a pair.
    #[serde(skip)]
    pub co_commits: Vec<Vec<String>>,
}

pub fn subject_is_fix(subject: &str, fix_words: &[String]) -> bool {
    let s = subject.to_ascii_lowercase();
    s.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| fix_words.iter().any(|f| f == w))
}

/// Case-insensitive substring match of the author name against `bot_patterns`.
pub fn author_is_bot(author: &str, bot_patterns: &[String]) -> bool {
    let a = author.to_lowercase();
    bot_patterns.iter().any(|p| !p.is_empty() && a.contains(&p.to_lowercase()))
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

/// Directory a tracked file sits in (`""` at the root).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `tracked` limits the result to paths we care about (the discovered source set),
/// which keeps the co-change pair matrix small.
pub fn collect(root: &Path, cfg: &Cfg, tracked: &HashSet<String>) -> Result<History> {
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
        .arg(format!("--since={}", cfg.since))
        .output()
        .context("running git log")?;
    if !out.status.success() {
        bail!("git log failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(parse(&String::from_utf8_lossy(&out.stdout), &prefix, cfg, tracked))
}

/// The pass proper, over the text of one `git log --name-only` in our format.
pub fn parse(text: &str, prefix: &str, cfg: &Cfg, tracked: &HashSet<String>) -> History {
    let mut hist = History { window: cfg.since.clone(), ..Default::default() };
    // (together, together_nonsweep)
    let mut pairs: HashMap<(String, String), (usize, usize)> = HashMap::new();
    let mut dir_size: HashMap<&str, usize> = HashMap::new();
    for f in tracked {
        *dir_size.entry(dir_of(f)).or_default() += 1;
    }

    for chunk in text.split('\u{1}').skip(1) {
        let mut lines = chunk.lines();
        let header = lines.next().unwrap_or("");
        let mut fields = header.split('\u{2}');
        let (hash, author, ts, subject) = (
            fields.next().unwrap_or(""),
            fields.next().unwrap_or(""),
            fields.next().and_then(|t| t.parse::<i64>().ok()).unwrap_or(0),
            fields.next().unwrap_or(""),
        );
        hist.commits_scanned += 1;
        let touched: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
        // Mass edits (formatters, lint sweeps, renames) say nothing about
        // coupling and are not fixes *of this file* whatever the subject says
        // (`fix(eslint): import/no-duplicates` over 66 files). Judge size on
        // everything the commit touched, not just the files we track.
        let mass_edit = touched.len() > cfg.max_cochange_commit_size;
        // A fix word on a mass edit ("review pass: fix eleven defects", 49 files) says nothing
        // about which file was broken: churn, not a fix, for every file it touched.
        let fix_worded = subject_is_fix(subject, &cfg.fix_words);
        let capped_fix = fix_worded && mass_edit && cfg.fix_mass_edit_cap;
        let is_fix = fix_worded && !capped_fix;
        // Bots (dependabot, renovate, pre-commit-ci) commit but do not know the code: churn,
        // never an author, so a file only ever touched by one human and a bot is bus factor 1.
        let is_bot = author_is_bot(author, &cfg.bot_patterns);
        hist.bot_commits += usize::from(is_bot);
        hist.capped_fix_commits += usize::from(capped_fix);
        let files: Vec<&str> = touched
            .iter()
            .filter_map(|l| l.strip_prefix(prefix))
            .filter(|l| tracked.contains(*l))
            .collect();
        // A sweep rewrites most of one directory: an agent session editing every command
        // file at once. Real churn, but "changed together" then means nothing.
        let mut by_dir: HashMap<&str, usize> = HashMap::new();
        for f in &files {
            *by_dir.entry(dir_of(f)).or_default() += 1;
        }
        let sweep_dir = by_dir
            .iter()
            .map(|(d, k)| (*d, *k, dir_size[d]))
            .filter(|(_, k, n)| *n >= cfg.sweep_min_dir_files && *k as f64 >= cfg.sweep_fraction * *n as f64 && *k >= cfg.sweep_min_files)
            .max_by(|x, y| x.1.cmp(&y.1).then_with(|| y.0.cmp(x.0)));
        let sweep = sweep_dir.is_some();
        if let Some((dir, dir_touched, dir_files)) = sweep_dir {
            hist.sweep_commits += 1;
            hist.sweeps.push(SweepCommit { hash: hash.to_string(), subject: subject.to_string(), files: files.len(), dir: dir.to_string(), dir_touched, dir_files });
        }
        for f in &files {
            let e = hist.files.entry((*f).to_string()).or_default();
            e.commits += 1;
            e.fix_commits += usize::from(is_fix);
            e.capped_fix_commits += usize::from(capped_fix);
            e.sweep_commits += usize::from(sweep);
            e.bot_commits += usize::from(is_bot);
            if !is_bot {
                e.author_set.insert(author.to_string());
            }
            e.last_touched = e.last_touched.max(ts);
        }
        if files.len() >= 2 && !mass_edit {
            for i in 0..files.len() {
                for j in i + 1..files.len() {
                    let (a, b) = if files[i] < files[j] { (files[i], files[j]) } else { (files[j], files[i]) };
                    let e = pairs.entry((a.to_string(), b.to_string())).or_default();
                    e.0 += 1;
                    e.1 += usize::from(!sweep);
                }
            }
            if !sweep {
                hist.co_commits.push(files.iter().map(|f| f.to_string()).collect());
            }
        }
    }

    for e in hist.files.values_mut() {
        e.authors = e.author_set.len();
    }

    // Null model: two files with ca and cb commits out of N meet ca x cb / N times by chance.
    // Strength is judged on non-sweep counts; lift keeps the per-file commit counts (sweeps
    // included) over the non-sweep N, so a file that rides every sweep must ship with its
    // partner that much more often outside them to count.
    let n = hist.commits_scanned - hist.sweep_commits;
    hist.lift_applied = n >= cfg.min_commits_for_lift;
    let mut co: Vec<CoChange> = pairs
        .into_iter()
        .filter(|(_, (_, nonsweep))| *nonsweep >= cfg.min_cochange_together)
        .filter_map(|((a, b), (together, together_nonsweep))| {
            let (fa, fb) = (hist.files.get(&a)?, hist.files.get(&b)?);
            let strength = together_nonsweep as f64 / fa.nonsweep().min(fb.nonsweep()).max(1) as f64;
            let lift = together_nonsweep as f64 * n as f64 / (fa.commits * fb.commits).max(1) as f64;
            let keep = strength >= cfg.min_cochange_strength && (!hist.lift_applied || lift >= cfg.min_lift);
            keep.then_some(CoChange { a, b, together, together_nonsweep, strength, lift })
        })
        .collect();
    co.sort_by(|x, y| {
        y.together_nonsweep
            .cmp(&x.together_nonsweep)
            .then(y.strength.partial_cmp(&x.strength).unwrap())
            .then(y.lift.partial_cmp(&x.lift).unwrap())
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    hist.co_changes = co;
    hist
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

    /// One `git log` chunk per (author, subject, files), in the `--format` we parse.
    fn log(commits: &[(&str, &str, &[&str])]) -> String {
        commits
            .iter()
            .enumerate()
            .map(|(i, (author, subject, files))| format!("\u{1}h{i}\u{2}{author}\u{2}{}\u{2}{subject}\n{}\n\n", 1000 + i, files.join("\n")))
            .collect()
    }

    fn tracked(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    const CMD: [&str; 6] = ["src/cmd/a.rs", "src/cmd/b.rs", "src/cmd/c.rs", "src/cmd/d.rs", "src/cmd/e.rs", "src/cmd/f.rs"];

    #[test]
    fn sweeps_are_churn_but_not_pair_evidence() {
        let mut t = tracked(&CMD);
        t.extend(tracked(&["src/x.rs", "src/y.rs", "src/z.rs"]));
        let five = &CMD[..5];
        let text = log(&[
            ("bob", "fix everything", &["src/cmd/a.rs", "src/cmd/b.rs", "src/cmd/c.rs", "src/cmd/d.rs", "src/cmd/e.rs", "src/cmd/f.rs", "src/x.rs"]),
            ("ann", "sweep again", &CMD),
            ("ann", "n1", &["src/cmd/a.rs", "src/cmd/b.rs"]),
            ("ann", "n2", &["src/cmd/a.rs", "src/cmd/b.rs"]),
            ("ann", "n3", &["src/cmd/a.rs", "src/cmd/b.rs", "src/cmd/c.rs"]),
            // All of src, but src holds only 3 tracked files: under sweep_min_dir_files.
            ("ann", "n4", &["src/x.rs", "src/y.rs", "src/z.rs", "src/cmd/c.rs"]),
            // 5 of 6 is over the fraction but under the absolute floor of 6.
            ("ann", "p", five),
        ]);
        let h = parse(&text, "", &Cfg::default(), &t);
        assert_eq!((h.commits_scanned, h.sweep_commits, h.lift_applied), (7, 2, false));
        assert_eq!(h.sweeps.iter().map(|s| (s.hash.as_str(), s.files, s.dir.as_str(), s.dir_touched, s.dir_files)).collect::<Vec<_>>(), vec![("h0", 7, "src/cmd", 6, 6), ("h1", 6, "src/cmd", 6, 6)]);
        let a = &h.files["src/cmd/a.rs"];
        // Sweeps count toward commits, fix commits and authors.
        assert_eq!((a.commits, a.sweep_commits, a.nonsweep(), a.fix_commits, a.authors), (6, 2, 4, 1, 2));
        assert_eq!(h.files["src/cmd/f.rs"].commits, 2);
        // a<->b: 6 raw, 4 outside sweeps; a<->c has only 2 outside sweeps; d<->e only 1.
        assert_eq!(h.co_changes.len(), 1, "{:?}", h.co_changes);
        let c = &h.co_changes[0];
        assert_eq!((c.a.as_str(), c.b.as_str(), c.together, c.together_nonsweep, c.strength), ("src/cmd/a.rs", "src/cmd/b.rs", 6, 4, 1.0));
        // lift = 4 / (6 x 6 / 5): reported but not applied below min_commits_for_lift.
        assert!((c.lift - 4.0 * 5.0 / 36.0).abs() < 1e-9, "{}", c.lift);
        assert_eq!(h.co_commits.len(), 5);
        assert!(h.co_commits.iter().all(|c| c.len() < 6));
    }

    #[test]
    fn lift_is_applied_once_the_window_is_long_enough() {
        let t = tracked(&["src/a.rs", "src/b.rs", "src/q.rs", "src/r.rs"]);
        let mut commits: Vec<(&str, &str, &[&str])> = vec![("ann", "ab", &["src/a.rs", "src/b.rs"]); 3];
        commits.extend(std::iter::repeat_n(("ann", "q", &["src/q.rs"][..]), 20));
        let h = parse(&log(&commits), "", &Cfg::default(), &t);
        assert!(h.lift_applied);
        assert_eq!(h.co_changes.len(), 1);
        assert!((h.co_changes[0].lift - 3.0 * 23.0 / 9.0).abs() < 1e-9);
        // Nine 4-file commits over all of src (4 < sweep_min_files: not sweeps) make every file
        // busy: a<->b is now 12 of N=32 with 12 commits each, lift 12 / (12 x 12 / 32) = 2.7, and
        // every pair through q.rs (29 commits) is lower still. All dropped.
        commits.extend(std::iter::repeat_n(("ann", "q", &["src/a.rs", "src/b.rs", "src/q.rs", "src/r.rs"][..]), 9));
        let h = parse(&log(&commits), "", &Cfg::default(), &t);
        assert_eq!((h.sweep_commits, h.lift_applied), (0, true));
        assert!(h.co_changes.is_empty(), "{:?}", h.co_changes);
        // Below the gate the same pairs are kept with their lift printed.
        let h = parse(&log(&commits), "", &Cfg { min_commits_for_lift: 100, ..Cfg::default() }, &t);
        let ab = h.co_changes.iter().find(|c| c.a == "src/a.rs" && c.b == "src/b.rs").unwrap();
        assert!(!h.lift_applied && ab.together_nonsweep == 12 && (ab.lift - 12.0 * 32.0 / 144.0).abs() < 1e-9, "{ab:?}");
    }

    #[test]
    fn bots_are_churn_but_never_authors() {
        let t = tracked(&["src/a.rs", "src/b.rs"]);
        let text = log(&[
            ("ann", "add a", &["src/a.rs", "src/b.rs"]),
            ("dependabot[bot]", "bump serde", &["src/a.rs"]),
            ("Renovate Bot", "update deps", &["src/a.rs"]),
            ("pre-commit-ci[bot]", "[pre-commit.ci] auto fixes", &["src/a.rs", "src/b.rs"]),
            ("bob", "fix b", &["src/b.rs"]),
        ]);
        let h = parse(&text, "", &Cfg::default(), &t);
        assert_eq!(h.bot_commits, 3);
        let a = &h.files["src/a.rs"];
        // Four commits, three by bots (matched case-insensitively): one author, so bus factor 1.
        assert_eq!((a.commits, a.bot_commits, a.authors, a.fix_commits), (4, 3, 1, 1));
        let b = &h.files["src/b.rs"];
        assert_eq!((b.commits, b.bot_commits, b.authors, b.fix_commits), (3, 1, 2, 2));
        // An empty pattern list counts everyone; a pattern is a substring of the name only.
        let h = parse(&text, "", &Cfg { bot_patterns: vec![], ..Cfg::default() }, &t);
        assert_eq!((h.bot_commits, h.files["src/a.rs"].authors), (0, 4));
        assert!(author_is_bot("GitHub Actions [BOT]", &Cfg::default().bot_patterns));
        assert!(!author_is_bot("Robot Jones", &Cfg::default().bot_patterns));
        assert!(!author_is_bot("ann", &["".to_string()]));
    }

    #[test]
    fn a_fix_word_on_a_mass_edit_is_not_a_fix() {
        let t = tracked(&["src/a.rs", "src/b.rs"]);
        let docs: Vec<String> = (0..24).map(|i| format!("docs/{i}.md")).collect();
        let mut big: Vec<&str> = vec!["src/a.rs", "src/b.rs"];
        big.extend(docs.iter().map(String::as_str));
        let text = log(&[("ann", "Review pass: fix eleven defects", &big), ("ann", "fix a", &["src/a.rs"])]);
        // 26 files > cap of 25: churn for both, a fix for neither; the small fix still counts.
        let h = parse(&text, "", &Cfg::default(), &t);
        assert_eq!(h.capped_fix_commits, 1);
        assert_eq!((h.files["src/a.rs"].commits, h.files["src/a.rs"].fix_commits, h.files["src/a.rs"].capped_fix_commits), (2, 1, 1));
        assert_eq!((h.files["src/b.rs"].commits, h.files["src/b.rs"].fix_commits, h.files["src/b.rs"].capped_fix_commits), (1, 0, 1));
        // Exactly at the cap it is not a mass edit.
        let at_cap: Vec<&str> = big[..25].to_vec();
        let h = parse(&log(&[("ann", "fix many", &at_cap)]), "", &Cfg::default(), &t);
        assert_eq!((h.capped_fix_commits, h.files["src/a.rs"].fix_commits), (0, 1));
        // Knob off: the old behaviour, every fix-worded commit is a fix.
        let h = parse(&text, "", &Cfg { fix_mass_edit_cap: false, ..Cfg::default() }, &t);
        assert_eq!((h.capped_fix_commits, h.files["src/a.rs"].fix_commits, h.files["src/b.rs"].fix_commits), (0, 2, 1));
    }

    #[test]
    fn mass_edits_stay_excluded_and_a_prefix_is_stripped() {
        let t = tracked(&CMD);
        let docs: Vec<String> = (0..24).map(|i| format!("pkg/docs/{i}.md")).collect();
        let mut big: Vec<&str> = vec!["pkg/src/cmd/a.rs", "pkg/src/cmd/b.rs", "pkg/src/cmd/c.rs", "pkg/src/cmd/d.rs", "pkg/src/cmd/e.rs", "pkg/src/cmd/f.rs"];
        big.extend(docs.iter().map(String::as_str));
        let text = log(&[("ann", "reformat", &big), ("ann", "ab", &["pkg/src/cmd/a.rs", "pkg/src/cmd/b.rs"])]);
        let h = parse(&text, "pkg/", &Cfg::default(), &t);
        // 30 files: a mass edit (no pair evidence) that is also a sweep of src/cmd.
        assert_eq!((h.commits_scanned, h.sweep_commits, h.sweeps[0].dir.as_str(), h.sweeps[0].files), (2, 1, "src/cmd", 6));
        assert_eq!((h.files["src/cmd/a.rs"].commits, h.files["src/cmd/a.rs"].sweep_commits), (2, 1));
        assert_eq!(h.co_commits, vec![vec!["src/cmd/a.rs".to_string(), "src/cmd/b.rs".to_string()]]);
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
