//! Python front end: tree-sitter parse plus the query files in `queries/python`.

use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

use super::{
    ParsedFile, RawBase, RawCall, RawDef, RawImport, RawImportAt, RawLocal, RawType, TypeTarget,
};

const DEFS: &str = include_str!("../../queries/python/defs.scm");
const IMPORTS: &str = include_str!("../../queries/python/imports.scm");
const CALLS: &str = include_str!("../../queries/python/calls.scm");
const INHERITS: &str = include_str!("../../queries/python/inherits.scm");
const TYPES: &str = include_str!("../../queries/python/types.scm");
const LOCALS: &str = include_str!("../../queries/python/locals.scm");

struct Queries {
    defs: Query,
    imports: Query,
    calls: Query,
    inherits: Query,
    types: Query,
    locals: Query,
}

fn language() -> Language {
    tree_sitter_python::LANGUAGE.into()
}

fn queries() -> &'static Queries {
    static Q: OnceLock<Queries> = OnceLock::new();
    Q.get_or_init(|| {
        let lang = language();
        let q = |name: &str, src: &str| {
            Query::new(&lang, src).unwrap_or_else(|e| panic!("bad query {name}: {e}"))
        };
        Queries {
            defs: q("defs", DEFS),
            imports: q("imports", IMPORTS),
            calls: q("calls", CALLS),
            inherits: q("inherits", INHERITS),
            types: q("types", TYPES),
            locals: q("locals", LOCALS),
        }
    })
}

/// Parse one Python file into raw facts.
pub fn parse(rel_path: &str, source: &str) -> Result<ParsedFile> {
    let mut parser = Parser::new();
    parser
        .set_language(&language())
        .context("loading the Python grammar")?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("tree-sitter returned no tree for {rel_path}"))?;
    let root = tree.root_node();
    let bytes = source.as_bytes();
    let q = queries();

    let text = |n: Node| n.utf8_text(bytes).unwrap_or("").to_string();
    let line = |n: Node| n.start_position().row as u32 + 1;

    // Definitions
    let mut defs = Vec::new();
    for_each_match(&q.defs, root, bytes, |caps| {
        let def = caps.iter().find(|(n, _)| n.starts_with("def."));
        let name = caps.iter().find(|(n, _)| *n == "name");
        if let (Some((kind, node)), Some((_, name))) = (def, name) {
            defs.push(RawDef {
                is_class: *kind == "def.class",
                name: text(*name),
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                start_line: line(*node),
                end_line: node.end_position().row as u32 + 1,
            });
        }
    });
    defs.sort_by_key(|d| (d.start_byte, std::cmp::Reverse(d.end_byte)));

    // Imports
    let mut imports = Vec::new();
    for_each_match(&q.imports, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        let alias = get("import.alias").map(text);
        let at = caps.first().map(|(_, n)| line(*n)).unwrap_or(0);
        let import = if let Some(m) = get("import.module") {
            Some(RawImport::Module {
                module: text(m),
                alias,
            })
        } else if let Some(from) = get("import.from") {
            if get("import.wildcard").is_some() {
                Some(RawImport::Wildcard {
                    from_module: text(from),
                })
            } else {
                get("import.name").map(|n| RawImport::From {
                    from_module: text(from),
                    name: text(n),
                    alias,
                })
            }
        } else {
            None
        };
        if let Some(import) = import {
            imports.push(RawImportAt { import, line: at });
        }
    });

    // Calls
    let mut calls = Vec::new();
    for_each_match(&q.calls, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        if let (Some(call), Some(callee)) = (get("call"), get("call.callee")) {
            calls.push(RawCall {
                callee: text(callee),
                receiver: get("call.receiver").map(text),
                byte: call.start_byte(),
                line: line(call),
            });
        }
    });

    // Bases
    let mut bases = Vec::new();
    for_each_match(&q.inherits, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        if let (Some(class), Some(base)) = (get("class"), get("base")) {
            bases.push(RawBase {
                class_byte: class.start_byte(),
                base: text(base),
                line: line(base),
            });
        }
    });

    // Type hints
    let mut types = Vec::new();
    for_each_match(&q.types, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        let ty = get("type.ctor").or_else(|| get("type.annot"));
        let Some(ty) = ty else { return };
        let target = if let Some(v) = get("type.var") {
            Some((TypeTarget::Var(text(v)), v.start_byte()))
        } else if let (Some(s), Some(a)) = (get("type.self"), get("type.attr")) {
            (text(s) == "self").then(|| (TypeTarget::SelfAttr(text(a)), s.start_byte()))
        } else {
            None
        };
        if let Some((target, byte)) = target {
            types.push(RawType {
                target,
                type_expr: text(ty),
                byte,
            });
        }
    });

    // Local bindings
    let mut locals = Vec::new();
    for_each_match(&q.locals, root, bytes, |caps| {
        for (_, n) in caps {
            locals.push(RawLocal {
                name: text(*n),
                byte: n.start_byte(),
            });
        }
    });

    Ok(ParsedFile {
        rel_path: rel_path.to_string(),
        hash: blake3::hash(bytes).to_hex().to_string(),
        line_count: source.lines().count() as u32,
        defs,
        imports,
        calls,
        bases,
        types,
        locals,
    })
}

/// Run a query and hand each match's captures, as (capture name, node), to `f`.
fn for_each_match<'t>(
    query: &Query,
    root: Node<'t>,
    bytes: &[u8],
    mut f: impl FnMut(&[(&str, Node<'t>)]),
) {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, bytes);
    let mut caps: Vec<(&str, Node<'t>)> = Vec::new();
    while let Some(m) = matches.next() {
        caps.clear();
        for c in m.captures {
            caps.push((names[c.index as usize], c.node));
        }
        f(&caps);
    }
}

/// Python built-in names. Calls to these are counted as external, not unresolved.
pub const BUILTINS: &[&str] = &[
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
    "__import__",
    "Exception",
    "BaseException",
    "ValueError",
    "TypeError",
    "KeyError",
    "IndexError",
    "AttributeError",
    "RuntimeError",
    "NotImplementedError",
    "StopIteration",
    "OSError",
    "IOError",
    "AssertionError",
    "ImportError",
    "LookupError",
    "ArithmeticError",
    "ZeroDivisionError",
    "PermissionError",
    "FileNotFoundError",
    "TimeoutError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "DeprecationWarning",
    "UserWarning",
    "Warning",
    "RecursionError",
    "MemoryError",
    "SystemExit",
    "KeyboardInterrupt",
    "GeneratorExit",
    "NameError",
    "OverflowError",
    "EOFError",
    "ConnectionError",
    "BufferError",
    "ResourceWarning",
    "RuntimeWarning",
    "FutureWarning",
    "PendingDeprecationWarning",
    "SyntaxWarning",
    "ImportWarning",
];

/// Method names common on builtin and stdlib objects (dict, list, str, files,
/// sockets, threading, pathlib, re). On an untyped receiver these are far more
/// likely to hit a builtin than a repo method, so they get no name-only edge.
pub const BUILTIN_METHODS: &[&str] = &[
    "get",
    "set",
    "items",
    "keys",
    "values",
    "pop",
    "popitem",
    "update",
    "setdefault",
    "clear",
    "copy",
    "append",
    "extend",
    "insert",
    "remove",
    "index",
    "count",
    "sort",
    "reverse",
    "add",
    "discard",
    "union",
    "intersection",
    "difference",
    "issubset",
    "issuperset",
    "join",
    "split",
    "rsplit",
    "splitlines",
    "strip",
    "lstrip",
    "rstrip",
    "replace",
    "format",
    "lower",
    "upper",
    "title",
    "capitalize",
    "startswith",
    "endswith",
    "find",
    "rfind",
    "encode",
    "decode",
    "isdigit",
    "isalpha",
    "partition",
    "rpartition",
    "zfill",
    "ljust",
    "rjust",
    "center",
    "read",
    "readline",
    "readlines",
    "write",
    "writelines",
    "seek",
    "tell",
    "close",
    "flush",
    "open",
    "fileno",
    "truncate",
    "wait",
    "acquire",
    "release",
    "start",
    "run",
    "is_set",
    "notify",
    "notify_all",
    "put",
    "put_nowait",
    "get_nowait",
    "send",
    "sendall",
    "recv",
    "connect",
    "bind",
    "listen",
    "accept",
    "shutdown",
    "settimeout",
    "match",
    "search",
    "group",
    "groups",
    "groupdict",
    "sub",
    "findall",
    "finditer",
    "fullmatch",
    "isoformat",
    "strftime",
    "total_seconds",
    "timestamp",
    "hexdigest",
    "digest",
    "exists",
    "mkdir",
    "unlink",
    "is_file",
    "is_dir",
    "iterdir",
    "glob",
    "rglob",
    "read_text",
    "write_text",
    "read_bytes",
    "write_bytes",
    "resolve",
    "relative_to",
    "with_suffix",
    "cancel",
    "result",
    "done",
    "exception",
    "add_done_callback",
    "submit",
    "map",
    "filter",
    "next",
    "send",
    "throw",
    "debug",
    "info",
    "warning",
    "error",
    "critical",
    "exception",
    "log",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_basic_facts() {
        let src = "import os\nfrom .a import b as c\n\nclass K(Base):\n    def m(self):\n        self.n()\n\ndef f(x: K):\n    y = K()\n    return g(1)\n";
        let p = parse("pkg/mod.py", src).unwrap();
        let names: Vec<_> = p.defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["K", "m", "f"]);
        assert!(p.defs[0].is_class && !p.defs[1].is_class);
        assert_eq!(p.imports.len(), 2);
        assert!(
            matches!(&p.imports[1].import, RawImport::From { from_module, name, alias }
            if from_module == ".a" && name == "b" && alias.as_deref() == Some("c"))
        );
        let callees: Vec<_> = p.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"n") && callees.contains(&"K") && callees.contains(&"g"));
        assert_eq!(p.bases.len(), 1);
        assert_eq!(p.bases[0].base, "Base");
        assert_eq!(p.types.len(), 2, "param hint and constructor assignment");
    }
}
