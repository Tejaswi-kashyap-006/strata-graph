//! strata-graph-core: build a confidence-labelled code graph from a repository
//! and answer the structural questions coding agents ask.
//!
//! ```no_run
//! let graph = strata_graph_core::index_repo(std::path::Path::new(".")).unwrap();
//! let id = graph.resolve_one("billing.invoice.compute_total").unwrap();
//! for hit in graph.callers(id, 2, strata_graph_core::Tier::NameOnly) {
//!     println!("{} {} {}", hit.tier.label(), hit.qname, hit.at);
//! }
//! ```

pub mod graph;
pub mod lang;
pub mod model;
pub mod resolve;

use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;

pub use graph::{Direction, Graph, Hit};
pub use model::{Edge, EdgeKind, Node, NodeId, NodeKind, ResolutionStats, Tier};

/// Files larger than this are skipped (usually generated or vendored).
pub const MAX_FILE_BYTES: u64 = 1_000_000;

/// Default location of the index inside a repo.
pub fn index_path(root: &Path) -> std::path::PathBuf {
    root.join(".strata").join("graph.json")
}

/// Walk `root` (respecting .gitignore), parse every Python file in parallel,
/// and resolve the result into a graph.
pub fn index_repo(root: &Path) -> Result<Graph> {
    let mut paths = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .filter_entry(|e| e.file_name() != ".strata")
        .build()
    {
        let entry = entry?;
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "py")
            && entry.file_type().is_some_and(|t| t.is_file())
            && entry
                .metadata()
                .map(|m| m.len() <= MAX_FILE_BYTES)
                .unwrap_or(false)
        {
            paths.push(p.to_path_buf());
        }
    }

    let parsed: Vec<_> = paths
        .par_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read(p).ok()?;
            let src = String::from_utf8_lossy(&src);
            lang::python::parse(&rel, &src).ok()
        })
        .collect();

    Ok(resolve::build(parsed))
}
