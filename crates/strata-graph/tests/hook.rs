//! End-to-end hook tests: run `strata-graph hook` with the JSON events Claude
//! Code and Codex send, and check the context it adds.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    let dst = std::env::temp_dir().join(format!("strata-hook-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dst);
    copy_dir(&src, &dst);
    let _ = fs::remove_dir_all(dst.join(".strata"));
    dst.canonicalize().unwrap()
}

/// Run the hook with `input` on stdin; return stdout (exit status must be 0).
fn hook_raw(input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_strata-graph"))
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hook must never fail the tool call");
    String::from_utf8(out.stdout).unwrap()
}

/// Run the hook on an event; return the added context, if any.
fn hook(event: Value) -> Option<String> {
    let out = hook_raw(&event.to_string());
    if out.trim().is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        event["hook_event_name"]
    );
    Some(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

fn event(root: &Path, session: &str, name: &str, extra: Value) -> Value {
    let mut e = json!({
        "session_id": session,
        "transcript_path": "/dev/null",
        "cwd": root.to_str().unwrap(),
        "hook_event_name": name,
    });
    for (k, v) in extra.as_object().unwrap() {
        e[k] = v.clone();
    }
    e
}

fn read(root: &Path, session: &str, file: &str) -> Value {
    event(
        root,
        session,
        "PostToolUse",
        json!({
            "tool_name": "Read",
            "tool_input": { "file_path": root.join(file).to_str().unwrap() },
            "tool_response": {},
            "tool_use_id": "t1",
        }),
    )
}

#[test]
fn claude_code_session() {
    let root = workspace("claude");

    // SessionStart builds the index and explains the tags.
    let start = hook(event(
        &root,
        "s1",
        "SessionStart",
        json!({ "source": "startup" }),
    ))
    .unwrap();
    assert!(
        start.starts_with("[strata-graph] This repo has a code graph:"),
        "{start}"
    );
    assert!(start.contains("(Python)"), "{start}");
    assert!(root.join(".strata/index.db").exists());

    // Reading a file appends its definitions' callers and callees.
    let facts = hook(read(&root, "s1", "shop/billing/tax.py")).unwrap();
    assert!(
        facts.starts_with("[strata-graph] shop/billing/tax.py\n"),
        "{facts}"
    );
    assert!(
        facts.contains("TaxCalculator.compute (method): called by 2: shop.api.quote T1"),
        "{facts}"
    );

    // The same facts are not sent twice in one session...
    assert_eq!(hook(read(&root, "s1", "shop/billing/tax.py")), None);
    // ...but are in another session, and again after /clear.
    assert!(hook(read(&root, "s2", "shop/billing/tax.py")).is_some());
    hook(event(
        &root,
        "s1",
        "SessionStart",
        json!({ "source": "clear" }),
    ));
    assert!(hook(read(&root, "s1", "shop/billing/tax.py")).is_some());

    // Grep: where the name is defined and where it is really called.
    let grep = hook(event(
        &root,
        "s1",
        "PostToolUse",
        json!({ "tool_name": "Grep", "tool_input": { "pattern": r"def\s+round_money" }, "tool_response": {} }),
    ))
    .unwrap();
    assert!(
        grep.starts_with("[strata-graph] definitions named round_money\n"),
        "{grep}"
    );
    assert!(
        grep.contains("shop.billing.tax.round_money (function, defined shop/billing/tax.py:"),
        "{grep}"
    );

    // An edit made during the session shows up in the next annotation.
    let views = root.join("shop/views.py");
    let text = fs::read_to_string(&views).unwrap()
        + "\n\ndef from_hook():\n    return round_money(1)\n\nfrom shop.billing.tax import round_money\n";
    fs::write(&views, text).unwrap();
    let after = hook(read(&root, "s3", "shop/billing/tax.py")).unwrap();
    assert!(after.contains("shop.views.from_hook"), "{after}");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn codex_shell_reads() {
    let root = workspace("codex");
    hook(event(
        &root,
        "c1",
        "SessionStart",
        json!({ "source": "startup" }),
    ));

    // Codex reads files through the shell, often from a subdirectory.
    let shell = |cmd: Value, cwd: &Path| {
        let mut e = event(
            &root,
            "c1",
            "PostToolUse",
            json!({ "tool_name": "Bash", "tool_input": { "command": cmd } }),
        );
        e["cwd"] = json!(cwd.to_str().unwrap());
        hook(e)
    };
    let facts = shell(
        json!("nl -ba billing/tax.py | sed -n '1,20p'"),
        &root.join("shop"),
    )
    .unwrap();
    assert!(
        facts.starts_with("[strata-graph] shop/billing/tax.py:1-20\n"),
        "{facts}"
    );
    assert!(facts.contains("TaxCalculator.compute"), "{facts}");

    let facts = shell(json!(["bash", "-lc", "rg -n 'class Invoice' shop"]), &root).unwrap();
    assert!(
        facts.contains("shop.billing.invoice.Invoice (class, defined"),
        "{facts}"
    );

    // Commands that read no indexed code add nothing.
    assert_eq!(shell(json!("cargo build && ls"), &root), None);
    assert_eq!(shell(json!("cat /etc/hostname"), &root), None);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn bad_input_and_unindexed_repos_add_nothing() {
    assert_eq!(hook_raw("{ not json"), "");
    assert_eq!(hook_raw(""), "");

    // No index yet: a tool call never builds one (that could stall the agent).
    let root = workspace("noindex");
    assert_eq!(hook(read(&root, "x", "shop/billing/tax.py")), None);
    assert!(!root.join(".strata/index.db").exists());
    // Unknown events are ignored.
    assert_eq!(hook(event(&root, "x", "Stop", json!({}))), None);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn install_hooks_merges_into_existing_settings() {
    let root = workspace("install");
    let settings = root.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        r#"{"permissions": {"allow": ["Bash(ls)"]}, "env": {"A": "1"}}"#,
    )
    .unwrap();

    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_strata-graph"))
            .args(["--root", root.to_str().unwrap(), "install-hooks"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert!(run(&["--agent", "claude"]).contains("added SessionStart and PostToolUse hook(s)"));
    let text = fs::read_to_string(&settings).unwrap();
    // The user's keys keep their place; ours are added after them.
    assert!(text.starts_with("{\n  \"permissions\""), "{text}");
    assert!(
        text.contains("\"matcher\": \"Read|Grep|Bash\",\n        \"hooks\""),
        "{text}"
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");
    assert_eq!(
        v["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "strata-graph hook"
    );
    assert!(run(&["--agent", "claude"]).contains("already installed"));

    assert!(run(&["--agent", "codex"]).contains("codex_hooks = true"));
    let c: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".codex/hooks.json")).unwrap()).unwrap();
    assert_eq!(
        c["hooks"]["SessionStart"][0]["matcher"],
        "startup|resume|clear"
    );

    // A broken settings file is reported, never overwritten.
    fs::write(&settings, "{ broken").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_strata-graph"))
        .args([
            "--root",
            root.to_str().unwrap(),
            "install-hooks",
            "--agent",
            "claude",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read_to_string(&settings).unwrap(), "{ broken");

    fs::remove_dir_all(&root).ok();
}
