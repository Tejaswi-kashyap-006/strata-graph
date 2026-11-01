//! On-disk index in `.strata/index.db` (SQLite).
//!
//! The store keeps two things: each file's parsed facts, keyed by content hash
//! so unchanged files are never re-parsed, and the resolved graph from the last
//! update. Resolution is global and cheap next to parsing, so every update
//! re-resolves from the cached facts; cross-file edges can't go stale.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::graph::{FileInfo, Graph};
use crate::lang::ParsedFile;
use crate::model::{Edge, EdgeKind, Node, NodeId, NodeKind, ResolutionStats, Tier};

/// Bump when the schema or the parsed-facts format changes; old stores are rebuilt.
pub const STORE_VERSION: i64 = 3;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS files (
    path TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    module TEXT NOT NULL DEFAULT '',
    mtime_ns INTEGER NOT NULL,
    size INTEGER NOT NULL,
    facts BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS nodes (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    qname TEXT NOT NULL,
    name TEXT NOT NULL,
    file TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    parent INTEGER
);
CREATE TABLE IF NOT EXISTS edges (
    src INTEGER NOT NULL,
    dst INTEGER NOT NULL,
    kind TEXT NOT NULL,
    tier TEXT NOT NULL,
    file TEXT NOT NULL,
    line INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS edges_src ON edges(src);
CREATE INDEX IF NOT EXISTS edges_dst ON edges(dst);
CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    qname, name, content='nodes', content_rowid='id', tokenize="unicode61 tokenchars '_'"
);
"#;

/// What the store remembers about a file without loading its facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// BLAKE3 hash of the file's bytes.
    pub hash: String,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Size in bytes.
    pub size: i64,
}

/// A connection to `.strata/index.db`.
pub struct Store {
    conn: Connection,
}

fn kind_label(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Module => "module",
        NodeKind::Class => "class",
        NodeKind::Function => "function",
        NodeKind::Method => "method",
    }
}

fn parse_kind(s: &str) -> Result<NodeKind> {
    Ok(match s {
        "module" => NodeKind::Module,
        "class" => NodeKind::Class,
        "function" => NodeKind::Function,
        "method" => NodeKind::Method,
        other => bail!("unknown node kind {other:?} in index"),
    })
}

fn edge_label(k: EdgeKind) -> &'static str {
    match k {
        EdgeKind::Contains => "CONTAINS",
        EdgeKind::Imports => "IMPORTS",
        EdgeKind::Calls => "CALLS",
        EdgeKind::Inherits => "INHERITS",
    }
}

fn parse_edge(s: &str) -> Result<EdgeKind> {
    Ok(match s {
        "CONTAINS" => EdgeKind::Contains,
        "IMPORTS" => EdgeKind::Imports,
        "CALLS" => EdgeKind::Calls,
        "INHERITS" => EdgeKind::Inherits,
        other => bail!("unknown edge kind {other:?} in index"),
    })
}

fn parse_tier(s: &str) -> Result<Tier> {
    Ok(match s {
        "T0-ambiguous" => Tier::Ambiguous,
        "T0" => Tier::NameOnly,
        "T1" => Tier::Scoped,
        "T2" => Tier::Exact,
        other => bail!("unknown tier {other:?} in index"),
    })
}

impl Store {
    /// Open (creating if needed) the store at `path`. A store written by an
    /// older version is wiped and rebuilt on the next update.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        let mut store = Store { conn };
        let version: Option<i64> = store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'store_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse().ok());
        if version != Some(STORE_VERSION) {
            store.reset()?;
        }
        Ok(store)
    }

    fn reset(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS nodes_fts; DROP TABLE IF EXISTS edges; DROP TABLE IF EXISTS nodes;
             DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS meta;",
        )?;
        self.conn.execute_batch(SCHEMA)?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES ('store_version', ?1)",
            params![STORE_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// A value from the meta table.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Overwrite a meta value. Exposed for tests that simulate old indexes.
    #[doc(hidden)]
    pub fn set_meta_for_tests(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// True once at least one update has been written.
    pub fn has_graph(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM meta WHERE key = 'stats'", [], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Hash, mtime and size of every indexed file, keyed by path.
    pub fn file_meta(&self) -> Result<HashMap<String, FileMeta>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, hash, mtime_ns, size FROM files")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                FileMeta {
                    hash: r.get(1)?,
                    mtime_ns: r.get(2)?,
                    size: r.get(3)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Cached facts for `paths`, deserialised in parallel.
    pub fn load_facts(&self, paths: &[String]) -> Result<Vec<ParsedFile>> {
        use rayon::prelude::*;
        let want: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();
        let mut stmt = self.conn.prepare("SELECT path, facts FROM files")?;
        let mut blobs = Vec::with_capacity(paths.len());
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            if want.contains(path.as_str()) {
                blobs.push(row.get::<_, Vec<u8>>(1)?);
            }
        }
        blobs
            .par_iter()
            .map(|b| serde_json::from_slice::<ParsedFile>(b).context("decoding cached facts"))
            .collect()
    }

    /// Persist one update: new or changed files with their facts, files whose
    /// metadata changed but content did not, removed files, and the new graph.
    pub fn write(
        &mut self,
        changed: &[(ParsedFile, i64, i64)],
        touched: &[(String, i64, i64)],
        removed: &[String],
        graph: &Graph,
        config_key: &str,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut put = tx.prepare(
                "INSERT OR REPLACE INTO files(path, hash, module, mtime_ns, size, facts)
                 VALUES (?1, ?2, '', ?3, ?4, ?5)",
            )?;
            for (pf, mtime, size) in changed {
                put.execute(params![
                    pf.rel_path,
                    pf.hash,
                    mtime,
                    size,
                    serde_json::to_vec(pf)?
                ])?;
            }
            let mut touch =
                tx.prepare("UPDATE files SET mtime_ns = ?2, size = ?3 WHERE path = ?1")?;
            for (path, mtime, size) in touched {
                touch.execute(params![path, mtime, size])?;
            }
            let mut del = tx.prepare("DELETE FROM files WHERE path = ?1")?;
            for path in removed {
                del.execute(params![path])?;
            }
            let mut module = tx.prepare("UPDATE files SET module = ?2 WHERE path = ?1")?;
            for (path, info) in &graph.files {
                module.execute(params![path, info.module])?;
            }

            tx.execute_batch("DELETE FROM nodes; DELETE FROM edges;")?;
            let mut node = tx.prepare(
                "INSERT INTO nodes(id, kind, qname, name, file, start_line, end_line, parent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for n in &graph.nodes {
                node.execute(params![
                    n.id,
                    kind_label(n.kind),
                    n.qname,
                    n.name,
                    n.file,
                    n.start_line,
                    n.end_line,
                    n.parent
                ])?;
            }
            let mut edge = tx.prepare(
                "INSERT INTO edges(src, dst, kind, tier, file, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for e in &graph.edges {
                edge.execute(params![
                    e.src,
                    e.dst,
                    edge_label(e.kind),
                    e.tier.label(),
                    e.file,
                    e.line
                ])?;
            }
            tx.execute("INSERT INTO nodes_fts(nodes_fts) VALUES ('rebuild')", [])?;
            tx.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('stats', ?1)",
                params![serde_json::to_string(&graph.stats)?],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('config', ?1)",
                params![config_key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The graph written by the last update.
    pub fn load_graph(&self) -> Result<Graph> {
        if !self.has_graph()? {
            bail!("the index is empty; run `strata-graph index` first");
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, qname, name, file, start_line, end_line, parent FROM nodes ORDER BY id",
        )?;
        let mut nodes = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let id: NodeId = r.get(0)?;
            if id as usize != nodes.len() {
                bail!(
                    "index is corrupt (node ids are not contiguous); re-run `strata-graph index`"
                );
            }
            nodes.push(Node {
                id,
                kind: parse_kind(&r.get::<_, String>(1)?)?,
                qname: r.get(2)?,
                name: r.get(3)?,
                file: r.get(4)?,
                start_line: r.get(5)?,
                end_line: r.get(6)?,
                parent: r.get(7)?,
            });
        }
        let mut stmt = self
            .conn
            .prepare("SELECT src, dst, kind, tier, file, line FROM edges")?;
        let mut edges = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            edges.push(Edge {
                src: r.get(0)?,
                dst: r.get(1)?,
                kind: parse_edge(&r.get::<_, String>(2)?)?,
                tier: parse_tier(&r.get::<_, String>(3)?)?,
                file: r.get(4)?,
                line: r.get(5)?,
            });
        }
        let mut files = BTreeMap::new();
        let mut stmt = self.conn.prepare("SELECT path, hash, module FROM files")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            files.insert(
                r.get::<_, String>(0)?,
                FileInfo {
                    hash: r.get(1)?,
                    module: r.get(2)?,
                },
            );
        }
        let stats: ResolutionStats = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'stats'", [], |r| {
                r.get::<_, String>(0)
            })
            .map(|s| serde_json::from_str(&s).unwrap_or_default())?;
        Ok(Graph::new(nodes, edges, files, stats))
    }

    /// Full-text search over qualified and simple names. Each word is matched
    /// as a prefix, so `inv tot` finds `billing.invoice.Invoice.total`.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<NodeId>> {
        let terms: Vec<String> = query
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{t}\"*"))
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH ?1 ORDER BY bm25(nodes_fts, 1.0, 4.0) LIMIT ?2",
        )?;
        let ids = stmt.query_map(params![terms.join(" AND "), limit as i64], |r| r.get(0))?;
        Ok(ids.collect::<rusqlite::Result<_>>()?)
    }
}
