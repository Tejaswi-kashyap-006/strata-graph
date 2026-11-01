//! Python front end: tree-sitter parse plus the query files in `queries/python`.

use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

use super::{
    ParsedFile, RawBase, RawCall, RawDef, RawImport, RawImportAt, RawLocal, RawReturn, RawType,
    TypeTarget,
};

const DEFS: &str = include_str!("../../queries/python/defs.scm");
const IMPORTS: &str = include_str!("../../queries/python/imports.scm");
const CALLS: &str = include_str!("../../queries/python/calls.scm");
const INHERITS: &str = include_str!("../../queries/python/inherits.scm");
const TYPES: &str = include_str!("../../queries/python/types.scm");
const LOCALS: &str = include_str!("../../queries/python/locals.scm");
const RETURNS: &str = include_str!("../../queries/python/returns.scm");
const DECORATORS: &str = include_str!("../../queries/python/decorators.scm");
const RETURN_VALUES: &str = include_str!("../../queries/python/return_values.scm");

struct Queries {
    defs: Query,
    imports: Query,
    calls: Query,
    inherits: Query,
    types: Query,
    locals: Query,
    returns: Query,
    decorators: Query,
    return_values: Query,
}

fn language() -> Language {
    tree_sitter_python::LANGUAGE.into()
}

fn queries() -> &'static Queries {
    static Q: OnceLock<Queries> = OnceLock::new();
    Q.get_or_init(|| {
        let lang = language();
        let q = |name: &str, src: &str| {
            // Queries are constants compiled into the binary and every one is
            // exercised by unit tests, so a failure here is a build bug, not bad input.
            Query::new(&lang, src).unwrap_or_else(|e| panic!("bad query {name}: {e}"))
        };
        Queries {
            defs: q("defs", DEFS),
            imports: q("imports", IMPORTS),
            calls: q("calls", CALLS),
            inherits: q("inherits", INHERITS),
            types: q("types", TYPES),
            locals: q("locals", LOCALS),
            returns: q("returns", RETURNS),
            decorators: q("decorators", DECORATORS),
            return_values: q("return_values", RETURN_VALUES),
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
                is_interface: false,
                name: text(*name),
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                start_line: line(*node),
                end_line: node.end_position().row as u32 + 1,
                returns: None,
                is_fixture: false,
            });
        }
    });
    defs.sort_by_key(|d| (d.start_byte, std::cmp::Reverse(d.end_byte)));

    // Return annotations, attached to their definitions by start byte.
    let mut returns = std::collections::HashMap::new();
    for_each_match(&q.returns, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        if let (Some(def), Some(ret)) = (get("def"), get("ret")) {
            if let Some(t) = normalize_type(&text(ret)) {
                returns.insert(def.start_byte(), t);
            }
        }
    });
    let mut fixtures = std::collections::HashSet::new();
    for_each_match(&q.decorators, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        if let (Some(dec), Some(def)) = (get("dec"), get("def")) {
            if text(dec).contains("fixture") {
                fixtures.insert(def.start_byte());
            }
        }
    });
    for d in &mut defs {
        d.returns = returns.remove(&d.start_byte);
        d.is_fixture = fixtures.contains(&d.start_byte);
    }

    // Returned and yielded values (only shapes the resolver can use).
    let mut return_values = Vec::new();
    for_each_match(&q.return_values, root, bytes, |caps| {
        if let Some((_, v)) = caps.first() {
            let t = text(*v);
            if t.len() <= 300 && !t.contains('\n') {
                return_values.push(RawReturn {
                    byte: v.start_byte(),
                    expr: t,
                });
            }
        }
    });

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
        let ty = match (get("type.ctor"), get("type.annot")) {
            (Some(c), _) => text(c),
            (None, Some(a)) => match normalize_type(&text(a)) {
                Some(t) => t,
                None => return,
            },
            _ => return,
        };
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
                type_expr: ty,
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
        lang: super::Lang::Python,
        rel_path: rel_path.to_string(),
        hash: blake3::hash(bytes).to_hex().to_string(),
        line_count: source.lines().count() as u32,
        defs,
        imports,
        calls,
        bases,
        types,
        locals,
        return_values,
        default_export: None,
    })
}

/// Reduce an annotation to one dotted class name, or None if it names several
/// types or a generic container. Handles `Optional[X]`, `X | None`, `Union[X, None]`,
/// string forward references and `Self` (returned as the literal `Self`).
pub fn normalize_type(raw: &str) -> Option<String> {
    let mut t = raw.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        t = t.get(1..t.len().saturating_sub(1))?.trim();
    }
    for wrapper in ["typing.Optional[", "Optional[", "t.Optional["] {
        if let Some(inner) = t.strip_prefix(wrapper).and_then(|r| r.strip_suffix(']')) {
            return normalize_type(inner);
        }
    }
    for wrapper in ["typing.Union[", "Union[", "t.Union["] {
        if let Some(inner) = t.strip_prefix(wrapper).and_then(|r| r.strip_suffix(']')) {
            return single_non_none(inner.split(','));
        }
    }
    if t.contains('|') {
        return single_non_none(t.split('|'));
    }
    let ok = !t.is_empty()
        && t.split('.').all(|p| {
            let mut c = p.chars();
            matches!(c.next(), Some(ch) if ch == '_' || ch.is_alphabetic())
                && c.all(|ch| ch == '_' || ch.is_alphanumeric())
        });
    (ok && t != "None").then(|| t.to_string())
}

fn single_non_none<'a>(parts: impl Iterator<Item = &'a str>) -> Option<String> {
    let rest: Vec<&str> = parts
        .map(str::trim)
        .filter(|p| *p != "None" && !p.is_empty())
        .collect();
    match rest.as_slice() {
        [one] => normalize_type(one),
        _ => None,
    }
}

/// Run a query and hand each match's captures, as (capture name, node), to `f`.
pub(crate) fn for_each_match<'t>(
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
    fn normalizes_annotations() {
        assert_eq!(normalize_type("Foo").as_deref(), Some("Foo"));
        assert_eq!(normalize_type("models.Foo").as_deref(), Some("models.Foo"));
        assert_eq!(normalize_type("Optional[Foo]").as_deref(), Some("Foo"));
        assert_eq!(normalize_type("Foo | None").as_deref(), Some("Foo"));
        assert_eq!(
            normalize_type("Union[None, a.Foo]").as_deref(),
            Some("a.Foo")
        );
        assert_eq!(normalize_type("\"Foo\"").as_deref(), Some("Foo"));
        assert_eq!(normalize_type("Self").as_deref(), Some("Self"));
        assert_eq!(normalize_type("list[Foo]"), None);
        assert_eq!(normalize_type("Foo | Bar"), None);
        assert_eq!(normalize_type("None"), None);
    }

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
