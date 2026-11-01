//! Context packs: one symbol's source, place in the class hierarchy, callers
//! and callees, rendered as compact text trimmed to a token budget. This is
//! what an agent reads instead of opening several files.

use std::path::Path;

use crate::graph::{Graph, Hit};
use crate::model::{EdgeKind, NodeId, NodeKind, Tier};

/// Rough characters per token for code and identifiers.
const CHARS_PER_TOKEN: usize = 4;
/// Most source lines shown, however large the budget.
const MAX_SOURCE_LINES: usize = 200;
/// Share of the budget the source excerpt may use before lists are added.
const SOURCE_SHARE: f64 = 0.6;

fn hit_line(h: &Hit) -> String {
    format!(
        "{:<12} {}  {}:{}  (at {})",
        h.tier.label(),
        h.qname,
        h.file,
        h.line,
        h.at
    )
}

/// Lines `start..=end` (1-based) of `file`, numbered, at most `max_chars` long.
fn source_excerpt(root: &Path, file: &str, start: u32, end: u32, max_chars: usize) -> String {
    let Ok(text) = std::fs::read_to_string(root.join(file)) else {
        return format!("  (could not read {file})\n");
    };
    let mut out = String::new();
    let mut shown = 0;
    let total = (end.saturating_sub(start) + 1) as usize;
    for (i, line) in text
        .lines()
        .enumerate()
        .skip(start.saturating_sub(1) as usize)
        .take(total.min(MAX_SOURCE_LINES))
    {
        let row = format!("{:>5} | {}\n", i + 1, line);
        if out.len() + row.len() > max_chars {
            break;
        }
        out.push_str(&row);
        shown += 1;
    }
    if shown < total {
        out.push_str(&format!(
            "      | ... {} more line(s) not shown\n",
            total - shown
        ));
    }
    out
}

/// Append `items` under `title`, as many as fit in `budget` characters.
fn push_list(out: &mut String, title: &str, items: &[String], budget: &mut usize) {
    if items.is_empty() {
        return;
    }
    let header = format!("--- {title} ({}) ---\n", items.len());
    if header.len() > *budget {
        return;
    }
    out.push_str(&header);
    *budget -= header.len();
    for (shown, item) in items.iter().enumerate() {
        let row = format!("{item}\n");
        if row.len() > *budget {
            let rest = format!("... {} more\n", items.len() - shown);
            if rest.len() <= *budget {
                out.push_str(&rest);
                *budget -= rest.len();
            }
            return;
        }
        out.push_str(&row);
        *budget -= row.len();
    }
}

/// Render a context pack for `id` in about `max_tokens` tokens: the header,
/// a source excerpt, the enclosing class and its bases and subclasses, then
/// callers and callees at `min_tier` or better.
pub fn context_pack(
    root: &Path,
    g: &Graph,
    id: NodeId,
    max_tokens: usize,
    min_tier: Tier,
) -> String {
    let n = g.node(id);
    let mut budget = max_tokens.max(64) * CHARS_PER_TOKEN;
    let mut out = format!(
        "{}  ({}, {}:{}-{})\n",
        n.qname,
        format!("{:?}", n.kind).to_lowercase(),
        n.file,
        n.start_line,
        n.end_line
    );
    budget = budget.saturating_sub(out.len());

    if let Some(class) = n.parent.filter(|p| g.node(*p).kind == NodeKind::Class) {
        let line = format!("in class: {}\n", g.node(class).qname);
        out.push_str(&line);
        budget = budget.saturating_sub(line.len());
    }

    if n.kind != NodeKind::Module {
        let source_budget = (budget as f64 * SOURCE_SHARE) as usize;
        let excerpt = source_excerpt(root, &n.file, n.start_line, n.end_line, source_budget);
        out.push_str("--- source ---\n");
        out.push_str(&excerpt);
        budget = budget.saturating_sub(excerpt.len() + 15);
    }

    if n.kind == NodeKind::Class {
        let bases: Vec<String> = g
            .walk(
                &[id],
                crate::Direction::Out,
                &[EdgeKind::Inherits],
                1,
                min_tier,
            )
            .iter()
            .map(hit_line)
            .collect();
        push_list(&mut out, "extends / implements", &bases, &mut budget);
        let subclasses: Vec<String> = g
            .walk(
                &[id],
                crate::Direction::In,
                &[EdgeKind::Inherits],
                1,
                min_tier,
            )
            .iter()
            .map(hit_line)
            .collect();
        push_list(
            &mut out,
            "subclasses / implementers",
            &subclasses,
            &mut budget,
        );
    }

    let callers: Vec<String> = g.callers(id, 1, min_tier).iter().map(hit_line).collect();
    push_list(&mut out, "called by", &callers, &mut budget);
    let callees: Vec<String> = g.callees(id, 1, min_tier).iter().map(hit_line).collect();
    push_list(&mut out, "calls", &callees, &mut budget);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py_shop")
    }

    #[test]
    fn pack_has_source_class_callers_and_callees() {
        let root = fixture();
        let g = crate::index_repo(&root).unwrap();
        let id = g.resolve_one("TaxCalculator.compute").unwrap();
        let pack = context_pack(&root, &g, id, 1500, Tier::NameOnly);
        assert!(
            pack.starts_with(
                "shop.billing.tax.TaxCalculator.compute  (method, shop/billing/tax.py:5-6)"
            ),
            "{pack}"
        );
        assert!(
            pack.contains("in class: shop.billing.tax.TaxCalculator"),
            "{pack}"
        );
        assert!(pack.contains("return round_money(amount * RATE)"), "{pack}");
        assert!(pack.contains("--- called by (2) ---"), "{pack}");
        assert!(pack.contains("shop.api.quote"), "{pack}");
        assert!(pack.contains("--- calls (1) ---"), "{pack}");
        assert!(pack.contains("shop.billing.tax.round_money"), "{pack}");
    }

    #[test]
    fn pack_respects_a_small_budget() {
        let root = fixture();
        let g = crate::index_repo(&root).unwrap();
        let id = g.resolve_one("shop.billing.invoice.Invoice").unwrap();
        let small = context_pack(&root, &g, id, 64, Tier::NameOnly);
        let large = context_pack(&root, &g, id, 4000, Tier::NameOnly);
        assert!(
            small.len() <= 64 * CHARS_PER_TOKEN + 80,
            "{} chars",
            small.len()
        );
        assert!(large.len() > small.len());
        assert!(
            large.contains("--- extends / implements (1) ---"),
            "{large}"
        );
        // Invoice has no subclasses, so that section is left out entirely.
        assert!(!large.contains("subclasses / implementers"), "{large}");

        let model = g.resolve_one("shop.core.base.Model").unwrap();
        let pack = context_pack(&root, &g, model, 4000, Tier::NameOnly);
        assert!(
            pack.contains("--- subclasses / implementers (2) ---"),
            "{pack}"
        );
        assert!(pack.contains("shop.extras.audit.AuditedModel"), "{pack}");
    }
}
