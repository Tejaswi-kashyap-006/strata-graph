//! In-band annotations: short graph facts about code an agent has just read
//! or searched for, meant to be appended to that tool result.
//!
//! Each fact is one self-contained line, so a caller can drop lines it has
//! already shown in a session and still have well-formed output.

use std::str::FromStr;

use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::graph::{Graph, Hit};
use crate::model::{EdgeKind, NodeId, NodeKind, Tier};
use crate::Direction;

/// Most related symbols named per relation before "+N more".
const LIST_MAX: usize = 4;
/// Most callees named per definition at [`Density::Capped`].
const CAPPED_CALLEES: usize = 2;
/// Test callers named when nothing else uses a definition.
const TESTS_NAMED_WHEN_ONLY_TESTS: usize = 2;
/// Most definitions described for one searched name.
const NAME_DEFS_MAX: usize = 5;
/// A definition with at least this many callers is always a hub...
const HUB_MIN_CALLERS: usize = 20;
/// ...and so is one in the top `1 / HUB_PERCENTILE` by caller count.
const HUB_PERCENTILE: usize = 100;
/// Average out-degree below which links go both ways (CodeAnchor heuristic).
const BIDIRECTIONAL_BELOW: f64 = 5.0;
/// Average out-degree above which only incoming links are shown.
const INVERSE_ABOVE: f64 = 8.0;
/// Words that appear in search patterns but are never the symbol sought.
const KEYWORDS: &[&str] = &[
    "def",
    "class",
    "async",
    "await",
    "return",
    "import",
    "from",
    "self",
    "cls",
    "function",
    "const",
    "let",
    "var",
    "export",
    "default",
    "new",
    "this",
    "super",
    "interface",
    "type",
    "extends",
    "implements",
    "public",
    "private",
    "protected",
    "static",
    "readonly",
    "None",
    "null",
    "true",
    "false",
    "True",
    "False",
    "lambda",
    "yield",
    "with",
    "for",
    "while",
];

/// One fact line plus how informative it is, for trimming to a budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// Rendered line, without a trailing newline.
    pub text: String,
    /// Number of related symbols mentioned; higher facts survive trimming.
    pub weight: usize,
    /// Source line the fact is about, for ordering.
    pub line: u32,
}

/// How much to say about each definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    /// Chosen from the repo's shape: see [`RepoShape::auto_density`].
    Auto,
    /// Callers and callees, bases and subtypes.
    Full,
    /// As `Full`, but at most two callees per definition.
    Capped,
    /// Only what depends on a definition: callers and subtypes ("who calls me").
    Inverse,
}

impl Density {
    /// Lower-case name, as accepted by [`str::parse`].
    pub fn label(self) -> &'static str {
        match self {
            Density::Auto => "auto",
            Density::Full => "full",
            Density::Capped => "capped",
            Density::Inverse => "inverse",
        }
    }
}

impl FromStr for Density {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s {
            "auto" => Density::Auto,
            "full" => Density::Full,
            "capped" => Density::Capped,
            "inverse" => Density::Inverse,
            other => bail!("unknown density {other:?}; use auto, full, capped or inverse"),
        })
    }
}

/// Call-graph statistics that decide delivery density and hub handling.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepoShape {
    /// Mean number of distinct resolved callees over the functions and
    /// methods that make at least one resolved call.
    pub avg_out_degree: f64,
    /// Callers at or above which a definition counts as a hub.
    pub hub_min_callers: usize,
    /// Number of hubs.
    pub hubs: usize,
    /// Share of resolved call edges that land on a hub.
    pub hub_call_share: f64,
}

impl RepoShape {
    /// The density `Density::Auto` stands for in this repo: both directions
    /// below an average out-degree of 5, callers only above 8, and both with
    /// a callee cap in between.
    pub fn auto_density(&self) -> Density {
        if self.avg_out_degree < BIDIRECTIONAL_BELOW {
            Density::Full
        } else if self.avg_out_degree > INVERSE_ABOVE {
            Density::Inverse
        } else {
            Density::Capped
        }
    }
}

/// Distinct resolved callees and callers of every node at `min_tier`.
fn call_degrees(g: &Graph, min_tier: Tier) -> (Vec<usize>, Vec<usize>) {
    let mut pairs: Vec<(NodeId, NodeId)> = g
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.tier >= min_tier)
        .map(|e| (e.src, e.dst))
        .collect();
    pairs.sort_unstable();
    pairs.dedup();
    let mut out = vec![0; g.nodes.len()];
    let mut inn = vec![0; g.nodes.len()];
    for (s, d) in pairs {
        out[s as usize] += 1;
        inn[d as usize] += 1;
    }
    (out, inn)
}

/// Produces the facts for one graph at one density. Build it once per
/// request; it precomputes the repo's shape and hub set.
pub struct Annotator<'g> {
    g: &'g Graph,
    min_tier: Tier,
    density: Density,
    shape: RepoShape,
    hub: Vec<bool>,
}

impl<'g> Annotator<'g> {
    /// Facts at `min_tier` or better. `Density::Auto` is resolved from the
    /// repo's shape here.
    pub fn new(g: &'g Graph, min_tier: Tier, density: Density) -> Self {
        let (out, inn) = call_degrees(g, min_tier);
        let calling: Vec<usize> = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
            .map(|n| out[n.id as usize])
            .filter(|&d| d > 0)
            .collect();
        let avg_out_degree = if calling.is_empty() {
            0.0
        } else {
            calling.iter().sum::<usize>() as f64 / calling.len() as f64
        };
        let mut called: Vec<usize> = inn.iter().copied().filter(|&d| d > 0).collect();
        called.sort_unstable_by(|a, b| b.cmp(a));
        let percentile = called
            .get(called.len() / HUB_PERCENTILE)
            .copied()
            .unwrap_or(0);
        let hub_min_callers = HUB_MIN_CALLERS.max(percentile);
        let hub: Vec<bool> = inn.iter().map(|&d| d >= hub_min_callers).collect();
        let total_calls: usize = inn.iter().sum();
        let hub_calls: usize = inn.iter().filter(|&&d| d >= hub_min_callers).sum();
        let shape = RepoShape {
            avg_out_degree,
            hub_min_callers,
            hubs: hub.iter().filter(|&&h| h).count(),
            hub_call_share: if total_calls == 0 {
                0.0
            } else {
                hub_calls as f64 / total_calls as f64
            },
        };
        let density = match density {
            Density::Auto => shape.auto_density(),
            d => d,
        };
        Annotator {
            g,
            min_tier,
            density,
            shape,
            hub,
        }
    }

    /// The density in effect (never `Auto`).
    pub fn density(&self) -> Density {
        self.density
    }

    /// The repo's call-graph shape.
    pub fn shape(&self) -> &RepoShape {
        &self.shape
    }

    /// Whether `id` is called so widely that naming it as a callee is noise.
    pub fn is_hub(&self, id: NodeId) -> bool {
        self.hub[id as usize]
    }

    /// Callers, callees, bases and subtypes of one node as clauses, with
    /// the number of related symbols they name.
    fn relations(&self, id: NodeId, module: &str, sites: bool) -> (Vec<String>, usize) {
        let g = self.g;
        let kind = g.node(id).kind;
        let mut clauses = Vec::new();
        let mut weight = 0;
        let mut push = |label: &str, hits: &[Hit], with_site: bool| {
            if let Some((text, named)) = dependents(label, hits, module, with_site) {
                clauses.push(text);
                weight += named;
            }
        };
        if kind.is_classlike() {
            let inherit = |dir| g.walk(&[id], dir, &[EdgeKind::Inherits], 1, self.min_tier);
            if self.density != Density::Inverse {
                let label = if kind == NodeKind::Interface {
                    "extends"
                } else {
                    "bases"
                };
                push(label, &inherit(Direction::Out), false);
            }
            let label = if kind == NodeKind::Interface {
                "implemented by"
            } else {
                "subclasses"
            };
            push(label, &inherit(Direction::In), false);
        }
        let label = if kind == NodeKind::Class {
            "constructed by"
        } else {
            "called by"
        };
        push(label, &g.callers(id, 1, self.min_tier), sites);
        if self.density != Density::Inverse {
            // Widely used helpers are counted, not named: they say little
            // about what this definition does and crowd out what does.
            let callees = g.callees(id, 1, self.min_tier);
            if !callees.is_empty() {
                let (helpers, rest): (Vec<&Hit>, Vec<&Hit>) =
                    callees.iter().partition(|h| self.is_hub(h.node));
                let cap = if self.density == Density::Capped {
                    CAPPED_CALLEES
                } else {
                    LIST_MAX
                };
                let mut parts: Vec<String> = rest
                    .iter()
                    .take(cap)
                    .map(|h| hit_name(h, module, false))
                    .collect();
                weight += parts.len();
                if rest.len() > cap {
                    parts.push(format!("+{} more", rest.len() - cap));
                }
                match helpers.len() {
                    0 => {}
                    1 => parts.push("1 widely used helper".to_string()),
                    n => parts.push(format!("{n} widely used helpers")),
                }
                clauses.push(format!("calls {}: {}", callees.len(), parts.join(", ")));
            }
        }
        (clauses, weight)
    }

    /// Facts about every definition in `file` that overlaps lines
    /// `start..=end` (1-based), including enclosing classes and functions.
    /// Functions and methods with no resolved relations are grouped into one
    /// trailing line; classes and interfaces with none are left out, since
    /// many are used only as types.
    pub fn range_facts(&self, file: &str, start: u32, end: u32) -> Vec<Fact> {
        let g = self.g;
        let Some(info) = g.files.get(file) else {
            return Vec::new();
        };
        let module = info.module.as_str();
        let mut facts = Vec::new();
        let mut quiet = Vec::new();
        for n in g
            .nodes
            .iter()
            .filter(|n| n.file == file && n.kind != NodeKind::Module)
        {
            if n.end_line < start || n.start_line > end {
                continue;
            }
            let name = short(&n.qname, module);
            let (clauses, weight) = self.relations(n.id, module, false);
            if clauses.is_empty() {
                if !n.kind.is_classlike() {
                    quiet.push(name.to_string());
                }
                continue;
            }
            facts.push(Fact {
                text: format!(
                    "L{}-{} {name} ({}): {}",
                    n.start_line,
                    n.end_line,
                    kind_label(n.kind),
                    clauses.join("; ")
                ),
                weight,
                line: n.start_line,
            });
        }
        if !quiet.is_empty() {
            let what = if self.density == Density::Inverse {
                "no resolved callers"
            } else {
                "no resolved callers or callees"
            };
            facts.push(Fact {
                text: format!("{what}: {}", quiet.join(", ")),
                weight: 0,
                line: u32::MAX,
            });
        }
        facts
    }

    /// Facts about the definitions named exactly `name`: where each is
    /// defined and its call sites, so search hits can be told apart from
    /// real callers.
    pub fn name_facts(&self, name: &str) -> Vec<Fact> {
        let defs: Vec<&crate::Node> = self
            .g
            .nodes
            .iter()
            .filter(|n| n.name == name && n.kind != NodeKind::Module)
            .collect();
        if defs.is_empty() {
            return Vec::new();
        }
        if defs.len() > NAME_DEFS_MAX {
            return vec![Fact {
                text: format!(
                    "{name}: {} definitions in this repo; ask for one by qualified name",
                    defs.len()
                ),
                weight: defs.len(),
                line: 0,
            }];
        }
        defs.iter()
            .map(|n| {
                let (clauses, weight) = self.relations(n.id, "", true);
                let rel = if clauses.is_empty() {
                    "no resolved callers or callees".to_string()
                } else {
                    clauses.join("; ")
                };
                Fact {
                    text: format!(
                        "{} ({}, defined {}:{}): {rel}",
                        n.qname,
                        kind_label(n.kind),
                        n.file,
                        n.start_line
                    ),
                    weight: weight + 1,
                    line: n.start_line,
                }
            })
            .collect()
    }

    /// Rendered facts for lines `start..=end` of `file` (`u32::MAX` for the
    /// end of the file) in at most `max_chars`, or `None` if there is
    /// nothing to say. This is what agent hooks deliver.
    pub fn annotate(&self, file: &str, start: u32, end: u32, max_chars: usize) -> Option<String> {
        let facts = self.range_facts(file, start, end);
        if facts.is_empty() {
            return None;
        }
        let lines = match (start <= 1, end == u32::MAX) {
            (true, true) => String::new(),
            (false, true) => format!(":{start}-end"),
            _ => format!(":{start}-{end}"),
        };
        Some(render(
            &format!("[strata-graph] {file}{lines}"),
            &facts,
            max_chars,
        ))
    }
}

fn short<'a>(qname: &'a str, module: &str) -> &'a str {
    qname
        .strip_prefix(module)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(qname)
}

fn kind_label(kind: NodeKind) -> String {
    format!("{kind:?}").to_lowercase()
}

/// One related symbol: "name T1", or "name (file:line) T1" with its site.
fn hit_name(h: &Hit, module: &str, with_site: bool) -> String {
    let name = short(&h.qname, module);
    if with_site {
        format!("{name} ({}) {}", h.at, h.tier.label())
    } else {
        format!("{name} {}", h.tier.label())
    }
}

/// A clause about what depends on a definition (callers, subtypes). Code
/// outside tests is named first; test code is counted, and named only when
/// nothing else depends on the definition. Returns the text and the number
/// of symbols named.
fn dependents(label: &str, hits: &[Hit], module: &str, with_site: bool) -> Option<(String, usize)> {
    if hits.is_empty() {
        return None;
    }
    let (code, tests): (Vec<&Hit>, Vec<&Hit>) = hits.iter().partition(|h| !is_test_path(&h.file));
    let (shown, cap, label) = if code.is_empty() {
        (
            tests.as_slice(),
            TESTS_NAMED_WHEN_ONLY_TESTS,
            format!("{label} {}, all in tests", hits.len()),
        )
    } else {
        (code.as_slice(), LIST_MAX, format!("{label} {}", hits.len()))
    };
    let mut parts: Vec<String> = shown
        .iter()
        .take(cap)
        .map(|h| hit_name(h, module, with_site))
        .collect();
    let named = parts.len();
    if shown.len() > cap {
        parts.push(format!("+{} more", shown.len() - cap));
    }
    if !code.is_empty() && !tests.is_empty() {
        parts.push(format!("{} in tests", tests.len()));
    }
    Some((format!("{label}: {}", parts.join(", ")), named))
}

/// Whether `path` (repo-relative, `/`-separated) is test code by the usual
/// conventions: a `tests`/`test`/`__tests__` directory, `test_*.py`,
/// `*_test.py`, `conftest.py`, or `*.test.*` / `*.spec.*` files.
pub fn is_test_path(path: &str) -> bool {
    let mut parts = path.split('/');
    let file = parts.next_back().unwrap_or_default();
    if parts.any(|d| matches!(d, "tests" | "test" | "__tests__")) {
        return true;
    }
    let stem = file.split('.').next().unwrap_or_default();
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem == "conftest"
        || file
            .split('.')
            .skip(1)
            .any(|part| part == "test" || part == "spec")
}

/// Identifiers in a search pattern that may name a symbol: regex escapes
/// such as `\b` and `\s` are dropped, keywords and short words skipped.
pub fn pattern_names(pattern: &str) -> Vec<String> {
    let mut cleaned = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // `\b`, `\w+`, `\(`: the escaped character is never part of a name.
            chars.next();
            cleaned.push(' ');
        } else {
            cleaned.push(c);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for word in cleaned.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')) {
        let word = word.trim_matches('$');
        if word.len() < 3
            || word.chars().all(|c| c.is_ascii_digit())
            || KEYWORDS.contains(&word)
            || out.iter().any(|w| w == word)
        {
            continue;
        }
        out.push(word.to_string());
    }
    out
}

/// Join `facts` under `header` in at most `max_chars` characters. When they
/// do not all fit, the most informative are kept, in source order, and the
/// number left out is stated.
pub fn render(header: &str, facts: &[Fact], max_chars: usize) -> String {
    let mut order: Vec<usize> = (0..facts.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(facts[i].weight), facts[i].line));
    let mut budget = max_chars.saturating_sub(header.len() + 1);
    let reserve = 32; // room for the "... N more" line
    let mut keep = Vec::new();
    for i in order {
        let cost = facts[i].text.len() + 1;
        if cost + reserve <= budget {
            budget -= cost;
            keep.push(i);
        }
    }
    keep.sort_by_key(|&i| (facts[i].line, i));
    let mut out = format!("{header}\n");
    for &i in &keep {
        out.push_str(&facts[i].text);
        out.push('\n');
    }
    let dropped = facts.len() - keep.len();
    if dropped > 0 {
        out.push_str(&format!("... {dropped} more not shown\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> Graph {
        crate::index_repo(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn texts(facts: &[Fact]) -> Vec<&str> {
        facts.iter().map(|f| f.text.as_str()).collect()
    }

    fn find<'a>(facts: &'a [Fact], needle: &str) -> &'a str {
        facts
            .iter()
            .map(|f| f.text.as_str())
            .find(|t| t.contains(needle))
            .unwrap_or_else(|| panic!("no fact with {needle:?} in {:#?}", texts(facts)))
    }

    /// A repo where `helper` is called by 25 functions: a hub.
    fn hub_repo() -> (PathBuf, Graph) {
        let dir = std::env::temp_dir().join(format!("strata-annotate-hub-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut src =
            String::from("def helper():\n    return 1\n\n\ndef work():\n    return 2\n\n\n");
        for i in 0..25 {
            src.push_str(&format!("def user{i}():\n    return helper()\n\n\n"));
        }
        src.push_str("def main():\n    helper()\n    return work()\n");
        fs::write(dir.join("app.py"), src).unwrap();
        let g = crate::index_repo(&dir).unwrap();
        (dir, g)
    }

    #[test]
    fn range_facts_cover_the_lines_read() {
        let g = fixture("py_shop");
        let a = Annotator::new(&g, Tier::NameOnly, Density::Full);
        let facts = a.range_facts("shop/billing/tax.py", 1, 200);
        let compute = find(&facts, "TaxCalculator.compute (method)");
        assert!(
            compute.contains("called by 2: shop.api.quote T1"),
            "{compute}"
        );
        assert!(compute.contains("calls 1: round_money T1"), "{compute}");
        // A class's callers construct it.
        let class = find(&facts, "TaxCalculator (class)");
        assert!(class.contains("constructed by 1:"), "{class}");
        // A window that misses a definition leaves it out.
        assert!(a.range_facts("shop/billing/tax.py", 9000, 9001).is_empty());
        assert!(a.range_facts("not/indexed.py", 1, 10).is_empty());

        // The "no resolved callers" line never names a class or interface.
        for file in g.files.keys() {
            for f in a.range_facts(file, 1, u32::MAX) {
                let Some(names) = f.text.strip_prefix("no resolved callers or callees: ") else {
                    continue;
                };
                for name in names.split(", ") {
                    let n = g
                        .nodes
                        .iter()
                        .find(|n| &n.file == file && n.qname.ends_with(name))
                        .unwrap();
                    assert!(!n.kind.is_classlike(), "{file}: {name}");
                }
            }
        }
    }

    #[test]
    fn class_and_interface_facts_name_their_relatives() {
        let g = fixture("py_shop");
        let a = Annotator::new(&g, Tier::NameOnly, Density::Full);
        let base = g.node(g.resolve_one("shop.core.base.Model").unwrap());
        let facts = a.range_facts(&base.file, base.start_line, base.start_line);
        let model = find(&facts, "Model (class)");
        assert!(model.contains("subclasses 2:"), "{model}");
        assert!(model.contains("shop.extras.audit.AuditedModel"), "{model}");

        let ts = fixture("ts_shop");
        let a = Annotator::new(&ts, Tier::NameOnly, Density::Full);
        let facts = a.range_facts("src/core/base.ts", 1, u32::MAX);
        let repo = find(&facts, "Repo (interface)");
        assert!(
            repo.contains("implemented by 1: src/core/memory_repo.MemoryRepo T1"),
            "{repo}"
        );
        let facts = a.range_facts("src/core/memory_repo.ts", 1, u32::MAX);
        let mem = find(&facts, "MemoryRepo (class)");
        assert!(mem.contains("bases 1: src/core/base.Repo T1"), "{mem}");
    }

    #[test]
    fn hubs_are_counted_not_named() {
        let (dir, g) = hub_repo();
        let a = Annotator::new(&g, Tier::NameOnly, Density::Full);
        let helper = g.resolve_one("app.helper").unwrap();
        assert!(a.is_hub(helper));
        assert_eq!(a.shape().hubs, 1);
        // With so few called functions the top-1% cut is the most-called one
        // (26 callers), above the floor of HUB_MIN_CALLERS.
        assert_eq!(a.shape().hub_min_callers, 26);
        let facts = a.range_facts("app.py", 1, u32::MAX);
        let main = find(&facts, "main (function)");
        assert!(
            main.contains("calls 2: work T1, 1 widely used helper"),
            "{main}"
        );
        // The hub itself still lists who calls it.
        let h = find(&facts, "helper (function)");
        assert!(h.contains("called by 26: "), "{h}");
        assert!(h.contains("+22 more"), "{h}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn density_controls_what_is_said() {
        let g = fixture("py_shop");
        let file = "shop/billing/tax.py";
        let full = Annotator::new(&g, Tier::NameOnly, Density::Full);
        let inverse = Annotator::new(&g, Tier::NameOnly, Density::Inverse);
        let compute =
            |a: &Annotator| find(&a.range_facts(file, 1, 200), "TaxCalculator.compute").to_string();
        assert!(compute(&full).contains("calls 1:"));
        let inv = compute(&inverse);
        assert!(
            inv.contains("called by 2:") && !inv.contains("calls"),
            "{inv}"
        );

        // Capped names at most two callees.
        let (dir, hub) = hub_repo();
        let capped = Annotator::new(&hub, Tier::NameOnly, Density::Capped);
        assert_eq!(capped.density(), Density::Capped);

        // Auto applies the out-degree thresholds to the repo's shape.
        let auto = Annotator::new(&g, Tier::NameOnly, Density::Auto);
        assert!(auto.shape().avg_out_degree < BIDIRECTIONAL_BELOW);
        assert_eq!(auto.density(), Density::Full);
        let shape = |d| RepoShape {
            avg_out_degree: d,
            hub_min_callers: 20,
            hubs: 0,
            hub_call_share: 0.0,
        };
        assert_eq!(shape(4.9).auto_density(), Density::Full);
        assert_eq!(shape(6.0).auto_density(), Density::Capped);
        assert_eq!(shape(8.1).auto_density(), Density::Inverse);
        assert_eq!("inverse".parse::<Density>().unwrap(), Density::Inverse);
        assert!("dense".parse::<Density>().is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capped_density_names_at_most_two_callees() {
        let dir = std::env::temp_dir().join(format!("strata-annotate-cap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("m.py"),
            "def a():\n    pass\n\ndef b():\n    pass\n\ndef c():\n    pass\n\ndef d():\n    pass\n\n\
             def top():\n    a()\n    b()\n    c()\n    d()\n",
        )
        .unwrap();
        let g = crate::index_repo(&dir).unwrap();
        let facts =
            Annotator::new(&g, Tier::NameOnly, Density::Capped).range_facts("m.py", 1, u32::MAX);
        let top = find(&facts, "top (function)");
        assert_eq!(top.matches(" T1").count(), 2, "{top}");
        assert!(
            top.contains("calls 4: ") && top.contains("+2 more"),
            "{top}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_paths_follow_common_conventions() {
        for p in [
            "tests/test_api.py",
            "pkg/tests/helpers.py",
            "shop/test_tax.py",
            "shop/tax_test.py",
            "conftest.py",
            "src/a.test.ts",
            "src/b.spec.tsx",
            "src/__tests__/c.ts",
            "adapters/bun/test/index.ts",
        ] {
            assert!(is_test_path(p), "{p}");
        }
        for p in [
            "shop/tax.py",
            "src/helper/testing/index.ts",
            "src/contest.ts",
            "attestation.py",
            "src/latest.ts",
        ] {
            assert!(!is_test_path(p), "{p}");
        }
    }

    #[test]
    fn test_callers_are_counted_after_real_ones() {
        let dir =
            std::env::temp_dir().join(format!("strata-annotate-tests-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("app.py"), "def used():\n    return 1\n\n\ndef only_tested():\n    return 2\n\n\ndef caller():\n    return used()\n").unwrap();
        let mut t = String::from("from app import used, only_tested\n\n\n");
        for i in 0..5 {
            t.push_str(&format!(
                "def test_{i}():\n    used()\n    only_tested()\n\n\n"
            ));
        }
        fs::write(dir.join("tests/test_app.py"), t).unwrap();
        let g = crate::index_repo(&dir).unwrap();
        let facts =
            Annotator::new(&g, Tier::NameOnly, Density::Full).range_facts("app.py", 1, u32::MAX);
        let used = find(&facts, "used (function)");
        assert!(
            used.contains("called by 6: caller T1, 5 in tests"),
            "{used}"
        );
        let only = find(&facts, "only_tested (function)");
        assert!(
            only.contains(
                "called by 5, all in tests: test_app.test_0 T1, test_app.test_1 T1, +3 more"
            ),
            "{only}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_patterns_yield_symbol_names() {
        assert_eq!(pattern_names(r"def\s+round_money\("), ["round_money"]);
        assert_eq!(pattern_names(r"\bTaxCalculator\b"), ["TaxCalculator"]);
        assert_eq!(pattern_names("Invoice.total"), ["Invoice", "total"]);
        assert_eq!(pattern_names("export function useThing"), ["useThing"]);
        assert!(pattern_names(r"\d+ id").is_empty());
    }

    #[test]
    fn name_facts_give_definition_and_call_sites() {
        let g = fixture("py_shop");
        let a = Annotator::new(&g, Tier::NameOnly, Density::Auto);
        let facts = a.name_facts("round_money");
        assert_eq!(facts.len(), 1, "{facts:?}");
        let t = &facts[0].text;
        assert!(
            t.starts_with("shop.billing.tax.round_money (function, defined shop/billing/tax.py:"),
            "{t}"
        );
        assert!(t.contains("called by"), "{t}");
        assert!(t.contains("(shop/billing/tax.py:6) T1"), "{t}");
        assert!(a.name_facts("no_such_name").is_empty());
    }

    #[test]
    fn annotate_renders_a_header_and_range() {
        let g = fixture("py_shop");
        let a = Annotator::new(&g, Tier::NameOnly, Density::Auto);
        let whole = a
            .annotate("shop/billing/tax.py", 1, u32::MAX, 2000)
            .unwrap();
        assert!(
            whole.starts_with("[strata-graph] shop/billing/tax.py\n"),
            "{whole}"
        );
        let part = a.annotate("shop/billing/tax.py", 5, 6, 2000).unwrap();
        assert!(
            part.starts_with("[strata-graph] shop/billing/tax.py:5-6\n"),
            "{part}"
        );
        assert!(a
            .annotate("shop/billing/tax.py", 9000, 9001, 2000)
            .is_none());
    }

    #[test]
    fn render_keeps_the_strongest_facts_in_source_order() {
        let f = |text: &str, weight, line| Fact {
            text: text.into(),
            weight,
            line,
        };
        let facts = [
            f("a-line-one", 1, 1),
            f("b-line-two-heavy", 9, 2),
            f("c-line-three", 5, 3),
        ];
        let all = render("H", &facts, 1000);
        assert_eq!(all, "H\na-line-one\nb-line-two-heavy\nc-line-three\n");
        let tight = render("H", &facts, 2 + 17 + 13 + 32);
        assert_eq!(
            tight,
            "H\nb-line-two-heavy\nc-line-three\n... 1 more not shown\n"
        );
    }
}
