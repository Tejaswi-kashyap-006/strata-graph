//! Golden-edge tests: every fixture ships an `expected_edges.txt` written by
//! hand from reading the fixture code. Each line must hold exactly (kind and
//! tier), and `!` lines guard known false positives.

use std::path::{Path, PathBuf};

use strata_graph_core::{index_repo, Edge, Graph};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn find_edge<'g>(g: &'g Graph, kind: &str, src: &str, dst: &str) -> Option<&'g Edge> {
    let s = g.nodes.iter().find(|n| n.qname == src)?.id;
    let d = g.nodes.iter().find(|n| n.qname == dst)?.id;
    g.edges
        .iter()
        .find(|e| e.src == s && e.dst == d && format!("{:?}", e.kind).to_uppercase() == kind)
}

fn check_fixture(name: &str) {
    let root = fixture(name);
    let g = index_repo(&root).expect("index");
    let spec =
        std::fs::read_to_string(root.join("expected_edges.txt")).expect("expected_edges.txt");
    let mut failures = Vec::new();
    let mut checked = 0;
    for raw in spec.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negate, line) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let parts: Vec<&str> = line.split_whitespace().collect();
        assert!(parts.len() == 5 && parts[3] == "->", "bad spec line: {raw}");
        let (kind, tier, src, dst) = (parts[0], parts[1], parts[2], parts[4]);
        checked += 1;
        let edge = find_edge(&g, kind, src, dst);
        match (negate, edge) {
            (false, None) => failures.push(format!("missing: {raw}")),
            (false, Some(e)) if e.tier.label() != tier => {
                failures.push(format!("wrong tier {}: {raw}", e.tier.label()))
            }
            (true, Some(_)) => failures.push(format!("false positive present: {raw}")),
            _ => {}
        }
    }
    assert!(checked > 0, "no expectations in {name}");
    assert!(
        failures.is_empty(),
        "{name}: {} of {checked} expectations failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn py_shop_edges() {
    check_fixture("py_shop");
}

#[test]
fn py_shop_queries() {
    let g = index_repo(&fixture("py_shop")).unwrap();
    let compute = g.resolve_one("TaxCalculator.compute").unwrap();
    let callers: Vec<_> = g
        .callers(compute, 1, strata_graph_core::Tier::Scoped)
        .into_iter()
        .map(|h| h.qname)
        .collect();
    assert_eq!(
        callers,
        ["shop.api.quote", "shop.billing.invoice.Invoice.total"]
    );

    // Two hops back from round_money reaches checkout through Invoice.total.
    let rm = g.resolve_one("round_money").unwrap();
    let impact: Vec<_> = g
        .impact(rm, 3, strata_graph_core::Tier::Scoped)
        .into_iter()
        .map(|h| h.qname)
        .collect();
    assert!(
        impact.contains(&"shop.api.checkout".to_string()),
        "{impact:?}"
    );

    let main = g.resolve_one("report.main").unwrap();
    let path = g
        .path(main, rm, 6, strata_graph_core::Tier::Scoped)
        .expect("path");
    let names: Vec<_> = path.iter().map(|i| g.node(*i).qname.as_str()).collect();
    assert_eq!(
        names,
        [
            "report.main",
            "shop.api.checkout",
            "shop.billing.invoice.Invoice.total",
            "shop.billing.tax.TaxCalculator.compute",
            "shop.billing.tax.round_money",
        ]
    );

    // Round trip through the on-disk index.
    let dir = std::env::temp_dir().join(format!("strata-golden-{}", std::process::id()));
    let p = dir.join("graph.json");
    g.save(&p).unwrap();
    let back = Graph::load(&p).unwrap();
    assert_eq!(back.edges.len(), g.edges.len());
    std::fs::remove_dir_all(dir).ok();
}
