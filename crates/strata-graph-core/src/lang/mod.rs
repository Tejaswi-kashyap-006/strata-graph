//! Language front ends. Each one turns a source file into raw, unresolved
//! facts (definitions, imports, calls, bases, type hints). Resolution into
//! graph edges is language-aware but lives in `resolve`.

pub mod python;
pub mod typescript;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Source language of a file. Resolution rules differ per language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Python,
    /// TypeScript and JavaScript (including TSX/JSX).
    TypeScript,
}

/// File extensions strata-graph indexes, and the language of each.
pub fn lang_for(path: &Path) -> Option<Lang> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".min.js") {
        return None;
    }
    match path.extension()?.to_str()? {
        "py" => Some(Lang::Python),
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TypeScript),
        _ => None,
    }
}

/// Parse a file with the front end for its extension.
pub fn parse(rel_path: &str, source: &str) -> anyhow::Result<ParsedFile> {
    match lang_for(Path::new(rel_path)) {
        Some(Lang::Python) => python::parse(rel_path, source),
        Some(Lang::TypeScript) => typescript::parse(rel_path, source),
        None => anyhow::bail!("unsupported file type: {rel_path}"),
    }
}

/// A definition as found in the syntax tree, before nesting is computed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawDef {
    pub is_class: bool,
    pub name: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub end_line: u32,
    /// Normalised return annotation (`-> Foo`), if any.
    pub returns: Option<String>,
    /// Decorated with a pytest fixture decorator.
    pub is_fixture: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RawImport {
    /// `import a.b` (binds `a`) or `import a.b as x` (binds `x`).
    Module {
        module: String,
        alias: Option<String>,
    },
    /// `from m import n [as k]`; `from_module` keeps leading dots if relative.
    From {
        from_module: String,
        name: String,
        alias: Option<String>,
    },
    /// `from m import *`
    Wildcard { from_module: String },
}
// TypeScript reuses these: `import {A as B} from "s"` and `export {A as B} from "s"`
// are `From` (default imports use name "default"), `import * as ns from "s"` is
// `Module` with an alias, and `export * from "s"` is `Wildcard`. Module strings are
// raw import specifiers, resolved against the importing file.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawImportAt {
    pub import: RawImport,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCall {
    pub callee: String,
    /// Source text of the receiver for `recv.callee(...)` calls.
    pub receiver: Option<String>,
    pub byte: usize,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawBase {
    /// Start byte of the class definition that declares the base.
    pub class_byte: usize,
    pub base: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeTarget {
    /// A local, a parameter, or a class-body field.
    Var(String),
    /// `self.<attr>`
    SelfAttr(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawType {
    pub target: TypeTarget,
    /// Constructor callee or annotation text, e.g. `Foo` or `models.Foo`.
    pub type_expr: String,
    pub byte: usize,
}

/// An expression a function returns or yields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawReturn {
    pub byte: usize,
    pub expr: String,
}

/// A name bound by a parameter, assignment, loop, `with` or `except` target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawLocal {
    pub name: String,
    pub byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedFile {
    #[serde(default)]
    pub lang: Lang,
    pub rel_path: String,
    pub hash: String,
    pub line_count: u32,
    pub defs: Vec<RawDef>,
    pub imports: Vec<RawImportAt>,
    pub calls: Vec<RawCall>,
    pub bases: Vec<RawBase>,
    pub types: Vec<RawType>,
    pub locals: Vec<RawLocal>,
    pub return_values: Vec<RawReturn>,
    /// Local name exported as `default` (TypeScript).
    #[serde(default)]
    pub default_export: Option<String>,
}
