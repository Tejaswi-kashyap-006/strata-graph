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

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod context;
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

pub use config::project_config;
pub use context::context_pack;
pub use graph::{tree_order, Direction, Graph, Hit};
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
    /// Absolute path on disk.
    pub abs: PathBuf,
    /// Path relative to the repo root, `/`-separated.
    pub rel: String,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Size in bytes.
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

/// What one incremental update did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexReport {
    /// Source files found.
    pub files: usize,
    /// Files parsed this time (new or changed content).
    pub parsed: usize,
    /// Files whose cached facts were reused.
    pub reused: usize,
    /// Files dropped from the index because they no longer exist.
    pub removed: usize,
    /// Wall-clock time for the whole update.
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

/// Number of source files added, removed, or modified (by size or mtime)
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
