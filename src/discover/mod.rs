//! Repository walk and file classification.
//!
//! Every later pass consumes the [`SourceFile`] list produced here. Classification
//! matters because raw metrics on the wrong kind of file produce confident nonsense:
//! a 2,000-line map fixture is not a refactoring candidate, and generated API types
//! churn on every schema change without anyone touching them by hand.

use crate::config::Discover as Cfg;
use crate::lang::Language;
use anyhow::{Context, Result};
use ignore::{WalkBuilder, overrides::OverrideBuilder};
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

/// Keywords whose presence marks a line as control flow rather than data.
const LOGIC_MARKERS: &[&str] = &[
    "if ", "if(", "for ", "for(", "while ", "while(", "return", "def ", "fn ", "function",
    "=>", "class ", "match ", "switch", "try", "elif ", "else", "yield", "await ", "impl ",
];

pub fn walk(root: &Path, cfg: &Cfg) -> Result<Vec<SourceFile>> {
    let root = root.canonicalize().with_context(|| format!("cannot open {}", root.display()))?;
    let mut overrides = OverrideBuilder::new(&root);
    for glob in &cfg.exclude {
        // Override globs are whitelist by default; a leading `!` makes them excludes.
        overrides.add(&format!("!{glob}")).with_context(|| format!("bad exclude glob {glob:?}"))?;
    }
    let overrides = overrides.build().context("building exclude globs")?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in WalkBuilder::new(&root).hidden(true).git_ignore(true).overrides(overrides).build() {
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
        .filter_map(|p| load(&root, p, cfg))
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn load(root: &Path, path: &Path, cfg: &Cfg) -> Option<SourceFile> {
    let lang = Language::from_path(path)?;
    let bytes = std::fs::read(path).ok()?;
    let content = String::from_utf8(bytes).ok()?; // binary or non-UTF8: skip
    let rel = path
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let lines = content.lines().count();
    let kind = classify(&rel, &content, lines, cfg);
    Some(SourceFile { path: rel, lang, kind, lines, bytes: content.len(), content })
}

pub fn classify(rel: &str, content: &str, lines: usize, cfg: &Cfg) -> FileKind {
    let parts: Vec<&str> = rel.split('/').collect();
    let file = parts.last().copied().unwrap_or("");
    let dirs = &parts[..parts.len().saturating_sub(1)];
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    let lower = file.to_ascii_lowercase();
    let in_dir = |names: &[String]| dirs.iter().any(|d| names.iter().any(|n| n == d));

    if in_dir(&cfg.vendor_dirs) || lower.ends_with(".min.js") {
        return FileKind::Vendored;
    }
    if lower.ends_with(".d.ts") || in_dir(&cfg.generated_dirs) || looks_generated(content, cfg) {
        return FileKind::Generated;
    }
    if in_dir(&cfg.test_dirs)
        || cfg.test_stem_prefixes.iter().any(|p| stem.starts_with(p.as_str()))
        || cfg.test_stem_suffixes.iter().any(|x| stem.ends_with(x.as_str()))
        || cfg.test_stems.iter().any(|t| t == stem)
    {
        return FileKind::Test;
    }
    if in_dir(&cfg.data_dirs) || cfg.data_stem_contains.iter().any(|d| stem.contains(d.as_str())) {
        return FileKind::Data;
    }
    if lines >= cfg.data_min_lines && logic_density(content) < cfg.data_max_logic_density {
        return FileKind::Data;
    }
    FileKind::Source
}

fn looks_generated(content: &str, cfg: &Cfg) -> bool {
    content.lines().take(cfg.generated_marker_lines).any(|l| {
        let l = l.to_ascii_lowercase();
        cfg.generated_markers.iter().any(|m| l.contains(m.as_str()))
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
        let c = Cfg::default();
        assert_eq!(classify("backend/tests/test_x.py", "", 1, &c), FileKind::Test);
        assert_eq!(classify("frontend/src/a.test.tsx", "", 1, &c), FileKind::Test);
        assert_eq!(classify("node_modules/x/index.js", "", 1, &c), FileKind::Vendored);
        assert_eq!(classify("app/migrations/0001_init.py", "", 1, &c), FileKind::Generated);
        assert_eq!(classify("src/types/api.ts", "/* eslint-disable */\n// This file is auto-generated\n", 2, &c), FileKind::Generated);
        assert_eq!(classify("src/heroes/fixtures.ts", "", 1, &c), FileKind::Data);
        assert_eq!(classify("src/engine/combat.py", "def f():\n    return 1\n", 2, &c), FileKind::Source);
    }

    #[test]
    fn config_changes_classification() {
        let mut c = Cfg::default();
        c.test_dirs = vec!["qa".into()];
        c.vendor_dirs.push("extern".into());
        assert_eq!(classify("qa/x.py", "", 1, &c), FileKind::Test);
        assert_eq!(classify("tests/x.py", "", 1, &c), FileKind::Source);
        assert_eq!(classify("extern/x.py", "", 1, &c), FileKind::Vendored);
        c.data_min_lines = 10;
        c.data_max_logic_density = 0.5;
        assert_eq!(classify("a.py", &"x = 1\n".repeat(12), 12, &c), FileKind::Data);
    }

    #[test]
    fn literal_tables_are_data() {
        let table: String = (0..300).map(|i| format!("  {{ x: {i}, y: {i}, tile: \"grass\" }},\n")).collect();
        let content = format!("export const MAP = [\n{table}];\n");
        assert_eq!(classify("src/dev/bigMap.ts", &content, 302, &Cfg::default()), FileKind::Data);
    }

    #[test]
    fn exclude_globs_drop_files_from_the_walk() {
        let dir = std::env::temp_dir().join(format!("scry-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("art")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("art/gen.py"), "x = 1\n").unwrap();
        std::fs::write(dir.join("src/a.py"), "x = 1\n").unwrap();
        std::fs::write(dir.join("src/b_old.py"), "x = 1\n").unwrap();
        let mut c = Cfg::default();
        c.exclude = vec!["art/**".into(), "*_old.py".into()];
        let files = walk(&dir, &c).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.py"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
