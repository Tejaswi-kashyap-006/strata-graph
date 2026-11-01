//! Graph schema: nodes, edges and confidence tiers.

use serde::{Deserialize, Serialize};

pub type NodeId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Module,
    Class,
    Function,
    Method,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Dotted qualified name, e.g. `billing.invoice.Invoice.total`.
    pub qname: String,
    pub name: String,
    /// Path relative to the repo root, `/`-separated.
    pub file: String,
    /// 1-based inclusive line range.
    pub start_line: u32,
    pub end_line: u32,
    pub parent: Option<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeKind {
    Contains,
    Imports,
    Calls,
    Inherits,
}

/// How sure the resolver is about an edge. Ordered from weakest to strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    /// Several definitions share the name; this edge is one of them.
    #[serde(rename = "T0-ambiguous")]
    Ambiguous,
    /// Matched by simple name only; a single candidate exists in the repo.
    #[serde(rename = "T0")]
    NameOnly,
    /// Resolved through scopes, imports, `self`, bases or a type hint.
    #[serde(rename = "T1")]
    Scoped,
    /// Structural fact (containment) or confirmed by a compiler index.
    #[serde(rename = "T2")]
    Exact,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Ambiguous => "T0-ambiguous",
            Tier::NameOnly => "T0",
            Tier::Scoped => "T1",
            Tier::Exact => "T2",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub src: NodeId,
    pub dst: NodeId,
    pub kind: EdgeKind,
    pub tier: Tier,
    /// File and 1-based line of the source span that produced the edge.
    pub file: String,
    pub line: u32,
}

/// Outcome counts for call resolution, reported by `stats`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolutionStats {
    pub calls_seen: u64,
    pub resolved_t1: u64,
    pub resolved_t0: u64,
    pub ambiguous: u64,
    pub external: u64,
    pub unresolved: u64,
}
