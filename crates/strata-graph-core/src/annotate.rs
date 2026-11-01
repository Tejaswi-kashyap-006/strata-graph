//! In-band annotations: short graph facts about code an agent has just read
//! or searched for, meant to be appended to that tool result.
//!
//! Each fact is one self-contained line, so a caller can drop lines it has
//! already shown in a session and still have well-formed output.

use crate::graph::{Graph, Hit};
use crate::model::{EdgeKind, NodeId, NodeKind, Tier};
use crate::Direction;

/// Most related symbols named per relation before "+N more".
const LIST_MAX: usize = 4;
/// Most definitions described for one searched name.
const NAME_DEFS_MAX: usize = 5;

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

fn short<'a>(qname: &'a str, module: &str) -> &'a str {
    qname
        .strip_prefix(module)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(qname)
}

fn kind_label(kind: NodeKind) -> String {
    format!("{kind:?}").to_lowercase()
}

/// "a T1, b T0 +3 more" for up to [`LIST_MAX`] hits.
fn name_list(hits: &[Hit], module: &str, with_site: bool) -> String {
    let mut parts: Vec<String> = hits
        .iter()
        .take(LIST_MAX)
        .map(|h| {
            let name = short(&h.qname, module);
            if with_site {
                format!("{name} ({}) {}", h.at, h.tier.label())
            } else {
                format!("{name} {}", h.tier.label())
            }
        })
        .collect();
    if hits.len() > LIST_MAX {
        parts.push(format!("+{} more", hits.len() - LIST_MAX));
    }
    parts.join(", ")
}

/// Callers, callees, bases and subclasses of one node as "; "-joined clauses.
fn relations(
    g: &Graph,
    id: NodeId,
    module: &str,
    min_tier: Tier,
    sites: bool,
) -> (Vec<String>, usize) {
    let mut clauses = Vec::new();
    let mut weight = 0;
    let mut add = |label: &str, hits: Vec<Hit>, with_site: bool| {
        if !hits.is_empty() {
            weight += hits.len();
            clauses.push(format!(
                "{label} {}: {}",
                hits.len(),
                name_list(&hits, module, with_site)
            ));
        }
    };
    if g.node(id).kind == NodeKind::Class {
        add(
            "extends",
            g.walk(&[id], Direction::Out, &[EdgeKind::Inherits], 1, min_tier),
            false,
        );
        add(
            "subclasses",
            g.walk(&[id], Direction::In, &[EdgeKind::Inherits], 1, min_tier),
            false,
        );
    }
    add("called by", g.callers(id, 1, min_tier), sites);
    add("calls", g.callees(id, 1, min_tier), false);
    (clauses, weight)
}

/// Facts about every definition in `file` that overlaps lines `start..=end`
/// (1-based), including enclosing classes and functions. Functions and
/// methods with no resolved relations are grouped into one trailing line;
/// classes and interfaces with none are left out, since many are used only
/// as types.
pub fn range_facts(g: &Graph, file: &str, start: u32, end: u32, min_tier: Tier) -> Vec<Fact> {
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
        let (clauses, weight) = relations(g, n.id, module, min_tier, false);
        if clauses.is_empty() {
            if n.kind != NodeKind::Class {
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
        facts.push(Fact {
            text: format!("no resolved callers or callees: {}", quiet.join(", ")),
            weight: 0,
            line: u32::MAX,
        });
    }
    facts
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

/// Facts about the definitions named exactly `name`: where each is defined
/// and its call sites, so search hits can be told apart from real callers.
pub fn name_facts(g: &Graph, name: &str, min_tier: Tier) -> Vec<Fact> {
    let defs: Vec<&crate::Node> = g
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
            let (clauses, weight) = relations(g, n.id, "", min_tier, true);
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
    use std::path::Path;

    fn shop() -> Graph {
        crate::index_repo(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py_shop"))
            .unwrap()
    }

    #[test]
    fn range_facts_cover_the_lines_read() {
        let g = shop();
        let facts = range_facts(&g, "shop/billing/tax.py", 1, 200, Tier::NameOnly);
        let text: Vec<&str> = facts.iter().map(|f| f.text.as_str()).collect();
        let compute = text
            .iter()
            .find(|t| t.contains("TaxCalculator.compute (method)"))
            .unwrap_or_else(|| panic!("{text:#?}"));
        assert!(
            compute.contains("called by 2: shop.api.quote T1"),
            "{compute}"
        );
        assert!(compute.contains("calls 1: round_money T1"), "{compute}");
        // A window that misses a definition leaves it out.
        let none = range_facts(&g, "shop/billing/tax.py", 9000, 9001, Tier::NameOnly);
        assert!(none.is_empty(), "{none:?}");
        assert!(range_facts(&g, "not/indexed.py", 1, 10, Tier::NameOnly).is_empty());

        // The "no resolved callers" line never names a class or interface.
        for file in g.files.keys() {
            for f in range_facts(&g, file, 1, u32::MAX, Tier::NameOnly) {
                let Some(names) = f.text.strip_prefix("no resolved callers or callees: ") else {
                    continue;
                };
                for name in names.split(", ") {
                    let n = g
                        .nodes
                        .iter()
                        .find(|n| &n.file == file && n.qname.ends_with(name))
                        .unwrap();
                    assert_ne!(n.kind, NodeKind::Class, "{file}: {name}");
                }
            }
        }
    }

    #[test]
    fn class_facts_name_bases_and_subclasses() {
        let g = shop();
        let base = g.node(g.resolve_one("shop.core.base.Model").unwrap());
        let facts = range_facts(
            &g,
            &base.file,
            base.start_line,
            base.start_line,
            Tier::NameOnly,
        );
        let model = facts
            .iter()
            .find(|f| f.text.contains("Model (class)"))
            .unwrap();
        assert!(model.text.contains("subclasses 2:"), "{}", model.text);
        assert!(
            model.text.contains("shop.extras.audit.AuditedModel"),
            "{}",
            model.text
        );
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
        let g = shop();
        let facts = name_facts(&g, "round_money", Tier::NameOnly);
        assert_eq!(facts.len(), 1, "{facts:?}");
        let t = &facts[0].text;
        assert!(
            t.starts_with("shop.billing.tax.round_money (function, defined shop/billing/tax.py:"),
            "{t}"
        );
        assert!(t.contains("called by"), "{t}");
        assert!(t.contains("(shop/billing/tax.py:6) T1"), "{t}");
        assert!(name_facts(&g, "no_such_name", Tier::NameOnly).is_empty());
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
