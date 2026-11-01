//! strata-graph-core: build a confidence-labelled code graph from a repository
//! and answer the structural questions coding agents ask.
//!
//! ```no_run
//! use std::path::Path;
//! let (graph, report) = strata_graph_core::update_index(Path::new(".")).unwrap();
//! println!("re-parsed {} of {} files", report.parsed, report.files);
//! let id = graph.resolve_one("billing.invoice.compute_total").unwrap();
//! for hit in graph.callers(id, 2, strata_graph_core::Tier::NameOnly) {
//!     println!("{} {} {}", hit.tier.label(), hit.qname, hit.at);
//! }
//! ```

pub mod graph;
pub mod lang;
pub mod model;
pub mod resolve;
pub mod store;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

pub use graph::{Direction, Graph, Hit};
pub use lang::Lang;
pub use model::{Edge, EdgeKind, Node, NodeId, NodeKind, ResolutionStats, Tier};
pub use resolve::ProjectConfig;
pub use store::Store;

/// Bumped whenever resolution rules change, so an existing index is rebuilt
/// by a newer engine even if no source file changed.
pub const RESOLVER_REVISION: u32 = 5;

/// Files larger than this are skipped (usually generated or vendored).
pub const MAX_FILE_BYTES: u64 = 1_000_000;

/// Location of the index inside a repo.
pub fn index_path(root: &Path) -> PathBuf {
    root.join(".strata").join("index.db")
}

/// A source file found on disk, with the metadata used to detect changes.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub abs: PathBuf,
    pub rel: String,
    pub mtime_ns: i64,
    pub size: i64,
}

/// Python, TypeScript and JavaScript files under `root`, respecting .gitignore
/// and skipping `.strata/` and `node_modules/`.
pub fn walk_sources(root: &Path) -> Result<Vec<SourceFile>> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .filter_entry(|e| e.file_name() != ".strata" && e.file_name() != "node_modules")
        .build()
    {
        let entry = entry?;
        let p = entry.path();
        if lang::lang_for(p).is_none() || !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        out.push(SourceFile {
            abs: p.to_path_buf(),
            rel: rel.to_string_lossy().replace('\\', "/"),
            mtime_ns,
            size: meta.len() as i64,
        });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn parse_file(f: &SourceFile) -> Option<lang::ParsedFile> {
    let bytes = std::fs::read(&f.abs).ok()?;
    lang::parse(&f.rel, &String::from_utf8_lossy(&bytes)).ok()
}

/// Parse and resolve `root` entirely in memory, without touching the store.
pub fn index_repo(root: &Path) -> Result<Graph> {
    let files = walk_sources(root)?;
    let parsed: Vec<_> = files.par_iter().filter_map(parse_file).collect();
    Ok(resolve::build(parsed, &project_config(root)))
}

/// Strip `//` and `/* */` comments, then trailing commas, from JSONC (tsconfig) text.
/// Two passes, so a comment between a trailing comma and `}` is handled.
fn strip_jsonc(src: &str) -> String {
    // Pass 1: comments, leaving string contents (e.g. "https://...") intact.
    let b = src.as_bytes();
    let mut no_comments = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            no_comments.push(c);
            if c == b'\\' && i + 1 < b.len() {
                no_comments.push(b[i + 1]);
                i += 2;
                continue;
            }
            in_str = c != b'"';
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            in_str = c == b'"';
            no_comments.push(c);
            i += 1;
        }
    }
    // Pass 2: commas whose next non-space character closes an object or array.
    let mut out = Vec::with_capacity(no_comments.len());
    let mut in_str = false;
    let mut i = 0;
    while i < no_comments.len() {
        let c = no_comments[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < no_comments.len() {
                out.push(no_comments[i + 1]);
                i += 2;
                continue;
            }
            in_str = c != b'"';
        } else if c == b',' {
            let next = no_comments[i + 1..]
                .iter()
                .find(|x| !x.is_ascii_whitespace());
            if !matches!(next, Some(b'}') | Some(b']')) {
                out.push(c);
            }
        } else {
            in_str = c == b'"';
            out.push(c);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn join(dir: &str, rest: &str) -> String {
    let rest = rest.trim_start_matches("./");
    let joined = if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    };
    // Normalise "a/b/../c" and a bare "." to root-relative form.
    let mut out: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// Read `rel` (a tsconfig/jsconfig) and its relative `extends` chain into `cfg`.
/// Parents are applied first so the child's settings win.
fn load_tsconfig(root: &Path, rel: &str, cfg: &mut ProjectConfig, depth: u32) {
    if depth > 4 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else {
        return;
    };
    let dir = dir_of(rel);
    if let Some(parent) = json.get("extends").and_then(|e| e.as_str()) {
        if parent.starts_with('.') {
            let mut p = join(dir, parent);
            if !p.ends_with(".json") {
                p.push_str(".json");
            }
            load_tsconfig(root, &p, cfg, depth + 1);
        }
    }
    let Some(opts) = json.get("compilerOptions") else {
        return;
    };
    let base = opts
        .get("baseUrl")
        .and_then(|b| b.as_str())
        .map(|b| join(dir, b));
    if let Some(b) = &base {
        cfg.base_url = Some(b.clone());
    }
    if let Some(paths) = opts.get("paths").and_then(|p| p.as_object()) {
        let target_dir = base.clone().unwrap_or_else(|| dir.to_string());
        cfg.paths = paths
            .iter()
            .map(|(pattern, targets)| {
                let ts = targets
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str())
                    .map(|t| join(&target_dir, t))
                    .collect();
                (pattern.clone(), ts)
            })
            .collect();
    }
}

/// Every string inside a package.json `exports` value, in file order.
fn string_leaves(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| string_leaves(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| string_leaves(x, out)),
        _ => {}
    }
}

/// package.json files inside the repo that declare a name. Ignore files are
/// not consulted here: some packages gitignore their own package.json
/// (zod's `packages/zod/.gitignore` does). Copies in build output are harmless,
/// since resolution only accepts targets that exist as indexed source files.
fn workspace_packages(root: &Path) -> Vec<resolve::WorkspacePackage> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .filter_entry(|e| {
            let n = e.file_name();
            n != ".strata" && n != "node_modules" && n != ".git"
        })
        .build()
        .flatten()
    {
        if entry.file_name() != "package.json" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(name) = json.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let dir = dir_of(&rel).to_string();
        let mut exports = Vec::new();
        match json.get("exports") {
            Some(serde_json::Value::Object(o)) if o.keys().any(|k| k.starts_with('.')) => {
                for (key, value) in o {
                    let mut leaves = Vec::new();
                    string_leaves(value, &mut leaves);
                    exports.push((key.clone(), leaves.iter().map(|t| join(&dir, t)).collect()));
                }
            }
            Some(v) => {
                let mut leaves = Vec::new();
                string_leaves(v, &mut leaves);
                exports.push((
                    ".".to_string(),
                    leaves.iter().map(|t| join(&dir, t)).collect(),
                ));
            }
            None => {}
        }
        let entries = ["source", "module", "main", "types"]
            .iter()
            .filter_map(|k| json.get(*k).and_then(|v| v.as_str()))
            .map(|t| join(&dir, t))
            .collect();
        out.push(resolve::WorkspacePackage {
            name: name.to_string(),
            dir,
            exports,
            entries,
        });
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// Import settings: the root `tsconfig.json` (or `jsconfig.json`) and the
/// repo's own JS/TS packages.
pub fn project_config(root: &Path) -> ProjectConfig {
    let mut cfg = ProjectConfig::default();
    for name in ["tsconfig.json", "jsconfig.json"] {
        if root.join(name).is_file() {
            load_tsconfig(root, name, &mut cfg, 0);
            break;
        }
    }
    cfg.packages = workspace_packages(root);
    cfg
}

/// What one incremental update did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexReport {
    pub files: usize,
    /// Files parsed this time (new or changed content).
    pub parsed: usize,
    /// Files whose cached facts were reused.
    pub reused: usize,
    pub removed: usize,
    pub elapsed_ms: u128,
}

/// Bring the on-disk index for `root` up to date and return the graph.
///
/// Files whose size and mtime match the store are reused without reading;
/// files whose bytes hash the same are reused without parsing. The graph is
/// then re-resolved from all facts, so edges into and out of changed files
/// are always current.
pub fn update_index(root: &Path) -> Result<(Graph, IndexReport)> {
    let start = Instant::now();
    let mut store = Store::open(&index_path(root))?;
    let known = store.file_meta()?;
    let sources = walk_sources(root)?;

    // Classify every file in parallel: unchanged, touched (same bytes), or changed.
    enum State {
        Same,
        Touched,
        Changed(Box<lang::ParsedFile>),
        Unreadable,
    }
    let states: Vec<State> = sources
        .par_iter()
        .map(|f| match known.get(&f.rel) {
            Some(m) if m.mtime_ns == f.mtime_ns && m.size == f.size => State::Same,
            prev => {
                let Ok(bytes) = std::fs::read(&f.abs) else {
                    return State::Unreadable;
                };
                let hash = blake3::hash(&bytes).to_hex().to_string();
                if prev.is_some_and(|m| m.hash == hash) {
                    return State::Touched;
                }
                match lang::parse(&f.rel, &String::from_utf8_lossy(&bytes)) {
                    Ok(pf) => State::Changed(Box::new(pf)),
                    Err(_) => State::Unreadable,
                }
            }
        })
        .collect();

    let mut reuse = Vec::new();
    let mut touched = Vec::new();
    let mut changed = Vec::new();
    for (f, st) in sources.iter().zip(states) {
        match st {
            State::Same => reuse.push(f.rel.clone()),
            State::Touched => {
                reuse.push(f.rel.clone());
                touched.push((f.rel.clone(), f.mtime_ns, f.size));
            }
            State::Changed(pf) => changed.push((*pf, f.mtime_ns, f.size)),
            State::Unreadable => {}
        }
    }
    let present: HashSet<&str> = sources.iter().map(|f| f.rel.as_str()).collect();
    let removed: Vec<String> = known
        .keys()
        .filter(|k| !present.contains(k.as_str()))
        .cloned()
        .collect();

    // A new engine or changed import settings (tsconfig paths, workspace
    // packages) re-resolve everything from the cached facts.
    let config = project_config(root);
    let config_key = format!(
        "{}+r{}:{}",
        env!("CARGO_PKG_VERSION"),
        RESOLVER_REVISION,
        serde_json::to_string(&config)?
    );
    let config_same = store.meta("config")?.as_deref() == Some(config_key.as_str());

    let graph = if changed.is_empty() && removed.is_empty() && config_same && store.has_graph()? {
        let g = store.load_graph()?;
        if !touched.is_empty() {
            store.write(&[], &touched, &[], &g, &config_key)?;
        }
        g
    } else {
        let mut all = store.load_facts(&reuse)?;
        all.extend(changed.iter().map(|(pf, _, _)| pf.clone()));
        let g = resolve::build(all, &config);
        store.write(&changed, &touched, &removed, &g, &config_key)?;
        g
    };

    let report = IndexReport {
        files: sources.len(),
        parsed: changed.len(),
        reused: reuse.len(),
        removed: removed.len(),
        elapsed_ms: start.elapsed().as_millis(),
    };
    Ok((graph, report))
}

/// Load the last written graph without checking files on disk.
pub fn open_index(root: &Path) -> Result<Graph> {
    Store::open(&index_path(root))?.load_graph()
}

/// Number of Python files added, removed, or modified (by size or mtime)
/// since the last update. Zero means the index is fresh.
pub fn stale_files(root: &Path) -> Result<usize> {
    let store = Store::open(&index_path(root))?;
    let known = store.file_meta()?;
    let sources = walk_sources(root)?;
    let mut stale = sources
        .iter()
        .filter(|f| {
            known
                .get(&f.rel)
                .is_none_or(|m| m.mtime_ns != f.mtime_ns || m.size != f.size)
        })
        .count();
    let present: HashSet<&str> = sources.iter().map(|f| f.rel.as_str()).collect();
    stale += known
        .keys()
        .filter(|k| !present.contains(k.as_str()))
        .count();
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_with_comments_and_trailing_commas() {
        let src = r#"{
  // line comment
  "a": "https://x.dev/*not-a-comment*/", /* block */
  "b": [1, 2,], /* comment after a trailing comma */
}"#;
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(src)).unwrap();
        assert_eq!(v["a"], "https://x.dev/*not-a-comment*/");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }

    #[test]
    fn reads_fixture_tsconfig_paths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ts_shop");
        let cfg = project_config(&root);
        assert_eq!(
            cfg.paths,
            vec![("@/*".to_string(), vec!["src/*".to_string()])]
        );
    }
}
