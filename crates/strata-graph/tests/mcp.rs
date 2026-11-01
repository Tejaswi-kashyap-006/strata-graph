//! End-to-end MCP tests: spawn `strata-graph mcp` and talk JSON-RPC over stdio.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

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
    let src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../strata-graph-core/tests/fixtures/py_shop");
    let dst = std::env::temp_dir().join(format!("strata-mcp-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dst);
    copy_dir(&src, &dst);
    let _ = fs::remove_dir_all(dst.join(".strata"));
    dst
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Session {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_strata-graph"))
            .args(["--root", root.to_str().unwrap(), "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn strata-graph mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Session {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "server closed stdout");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad JSON {line:?}: {e}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        );
        let reply = self.read();
        assert_eq!(reply["id"], id, "reply id mismatch: {reply}");
        reply
    }

    fn notify(&mut self, method: &str) {
        self.send_raw(&json!({ "jsonrpc": "2.0", "method": method }).to_string());
    }

    fn call(&mut self, tool: &str, args: Value) -> (String, bool) {
        let reply = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        let result = &reply["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (text, result["isError"].as_bool().unwrap_or(false))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn legacy_handshake_and_tools() {
    let root = workspace("legacy");
    let mut s = Session::start(&root);

    let init = s.request(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "strata-graph");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("T1"));

    // A notification gets no reply: the next line read must answer the ping.
    s.notify("notifications/initialized");
    assert_eq!(s.request("ping", json!({}))["result"], json!({}));

    let tools = s.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "strata_context",
            "strata_callers",
            "strata_callees",
            "strata_impact",
            "strata_path",
            "strata_find",
            "strata_stats"
        ]
    );
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{t}");
    }

    let (text, err) = s.call(
        "strata_callers",
        json!({ "symbol": "TaxCalculator.compute", "min_tier": "t1" }),
    );
    assert!(!err, "{text}");
    assert!(
        text.starts_with("callers of shop.billing.tax.TaxCalculator.compute (2):"),
        "{text}"
    );
    assert!(
        text.contains("T1           shop.api.quote  shop/api.py:12  (at shop/api.py:13)"),
        "{text}"
    );

    let (text, err) = s.call(
        "strata_context",
        json!({ "symbol": "TaxCalculator.compute" }),
    );
    assert!(!err, "{text}");
    assert!(text.contains("return round_money(amount * RATE)"), "{text}");
    assert!(text.contains("--- called by (2) ---"), "{text}");

    let (text, err) = s.call(
        "strata_path",
        json!({ "from": "report.main", "to": "round_money" }),
    );
    assert!(!err, "{text}");
    assert!(text.starts_with("call path (4 hops):"), "{text}");

    let (text, _) = s.call("strata_impact", json!({ "symbol": "round_money" }));
    assert!(text.contains("shop.api.checkout"), "{text}");

    let (text, _) = s.call("strata_find", json!({ "query": "invoice tot" }));
    assert!(
        text.contains("shop.billing.invoice.Invoice.total"),
        "{text}"
    );

    let (text, _) = s.call("strata_stats", json!({}));
    assert!(text.contains("\"resolved_t1\""), "{text}");

    drop(s);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn version_negotiation_and_errors() {
    let root = workspace("errors");
    let mut s = Session::start(&root);

    // An unknown legacy version is answered with the newest legacy one.
    let init = s.request(
        "initialize",
        json!({ "protocolVersion": "1999-01-01", "capabilities": {} }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");

    let (text, err) = s.call("strata_callers", json!({ "symbol": "does_not_exist" }));
    assert!(err && text.contains("no symbol matches"), "{text}");
    let (text, err) = s.call("strata_context", json!({ "symbol": "render" }));
    assert!(err && text.contains("matches 2 symbols"), "{text}");
    let (text, err) = s.call(
        "strata_callers",
        json!({ "symbol": "round_money", "min_tier": "t9" }),
    );
    assert!(err && text.contains("min_tier"), "{text}");
    let (text, err) = s.call("strata_callers", json!({}));
    assert!(err && text.contains("symbol"), "{text}");

    let unknown_tool = s.request("tools/call", json!({ "name": "nope", "arguments": {} }));
    assert_eq!(unknown_tool["error"]["code"], -32602);
    let unknown_method = s.request("resources/list", json!({}));
    assert_eq!(unknown_method["error"]["code"], -32601);

    s.send_raw("{not json");
    let parse = s.read();
    assert_eq!(parse["error"]["code"], -32700);
    assert_eq!(parse["id"], Value::Null);

    drop(s);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn modern_revision_without_handshake() {
    let root = workspace("modern");
    let mut s = Session::start(&root);
    let meta = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28" });

    let discover = s.request("server/discover", json!({ "_meta": meta }));
    let r = &discover["result"];
    assert!(r["supportedVersions"]
        .as_array()
        .unwrap()
        .contains(&json!("2026-07-28")));
    assert!(r["supportedVersions"]
        .as_array()
        .unwrap()
        .contains(&json!("2025-11-25")));
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "strata-graph"
    );
    assert_eq!(r["resultType"], "complete");

    // Tools work with no initialize at all.
    let reply = s.request(
        "tools/call",
        json!({ "name": "strata_callees", "arguments": { "symbol": "report.main" }, "_meta": meta }),
    );
    assert_eq!(reply["result"]["resultType"], "complete");
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("shop.api.checkout"));

    let bad = s.request(
        "tools/list",
        json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "1900-01-01" } }),
    );
    assert_eq!(bad["error"]["code"], -32022);
    assert_eq!(bad["error"]["data"]["requested"], "1900-01-01");
    assert!(bad["error"]["data"]["supported"]
        .as_array()
        .unwrap()
        .contains(&json!("2026-07-28")));

    drop(s);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn answers_follow_file_edits() {
    let root = workspace("fresh");
    let mut s = Session::start(&root);
    s.request(
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {} }),
    );

    let (before, _) = s.call("strata_callers", json!({ "symbol": "shop.views.show_all" }));
    assert!(!before.contains("from_mcp"), "{before}");

    let views = root.join("shop/views.py");
    let text =
        fs::read_to_string(&views).unwrap() + "\n\ndef from_mcp():\n    return show_all(None)\n";
    fs::write(&views, text).unwrap();

    let (after, err) = s.call("strata_callers", json!({ "symbol": "shop.views.show_all" }));
    assert!(!err && after.contains("shop.views.from_mcp"), "{after}");

    drop(s);
    fs::remove_dir_all(&root).ok();
}
