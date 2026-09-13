//! Language identification and tree-sitter grammar lookup.

use serde::Serialize;
use std::path::Path;

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
