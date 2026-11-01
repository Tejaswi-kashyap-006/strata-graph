//! The resolved graph and the queries agents run against it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Edge, EdgeKind, Node, NodeId, NodeKind, ResolutionStats, Tier};

/// Version of the JSON export format written by [`Graph::save`].
pub const SCHEMA_VERSION: u32 = 1;

/// What the graph records about each indexed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    /// BLAKE3 hash of the file's bytes when it was indexed.
    pub hash: String,
    /// Module name the file was given.
    pub module: String,
}

/// A resolved code graph with lookup indexes for queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    /// Export format version; see [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// All nodes; a node's position is its id.
    pub nodes: Vec<Node>,
    /// All edges, sorted by source, destination and kind.
    pub edges: Vec<Edge>,
    /// Indexed files by repo-relative path.
    pub files: BTreeMap<String, FileInfo>,
    /// Call-resolution counts from the build.
    pub stats: ResolutionStats,
    #[serde(skip)]
    out_edges: Vec<Vec<usize>>,
    #[serde(skip)]
    in_edges: Vec<Vec<usize>>,
    #[serde(skip)]
    by_qname: HashMap<String, Vec<NodeId>>,
    #[serde(skip)]
    by_name: HashMap<String, Vec<NodeId>>,
}

/// Which way a traversal follows edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From source to destination (e.g. what a function calls).
    Out,
    /// From destination back to source (e.g. who calls a function).
    In,
}

/// One node reached by a traversal, with the edge that reached it.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    /// The node reached.
    pub node: NodeId,
    /// Its qualified name.
    pub qname: String,
    /// Its kind.
    pub kind: NodeKind,
    /// File that defines it.
    pub file: String,
    /// First line of its definition.
    pub line: u32,
    /// Hops from the start of the traversal (1 = direct).
    pub depth: u32,
    /// Kind of the edge that reached it.
    pub via: EdgeKind,
    /// Confidence of that edge.
    pub tier: Tier,
    /// Where the edge was observed (e.g. the call site).
    pub at: String,
    /// The node this one was reached from: the start of the traversal for
    /// depth 1, otherwise a node at the previous depth.
    pub from: NodeId,
}

/// `hits` from one traversal in depth-first tree order: each hit directly
/// after the hit it was reached from, so indenting by depth draws the real
/// chain. Breadth-first order would place a depth-3 hit after unrelated
/// depth-2 hits.
pub fn tree_order(hits: &[Hit]) -> Vec<&Hit> {
    let mut children: HashMap<NodeId, Vec<&Hit>> = HashMap::new();
    let reached: HashSet<NodeId> = hits.iter().map(|h| h.node).collect();
    let mut roots = Vec::new();
    for h in hits {
        if h.depth == 1 || !reached.contains(&h.from) {
            roots.push(h);
        } else {
            children.entry(h.from).or_default().push(h);
        }
    }
    let mut out = Vec::with_capacity(hits.len());
    let mut stack: Vec<&Hit> = roots.into_iter().rev().collect();
    while let Some(h) = stack.pop() {
        out.push(h);
        if let Some(kids) = children.get(&h.node) {
            stack.extend(kids.iter().rev());
        }
    }
    out
}

impl Graph {
    /// Build a graph from nodes and edges and index it for queries.
    pub fn new(
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        files: BTreeMap<String, FileInfo>,
        stats: ResolutionStats,
    ) -> Self {
        let mut g = Graph {
            schema_version: SCHEMA_VERSION,
            nodes,
            edges,
            files,
            stats,
            out_edges: Vec::new(),
            in_edges: Vec::new(),
            by_qname: HashMap::new(),
            by_name: HashMap::new(),
        };
        g.reindex();
        g
    }

    fn reindex(&mut self) {
        let n = self.nodes.len();
        self.out_edges = vec![Vec::new(); n];
        self.in_edges = vec![Vec::new(); n];
        for (i, e) in self.edges.iter().enumerate() {
            self.out_edges[e.src as usize].push(i);
            self.in_edges[e.dst as usize].push(i);
        }
        self.by_qname.clear();
        self.by_name.clear();
        for node in &self.nodes {
            self.by_qname
                .entry(node.qname.clone())
                .or_default()
                .push(node.id);
            self.by_name
                .entry(node.name.clone())
                .or_default()
                .push(node.id);
        }
    }

    /// The node with id `id`. Panics if `id` is not from this graph.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Write the graph as JSON (an export; the index itself lives in SQLite).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read a graph written by [`Graph::save`].
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| {
            format!(
                "no index at {}; run `strata-graph index` first",
                path.display()
            )
        })?;
        let mut g: Graph = serde_json::from_slice(&bytes)?;
        if g.schema_version != SCHEMA_VERSION {
            bail!(
                "index schema {} is out of date; re-run `strata-graph index`",
                g.schema_version
            );
        }
        g.reindex();
        Ok(g)
    }

    /// Symbols matching `query`: exact qualified name, then qualified suffix, then simple name.
    pub fn find(&self, query: &str) -> Vec<NodeId> {
        if let Some(ids) = self.by_qname.get(query) {
            return ids.clone();
        }
        let suffix = format!(".{query}");
        let mut hits: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|n| n.qname.ends_with(&suffix))
            .map(|n| n.id)
            .collect();
        if hits.is_empty() {
            if let Some(ids) = self.by_name.get(query) {
                hits = ids.clone();
            }
        }
        hits.sort_by(|a, b| self.node(*a).qname.cmp(&self.node(*b).qname));
        hits
    }

    /// Exactly one symbol for `query`, or an error listing the candidates.
    pub fn resolve_one(&self, query: &str) -> Result<NodeId> {
        let hits = self.find(query);
        match hits.as_slice() {
            [one] => Ok(*one),
            [] => bail!("no symbol matches `{query}`"),
            many => {
                let list: Vec<_> = many
                    .iter()
                    .take(10)
                    .map(|i| self.node(*i).qname.as_str())
                    .collect();
                bail!(
                    "`{query}` matches {} symbols; use a qualified name, e.g. {}",
                    many.len(),
                    list.join(", ")
                )
            }
        }
    }

    /// Breadth-first walk from `starts` along `kinds` in `dir`, up to `depth` hops.
    pub fn walk(
        &self,
        starts: &[NodeId],
        dir: Direction,
        kinds: &[EdgeKind],
        depth: u32,
        min_tier: Tier,
    ) -> Vec<Hit> {
        let mut seen: HashSet<NodeId> = starts.iter().copied().collect();
        let mut queue: VecDeque<(NodeId, u32)> = starts.iter().map(|s| (*s, 0)).collect();
        let mut hits = Vec::new();
        while let Some((cur, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }
            let list = match dir {
                Direction::Out => &self.out_edges[cur as usize],
                Direction::In => &self.in_edges[cur as usize],
            };
            for &ei in list {
                let e = &self.edges[ei];
                if !kinds.contains(&e.kind) || e.tier < min_tier {
                    continue;
                }
                let next = if dir == Direction::Out { e.dst } else { e.src };
                if seen.insert(next) {
                    let n = self.node(next);
                    hits.push(Hit {
                        node: next,
                        qname: n.qname.clone(),
                        kind: n.kind,
                        file: n.file.clone(),
                        line: n.start_line,
                        depth: d + 1,
                        via: e.kind,
                        tier: e.tier,
                        at: format!("{}:{}", e.file, e.line),
                        from: cur,
                    });
                    queue.push_back((next, d + 1));
                }
            }
        }
        hits
    }

    /// Who calls `id`, up to `depth` hops back, at `min_tier` or better.
    pub fn callers(&self, id: NodeId, depth: u32, min_tier: Tier) -> Vec<Hit> {
        self.walk(&[id], Direction::In, &[EdgeKind::Calls], depth, min_tier)
    }

    /// What `id` calls, up to `depth` hops forward, at `min_tier` or better.
    pub fn callees(&self, id: NodeId, depth: u32, min_tier: Tier) -> Vec<Hit> {
        self.walk(&[id], Direction::Out, &[EdgeKind::Calls], depth, min_tier)
    }

    /// Everything that may break if `id` changes: transitive callers and
    /// subclasses. For a class, its methods' callers are included too.
    pub fn impact(&self, id: NodeId, depth: u32, min_tier: Tier) -> Vec<Hit> {
        let mut starts = vec![id];
        if self.node(id).kind.is_classlike() {
            starts.extend(
                self.out_edges[id as usize]
                    .iter()
                    .map(|&ei| &self.edges[ei])
                    .filter(|e| e.kind == EdgeKind::Contains)
                    .map(|e| e.dst),
            );
        }
        self.walk(
            &starts,
            Direction::In,
            &[EdgeKind::Calls, EdgeKind::Inherits],
            depth,
            min_tier,
        )
    }

    /// Shortest call path from `from` to `to`, if one exists within `max_depth`.
    pub fn path(
        &self,
        from: NodeId,
        to: NodeId,
        max_depth: u32,
        min_tier: Tier,
    ) -> Option<Vec<NodeId>> {
        let mut prev: HashMap<NodeId, NodeId> = HashMap::new();
        let mut queue = VecDeque::from([(from, 0u32)]);
        let mut seen = HashSet::from([from]);
        while let Some((cur, d)) = queue.pop_front() {
            if cur == to {
                let mut path = vec![to];
                let mut c = to;
                while let Some(&p) = prev.get(&c) {
                    path.push(p);
                    c = p;
                }
                path.reverse();
                return Some(path);
            }
            if d >= max_depth {
                continue;
            }
            for &ei in &self.out_edges[cur as usize] {
                let e = &self.edges[ei];
                if e.kind == EdgeKind::Calls && e.tier >= min_tier && seen.insert(e.dst) {
                    prev.insert(e.dst, cur);
                    queue.push_back((e.dst, d + 1));
                }
            }
        }
        None
    }

    /// Node and edge counts by kind and tier.
    pub fn summary(&self) -> serde_json::Value {
        let mut nodes: BTreeMap<String, u64> = BTreeMap::new();
        for n in &self.nodes {
            *nodes
                .entry(format!("{:?}", n.kind).to_lowercase())
                .or_default() += 1;
        }
        let mut edges: BTreeMap<String, BTreeMap<&str, u64>> = BTreeMap::new();
        for e in &self.edges {
            *edges
                .entry(format!("{:?}", e.kind).to_uppercase())
                .or_default()
                .entry(e.tier.label())
                .or_default() += 1;
        }
        serde_json::json!({
            "files": self.files.len(),
            "nodes": nodes,
            "edges": edges,
            "calls": self.stats,
        })
    }
}
