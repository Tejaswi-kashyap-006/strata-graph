//! Graph schema: nodes, edges and confidence tiers.

use serde::{Deserialize, Serialize};

/// Index of a node in [`crate::Graph::nodes`]; ids are contiguous from 0.
pub type NodeId = u32;

/// What a node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A source file (Python module or TypeScript/JavaScript file).
    Module,
    /// A class.
    Class,
    /// A TypeScript interface.
    Interface,
    /// A function not defined directly in a class body.
    Function,
    /// A function defined in a class or interface body.
    Method,
}

impl NodeKind {
    /// Classes and interfaces: kinds that have members, bases and
    /// subtypes. Resolution treats both alike.
    pub fn is_classlike(self) -> bool {
        matches!(self, NodeKind::Class | NodeKind::Interface)
    }
}

/// A definition in the repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Position of this node in the graph's node list.
    pub id: NodeId,
    /// What the node represents.
    pub kind: NodeKind,
    /// Qualified name: the module name (dotted for Python, a path for
    /// TypeScript) followed by `.`-separated definition names, e.g.
    /// `billing.invoice.Invoice.total` or `src/billing/invoice.Invoice.total`.
    pub qname: String,
    /// The definition's own name (last segment of `qname`).
    pub name: String,
    /// Path relative to the repo root, `/`-separated.
    pub file: String,
    /// First line of the definition (1-based).
    pub start_line: u32,
    /// Last line of the definition (1-based, inclusive).
    pub end_line: u32,
    /// The enclosing definition or module; `None` for modules.
    pub parent: Option<NodeId>,
}

/// The relationship an edge records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeKind {
    /// `src` defines `dst` (module to class, class to method, ...).
    Contains,
    /// Module `src` imports module `dst`.
    Imports,
    /// `src` calls or instantiates `dst` (JSX component use counts as a call).
    Calls,
    /// Class `src` extends or implements `dst`.
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
    /// The label used in output and storage: `T0-ambiguous`, `T0`, `T1` or `T2`.
    pub fn label(self) -> &'static str {
        match self {
            Tier::Ambiguous => "T0-ambiguous",
            Tier::NameOnly => "T0",
            Tier::Scoped => "T1",
            Tier::Exact => "T2",
        }
    }
}

/// A directed relationship between two nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    /// The caller, importer, container or subclass.
    pub src: NodeId,
    /// The callee, imported module, contained definition or base class.
    pub dst: NodeId,
    /// The relationship.
    pub kind: EdgeKind,
    /// How sure the resolver is.
    pub tier: Tier,
    /// File of the source span that produced the edge (e.g. the call site).
    pub file: String,
    /// 1-based line of that span.
    pub line: u32,
}

/// Outcome counts for call resolution across the repo, reported by `stats`.
/// Every call site lands in exactly one bucket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolutionStats {
    /// Call sites found.
    pub calls_seen: u64,
    /// Resolved through language rules (T1 or better).
    pub resolved_t1: u64,
    /// Resolved by a unique name match (T0).
    pub resolved_t0: u64,
    /// Matched 2 or 3 same-named definitions (T0-ambiguous).
    pub ambiguous: u64,
    /// Calls into the standard library, builtins or third-party packages.
    pub external: u64,
    /// Calls left unresolved rather than guessed.
    pub unresolved: u64,
}
