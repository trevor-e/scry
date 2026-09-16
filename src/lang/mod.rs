//! Language identification and tree-sitter grammar lookup.

use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

thread_local! {
    /// One parser per grammar per thread: `Parser::new` + `set_language` is
    /// not free, and a scan parses thousands of files on a handful of threads.
    static PARSERS: RefCell<HashMap<Language, tree_sitter::Parser>> = RefCell::new(HashMap::new());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Rust,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        Some(match ext {
            "py" | "pyi" => Self::Python,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Self::JavaScript,
            "rs" => Self::Rust,
            _ => return None,
        })
    }

    pub fn grammar(self) -> tree_sitter::Language {
        match self {
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            // The TSX grammar is a superset of JS syntax; good enough for JSX files too.
            Self::Tsx | Self::JavaScript => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    pub fn parser(self) -> tree_sitter::Parser {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&self.grammar())
            .expect("grammar ABI matches the linked tree-sitter runtime");
        parser
    }

    /// Parse `src` with this thread's cached parser for the grammar. `None`
    /// only when tree-sitter itself gives up, which without a timeout or
    /// cancellation flag it does not; callers treat it as an empty file.
    pub fn parse(self, src: &str) -> Option<tree_sitter::Tree> {
        PARSERS.with(|cache| {
            let mut cache = cache.borrow_mut();
            let parser = cache.entry(self).or_insert_with(|| self.parser());
            parser.parse(src, None)
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
        }
    }
}
