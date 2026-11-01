//! Gate 2 behaviour: the on-disk index stays correct as files are edited,
//! re-parsing only what changed while cross-file edges stay current.

use std::fs;
use std::path::{Path, PathBuf};

use strata_graph_core::{open_index, stale_files, update_index, EdgeKind, Graph, Store, Tier};

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn workspace(name: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py_shop");
    let dst = std::env::temp_dir().join(format!("strata-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dst);
    copy_dir(&src, &dst);
    let _ = fs::remove_dir_all(dst.join(".strata"));
    dst
}

fn has_call(g: &Graph, src: &str, dst: &str) -> Option<Tier> {
    let s = g.nodes.iter().find(|n| n.qname == src)?.id;
    let d = g.nodes.iter().find(|n| n.qname == dst)?.id;
    g.edges
        .iter()
        .find(|e| e.src == s && e.dst == d && e.kind == EdgeKind::Calls)
        .map(|e| e.tier)
}

#[test]
fn incremental_updates_stay_correct() {
    let root = workspace("incr");

    // First build parses everything.
    let (g1, r1) = update_index(&root).unwrap();
    assert_eq!(r1.parsed, r1.files);
    assert_eq!(r1.reused, 0);
    assert!(r1.files >= 10);

    // No changes: nothing is parsed, the stored graph comes back intact.
    let (g2, r2) = update_index(&root).unwrap();
    assert_eq!((r2.parsed, r2.reused, r2.removed), (0, r1.files, 0));
    assert_eq!(g2.edges.len(), g1.edges.len());
    assert_eq!(stale_files(&root).unwrap(), 0);

    // Add a caller in one file: only that file is parsed.
    let views = root.join("shop/views.py");
    let mut text = fs::read_to_string(&views).unwrap();
    text.push_str(
        "\n\ndef late():\n    from shop.billing.tax import round_money\n    return round_money(2)\n",
    );
    fs::write(&views, &text).unwrap();
    assert!(stale_files(&root).unwrap() >= 1);
    let (g3, r3) = update_index(&root).unwrap();
    assert_eq!(r3.parsed, 1);
    assert_eq!(
        has_call(&g3, "shop.views.late", "shop.billing.tax.round_money"),
        Some(Tier::Scoped)
    );

    // Rename the callee in a different file. Only tax.py is re-parsed, yet
    // edges from files that were not re-parsed must follow the rename.
    let tax = root.join("shop/billing/tax.py");
    let renamed = fs::read_to_string(&tax)
        .unwrap()
        .replace("round_money", "round_cash");
    fs::write(&tax, renamed).unwrap();
    let (g4, r4) = update_index(&root).unwrap();
    assert_eq!(r4.parsed, 1);
    assert!(g4.find("shop.billing.tax.round_money").is_empty());
    assert_eq!(
        has_call(
            &g4,
            "shop.billing.tax.TaxCalculator.compute",
            "shop.billing.tax.round_cash"
        ),
        Some(Tier::Scoped)
    );
    assert_eq!(
        has_call(&g4, "shop.views.late", "shop.billing.tax.round_cash"),
        None
    );

    // Rewriting identical bytes is detected by hash: no parse.
    let same = fs::read(&tax).unwrap();
    fs::write(&tax, &same).unwrap();
    let (_, r5) = update_index(&root).unwrap();
    assert_eq!(r5.parsed, 0);

    // Deleting a file removes its nodes.
    fs::remove_file(root.join("scripts/report.py")).unwrap();
    let (g6, r6) = update_index(&root).unwrap();
    assert_eq!(r6.removed, 1);
    assert!(g6.find("report.main").is_empty());

    // The store round-trips the latest graph and serves name search.
    let reopened = open_index(&root).unwrap();
    assert_eq!(reopened.edges.len(), g6.edges.len());
    assert_eq!(reopened.nodes.len(), g6.nodes.len());
    let store = Store::open(&strata_graph_core::index_path(&root)).unwrap();
    let hits: Vec<String> = store
        .search("invoice tot", 5)
        .unwrap()
        .into_iter()
        .map(|id| reopened.node(id).qname.clone())
        .collect();
    assert!(
        hits.contains(&"shop.billing.invoice.Invoice.total".to_string()),
        "{hits:?}"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn engine_change_forces_reresolve() {
    let root = workspace("engine");
    let (g1, _) = update_index(&root).unwrap();
    let path = strata_graph_core::index_path(&root);

    // Simulate an index written by an older engine.
    Store::open(&path)
        .unwrap()
        .set_meta_for_tests("config", "0.0.0+r0:{}")
        .unwrap();

    let (g2, r2) = update_index(&root).unwrap();
    assert_eq!(
        r2.parsed, 0,
        "cached facts are reused; only resolution reruns"
    );
    assert_eq!(g2.edges.len(), g1.edges.len());
    let stamp = Store::open(&path).unwrap().meta("config").unwrap().unwrap();
    assert!(
        stamp.starts_with(&format!("{}+r", env!("CARGO_PKG_VERSION"))),
        "index was not re-resolved: {stamp}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn concurrent_updates_all_succeed() {
    // Agents run hooks for parallel tool calls while the MCP server may be
    // updating too; every writer must wait its turn rather than fail.
    let root = workspace("concurrent");
    update_index(&root).unwrap();
    let views = root.join("shop/views.py");
    let text =
        fs::read_to_string(&views).unwrap() + "\n\ndef racer():\n    return show_all(None)\n";
    fs::write(&views, text).unwrap();

    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..6).map(|_| s.spawn(|| update_index(&root))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for r in &results {
        assert!(r.is_ok(), "{:?}", r.as_ref().err());
    }
    let g = open_index(&root).unwrap();
    assert_eq!(
        has_call(&g, "shop.views.racer", "shop.views.show_all"),
        Some(Tier::Scoped)
    );
    assert_eq!(stale_files(&root).unwrap(), 0);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn an_update_waits_out_a_slow_writer() {
    // A cold index of a large repo holds the write lock for several seconds
    // (django: ~7 s). Another update must wait for it, not fail at 5 s.
    let root = workspace("slow-writer");
    update_index(&root).unwrap();
    fs::write(root.join("shop/extra.py"), "def extra():\n    return 1\n").unwrap();

    let db = strata_graph_core::index_path(&root);
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(6500));
        conn.execute_batch("COMMIT").unwrap();
    });
    locked_rx.recv().unwrap();
    let (g, r) = update_index(&root).expect("update should wait for the lock");
    holder.join().unwrap();
    assert_eq!(r.parsed, 1);
    assert!(g.nodes.iter().any(|n| n.qname == "shop.extra.extra"));
    fs::remove_dir_all(&root).ok();
}
