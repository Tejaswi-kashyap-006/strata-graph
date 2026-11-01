//! TypeScript / JavaScript front end. `.ts`, `.mts`, `.cts` use the TypeScript
//! grammar; `.tsx`, `.jsx`, `.js`, `.mjs`, `.cjs` use TSX, which also accepts
//! plain JavaScript and JSX.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use tree_sitter::{Language, Node, Parser, Query};

use super::python::for_each_match;
use super::{
    Lang, ParsedFile, RawBase, RawCall, RawDef, RawImport, RawImportAt, RawLocal, RawReturn,
    RawType, TypeTarget,
};

const DEFS: &str = include_str!("../../queries/typescript/defs.scm");
const RETURNS: &str = include_str!("../../queries/typescript/returns.scm");
const IMPORTS: &str = include_str!("../../queries/typescript/imports.scm");
const CALLS: &str = include_str!("../../queries/typescript/calls.scm");
const CALLS_JSX: &str = include_str!("../../queries/typescript/calls_jsx.scm");
const INHERITS: &str = include_str!("../../queries/typescript/inherits.scm");
const TYPES: &str = include_str!("../../queries/typescript/types.scm");
const LOCALS: &str = include_str!("../../queries/typescript/locals.scm");
const RETURN_VALUES: &str = include_str!("../../queries/typescript/return_values.scm");

/// Node kinds that start a new function body.
const FN_KINDS: &[&str] = &[
    "arrow_function",
    "function_expression",
    "function_declaration",
    "generator_function_declaration",
    "generator_function",
    "method_definition",
];

struct Queries {
    defs: Query,
    returns: Query,
    imports: Query,
    calls: Query,
    inherits: Query,
    types: Query,
    locals: Query,
    return_values: Query,
}

fn grammar(tsx: bool) -> Language {
    if tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
}

fn build_queries(tsx: bool) -> Queries {
    let lang = grammar(tsx);
    let q = |name: &str, src: &str| {
        Query::new(&lang, src).unwrap_or_else(|e| panic!("bad typescript query {name}: {e}"))
    };
    let calls = if tsx {
        format!("{CALLS}\n{CALLS_JSX}")
    } else {
        CALLS.to_string()
    };
    Queries {
        defs: q("defs", DEFS),
        returns: q("returns", RETURNS),
        imports: q("imports", IMPORTS),
        calls: q("calls", &calls),
        inherits: q("inherits", INHERITS),
        types: q("types", TYPES),
        locals: q("locals", LOCALS),
        return_values: q("return_values", RETURN_VALUES),
    }
}

fn queries(tsx: bool) -> &'static Queries {
    static TS: OnceLock<Queries> = OnceLock::new();
    static TSX: OnceLock<Queries> = OnceLock::new();
    if tsx {
        TSX.get_or_init(|| build_queries(true))
    } else {
        TS.get_or_init(|| build_queries(false))
    }
}

fn uses_tsx(rel_path: &str) -> bool {
    let ext = rel_path.rsplit('.').next().unwrap_or("");
    matches!(ext, "tsx" | "jsx" | "js" | "mjs" | "cjs")
}

/// Parse one TypeScript or JavaScript file into raw facts.
pub fn parse(rel_path: &str, source: &str) -> Result<ParsedFile> {
    let tsx = uses_tsx(rel_path);
    let mut parser = Parser::new();
    parser
        .set_language(&grammar(tsx))
        .context("loading the TypeScript grammar")?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("tree-sitter returned no tree for {rel_path}"))?;
    let root = tree.root_node();
    let bytes = source.as_bytes();
    let q = queries(tsx);

    let text = |n: Node| n.utf8_text(bytes).unwrap_or("").to_string();
    let line = |n: Node| n.start_position().row as u32 + 1;

    // Definitions, and a map from each function body node to its definition.
    let mut defs = Vec::new();
    let mut fn_to_def: HashMap<usize, usize> = HashMap::new();
    for_each_match(&q.defs, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        let def = caps.iter().find(|(n, _)| n.starts_with("def."));
        if let (Some((kind, node)), Some(name)) = (def, get("name")) {
            let start = node.start_byte();
            fn_to_def.insert(get("fn").map_or(start, |f| f.start_byte()), start);
            defs.push(RawDef {
                is_class: *kind == "def.class",
                name: text(name),
                start_byte: start,
                end_byte: node.end_byte(),
                start_line: line(*node),
                end_line: node.end_position().row as u32 + 1,
                returns: None,
                is_fixture: false,
            });
        }
    });
    defs.sort_by_key(|d| (d.start_byte, std::cmp::Reverse(d.end_byte)));
    defs.dedup_by_key(|d| d.start_byte);

    let mut returns = HashMap::new();
    for_each_match(&q.returns, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        if let (Some(def), Some(ret)) = (get("def"), get("ret")) {
            if let Some(t) = normalize_ts_type(&text(ret)) {
                returns.insert(def.start_byte(), t);
            }
        }
    });
    for d in &mut defs {
        d.returns = returns.remove(&d.start_byte);
    }

    // Imports, re-exports and the default export.
    let mut imports = Vec::new();
    let mut default_export = None;
    for_each_match(&q.imports, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        let at = caps.first().map(|(_, n)| line(*n)).unwrap_or(0);
        let source_spec = get("import.source").map(text);
        let import = if let Some(local) = get("export.local") {
            // Only aliases without a source; sourced specifiers match the
            // re-export pattern above.
            let stmt = local
                .parent()
                .and_then(|spec| spec.parent())
                .and_then(|c| c.parent());
            let alias = local
                .parent()
                .and_then(|spec| spec.child_by_field_name("alias"));
            match (stmt.and_then(|st| st.child_by_field_name("source")), alias) {
                (None, Some(alias)) => Some(RawImport::From {
                    from_module: String::new(),
                    name: text(local),
                    alias: Some(text(alias)),
                }),
                _ => None,
            }
        } else if let Some(d) = get("export.default") {
            default_export = Some(text(d));
            None
        } else if let Some(star) = get("import.star") {
            Some(RawImport::Wildcard {
                from_module: text(star),
            })
        } else if let (Some(ns), Some(src)) = (get("import.namespace"), source_spec.clone()) {
            Some(RawImport::Module {
                module: src,
                alias: Some(text(ns)),
            })
        } else if let (Some(local), Some(src)) = (get("import.default"), source_spec.clone()) {
            Some(RawImport::From {
                from_module: src,
                name: "default".into(),
                alias: Some(text(local)),
            })
        } else if let (Some(name), Some(src)) = (get("import.name"), source_spec) {
            let alias = name
                .parent()
                .and_then(|spec| spec.child_by_field_name("alias"))
                .map(text);
            Some(RawImport::From {
                from_module: src,
                name: text(name),
                alias,
            })
        } else {
            None
        };
        if let Some(import) = import {
            imports.push(RawImportAt { import, line: at });
        }
    });

    // Calls, `new` expressions and JSX components.
    let mut calls = Vec::new();
    for_each_match(&q.calls, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        let (Some(call), Some(callee)) = (get("call"), get("call.callee")) else {
            return;
        };
        let receiver = get("call.receiver").map(text);
        let callee = text(callee);
        let intrinsic_tag = call.kind().starts_with("jsx")
            && receiver.is_none()
            && callee.starts_with(|c: char| c.is_ascii_lowercase());
        if !intrinsic_tag {
            calls.push(RawCall {
                callee,
                receiver,
                byte: call.start_byte(),
                line: line(call),
            });
        }
    });

    let mut bases = Vec::new();
    for_each_match(&q.inherits, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        if let (Some(class), Some(base)) = (get("class"), get("base")) {
            let b = text(base);
            let b = b.split('<').next().unwrap_or("").trim().to_string();
            if !b.is_empty() {
                bases.push(RawBase {
                    class_byte: class.start_byte(),
                    base: b,
                    line: line(base),
                });
            }
        }
    });

    let mut types = Vec::new();
    for_each_match(&q.types, root, bytes, |caps| {
        let get = |want: &str| caps.iter().find(|(n, _)| *n == want).map(|(_, nd)| *nd);
        let ty = match (get("type.ctor"), get("type.annot")) {
            (Some(c), _) => text(c),
            (None, Some(a)) => match normalize_ts_type(&text(a)) {
                Some(t) => t,
                None => return,
            },
            _ => return,
        };
        let target = if let Some(v) = get("type.var") {
            (TypeTarget::Var(text(v)), v.start_byte())
        } else if let Some(f) = get("type.field") {
            (TypeTarget::SelfAttr(text(f)), f.start_byte())
        } else {
            return;
        };
        types.push(RawType {
            target: target.0,
            type_expr: ty,
            byte: target.1,
        });
    });

    let mut locals = Vec::new();
    for_each_match(&q.locals, root, bytes, |caps| {
        for (_, n) in caps {
            locals.push(RawLocal {
                name: text(*n),
                byte: n.start_byte(),
            });
        }
    });

    // Returned values, attributed to the named function that owns them. A
    // return inside an anonymous callback belongs to the callback, not to the
    // enclosing function, so it is dropped.
    let def_starts: HashSet<usize> = defs.iter().map(|d| d.start_byte).collect();
    let mut return_values = Vec::new();
    for_each_match(&q.return_values, root, bytes, |caps| {
        let Some((_, value)) = caps.first() else {
            return;
        };
        if value.kind() == "statement_block" {
            return;
        }
        let mut owner = value.parent();
        while let Some(p) = owner {
            if FN_KINDS.contains(&p.kind()) {
                break;
            }
            owner = p.parent();
        }
        let Some(owner) = owner else { return };
        let Some(&def_start) = fn_to_def.get(&owner.start_byte()) else {
            return;
        };
        if !def_starts.contains(&def_start) {
            return;
        }
        let t = text(*value);
        let t = t.strip_prefix("await ").unwrap_or(&t).trim().to_string();
        if t.len() <= 300 && !t.contains('\n') {
            return_values.push(RawReturn {
                byte: def_start,
                expr: t,
            });
        }
    });

    Ok(ParsedFile {
        lang: Lang::TypeScript,
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
        default_export,
    })
}

/// Split `s` on `sep` at the top level only (not inside `<>`, `()`, `[]`, `{}`).
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            _ if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Reduce a TypeScript type annotation to one class or interface name.
/// `Foo | null`, `Promise<Foo>`, `Repo<User>` (generic instance) and
/// `ns.Foo` reduce; arrays, primitives, unions of several types do not.
pub fn normalize_ts_type(raw: &str) -> Option<String> {
    let t = raw.trim().trim_start_matches(':').trim();
    let parts: Vec<&str> = split_top(t, '|')
        .into_iter()
        .map(str::trim)
        .filter(|p| !matches!(*p, "null" | "undefined" | "void" | ""))
        .collect();
    let [one] = parts.as_slice() else { return None };
    let one = one.trim_start_matches('(').trim_end_matches(')').trim();
    if one.ends_with("[]") {
        return None;
    }
    if let Some(inner) = one
        .strip_prefix("Promise<")
        .and_then(|r| r.strip_suffix('>'))
    {
        return normalize_ts_type(inner);
    }
    let base = one.split('<').next().unwrap_or("").trim();
    const PRIMITIVES: &[&str] = &[
        "string", "number", "boolean", "any", "unknown", "never", "object", "bigint", "symbol",
        "this",
    ];
    let ok = !base.is_empty()
        && !PRIMITIVES.contains(&base)
        && base.split('.').all(|p| {
            let mut c = p.chars();
            matches!(c.next(), Some(ch) if ch == '_' || ch == '$' || ch.is_alphabetic())
                && c.all(|ch| ch == '_' || ch == '$' || ch.is_alphanumeric())
        });
    ok.then(|| base.to_string())
}

/// Global names in browsers, Node and common test runners: calls to these are external.
pub const BUILTINS: &[&str] = &[
    "console",
    "Math",
    "JSON",
    "Object",
    "Array",
    "Promise",
    "String",
    "Number",
    "Boolean",
    "Date",
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "RegExp",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Symbol",
    "BigInt",
    "Reflect",
    "Proxy",
    "Intl",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "setTimeout",
    "clearTimeout",
    "setInterval",
    "clearInterval",
    "setImmediate",
    "queueMicrotask",
    "structuredClone",
    "fetch",
    "require",
    "process",
    "Buffer",
    "window",
    "document",
    "globalThis",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
    "URL",
    "URLSearchParams",
    "Response",
    "Request",
    "Headers",
    "FormData",
    "Blob",
    "File",
    "AbortController",
    "TextEncoder",
    "TextDecoder",
    "Uint8Array",
    "ArrayBuffer",
    "DataView",
    "Function",
    "describe",
    "it",
    "test",
    "expect",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
    "jest",
    "vi",
    "alert",
    "atob",
    "btoa",
    "Event",
    "CustomEvent",
    "EventTarget",
    "Worker",
    "crypto",
    "performance",
    "navigator",
    "location",
    "history",
    "localStorage",
    "sessionStorage",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "import",
    "eval",
    "Iterator",
];

/// Method names common on builtin JS objects (arrays, strings, maps, promises,
/// events, DOM). On an untyped receiver these get no name-only edge.
pub const BUILTIN_METHODS: &[&str] = &[
    "push",
    "pop",
    "shift",
    "unshift",
    "map",
    "filter",
    "reduce",
    "reduceRight",
    "forEach",
    "find",
    "findIndex",
    "findLast",
    "some",
    "every",
    "includes",
    "indexOf",
    "lastIndexOf",
    "slice",
    "splice",
    "concat",
    "join",
    "split",
    "trim",
    "trimStart",
    "trimEnd",
    "toString",
    "toLowerCase",
    "toUpperCase",
    "replace",
    "replaceAll",
    "startsWith",
    "endsWith",
    "then",
    "catch",
    "finally",
    "get",
    "set",
    "has",
    "delete",
    "keys",
    "values",
    "entries",
    "add",
    "clear",
    "sort",
    "reverse",
    "flat",
    "flatMap",
    "at",
    "charAt",
    "charCodeAt",
    "substring",
    "substr",
    "padStart",
    "padEnd",
    "match",
    "matchAll",
    "test",
    "exec",
    "toFixed",
    "json",
    "text",
    "emit",
    "on",
    "off",
    "once",
    "bind",
    "call",
    "apply",
    "log",
    "error",
    "warn",
    "info",
    "debug",
    "assign",
    "freeze",
    "stringify",
    "parse",
    "resolve",
    "reject",
    "all",
    "allSettled",
    "race",
    "fill",
    "from",
    "of",
    "isArray",
    "addEventListener",
    "removeEventListener",
    "dispatchEvent",
    "querySelector",
    "querySelectorAll",
    "getElementById",
    "appendChild",
    "removeChild",
    "setAttribute",
    "getAttribute",
    "focus",
    "blur",
    "click",
    "preventDefault",
    "stopPropagation",
    "next",
    "return",
    "throw",
    "send",
    "write",
    "end",
    "close",
    "open",
    "read",
    "abort",
    "toISOString",
    "getTime",
    "valueOf",
    "localeCompare",
    "toJSON",
    "hasOwnProperty",
    "length",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ts_types() {
        assert_eq!(normalize_ts_type(": Foo").as_deref(), Some("Foo"));
        assert_eq!(normalize_ts_type(": Foo | null").as_deref(), Some("Foo"));
        assert_eq!(
            normalize_ts_type(": Promise<Res | undefined>").as_deref(),
            Some("Res")
        );
        assert_eq!(normalize_ts_type(": Repo<User>").as_deref(), Some("Repo"));
        assert_eq!(normalize_ts_type(": ns.Foo").as_deref(), Some("ns.Foo"));
        assert_eq!(normalize_ts_type(": Foo[]"), None);
        assert_eq!(normalize_ts_type(": string"), None);
        assert_eq!(normalize_ts_type(": A | B"), None);
        assert_eq!(
            normalize_ts_type(": Map<string, A | B>").as_deref(),
            Some("Map")
        );
    }

    #[test]
    fn extracts_ts_facts() {
        let src = r#"
import Def, { A as B, C } from "./x";
import * as ns from "../y";
export { D, E as F } from "./z";
export * from "./w";
export default class Foo extends Base implements I {
  private svc: Service;
  handle = (e: Ev): Out => { this.run(); };
  constructor(private repo: Repo) { super(); this.cache = new Cache(); }
  async run(x: number): Promise<Res | null> { return super.go(x); }
}
export const make = (a: string): Foo => new ns.Foo();
export function bar() {
  const v = make();
  v.z();
  [1].map(() => { return new Other(); });
}
interface I extends J { m(a: string): R; }
const obj = { notADef() { return 1; } };
"#;
        let p = parse("src/a.ts", src).unwrap();
        let names: Vec<_> = p.defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Foo",
                "handle",
                "constructor",
                "run",
                "make",
                "bar",
                "I",
                "m"
            ]
        );
        assert_eq!(p.default_export.as_deref(), Some("Foo"));
        let run = p.defs.iter().find(|d| d.name == "run").unwrap();
        assert_eq!(run.returns.as_deref(), Some("Res"));
        assert_eq!(
            p.defs
                .iter()
                .find(|d| d.name == "make")
                .unwrap()
                .returns
                .as_deref(),
            Some("Foo")
        );

        let imports: Vec<String> = p
            .imports
            .iter()
            .map(|i| format!("{:?}", i.import))
            .collect();
        assert!(
            imports
                .iter()
                .any(|s| s.contains("\"default\"") && s.contains("Some(\"Def\")")),
            "{imports:?}"
        );
        assert!(imports
            .iter()
            .any(|s| s.contains("name: \"A\"") && s.contains("Some(\"B\")")));
        assert!(imports
            .iter()
            .any(|s| s.contains("name: \"C\"") && s.contains("alias: None")));
        assert!(imports
            .iter()
            .any(|s| s.contains("Module") && s.contains("Some(\"ns\")")));
        assert!(imports
            .iter()
            .any(|s| s.contains("name: \"E\"") && s.contains("Some(\"F\")")));
        assert!(imports
            .iter()
            .any(|s| s.contains("Wildcard") && s.contains("./w")));

        let bases: Vec<_> = p.bases.iter().map(|b| b.base.as_str()).collect();
        assert_eq!(bases, ["Base", "I", "J"]);
        let calls: Vec<_> = p
            .calls
            .iter()
            .map(|c| format!("{}.{}", c.receiver.as_deref().unwrap_or(""), c.callee))
            .collect();
        for want in [
            "this.run", ".Cache", "super.go", "ns.Foo", ".make", "v.z", ".Other",
        ] {
            assert!(calls.contains(&want.to_string()), "{want} not in {calls:?}");
        }
        let fields: Vec<_> = p
            .types
            .iter()
            .filter_map(|t| match &t.target {
                TypeTarget::SelfAttr(f) => Some(format!("{f}:{}", t.type_expr)),
                _ => None,
            })
            .collect();
        for want in ["svc:Service", "repo:Repo", "cache:Cache"] {
            assert!(
                fields.contains(&want.to_string()),
                "{want} not in {fields:?}"
            );
        }
        // `new Other()` is returned by an anonymous callback, not by `bar`.
        let rets: Vec<_> = p.return_values.iter().map(|r| r.expr.as_str()).collect();
        assert!(rets.contains(&"new ns.Foo()"), "{rets:?}");
        assert!(!rets.contains(&"new Other()"), "{rets:?}");
    }

    #[test]
    fn jsx_components_are_calls_and_tags_are_not() {
        let src = "export function App() { return <Layout><div><ui.Button /></div></Layout>; }";
        let p = parse("src/App.tsx", src).unwrap();
        let calls: Vec<_> = p
            .calls
            .iter()
            .map(|c| format!("{}.{}", c.receiver.as_deref().unwrap_or(""), c.callee))
            .collect();
        assert!(calls.contains(&".Layout".to_string()), "{calls:?}");
        assert!(calls.contains(&"ui.Button".to_string()), "{calls:?}");
        assert!(!calls.iter().any(|c| c.ends_with(".div")), "{calls:?}");
    }
}
