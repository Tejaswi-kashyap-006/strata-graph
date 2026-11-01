//! Turn raw per-file facts into a resolved graph.
//!
//! Resolution is tiered. T1 edges come from Python's own lookup rules as far
//! as syntax can follow them: enclosing scopes, module members, imports and
//! re-exports, `self`/`cls`/`super()` through resolved bases, and simple type
//! hints (constructor assignments, annotations, typed parameters). When none
//! of that applies, a unique repo-wide name match gives a T0 edge and two or
//! three matches give T0-ambiguous edges. Anything wider is left unresolved
//! rather than guessed.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::graph::{FileInfo, Graph};
use serde::{Deserialize, Serialize};

use crate::lang::{python, typescript, Lang, ParsedFile, RawImport, TypeTarget};
use crate::model::{Edge, EdgeKind, Node, NodeId, NodeKind, ResolutionStats, Tier};

/// Most candidates a name-only match may have before it is left unresolved.
const MAX_AMBIGUOUS: usize = 3;
const MAX_DEPTH: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Res {
    Found(NodeId, Tier),
    /// Bound to something outside the repo (stdlib, third-party, builtin).
    External,
    /// A parameter or local variable: a value, not a definition.
    Local,
    NotFound,
}

/// (file index, scope the hint appears in, type expression text)
type TypeHint = (usize, NodeId, String);

struct FileCtx {
    module: NodeId,
    lang: Lang,
    /// (start_byte, end_byte, node) for every definition, sorted by start.
    spans: Vec<(usize, usize, NodeId)>,
}

impl FileCtx {
    /// Innermost definition containing `byte`, if any.
    fn innermost(&self, byte: usize) -> Option<NodeId> {
        let upto = self.spans.partition_point(|s| s.0 <= byte);
        self.spans[..upto]
            .iter()
            .rev()
            .find(|s| byte < s.1)
            .map(|s| s.2)
    }
}

struct Resolver {
    nodes: Vec<Node>,
    files: Vec<FileCtx>,
    children: HashMap<NodeId, HashMap<String, NodeId>>,
    module_by_name: HashMap<String, NodeId>,
    /// Module → local name → dotted import path.
    bindings: HashMap<NodeId, HashMap<String, String>>,
    /// Module → absolute modules it star-imports.
    stars: HashMap<NodeId, Vec<String>>,
    bases: HashMap<NodeId, Vec<NodeId>>,
    /// (scope, var) → type hints for locals, params, module vars.
    local_types: HashMap<(NodeId, String), Vec<TypeHint>>,
    /// (class, field) → type hints for self.x and class-body fields.
    field_types: HashMap<(NodeId, String), Vec<TypeHint>>,
    /// Simple name → top-level-ish functions and classes, for T0 fallback.
    callables_by_name: HashMap<(Lang, String), Vec<NodeId>>,
    methods_by_name: HashMap<(Lang, String), Vec<NodeId>>,
    /// Scope (function, class or module) → names bound locally in it.
    locals: HashMap<NodeId, HashSet<String>>,
    /// Function or method → its return annotation, resolved in the enclosing scope.
    returns: HashMap<NodeId, TypeHint>,
    /// Function or method → expressions it returns or yields (for inference).
    ret_exprs: HashMap<NodeId, Vec<TypeHint>>,
    /// pytest fixture name → fixture functions with that name.
    fixtures: HashMap<String, Vec<NodeId>>,
    is_fixture: HashSet<NodeId>,
    /// Classes with at least one base defined outside the repo.
    external_based: HashSet<NodeId>,
    /// Inferred return class per function (None = unknown or in progress).
    infer_cache: RefCell<HashMap<NodeId, Option<NodeId>>>,
    /// Recursion depth of expression inference, to stop cyclic hints.
    infer_depth: Cell<u32>,
    /// Memoised variable and field classes, keyed by (scope or class, name).
    /// An entry is inserted as None before computing, which also breaks cycles.
    var_cache: RefCell<HashMap<(NodeId, String), Option<NodeId>>>,
    field_cache: RefCell<HashMap<(NodeId, String), Option<NodeId>>>,
}

/// Deepest chain of expression inference before giving up.
const MAX_INFER_DEPTH: u32 = 12;

/// Dotted module name for a repo-relative path, following `__init__.py` packages.
fn module_name(rel: &str, package_dirs: &HashSet<String>) -> (String, bool) {
    let rel = rel.strip_suffix(".py").unwrap_or(rel);
    let mut parts: Vec<&str> = rel.split('/').collect();
    let stem = parts.pop().unwrap_or("");
    let is_init = stem == "__init__";
    // Top down, a directory is a package if it has an __init__.py, or if its
    // parent is a package (a PEP 420 namespace subpackage such as flask/sansio).
    let mut is_pkg = vec![false; parts.len()];
    for i in 0..parts.len() {
        let dir = parts[..=i].join("/");
        is_pkg[i] = package_dirs.contains(&dir) || (i > 0 && is_pkg[i - 1]);
    }
    // Keep the trailing run of package directories.
    let keep = is_pkg.iter().rev().take_while(|p| **p).count();
    let mut out: Vec<&str> = parts[parts.len() - keep..].to_vec();
    if !is_init {
        out.push(stem);
    }
    let name = if out.is_empty() {
        stem.to_string()
    } else {
        out.join(".")
    };
    (name, is_init)
}

/// Absolute module for a possibly relative `from` target.
fn absolute_from(from: &str, module: &str, is_init: bool) -> Option<String> {
    let dots = from.chars().take_while(|c| *c == '.').count();
    if dots == 0 {
        return Some(from.to_string());
    }
    let mut pkg: Vec<&str> = module.split('.').collect();
    if !is_init {
        pkg.pop();
    }
    for _ in 1..dots {
        pkg.pop()?;
    }
    let rest = &from[dots..];
    let mut s = pkg.join(".");
    if !rest.is_empty() {
        if !s.is_empty() {
            s.push('.');
        }
        s.push_str(rest);
    }
    Some(s)
}

fn is_dotted_ident(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|p| {
            let mut c = p.chars();
            matches!(c.next(), Some(ch) if ch == '_' || ch.is_alphabetic())
                && c.all(|ch| ch == '_' || ch.is_alphanumeric())
        })
}

/// For `a.b.C(args)`, return `a.b.C` when the whole text is exactly one call.
fn single_call_callee(s: &str) -> Option<&str> {
    let open = s.find('(')?;
    let head = &s[..open];
    if !s.ends_with(')') || !is_dotted_ident(head) {
        return None;
    }
    // The paren opened at `open` must be the one closed by the last char.
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    for (i, ch) in s[open..].char_indices() {
        match quote {
            Some(q) => {
                if ch == q && prev != '\\' {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (open + i == s.len() - 1).then_some(head);
                    }
                }
                _ => {}
            },
        }
        prev = ch;
    }
    None
}

/// Project-level settings that change how imports resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// TypeScript `baseUrl`, relative to the repo root ("" is the root).
    pub base_url: Option<String>,
    /// TypeScript `paths`: pattern -> targets, targets relative to the repo root.
    pub paths: Vec<(String, Vec<String>)>,
    /// JS/TS packages defined inside the repo (monorepo workspaces), so that
    /// `import "pkg/sub"` resolves to the package's own source files.
    pub packages: Vec<WorkspacePackage>,
}

/// A package.json found in the repo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePackage {
    pub name: String,
    /// Package directory, relative to the repo root.
    pub dir: String,
    /// `exports` subpath (".", "./sub", "./*") -> every target path it lists,
    /// relative to the repo root, in file order.
    pub exports: Vec<(String, Vec<String>)>,
    /// `source`, `module`, `main` and `types` fields, relative to the repo root.
    pub entries: Vec<String>,
}

/// Module name for a TypeScript/JavaScript file: its path without extension.
fn ts_module_name(rel: &str) -> String {
    for ext in [".d.ts", ".d.mts", ".d.cts"] {
        if let Some(stem) = rel.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    match rel.rsplit_once('.') {
        Some((stem, _)) if !stem.ends_with('/') => stem.to_string(),
        _ => rel.to_string(),
    }
}

/// Collapse `.` and `..` segments; None if the path climbs above the root.
fn normalize_rel(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

fn join_rel(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// Resolve an import specifier in `from_rel` to a repo module name: relative
/// paths, `index` files, `.js` specifiers that point at `.ts` sources, and
/// tsconfig `paths` / `baseUrl`. None means a package outside the repo.
fn ts_resolve(
    modules: &HashMap<String, NodeId>,
    config: &ProjectConfig,
    from_rel: &str,
    spec: &str,
) -> Option<String> {
    let dir = from_rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut bases = Vec::new();
    if spec.starts_with('.') {
        bases.push(join_rel(dir, spec));
    } else {
        for (pattern, targets) in &config.paths {
            if let Some(prefix) = pattern.strip_suffix('*') {
                if let Some(rest) = spec.strip_prefix(prefix) {
                    bases.extend(targets.iter().map(|t| t.replacen('*', rest, 1)));
                }
            } else if pattern == spec {
                bases.extend(targets.iter().cloned());
            }
        }
        for pkg in &config.packages {
            let sub = if spec == pkg.name {
                ".".to_string()
            } else if let Some(rest) = spec.strip_prefix(&format!("{}/", pkg.name)) {
                format!("./{rest}")
            } else {
                continue;
            };
            for (key, targets) in &pkg.exports {
                if *key == sub {
                    bases.extend(targets.iter().cloned());
                } else if let Some((pre, post)) = key.split_once('*') {
                    if let Some(mid) = sub.strip_prefix(pre).and_then(|r| r.strip_suffix(post)) {
                        bases.extend(targets.iter().map(|t| t.replacen('*', mid, 1)));
                    }
                }
            }
            if sub == "." {
                bases.extend(pkg.entries.iter().cloned());
                bases.push(join_rel(&pkg.dir, "src/index"));
                bases.push(join_rel(&pkg.dir, "index"));
            } else {
                let rest = &sub[2..];
                bases.push(join_rel(&pkg.dir, &format!("src/{rest}")));
                bases.push(join_rel(&pkg.dir, rest));
            }
        }
        if let Some(b) = &config.base_url {
            bases.push(join_rel(b, spec));
        }
    }
    for b in bases {
        let Some(norm) = normalize_rel(&b) else {
            continue;
        };
        let mut stem = norm.as_str();
        for ext in [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"] {
            if let Some(s) = stem.strip_suffix(ext) {
                stem = s;
                break;
            }
        }
        for cand in [stem.to_string(), join_rel(stem, "index")] {
            if modules.contains_key(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

impl Resolver {
    fn kind(&self, id: NodeId) -> NodeKind {
        self.nodes[id as usize].kind
    }

    fn resolve_path(&self, path: &str, depth: u32) -> Option<NodeId> {
        if depth > MAX_DEPTH {
            return None;
        }
        // TypeScript bindings: "<module path>::<member>.<member>"
        if let Some((m, rest)) = path.split_once("::") {
            let mut cur = *self.module_by_name.get(m)?;
            if !rest.is_empty() {
                for part in rest.split('.') {
                    cur = self.member(cur, part, depth + 1)?;
                }
            }
            return Some(cur);
        }
        let parts: Vec<&str> = path.split('.').collect();
        for i in (1..=parts.len()).rev() {
            if let Some(&m) = self.module_by_name.get(&parts[..i].join(".")) {
                let mut cur = m;
                for part in &parts[i..] {
                    cur = self.member(cur, part, depth + 1)?;
                }
                return Some(cur);
            }
        }
        None
    }

    fn member(&self, container: NodeId, name: &str, depth: u32) -> Option<NodeId> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.kind(container) {
            NodeKind::Class => self.mro_lookup(container, name),
            NodeKind::Module => {
                if let Some(&c) = self.children.get(&container).and_then(|m| m.get(name)) {
                    return Some(c);
                }
                let sub = format!("{}.{}", self.nodes[container as usize].qname, name);
                if let Some(&m) = self.module_by_name.get(&sub) {
                    return Some(m);
                }
                if let Some(path) = self.bindings.get(&container).and_then(|b| b.get(name)) {
                    if let Some(found) = self.resolve_path(path, depth + 1) {
                        return Some(found);
                    }
                }
                for star in self.stars.get(&container).into_iter().flatten() {
                    if let Some(m) = self.module_by_name.get(star) {
                        if let Some(found) = self.member(*m, name, depth + 1) {
                            return Some(found);
                        }
                    }
                }
                None
            }
            _ => self
                .children
                .get(&container)
                .and_then(|m| m.get(name))
                .copied(),
        }
    }

    /// Look `name` up on a class and then its resolved bases, breadth first.
    fn mro_lookup(&self, class: NodeId, name: &str) -> Option<NodeId> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([class]);
        while let Some(c) = queue.pop_front() {
            if !seen.insert(c) || seen.len() > 64 {
                continue;
            }
            if let Some(&m) = self.children.get(&c).and_then(|m| m.get(name)) {
                return Some(m);
            }
            queue.extend(self.bases.get(&c).into_iter().flatten().copied());
        }
        None
    }

    /// Resolve a bare name from inside `scope` (None = module level).
    fn resolve_name(&self, file: usize, scope: Option<NodeId>, name: &str) -> Res {
        let module = self.files[file].module;
        // Names defined in enclosing function scopes (class bodies are skipped,
        // as in Python, except when the call sits directly in the class body).
        let mut s = scope;
        while let Some(id) = s {
            let k = self.kind(id);
            let visible = matches!(k, NodeKind::Function | NodeKind::Method)
                || (k == NodeKind::Class && Some(id) == scope);
            if visible {
                if let Some(&c) = self.children.get(&id).and_then(|m| m.get(name)) {
                    return Res::Found(c, Tier::Scoped);
                }
                if self.locals.get(&id).is_some_and(|l| l.contains(name)) {
                    return Res::Local;
                }
            }
            s = self.nodes[id as usize].parent;
        }
        if let Some(found) = self.member(module, name, 0) {
            return Res::Found(found, Tier::Scoped);
        }
        if self.locals.get(&module).is_some_and(|l| l.contains(name)) {
            return Res::Local;
        }
        if self
            .bindings
            .get(&module)
            .is_some_and(|b| b.contains_key(name))
        {
            return Res::External;
        }
        let builtins = match self.files[file].lang {
            Lang::Python => python::BUILTINS,
            Lang::TypeScript => typescript::BUILTINS,
        };
        if builtins.contains(&name) {
            return Res::External;
        }
        Res::NotFound
    }

    /// Resolve a dotted expression such as `models.Invoice` or `pkg.mod.f`.
    fn resolve_dotted(&self, file: usize, scope: Option<NodeId>, expr: &str) -> Res {
        let mut parts = expr.split('.');
        let first = parts.next().unwrap_or("");
        match self.resolve_name(file, scope, first) {
            Res::Found(mut cur, tier) => {
                for p in parts {
                    match self.member(cur, p, 0) {
                        Some(n) => cur = n,
                        None => {
                            // Walking into something we can't see, e.g. an
                            // attribute set at runtime. Not an edge.
                            return Res::NotFound;
                        }
                    }
                }
                Res::Found(cur, tier)
            }
            other => other,
        }
    }

    fn enclosing_class(&self, scope: Option<NodeId>) -> Option<NodeId> {
        let mut s = scope;
        while let Some(id) = s {
            if self.kind(id) == NodeKind::Class {
                return Some(id);
            }
            s = self.nodes[id as usize].parent;
        }
        None
    }

    /// Resolve a list of recorded type expressions to exactly one repo class.
    fn class_from_types(&self, hints: &[TypeHint]) -> Option<NodeId> {
        let mut classes = HashSet::new();
        for hint in hints {
            if let Some(c) = self.hint_class(hint) {
                classes.insert(c);
            }
        }
        (classes.len() == 1).then(|| *classes.iter().next().unwrap())
    }

    /// The class one type hint names: an annotation, a constructor, or a call to
    /// a function whose return type is annotated.
    fn hint_class(&self, (file, scope, expr): &TypeHint) -> Option<NodeId> {
        let scope = (self.kind(*scope) != NodeKind::Module).then_some(*scope);
        if expr == "Self" {
            return self.enclosing_class(scope);
        }
        self.call_value_class(*file, scope, expr)
    }

    /// The class of the value produced by calling `n`: the class itself, or the
    /// declared return class of a function or method.
    fn value_class(&self, n: NodeId) -> Option<NodeId> {
        match self.kind(n) {
            NodeKind::Class => Some(n),
            NodeKind::Module => None,
            NodeKind::Function | NodeKind::Method => {
                if let Some((file, scope, expr)) = self.returns.get(&n) {
                    let scope_opt = (self.kind(*scope) != NodeKind::Module).then_some(*scope);
                    let c = if expr == "Self" {
                        self.enclosing_class(Some(n))
                    } else {
                        match self.resolve_dotted(*file, scope_opt, expr) {
                            Res::Found(c, _) => Some(c),
                            _ => None,
                        }
                    };
                    return c.filter(|c| self.kind(*c) == NodeKind::Class);
                }
                // No annotation: infer from what the function returns, if every
                // returned value (other than None) is the same repo class.
                if let Some(cached) = self.infer_cache.borrow().get(&n) {
                    return *cached;
                }
                self.infer_cache.borrow_mut().insert(n, None);
                let mut classes = HashSet::new();
                let mut unknown = false;
                for (file, _, expr) in self.ret_exprs.get(&n).into_iter().flatten() {
                    match self.expr_class(*file, Some(n), expr) {
                        Some(c) => {
                            classes.insert(c);
                        }
                        None => unknown = true,
                    }
                }
                let out = (classes.len() == 1 && !unknown).then(|| *classes.iter().next().unwrap());
                self.infer_cache.borrow_mut().insert(n, out);
                out
            }
        }
    }

    /// Class of the value an expression evaluates to, where syntax allows:
    /// `self`, typed variables and fields, pytest fixtures, and calls to
    /// classes or functions with a known return class.
    fn expr_class(&self, file: usize, scope: Option<NodeId>, expr: &str) -> Option<NodeId> {
        let d = self.infer_depth.get();
        if d >= MAX_INFER_DEPTH {
            return None;
        }
        self.infer_depth.set(d + 1);
        let out = self.expr_class_inner(file, scope, expr.trim());
        self.infer_depth.set(d);
        out
    }

    fn expr_class_inner(&self, file: usize, scope: Option<NodeId>, expr: &str) -> Option<NodeId> {
        if expr == "self" || expr == "cls" || expr == "this" {
            return self.enclosing_class(scope);
        }
        if let Some(rest) = expr.strip_prefix("new ") {
            let rest = rest.trim();
            if let Some(head) = single_call_callee(rest) {
                return self.call_value_class(file, scope, head);
            }
            if is_dotted_ident(rest) {
                return self.call_value_class(file, scope, rest);
            }
            return None;
        }
        if let Some(head) = single_call_callee(expr) {
            return self.call_value_class(file, scope, head);
        }
        if !is_dotted_ident(expr) {
            return None;
        }
        if !expr.contains('.') {
            return self.var_class(file, scope, expr);
        }
        if let Some(field) = expr
            .strip_prefix("self.")
            .or_else(|| expr.strip_prefix("this."))
            .filter(|f| !f.contains('.'))
        {
            return self
                .enclosing_class(scope)
                .and_then(|c| self.field_class(c, field));
        }
        None
    }

    /// Class of the value returned by calling `head` (a dotted callee).
    fn call_value_class(&self, file: usize, scope: Option<NodeId>, head: &str) -> Option<NodeId> {
        match self.resolve_head(file, scope, head) {
            Res::Found(n, _) => return self.value_class(n),
            Res::External => return None,
            Res::Local | Res::NotFound => {}
        }
        let (recv, method) = head.rsplit_once('.')?;
        let rc = self.expr_class(file, scope, recv)?;
        let target = self.mro_lookup(rc, method)?;
        self.value_class(target)
    }

    /// The pytest fixture a parameter named `name` receives in `file`:
    /// one in the same module first, else the nearest enclosing conftest.py.
    fn pick_fixture(&self, file: usize, name: &str) -> Option<NodeId> {
        let cands = self.fixtures.get(name)?;
        let here = &self.nodes[self.files[file].module as usize].file;
        if let Some(c) = cands
            .iter()
            .find(|c| &self.nodes[**c as usize].file == here)
        {
            return Some(*c);
        }
        let dir_of = |p: &str| {
            p.rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default()
        };
        let here_dir = dir_of(here);
        cands
            .iter()
            .filter_map(|c| {
                let f = &self.nodes[*c as usize].file;
                if !(f == "conftest.py" || f.ends_with("/conftest.py")) {
                    return None;
                }
                let d = dir_of(f);
                let visible =
                    d.is_empty() || here_dir == d || here_dir.starts_with(&format!("{d}/"));
                visible.then_some((d.len(), *c))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, c)| c)
    }

    fn has_external_ancestor(&self, class: NodeId) -> bool {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([class]);
        while let Some(c) = queue.pop_front() {
            if !seen.insert(c) || seen.len() > 64 {
                continue;
            }
            if self.external_based.contains(&c) {
                return true;
            }
            queue.extend(self.bases.get(&c).into_iter().flatten().copied());
        }
        false
    }

    /// Resolve the callee of a call expression: `self.m` / `cls.m` through the
    /// enclosing class, anything else as a dotted name.
    fn resolve_head(&self, file: usize, scope: Option<NodeId>, head: &str) -> Res {
        for prefix in ["self.", "cls.", "this."] {
            if let Some(m) = head.strip_prefix(prefix).filter(|m| !m.contains('.')) {
                return match self
                    .enclosing_class(scope)
                    .and_then(|c| self.mro_lookup(c, m))
                {
                    Some(found) => Res::Found(found, Tier::Scoped),
                    None => Res::NotFound,
                };
            }
        }
        self.resolve_dotted(file, scope, head)
    }

    /// Class of a variable visible from `scope`, from type hints in enclosing scopes.
    fn var_class(&self, file: usize, scope: Option<NodeId>, var: &str) -> Option<NodeId> {
        let key = (scope.unwrap_or(self.files[file].module), var.to_string());
        if let Some(hit) = self.var_cache.borrow().get(&key) {
            return *hit;
        }
        self.var_cache.borrow_mut().insert(key.clone(), None);
        let out = self.var_class_uncached(file, scope, var);
        self.var_cache.borrow_mut().insert(key, out);
        out
    }

    fn var_class_uncached(&self, file: usize, scope: Option<NodeId>, var: &str) -> Option<NodeId> {
        let mut s = scope;
        while let Some(id) = s {
            if matches!(self.kind(id), NodeKind::Function | NodeKind::Method) {
                if let Some(h) = self.local_types.get(&(id, var.to_string())) {
                    return self.class_from_types(h);
                }
                if self.locals.get(&id).is_some_and(|l| l.contains(var)) {
                    // An untyped parameter of a test or fixture is a pytest fixture.
                    let is_test = self.files[file].lang == Lang::Python
                        && self.nodes[id as usize].name.starts_with("test");
                    if is_test || self.is_fixture.contains(&id) {
                        if let Some(f) = self.pick_fixture(file, var) {
                            if f != id {
                                return self.value_class(f);
                            }
                        }
                    }
                    return None;
                }
            }
            s = self.nodes[id as usize].parent;
        }
        let module = self.files[file].module;
        self.local_types
            .get(&(module, var.to_string()))
            .and_then(|h| self.class_from_types(h))
    }

    fn field_class(&self, class: NodeId, field: &str) -> Option<NodeId> {
        let key = (class, field.to_string());
        if let Some(hit) = self.field_cache.borrow().get(&key) {
            return *hit;
        }
        self.field_cache.borrow_mut().insert(key.clone(), None);
        let out = self.field_class_uncached(class, field);
        self.field_cache.borrow_mut().insert(key, out);
        out
    }

    fn field_class_uncached(&self, class: NodeId, field: &str) -> Option<NodeId> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([class]);
        while let Some(c) = queue.pop_front() {
            if !seen.insert(c) || seen.len() > 64 {
                continue;
            }
            if let Some(h) = self.field_types.get(&(c, field.to_string())) {
                return self.class_from_types(h);
            }
            queue.extend(self.bases.get(&c).into_iter().flatten().copied());
        }
        None
    }

    fn fallback(
        &self,
        table: &HashMap<(Lang, String), Vec<NodeId>>,
        lang: Lang,
        name: &str,
    ) -> Vec<(NodeId, Tier)> {
        match table.get(&(lang, name.to_string())).map(Vec::as_slice) {
            Some([one]) => vec![(*one, Tier::NameOnly)],
            Some(many) if many.len() <= MAX_AMBIGUOUS => {
                many.iter().map(|n| (*n, Tier::Ambiguous)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Resolve one call to zero or more targets, or mark it external.
    fn resolve_call(
        &self,
        file: usize,
        scope: Option<NodeId>,
        receiver: Option<&str>,
        callee: &str,
    ) -> Result<Vec<(NodeId, Tier)>, ()> {
        let Some(recv) = receiver else {
            return match self.resolve_name(file, scope, callee) {
                Res::Found(n, t) => Ok(vec![(n, t)]),
                Res::External => Err(()),
                Res::Local => Ok(Vec::new()),
                Res::NotFound => {
                    Ok(self.fallback(&self.callables_by_name, self.files[file].lang, callee))
                }
            };
        };

        // Literals and builtin containers: "x".join(...), [..].append(...)
        if recv.starts_with(['"', '\'', '[', '{', '('])
            || recv.starts_with(|c: char| c.is_ascii_digit())
            || recv.starts_with("f\"")
            || recv.starts_with("f'")
            || recv.starts_with("b\"")
            || recv.starts_with("b'")
        {
            return Err(());
        }

        if recv.starts_with("super(") || recv == "super" {
            if let Some(class) = self.enclosing_class(scope) {
                for b in self.bases.get(&class).into_iter().flatten() {
                    if let Some(m) = self.mro_lookup(*b, callee) {
                        return Ok(vec![(m, Tier::Scoped)]);
                    }
                }
                if self.has_external_ancestor(class) {
                    return Err(());
                }
            }
        } else {
            // A module or class object: `pkg.mod.f()`, `Model.create()`.
            if is_dotted_ident(recv)
                && !matches!(recv, "self" | "cls" | "this")
                && !recv.starts_with("self.")
                && !recv.starts_with("this.")
            {
                match self.resolve_dotted(file, scope, recv) {
                    Res::Found(n, t)
                        if matches!(self.kind(n), NodeKind::Module | NodeKind::Class) =>
                    {
                        if let Some(m) = self.member(n, callee, 0) {
                            return Ok(vec![(m, t)]);
                        }
                        if self.kind(n) == NodeKind::Class && self.has_external_ancestor(n) {
                            return Err(());
                        }
                        // A repo module or class without that member: leave it
                        // unresolved. It is never some other class's method.
                        return Ok(Vec::new());
                    }
                    Res::External => return Err(()),
                    _ => {}
                }
            }
            // An instance whose class can be inferred.
            if let Some(class) = self.expr_class(file, scope, recv) {
                if let Some(m) = self.mro_lookup(class, callee) {
                    return Ok(vec![(m, Tier::Scoped)]);
                }
                if self.has_external_ancestor(class) {
                    // Inherited from a library base class: not in this repo.
                    return Err(());
                }
            }
        }

        // A chain rooted at something outside the repo (`np.array(x).reshape()`,
        // `z.string().optional()` with z from a package) is external, not a guess.
        let root_expr = recv.strip_prefix("new ").unwrap_or(recv);
        let root_len = root_expr
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .unwrap_or(root_expr.len());
        let root = &root_expr[..root_len];
        if !root.is_empty()
            && !matches!(root, "self" | "cls" | "this" | "super")
            && self.resolve_name(file, scope, root) == Res::External
        {
            return Err(());
        }

        let lang = self.files[file].lang;
        let builtin_methods = match lang {
            Lang::Python => python::BUILTIN_METHODS,
            Lang::TypeScript => typescript::BUILTIN_METHODS,
        };
        if (callee.starts_with("__") && callee.ends_with("__")) || builtin_methods.contains(&callee)
        {
            return Ok(Vec::new());
        }
        let mut targets = self.fallback(&self.methods_by_name, lang, callee);
        if lang == Lang::TypeScript {
            // Hand checks showed unique-name method matches on untyped receivers
            // are reliable in Python (26/26) but not in TypeScript (about half
            // right on zod), where callback parameters and object-literal
            // namespaces are common. Label them ambiguous there.
            for t in &mut targets {
                t.1 = Tier::Ambiguous;
            }
        }
        Ok(targets)
    }
}

/// Build the graph for a set of parsed files.
pub fn build(mut parsed: Vec<ParsedFile>, config: &ProjectConfig) -> Graph {
    parsed.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    let package_dirs: HashSet<String> = parsed
        .iter()
        .filter_map(|f| f.rel_path.strip_suffix("/__init__.py").map(str::to_string))
        .collect();

    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: HashMap<(NodeId, NodeId, EdgeKind), Edge> = HashMap::new();
    let add_edge = |edges: &mut HashMap<(NodeId, NodeId, EdgeKind), Edge>, e: Edge| {
        edges
            .entry((e.src, e.dst, e.kind))
            .and_modify(|old| {
                if e.tier > old.tier {
                    *old = e.clone();
                }
            })
            .or_insert(e);
    };

    let mut files = Vec::with_capacity(parsed.len());
    let mut file_info = BTreeMap::new();
    let mut module_by_name = HashMap::new();
    let mut children: HashMap<NodeId, HashMap<String, NodeId>> = HashMap::new();
    let mut def_at: HashMap<(usize, usize), NodeId> = HashMap::new();
    let mut module_meta = Vec::with_capacity(parsed.len());
    let mut returns: HashMap<NodeId, TypeHint> = HashMap::new();
    let mut fixtures: HashMap<String, Vec<NodeId>> = HashMap::new();
    let mut is_fixture: HashSet<NodeId> = HashSet::new();

    // 1. Nodes and containment.
    for (fi, pf) in parsed.iter().enumerate() {
        let (mname, is_init) = match pf.lang {
            Lang::Python => module_name(&pf.rel_path, &package_dirs),
            Lang::TypeScript => (ts_module_name(&pf.rel_path), false),
        };
        let module_id = nodes.len() as NodeId;
        nodes.push(Node {
            id: module_id,
            kind: NodeKind::Module,
            qname: mname.clone(),
            name: match pf.lang {
                Lang::Python => mname.rsplit('.').next(),
                Lang::TypeScript => mname.rsplit('/').next(),
            }
            .unwrap_or(&mname)
            .to_string(),
            file: pf.rel_path.clone(),
            start_line: 1,
            end_line: pf.line_count.max(1),
            parent: None,
        });
        module_by_name.entry(mname.clone()).or_insert(module_id);
        file_info.insert(
            pf.rel_path.clone(),
            FileInfo {
                hash: pf.hash.clone(),
                module: mname.clone(),
            },
        );
        module_meta.push((mname, is_init));

        let mut spans = Vec::with_capacity(pf.defs.len());
        let mut stack: Vec<(usize, NodeId)> = Vec::new(); // (end_byte, id)
        for d in &pf.defs {
            while stack.last().is_some_and(|(end, _)| d.start_byte >= *end) {
                stack.pop();
            }
            let parent = stack.last().map(|(_, id)| *id).unwrap_or(module_id);
            let pkind = nodes[parent as usize].kind;
            let kind = if d.is_class {
                NodeKind::Class
            } else if pkind == NodeKind::Class {
                NodeKind::Method
            } else {
                NodeKind::Function
            };
            let id = nodes.len() as NodeId;
            let qname = format!("{}.{}", nodes[parent as usize].qname, d.name);
            nodes.push(Node {
                id,
                kind,
                qname,
                name: d.name.clone(),
                file: pf.rel_path.clone(),
                start_line: d.start_line,
                end_line: d.end_line,
                parent: Some(parent),
            });
            children
                .entry(parent)
                .or_default()
                .insert(d.name.clone(), id);
            add_edge(
                &mut edges,
                Edge {
                    src: parent,
                    dst: id,
                    kind: EdgeKind::Contains,
                    tier: Tier::Exact,
                    file: pf.rel_path.clone(),
                    line: d.start_line,
                },
            );
            if let Some(ret) = &d.returns {
                returns.insert(id, (fi, parent, ret.clone()));
            }
            if d.is_fixture && !d.is_class {
                fixtures.entry(d.name.clone()).or_default().push(id);
                is_fixture.insert(id);
            }
            spans.push((d.start_byte, d.end_byte, id));
            def_at.insert((fi, d.start_byte), id);
            stack.push((d.end_byte, id));
        }
        files.push(FileCtx {
            module: module_id,
            lang: pf.lang,
            spans,
        });
    }

    // 2. Import bindings.
    let mut bindings: HashMap<NodeId, HashMap<String, String>> = HashMap::new();
    let mut stars: HashMap<NodeId, Vec<String>> = HashMap::new();
    let mut import_targets: Vec<(usize, String, u32)> = Vec::new();
    for (fi, pf) in parsed.iter().enumerate() {
        let module = files[fi].module;
        let (mname, is_init) = &module_meta[fi];
        if pf.lang == Lang::TypeScript {
            for imp in &pf.imports {
                let resolve = |spec: &str| ts_resolve(&module_by_name, config, &pf.rel_path, spec);
                match &imp.import {
                    RawImport::From {
                        from_module,
                        name,
                        alias,
                    } if from_module.is_empty() => {
                        // `export { a as b }` inside this module.
                        if let Some(alias) = alias {
                            bindings
                                .entry(module)
                                .or_default()
                                .insert(alias.clone(), format!("{mname}::{name}"));
                        }
                    }
                    RawImport::From {
                        from_module,
                        name,
                        alias,
                    } => {
                        let local = alias.clone().unwrap_or_else(|| name.clone());
                        let path = match resolve(from_module) {
                            Some(m) => {
                                import_targets.push((fi, format!("{m}::"), imp.line));
                                format!("{m}::{name}")
                            }
                            None => format!("@external::{from_module}"),
                        };
                        bindings.entry(module).or_default().insert(local, path);
                    }
                    RawImport::Module {
                        module: spec,
                        alias: Some(ns),
                    } => {
                        let path = match resolve(spec) {
                            Some(m) => {
                                import_targets.push((fi, format!("{m}::"), imp.line));
                                format!("{m}::")
                            }
                            None => format!("@external::{spec}"),
                        };
                        bindings.entry(module).or_default().insert(ns.clone(), path);
                    }
                    RawImport::Wildcard { from_module } => {
                        if let Some(m) = resolve(from_module) {
                            import_targets.push((fi, format!("{m}::"), imp.line));
                            stars.entry(module).or_default().push(m);
                        }
                    }
                    RawImport::Module { alias: None, .. } => {}
                }
            }
            continue;
        }
        for imp in &pf.imports {
            match &imp.import {
                RawImport::Module { module: m, alias } => {
                    let (local, path) = match alias {
                        Some(a) => (a.clone(), m.clone()),
                        None => {
                            let top = m.split('.').next().unwrap_or(m).to_string();
                            (top.clone(), top)
                        }
                    };
                    bindings.entry(module).or_default().insert(local, path);
                    import_targets.push((fi, m.clone(), imp.line));
                }
                RawImport::From {
                    from_module,
                    name,
                    alias,
                } => {
                    if let Some(abs) = absolute_from(from_module, mname, *is_init) {
                        let full = if abs.is_empty() {
                            name.clone()
                        } else {
                            format!("{abs}.{name}")
                        };
                        let local = alias.clone().unwrap_or_else(|| name.clone());
                        bindings
                            .entry(module)
                            .or_default()
                            .insert(local, full.clone());
                        import_targets.push((fi, full, imp.line));
                        import_targets.push((fi, abs, imp.line));
                    }
                }
                RawImport::Wildcard { from_module } => {
                    if let Some(abs) = absolute_from(from_module, mname, *is_init) {
                        stars.entry(module).or_default().push(abs.clone());
                        import_targets.push((fi, abs, imp.line));
                    }
                }
            }
        }
    }

    // `export default X` makes X reachable as the module member "default".
    for (fi, pf) in parsed.iter().enumerate() {
        let Some(name) = &pf.default_export else {
            continue;
        };
        let m = files[fi].module;
        if let Some(&id) = children.get(&m).and_then(|c| c.get(name)) {
            children.entry(m).or_default().insert("default".into(), id);
        } else if let Some(path) = bindings.get(&m).and_then(|b| b.get(name)).cloned() {
            bindings
                .entry(m)
                .or_default()
                .insert("default".into(), path);
        }
    }

    let file_lang: HashMap<&str, Lang> = parsed
        .iter()
        .map(|p| (p.rel_path.as_str(), p.lang))
        .collect();
    let mut callables_by_name: HashMap<(Lang, String), Vec<NodeId>> = HashMap::new();
    let mut methods_by_name: HashMap<(Lang, String), Vec<NodeId>> = HashMap::new();
    for n in &nodes {
        let key = (file_lang[n.file.as_str()], n.name.clone());
        match n.kind {
            NodeKind::Function | NodeKind::Class => {
                callables_by_name.entry(key).or_default().push(n.id)
            }
            NodeKind::Method => methods_by_name.entry(key).or_default().push(n.id),
            NodeKind::Module => {}
        }
    }

    let mut r = Resolver {
        nodes,
        files,
        children,
        module_by_name,
        bindings,
        stars,
        bases: HashMap::new(),
        local_types: HashMap::new(),
        field_types: HashMap::new(),
        callables_by_name,
        methods_by_name,
        locals: HashMap::new(),
        returns,
        ret_exprs: HashMap::new(),
        fixtures,
        is_fixture,
        external_based: HashSet::new(),
        infer_cache: RefCell::new(HashMap::new()),
        infer_depth: Cell::new(0),
        var_cache: RefCell::new(HashMap::new()),
        field_cache: RefCell::new(HashMap::new()),
    };

    for (fi, pf) in parsed.iter().enumerate() {
        for rv in &pf.return_values {
            if rv.expr == "None" {
                continue;
            }
            if let Some(scope) = r.files[fi].innermost(rv.byte) {
                if matches!(r.kind(scope), NodeKind::Function | NodeKind::Method) {
                    r.ret_exprs
                        .entry(scope)
                        .or_default()
                        .push((fi, scope, rv.expr.clone()));
                }
            }
        }
    }

    for (fi, pf) in parsed.iter().enumerate() {
        for l in &pf.locals {
            let scope = r.files[fi].innermost(l.byte).unwrap_or(r.files[fi].module);
            r.locals.entry(scope).or_default().insert(l.name.clone());
        }
    }

    // IMPORTS edges between repo modules (module-level only; deduplicated).
    for (fi, target, line) in import_targets {
        let src = r.files[fi].module;
        if let Some(mut dst) = r.resolve_path(&target, 0) {
            // `from pkg import func` imports the module that holds func.
            while r.kind(dst) != NodeKind::Module {
                match r.nodes[dst as usize].parent {
                    Some(p) => dst = p,
                    None => break,
                }
            }
            if dst != src {
                add_edge(
                    &mut edges,
                    Edge {
                        src,
                        dst,
                        kind: EdgeKind::Imports,
                        tier: Tier::Scoped,
                        file: parsed[fi].rel_path.clone(),
                        line,
                    },
                );
            }
        }
    }

    // 3. Bases (resolved in the scope enclosing the class).
    for (fi, pf) in parsed.iter().enumerate() {
        for b in &pf.bases {
            let Some(&class) = def_at.get(&(fi, b.class_byte)) else {
                continue;
            };
            let outer = r.nodes[class as usize]
                .parent
                .filter(|p| r.kind(*p) != NodeKind::Module);
            let res = r.resolve_dotted(fi, outer, &b.base);
            let res = match res {
                Res::NotFound if !b.base.contains('.') => {
                    match r
                        .fallback(&r.callables_by_name, r.files[fi].lang, &b.base)
                        .as_slice()
                    {
                        [(n, Tier::NameOnly)] => Res::Found(*n, Tier::NameOnly),
                        _ => Res::NotFound,
                    }
                }
                other => other,
            };
            if res == Res::External {
                r.external_based.insert(class);
            }
            if let Res::Found(base, tier) = res {
                if r.kind(base) == NodeKind::Class && base != class {
                    r.bases.entry(class).or_default().push(base);
                    add_edge(
                        &mut edges,
                        Edge {
                            src: class,
                            dst: base,
                            kind: EdgeKind::Inherits,
                            tier,
                            file: pf.rel_path.clone(),
                            line: b.line,
                        },
                    );
                }
            }
        }
    }

    // 4. Type hints.
    for (fi, pf) in parsed.iter().enumerate() {
        for t in &pf.types {
            let scope = r.files[fi].innermost(t.byte);
            let scope_id = scope.unwrap_or(r.files[fi].module);
            match &t.target {
                TypeTarget::Var(v) => {
                    let entry = (fi, scope_id, t.type_expr.clone());
                    if r.kind(scope_id) == NodeKind::Class {
                        r.field_types
                            .entry((scope_id, v.clone()))
                            .or_default()
                            .push(entry);
                    } else {
                        r.local_types
                            .entry((scope_id, v.clone()))
                            .or_default()
                            .push(entry);
                    }
                }
                TypeTarget::SelfAttr(a) => {
                    if let Some(class) = r.enclosing_class(scope) {
                        r.field_types.entry((class, a.clone())).or_default().push((
                            fi,
                            scope_id,
                            t.type_expr.clone(),
                        ));
                    }
                }
            }
        }
    }

    // 5. Calls.
    let mut stats = ResolutionStats::default();
    for (fi, pf) in parsed.iter().enumerate() {
        for c in &pf.calls {
            stats.calls_seen += 1;
            let scope = r.files[fi].innermost(c.byte);
            let src = scope.unwrap_or(r.files[fi].module);
            match r.resolve_call(fi, scope, c.receiver.as_deref(), &c.callee) {
                Err(()) => stats.external += 1,
                Ok(targets) if targets.is_empty() => stats.unresolved += 1,
                Ok(targets) => {
                    match targets[0].1 {
                        Tier::Scoped | Tier::Exact => stats.resolved_t1 += 1,
                        Tier::NameOnly => stats.resolved_t0 += 1,
                        Tier::Ambiguous => stats.ambiguous += 1,
                    }
                    for (dst, tier) in targets {
                        if dst == src {
                            // Recursion is real but adds nothing to navigation.
                            continue;
                        }
                        add_edge(
                            &mut edges,
                            Edge {
                                src,
                                dst,
                                kind: EdgeKind::Calls,
                                tier,
                                file: pf.rel_path.clone(),
                                line: c.line,
                            },
                        );
                    }
                }
            }
        }
    }

    let mut edges: Vec<Edge> = edges.into_values().collect();
    edges.sort_by_key(|e| (e.src, e.dst, e.kind as u8));
    Graph::new(r.nodes, edges, file_info, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_follow_packages() {
        let pk: HashSet<String> = ["src/app", "src/app/core"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            module_name("src/app/core/db.py", &pk),
            ("app.core.db".into(), false)
        );
        assert_eq!(
            module_name("src/app/__init__.py", &pk),
            ("app".into(), true)
        );
        assert_eq!(module_name("scripts/run.py", &pk), ("run".into(), false));
        // Namespace subpackage under a regular package.
        assert_eq!(
            module_name("src/app/sansio/core.py", &pk),
            ("app.sansio.core".into(), false)
        );
    }

    #[test]
    fn relative_imports() {
        assert_eq!(
            absolute_from(".models", "app.api.views", false).as_deref(),
            Some("app.api.models")
        );
        assert_eq!(
            absolute_from("..core", "app.api.views", false).as_deref(),
            Some("app.core")
        );
        assert_eq!(
            absolute_from(".", "app.api", true).as_deref(),
            Some("app.api")
        );
        assert_eq!(absolute_from("...x", "a.b", false), None);
    }

    #[test]
    fn single_call_receivers() {
        assert_eq!(
            single_call_callee("requests.Request(\"GET\", url)"),
            Some("requests.Request")
        );
        assert_eq!(single_call_callee("Foo(bar(1), \")\")"), Some("Foo"));
        assert_eq!(single_call_callee("foo(1)(2)"), None);
        assert_eq!(single_call_callee("foo(1).x"), None);
        assert_eq!(single_call_callee("a[0](1)"), None);
    }
}
