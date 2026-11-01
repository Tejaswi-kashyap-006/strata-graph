//! Language front ends. Each one turns a source file into raw, unresolved
//! facts (definitions, imports, calls, bases, type hints). Resolution into
//! graph edges is language-aware but lives in `resolve`.

pub mod python;

use serde::{Deserialize, Serialize};

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
}
