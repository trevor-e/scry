//! `tsconfig.json` / `jsconfig.json` path mapping for bare specifiers.
//!
//! A repo that writes `import x from 'sentry/utils'` has told the compiler
//! where `sentry/*` lives in `compilerOptions.paths`; without reading it the
//! whole graph is missing (Sentry: 92% of imports). The nearest config above
//! the importing file applies, `extends` chains are followed for local files,
//! and comments / trailing commas are tolerated because tsc tolerates them.

use ignore::WalkBuilder;
use std::path::Path;

use super::{dir_of, join};

#[derive(Debug, Clone)]
struct Pattern {
    /// Text before the `*`; the whole key when there is no `*`.
    prefix: String,
    /// Text after the `*`; `None` means the key is exact.
    suffix: Option<String>,
    /// Root-relative targets, still carrying their `*`.
    targets: Vec<String>,
}

impl Pattern {
    fn captured<'a>(&self, spec: &'a str) -> Option<&'a str> {
        match &self.suffix {
            None => (spec == self.prefix).then_some(""),
            Some(suf) => spec.strip_prefix(self.prefix.as_str())?.strip_suffix(suf.as_str()),
        }
    }
}

#[derive(Debug, Clone)]
struct TsConfig {
    /// Root-relative directory holding the config file.
    dir: String,
    /// Root-relative `baseUrl`, when set.
    base_url: Option<String>,
    paths: Vec<Pattern>,
}

/// Every config found under the scanned root, deepest directory first.
#[derive(Debug, Clone, Default)]
pub struct TsConfigs {
    configs: Vec<TsConfig>,
}

/// What one file contributes after its `extends` chain is merged in.
#[derive(Default)]
struct Raw {
    base_url: Option<String>,
    paths: Option<Vec<Pattern>>,
}

impl TsConfigs {
    /// Walk `root` for `tsconfig.json` / `jsconfig.json`, honouring `.gitignore`
    /// and skipping hidden dirs and `node_modules` like discovery does.
    pub fn load(root: &Path) -> Self {
        let mut configs = Vec::new();
        let walker = WalkBuilder::new(root)
            .hidden(true)
            .git_ignore(true)
            .filter_entry(|e| e.file_name() != "node_modules")
            .build();
        for entry in walker.flatten() {
            let name = entry.file_name().to_string_lossy();
            if (name != "tsconfig.json" && name != "jsconfig.json") || !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let raw = read(root, &rel, 0);
            configs.push(TsConfig {
                dir: dir_of(&rel).to_string(),
                base_url: raw.base_url,
                paths: raw.paths.unwrap_or_default(),
            });
        }
        configs.sort_by(|a, b| b.dir.len().cmp(&a.dir.len()).then_with(|| a.dir.cmp(&b.dir)));
        Self { configs }
    }

    /// Root-relative candidates for a bare specifier imported from `from_dir`,
    /// before extension / index probing: the longest matching `paths` key's
    /// targets in order, then `<baseUrl>/<spec>` when a baseUrl is set.
    pub fn candidates(&self, from_dir: &str, spec: &str) -> Vec<String> {
        let nearest = self
            .configs
            .iter()
            .find(|c| c.dir.is_empty() || from_dir == c.dir || from_dir.starts_with(&format!("{}/", c.dir)));
        let Some(c) = nearest else { return Vec::new() };
        let mut out = Vec::new();
        let best = c
            .paths
            .iter()
            .filter_map(|p| p.captured(spec).map(|cap| (p, cap)))
            .max_by_key(|(p, _)| (p.suffix.is_none(), p.prefix.len()));
        if let Some((p, cap)) = best {
            out.extend(p.targets.iter().map(|t| t.replacen('*', cap, 1)));
        }
        if let Some(b) = &c.base_url {
            out.push(join(b, spec));
        }
        out
    }
}

/// Parse one config file, folding in what it `extends` (local files only).
fn read(root: &Path, rel: &str, depth: usize) -> Raw {
    if depth > 8 {
        return Raw::default();
    }
    let Ok(text) = std::fs::read_to_string(root.join(rel)) else { return Raw::default() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else { return Raw::default() };
    let dir = dir_of(rel);
    let opts = v.get("compilerOptions");
    let base_url = opts.and_then(|o| o.get("baseUrl")).and_then(|b| b.as_str()).map(|b| join(dir, b));
    let target_base = base_url.clone().unwrap_or_else(|| dir.to_string());
    let paths = opts.and_then(|o| o.get("paths")).and_then(|p| p.as_object()).map(|map| {
        map.iter()
            .map(|(key, targets)| {
                let targets: Vec<String> = targets
                    .as_array()
                    .map(|a| a.iter().filter_map(|t| t.as_str()).map(|t| join(&target_base, t)).collect())
                    .unwrap_or_default();
                match key.split_once('*') {
                    Some((pre, suf)) => Pattern { prefix: pre.to_string(), suffix: Some(suf.to_string()), targets },
                    None => Pattern { prefix: key.clone(), suffix: None, targets },
                }
            })
            .collect()
    });
    let mut own = Raw { base_url, paths };
    // `extends` may be a string or (TS 5) an array; package names are not ours to read.
    let parents: Vec<String> = match v.get("extends") {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|s| s.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    };
    for p in parents {
        if !p.starts_with('.') {
            continue;
        }
        let mut path = join(dir, &p);
        if !path.ends_with(".json") {
            path.push_str(".json");
        }
        let parent = read(root, &path, depth + 1);
        own.base_url = own.base_url.or(parent.base_url);
        own.paths = own.paths.or(parent.paths);
    }
    own
}

/// Drop `//` and `/* */` comments outside strings and commas that directly
/// precede a closing bracket, which is all tsc's tolerant parser adds to JSON.
fn strip_jsonc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                out.extend_from_slice(&b[start..i]);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b',' => {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if !(j < b.len() && (b[j] == b'}' || b[j] == b']')) {
                    out.push(b',');
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_comments_and_trailing_commas_are_tolerated() {
        let s = "{\n  // c\n  \"a\": [1, 2,], /* d */ \"b\": \"x//y\",\n}";
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(s)).unwrap();
        assert_eq!(v["a"], serde_json::json!([1, 2]));
        assert_eq!(v["b"], "x//y");
    }

    #[test]
    fn nearest_config_paths_and_extends() {
        let dir = std::env::temp_dir().join(format!("scry-tsconfig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("packages/web")).unwrap();
        std::fs::write(
            dir.join("tsconfig.base.json"),
            "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"sentry/*\": [\"./static/app/*\"], \"lodash\": [\"./shims/lodash\"] } } }",
        )
        .unwrap();
        std::fs::write(dir.join("tsconfig.json"), "{ \"extends\": \"./tsconfig.base\", // root\n \"include\": [\"static\"], }").unwrap();
        std::fs::write(
            dir.join("packages/web/tsconfig.json"),
            "{ \"compilerOptions\": { \"paths\": { \"@web/*\": [\"./src/*\"] } } }",
        )
        .unwrap();
        let ts = TsConfigs::load(&dir);
        assert_eq!(ts.candidates("static/app/views", "sentry/utils/x"), vec!["static/app/utils/x", "sentry/utils/x"]);
        assert_eq!(ts.candidates("static/app", "lodash"), vec!["shims/lodash", "lodash"]);
        // The nearer config applies below packages/web and has no baseUrl.
        assert_eq!(ts.candidates("packages/web/src", "@web/a"), vec!["packages/web/src/a"]);
        assert!(ts.candidates("packages/web/src", "sentry/x").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
