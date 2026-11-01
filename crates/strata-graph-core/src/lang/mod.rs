//! Language front ends. Each one turns a source file into raw, unresolved
//! facts (definitions, imports, calls, bases, type hints). Resolution into
//! graph edges is language-aware but lives in `resolve`.
//!
//! Byte offsets in these facts are positions in the file's UTF-8 source; they
//! are used to work out which definition encloses a call, a type hint or a
//! local, never shown to users. Lines are 1-based.

pub mod python;
pub mod typescript;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Source language of a file. Resolution rules differ per language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// Python (`.py`).
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
    /// A class or TypeScript interface rather than a function.
    pub is_class: bool,
    /// A TypeScript interface (`is_class` is also set).
    #[serde(default)]
    pub is_interface: bool,
    /// The defined name.
    pub name: String,
    /// Start of the definition node (for arrow-function consts, the declarator).
    pub start_byte: usize,
    /// End of the definition node.
    pub end_byte: usize,
    /// First line of the definition.
    pub start_line: u32,
    /// Last line of the definition.
    pub end_line: u32,
    /// Normalised return annotation (`-> Foo`), if any.
    pub returns: Option<String>,
    /// Decorated with a pytest fixture decorator.
    pub is_fixture: bool,
}

/// An import or re-export.
///
/// TypeScript reuses these variants: `import {A as B} from "s"` and
/// `export {A as B} from "s"` are `From` (default imports use name
/// `"default"`), `import * as ns from "s"` is `Module` with an alias,
/// `export * from "s"` is `Wildcard`, and `export {a as b}` without a source is
/// `From` with an empty `from_module`. Module strings are raw import
/// specifiers, resolved against the importing file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RawImport {
    /// `import a.b` (binds `a`) or `import a.b as x` (binds `x`).
    Module {
        /// The imported module path or specifier.
        module: String,
        /// The local name, when given with `as`.
        alias: Option<String>,
    },
    /// `from m import n [as k]`; `from_module` keeps leading dots if relative.
    From {
        /// The module imported from.
        from_module: String,
        /// The imported member.
        name: String,
        /// The local name, when given with `as`.
        alias: Option<String>,
    },
    /// `from m import *`
    Wildcard {
        /// The module whose members are all imported.
        from_module: String,
    },
}

/// An import with the line it appears on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawImportAt {
    /// The import itself.
    pub import: RawImport,
    /// Line of the import statement.
    pub line: u32,
}

/// A call site: `callee(...)` or `receiver.callee(...)` (also `new` and JSX).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCall {
    /// The called name.
    pub callee: String,
    /// Source text of the receiver for `recv.callee(...)` calls.
    pub receiver: Option<String>,
    /// Start of the call expression.
    pub byte: usize,
    /// Line of the call.
    pub line: u32,
}

/// A base class (or implemented interface) written in a class header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawBase {
    /// Start byte of the class definition that declares the base.
    pub class_byte: usize,
    /// The base as written, without type arguments, e.g. `models.Model`.
    pub base: String,
    /// Line of the base in the header.
    pub line: u32,
}

/// What a type hint describes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeTarget {
    /// A local, a parameter, or a class-body field.
    Var(String),
    /// `self.<attr>` / `this.<attr>`, or a TypeScript class field.
    SelfAttr(String),
}

/// A syntactic hint about the class of a variable or field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawType {
    /// The variable or field the hint is about.
    pub target: TypeTarget,
    /// Constructor callee or annotation text, e.g. `Foo` or `models.Foo`.
    pub type_expr: String,
    /// Position of the target, used to find its enclosing scope.
    pub byte: usize,
}

/// An expression a function returns or yields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawReturn {
    /// A position inside the owning function (its start, for TypeScript).
    pub byte: usize,
    /// The returned expression's source text.
    pub expr: String,
}

/// A name bound by a parameter, assignment, loop, `with` or `except` target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawLocal {
    /// The bound name.
    pub name: String,
    /// Position of the binding, used to find its scope.
    pub byte: usize,
}

/// Everything extracted from one file. Cached in the index by content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedFile {
    /// Source language.
    #[serde(default)]
    pub lang: Lang,
    /// Path relative to the repo root, `/`-separated.
    pub rel_path: String,
    /// BLAKE3 hash of the file's bytes.
    pub hash: String,
    /// Number of lines in the file.
    pub line_count: u32,
    /// Classes, functions and methods, sorted by position.
    pub defs: Vec<RawDef>,
    /// Imports and re-exports.
    pub imports: Vec<RawImportAt>,
    /// Call sites.
    pub calls: Vec<RawCall>,
    /// Base classes and implemented interfaces.
    pub bases: Vec<RawBase>,
    /// Type hints for variables and fields.
    pub types: Vec<RawType>,
    /// Locally bound names.
    pub locals: Vec<RawLocal>,
    /// Returned and yielded values.
    pub return_values: Vec<RawReturn>,
    /// Local name exported as `default` (TypeScript).
    #[serde(default)]
    pub default_export: Option<String>,
}
