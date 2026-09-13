//! Repository walk and file classification.
//!
//! Every later pass consumes the [`SourceFile`] list produced here. Classification
//! matters because raw metrics on the wrong kind of file produce confident nonsense:
//! a 2,000-line map fixture is not a refactoring candidate, and generated API types
//! churn on every schema change without anyone touching them by hand.

use crate::lang::Language;
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// Hand-written logic. The only kind that is ranked.
    Source,
    /// Test code. Tracked for test-proximity, never ranked.
    Test,
    /// Data-shaped code: fixtures, big literal tables, map definitions.
    Data,
    /// Machine-written: codegen output, migrations, `.d.ts`, minified bundles.
    Generated,
    /// Third-party code checked into the tree.
    Vendored,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFile {
    /// Path relative to the scanned root, always with `/` separators.
    pub path: String,
    pub lang: Language,
    pub kind: FileKind,
    pub lines: usize,
    pub bytes: usize,
    #[serde(skip)]
    pub content: String,
}

impl SourceFile {
    pub fn module_dir(&self) -> &str {
        match self.path.rfind('/') {
            Some(i) => &self.path[..i],
            None => "",
        }
    }
}

const VENDOR_DIRS: &[&str] = &[
    "node_modules", "vendor", "third_party", "thirdparty", "dist", "build", "target", ".venv",
    "venv", "site-packages", "__pycache__", ".git",
];
const TEST_DIRS: &[&str] = &["test", "tests", "__tests__", "e2e", "spec", "specs", "testing"];
const DATA_DIRS: &[&str] = &["fixtures", "fixture", "snapshots", "__snapshots__", "testdata"];
const GENERATED_DIRS: &[&str] = &["migrations", "generated", "__generated__", "gen"];

/// Keywords whose presence marks a line as control flow rather than data.
const LOGIC_MARKERS: &[&str] = &[
    "if ", "if(", "for ", "for(", "while ", "while(", "return", "def ", "fn ", "function",
    "=>", "class ", "match ", "switch", "try", "elif ", "else", "yield", "await ", "impl ",
];

pub fn walk(root: &Path) -> Result<Vec<SourceFile>> {
    let root = root.canonicalize().with_context(|| format!("cannot open {}", root.display()))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in WalkBuilder::new(&root).hidden(true).git_ignore(true).build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if Language::from_path(entry.path()).is_some() {
            paths.push(entry.into_path());
        }
    }

    let mut files: Vec<SourceFile> = paths
        .par_iter()
        .filter_map(|p| load(&root, p))
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn load(root: &Path, path: &Path) -> Option<SourceFile> {
    let lang = Language::from_path(path)?;
    let bytes = std::fs::read(path).ok()?;
    let content = String::from_utf8(bytes).ok()?; // binary or non-UTF8: skip
    let rel = path
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let lines = content.lines().count();
    let kind = classify(&rel, &content, lines);
    Some(SourceFile { path: rel, lang, kind, lines, bytes: content.len(), content })
}

pub fn classify(rel: &str, content: &str, lines: usize) -> FileKind {
    let parts: Vec<&str> = rel.split('/').collect();
    let file = parts.last().copied().unwrap_or("");
    let dirs = &parts[..parts.len().saturating_sub(1)];
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    let lower = file.to_ascii_lowercase();

    if dirs.iter().any(|d| VENDOR_DIRS.contains(d)) || lower.ends_with(".min.js") {
        return FileKind::Vendored;
    }
    if lower.ends_with(".d.ts") || dirs.iter().any(|d| GENERATED_DIRS.contains(d)) || looks_generated(content) {
        return FileKind::Generated;
    }
    if dirs.iter().any(|d| TEST_DIRS.contains(d))
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
        || stem == "conftest"
        || stem.ends_with(".snapshots")
        || stem.ends_with("Probes")
    {
        return FileKind::Test;
    }
    if dirs.iter().any(|d| DATA_DIRS.contains(d)) || stem.contains("fixture") {
        return FileKind::Data;
    }
    if lines >= 150 && logic_density(content) < 0.04 {
        return FileKind::Data;
    }
    FileKind::Source
}

fn looks_generated(content: &str) -> bool {
    content.lines().take(8).any(|l| {
        let l = l.to_ascii_lowercase();
        l.contains("@generated")
            || l.contains("do not edit")
            || l.contains("auto-generated")
            || l.contains("automatically generated")
            || l.contains("generated by")
    })
}

/// Fraction of non-blank lines that carry a control-flow marker. Big literal
/// tables sit near zero; real logic sits well above 0.1.
pub fn logic_density(content: &str) -> f64 {
    let mut total = 0usize;
    let mut logic = 0usize;
    for line in content.lines() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with("//") || t.starts_with('#') || t.starts_with('*') {
            continue;
        }
        total += 1;
        if LOGIC_MARKERS.iter().any(|m| t.contains(m)) {
            logic += 1;
        }
    }
    if total == 0 { 0.0 } else { logic as f64 / total as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_path() {
        assert_eq!(classify("backend/tests/test_x.py", "", 1), FileKind::Test);
        assert_eq!(classify("frontend/src/a.test.tsx", "", 1), FileKind::Test);
        assert_eq!(classify("node_modules/x/index.js", "", 1), FileKind::Vendored);
        assert_eq!(classify("app/migrations/0001_init.py", "", 1), FileKind::Generated);
        assert_eq!(classify("src/types/api.ts", "/* eslint-disable */\n// This file is auto-generated\n", 2), FileKind::Generated);
        assert_eq!(classify("src/heroes/fixtures.ts", "", 1), FileKind::Data);
        assert_eq!(classify("src/engine/combat.py", "def f():\n    return 1\n", 2), FileKind::Source);
    }

    #[test]
    fn literal_tables_are_data() {
        let table: String = (0..300).map(|i| format!("  {{ x: {i}, y: {i}, tile: \"grass\" }},\n")).collect();
        let content = format!("export const MAP = [\n{table}];\n");
        assert_eq!(classify("src/dev/bigMap.ts", &content, 302), FileKind::Data);
    }
}
