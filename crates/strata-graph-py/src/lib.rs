//! Native module behind the `strata_graph` Python package.
//!
//! Every query returns a JSON string; the pure-Python layer in
//! `python/strata_graph/__init__.py` decodes it into plain dicts and lists.
//! Long-running work (indexing, staleness checks) runs without the GIL.

use std::path::PathBuf;

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use strata_graph_core::{
    context_pack, index_path, open_index, stale_files, update_index, Graph, Store, Tier,
};

create_exception!(
    _native,
    StrataError,
    PyException,
    "Raised for any strata-graph failure."
);

fn err(e: anyhow::Error) -> PyErr {
    StrataError::new_err(format!("{e:#}"))
}

fn json<T: serde::Serialize>(v: &T) -> PyResult<String> {
    serde_json::to_string(v).map_err(|e| StrataError::new_err(e.to_string()))
}

fn tier(s: &str) -> PyResult<Tier> {
    match s.to_ascii_lowercase().as_str() {
        "ambiguous" | "t0-ambiguous" => Ok(Tier::Ambiguous),
        "t0" => Ok(Tier::NameOnly),
        "t1" => Ok(Tier::Scoped),
        "t2" => Ok(Tier::Exact),
        other => Err(StrataError::new_err(format!(
            "min_tier must be one of 'ambiguous', 't0', 't1', 't2'; got {other:?}"
        ))),
    }
}

/// An open index for one repository.
#[pyclass(name = "NativeGraph", module = "strata_graph._native")]
struct NativeGraph {
    root: PathBuf,
    graph: Graph,
    auto_refresh: bool,
}

impl NativeGraph {
    /// Pick up file changes before answering, if auto-refresh is on.
    fn fresh(&mut self, py: Python<'_>) -> PyResult<()> {
        if !self.auto_refresh {
            return Ok(());
        }
        let root = self.root.clone();
        let updated = py
            .detach(move || -> anyhow::Result<Option<Graph>> {
                if stale_files(&root)? == 0 {
                    return Ok(None);
                }
                Ok(Some(update_index(&root)?.0))
            })
            .map_err(err)?;
        if let Some(g) = updated {
            self.graph = g;
        }
        Ok(())
    }

    fn one(&self, symbol: &str) -> PyResult<u32> {
        self.graph.resolve_one(symbol).map_err(err)
    }
}

#[pymethods]
impl NativeGraph {
    #[getter]
    fn root(&self) -> String {
        self.root.display().to_string()
    }

    /// Re-index changed files now. Returns the update report as JSON.
    fn refresh(&mut self, py: Python<'_>) -> PyResult<String> {
        let root = self.root.clone();
        let (g, report) = py.detach(move || update_index(&root)).map_err(err)?;
        self.graph = g;
        json(&report)
    }

    /// Number of files changed on disk since the last update.
    fn stale_files(&self, py: Python<'_>) -> PyResult<usize> {
        let root = self.root.clone();
        py.detach(move || stale_files(&root)).map_err(err)
    }

    fn stats(&mut self, py: Python<'_>) -> PyResult<String> {
        self.fresh(py)?;
        json(&self.graph.summary())
    }

    fn find(&mut self, py: Python<'_>, query: &str) -> PyResult<String> {
        self.fresh(py)?;
        let nodes: Vec<_> = self
            .graph
            .find(query)
            .into_iter()
            .map(|i| self.graph.node(i))
            .collect();
        json(&nodes)
    }

    #[pyo3(signature = (query, limit=20))]
    fn search(&mut self, py: Python<'_>, query: &str, limit: usize) -> PyResult<String> {
        self.fresh(py)?;
        let ids = Store::open(&index_path(&self.root))
            .and_then(|s| s.search(query, limit))
            .map_err(err)?;
        let nodes: Vec<_> = ids.into_iter().map(|i| self.graph.node(i)).collect();
        json(&nodes)
    }

    fn node(&mut self, py: Python<'_>, symbol: &str) -> PyResult<String> {
        self.fresh(py)?;
        let id = self.one(symbol)?;
        json(self.graph.node(id))
    }

    #[pyo3(signature = (symbol, depth=1, min_tier="t0"))]
    fn callers(
        &mut self,
        py: Python<'_>,
        symbol: &str,
        depth: u32,
        min_tier: &str,
    ) -> PyResult<String> {
        self.fresh(py)?;
        let id = self.one(symbol)?;
        json(&self.graph.callers(id, depth, tier(min_tier)?))
    }

    #[pyo3(signature = (symbol, depth=1, min_tier="t0"))]
    fn callees(
        &mut self,
        py: Python<'_>,
        symbol: &str,
        depth: u32,
        min_tier: &str,
    ) -> PyResult<String> {
        self.fresh(py)?;
        let id = self.one(symbol)?;
        json(&self.graph.callees(id, depth, tier(min_tier)?))
    }

    #[pyo3(signature = (symbol, depth=3, min_tier="t0"))]
    fn impact(
        &mut self,
        py: Python<'_>,
        symbol: &str,
        depth: u32,
        min_tier: &str,
    ) -> PyResult<String> {
        self.fresh(py)?;
        let id = self.one(symbol)?;
        json(&self.graph.impact(id, depth, tier(min_tier)?))
    }

    /// Source, class, callers and callees of `symbol` as text, in about
    /// `max_tokens` tokens.
    #[pyo3(signature = (symbol, max_tokens=1500, min_tier="t0"))]
    fn context(
        &mut self,
        py: Python<'_>,
        symbol: &str,
        max_tokens: usize,
        min_tier: &str,
    ) -> PyResult<String> {
        self.fresh(py)?;
        let id = self.one(symbol)?;
        Ok(context_pack(
            &self.root,
            &self.graph,
            id,
            max_tokens,
            tier(min_tier)?,
        ))
    }

    /// Shortest call path as a JSON list of qualified names, or None.
    #[pyo3(signature = (source, target, max_depth=8, min_tier="t0"))]
    fn path(
        &mut self,
        py: Python<'_>,
        source: &str,
        target: &str,
        max_depth: u32,
        min_tier: &str,
    ) -> PyResult<Option<String>> {
        self.fresh(py)?;
        let (a, b) = (self.one(source)?, self.one(target)?);
        match self.graph.path(a, b, max_depth, tier(min_tier)?) {
            Some(p) => {
                let names: Vec<&str> = p
                    .iter()
                    .map(|i| self.graph.node(*i).qname.as_str())
                    .collect();
                Ok(Some(json(&names)?))
            }
            None => Ok(None),
        }
    }
}

/// Open (building or updating as needed) the index for the repo at `path`.
#[pyfunction]
#[pyo3(signature = (path=".", auto_refresh=true))]
fn open(py: Python<'_>, path: &str, auto_refresh: bool) -> PyResult<NativeGraph> {
    let root =
        std::fs::canonicalize(path).map_err(|e| StrataError::new_err(format!("{path}: {e}")))?;
    let r = root.clone();
    let graph = py
        .detach(move || -> anyhow::Result<Graph> {
            if !index_path(&r).exists() || stale_files(&r)? > 0 {
                Ok(update_index(&r)?.0)
            } else {
                open_index(&r)
            }
        })
        .map_err(err)?;
    Ok(NativeGraph {
        root,
        graph,
        auto_refresh,
    })
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeGraph>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add("StrataError", m.py().get_type::<StrataError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
