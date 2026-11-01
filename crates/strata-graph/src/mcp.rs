//! Model Context Protocol server over stdio: `strata-graph mcp`.
//!
//! Speaks both protocol eras. Clients on `2024-11-05` through `2025-11-25`
//! use the `initialize` handshake; clients on `2026-07-28` call
//! `server/discover` and put the protocol version in each request's `_meta`.
//! Messages are newline-delimited JSON on stdin/stdout; diagnostics go to
//! stderr only, since anything else on stdout would corrupt the stream.
//!
//! Every tool is read-only. Before each call the server picks up file
//! changes, so answers match the code on disk.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};
use strata_graph_core::{
    context_pack, index_path, stale_files, tree_order, update_index, Graph, Hit, Store, Tier,
};

/// The stateless revision: version per request, `server/discover`.
pub const MODERN_VERSION: &str = "2026-07-28";
/// Revisions negotiated through `initialize`, newest first.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const VERSION_META_KEY: &str = "io.modelcontextprotocol/protocolVersion";

/// JSON-RPC and MCP error codes used here.
mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const UNSUPPORTED_VERSION: i64 = -32022;
}

/// Most result lines a list tool returns before summarising the rest.
const MAX_LINES: usize = 60;

const INSTRUCTIONS: &str = "strata-graph answers structural questions about this repository from a \
precomputed code graph: who calls a function, what it calls, what breaks if it changes, and how two \
functions connect. Prefer strata_context over opening files to understand one symbol; use \
strata_impact before changing a function. Every edge has a confidence tier: T1 is resolved through \
the language's own rules and is reliable; T0 is the only definition with that name (Python only); \
T0-ambiguous is one of 2-3 same-named definitions. Calls into libraries are not edges.";

fn supported_versions() -> Vec<&'static str> {
    std::iter::once(MODERN_VERSION)
        .chain(LEGACY_VERSIONS.iter().copied())
        .collect()
}

fn server_info() -> Value {
    json!({
        "name": "strata-graph",
        "title": "strata-graph code graph",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn tool_specs() -> Value {
    let symbol = json!({
        "type": "string",
        "description": "Qualified name (e.g. billing.tax.round_money or src/billing/tax.roundMoney), a suffix such as Invoice.total, or a simple name if it is unique."
    });
    let min_tier = json!({
        "type": "string",
        "enum": ["t1", "t0", "ambiguous"],
        "default": "t0",
        "description": "Weakest edge tier to include. t0 leaves ambiguous name matches out."
    });
    let read_only = json!({ "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false });
    json!([
        {
            "name": "strata_context",
            "title": "Symbol context",
            "description": "Source, enclosing class, bases and subclasses, callers and callees of one symbol, trimmed to a token budget. Use this instead of opening files to understand a function or class.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "max_tokens": { "type": "integer", "minimum": 64, "maximum": 20000, "default": 1500 },
                    "min_tier": min_tier
                },
                "required": ["symbol"]
            },
            "annotations": read_only
        },
        {
            "name": "strata_callers",
            "title": "Callers",
            "description": "Who calls a symbol, up to `depth` hops back, with the call site of each edge.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "depth": { "type": "integer", "minimum": 1, "maximum": 6, "default": 1 },
                    "min_tier": min_tier
                },
                "required": ["symbol"]
            },
            "annotations": read_only
        },
        {
            "name": "strata_callees",
            "title": "Callees",
            "description": "What a symbol calls, up to `depth` hops forward.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "depth": { "type": "integer", "minimum": 1, "maximum": 6, "default": 1 },
                    "min_tier": min_tier
                },
                "required": ["symbol"]
            },
            "annotations": read_only
        },
        {
            "name": "strata_impact",
            "title": "Impact",
            "description": "Everything that may break if a symbol changes: transitive callers and subclasses (for a class, also callers of its methods). Run before editing a function.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "depth": { "type": "integer", "minimum": 1, "maximum": 8, "default": 3 },
                    "min_tier": min_tier
                },
                "required": ["symbol"]
            },
            "annotations": read_only
        },
        {
            "name": "strata_path",
            "title": "Call path",
            "description": "Shortest call path from one symbol to another, or a note that none exists within max_depth.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": symbol,
                    "to": symbol,
                    "max_depth": { "type": "integer", "minimum": 1, "maximum": 12, "default": 8 },
                    "min_tier": min_tier
                },
                "required": ["from", "to"]
            },
            "annotations": read_only
        },
        {
            "name": "strata_find",
            "title": "Find symbols",
            "description": "Find classes, functions and methods by qualified name, suffix or simple name; falls back to prefix search over names (e.g. 'invoice tot').",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                },
                "required": ["query"]
            },
            "annotations": read_only
        },
        {
            "name": "strata_stats",
            "title": "Index stats",
            "description": "Files, nodes, edges by kind and tier, and how many calls resolved, for judging coverage.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only
        }
    ])
}

/// A tool failure the agent should see as a tool result (`isError: true`),
/// as opposed to a protocol error.
struct ToolError(String);

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError(format!("{e:#}"))
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ToolError(format!("missing required string argument `{key}`")))
}

fn arg_u32(args: &Value, key: &str, default: u32, max: u32) -> Result<u32, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .map(|n| n.min(u64::from(max)) as u32)
            .ok_or_else(|| ToolError(format!("`{key}` must be a positive integer"))),
    }
}

fn arg_tier(args: &Value) -> Result<Tier, ToolError> {
    match args.get("min_tier").and_then(Value::as_str).unwrap_or("t0") {
        "t1" | "T1" => Ok(Tier::Scoped),
        "t0" | "T0" => Ok(Tier::NameOnly),
        "ambiguous" | "T0-ambiguous" => Ok(Tier::Ambiguous),
        other => Err(ToolError(format!(
            "`min_tier` must be t1, t0 or ambiguous, not {other:?}"
        ))),
    }
}

fn render_hits(title: &str, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return format!(
            "{title}: none found at this tier. Try min_tier \"ambiguous\" or a larger depth."
        );
    }
    let mut out = format!("{title} ({}):\n", hits.len());
    for h in tree_order(hits).into_iter().take(MAX_LINES) {
        let indent = "  ".repeat(h.depth.saturating_sub(1) as usize);
        out.push_str(&format!(
            "{:<12} {indent}{}  {}:{}  (at {})\n",
            h.tier.label(),
            h.qname,
            h.file,
            h.line,
            h.at
        ));
    }
    if hits.len() > MAX_LINES {
        out.push_str(&format!(
            "... {} more; narrow with a smaller depth or min_tier \"t1\".\n",
            hits.len() - MAX_LINES
        ));
    }
    out
}

/// Server state: the repo root and the graph, refreshed before each call.
pub struct Server {
    root: PathBuf,
    graph: Option<Graph>,
}

impl Server {
    /// A server for the repository at `root`. The index is built on first use.
    pub fn new(root: PathBuf) -> Self {
        Server { root, graph: None }
    }

    /// The current graph, re-indexing changed files first.
    fn graph(&mut self) -> Result<&Graph> {
        let stale = match &self.graph {
            None => true,
            Some(_) => !index_path(&self.root).exists() || stale_files(&self.root)? > 0,
        };
        if stale {
            let (g, report) = update_index(&self.root)?;
            if report.parsed > 0 || report.removed > 0 {
                eprintln!(
                    "strata-graph mcp: indexed {} file(s), {} removed, in {} ms",
                    report.parsed, report.removed, report.elapsed_ms
                );
            }
            self.graph = Some(g);
        }
        self.graph
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("index unavailable"))
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Result<String, ToolError> {
        let root = self.root.clone();
        match name {
            "strata_context" => {
                let tier = arg_tier(args)?;
                let max_tokens = arg_u32(args, "max_tokens", 1500, 20_000)? as usize;
                let g = self.graph()?;
                let id = g.resolve_one(arg_str(args, "symbol")?)?;
                Ok(context_pack(&root, g, id, max_tokens, tier))
            }
            "strata_callers" | "strata_callees" | "strata_impact" => {
                let tier = arg_tier(args)?;
                let default_depth = if name == "strata_impact" { 3 } else { 1 };
                let depth = arg_u32(args, "depth", default_depth, 8)?;
                let symbol = arg_str(args, "symbol")?;
                let g = self.graph()?;
                let id = g.resolve_one(symbol)?;
                let qname = &g.node(id).qname;
                Ok(match name {
                    "strata_callers" => {
                        render_hits(&format!("callers of {qname}"), &g.callers(id, depth, tier))
                    }
                    "strata_callees" => {
                        render_hits(&format!("callees of {qname}"), &g.callees(id, depth, tier))
                    }
                    _ => render_hits(
                        &format!("impact of changing {qname}"),
                        &g.impact(id, depth, tier),
                    ),
                })
            }
            "strata_path" => {
                let tier = arg_tier(args)?;
                let max_depth = arg_u32(args, "max_depth", 8, 12)?;
                let (from, to) = (arg_str(args, "from")?, arg_str(args, "to")?);
                let g = self.graph()?;
                let (a, b) = (g.resolve_one(from)?, g.resolve_one(to)?);
                Ok(match g.path(a, b, max_depth, tier) {
                    Some(p) => {
                        let names: Vec<&str> =
                            p.iter().map(|i| g.node(*i).qname.as_str()).collect();
                        format!(
                            "call path ({} hops):\n  {}",
                            names.len() - 1,
                            names.join("\n  -> ")
                        )
                    }
                    None => format!(
                        "no call path from {} to {} within {max_depth} hops at this tier",
                        g.node(a).qname,
                        g.node(b).qname
                    ),
                })
            }
            "strata_find" => {
                let query = arg_str(args, "query")?;
                let limit = arg_u32(args, "limit", 20, 100)? as usize;
                let g = self.graph()?;
                let mut ids = g.find(query);
                if ids.is_empty() {
                    ids = Store::open(&index_path(&root))?.search(query, limit)?;
                }
                if ids.is_empty() {
                    return Ok(format!("no symbols match {query:?}"));
                }
                let mut out = String::new();
                for id in ids.iter().take(limit) {
                    let n = g.node(*id);
                    out.push_str(&format!(
                        "{:<9}{}  {}:{}\n",
                        format!("{:?}", n.kind).to_lowercase(),
                        n.qname,
                        n.file,
                        n.start_line
                    ));
                }
                if ids.len() > limit {
                    out.push_str(&format!("... {} more\n", ids.len() - limit));
                }
                Ok(out)
            }
            "strata_stats" => Ok(serde_json::to_string_pretty(&self.graph()?.summary())
                .map_err(|e| ToolError(e.to_string()))?),
            other => Err(ToolError(format!("unknown tool {other:?}"))),
        }
    }

    /// Handle one decoded JSON-RPC message; `None` for notifications and responses.
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to a server request (none are sent) or garbage.
            return id.map(|id| error(id, code::INVALID_REQUEST, "missing method", None));
        };
        let id = id?; // notifications (e.g. notifications/initialized) get no reply
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let requested = params
            .pointer("/_meta")
            .and_then(|m| m.get(VERSION_META_KEY))
            .and_then(Value::as_str);
        if let Some(v) = requested {
            if !supported_versions().contains(&v) {
                return Some(error(
                    id,
                    code::UNSUPPORTED_VERSION,
                    "Unsupported protocol version",
                    Some(json!({ "supported": supported_versions(), "requested": v })),
                ));
            }
        }
        let modern = requested == Some(MODERN_VERSION);

        let mut result = match method {
            "initialize" => {
                let asked = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let version = if LEGACY_VERSIONS.contains(&asked) {
                    asked
                } else {
                    LEGACY_VERSIONS[0]
                };
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": server_info(),
                    "instructions": INSTRUCTIONS,
                })
            }
            "server/discover" => json!({
                "supportedVersions": supported_versions(),
                "capabilities": { "tools": { "listChanged": false } },
                "_meta": { "io.modelcontextprotocol/serverInfo": server_info() },
                "instructions": INSTRUCTIONS,
                "ttlMs": 3_600_000,
                "cacheScope": "private",
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tool_specs() }),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(error(
                        id,
                        code::INVALID_PARAMS,
                        "tools/call needs a tool name",
                        None,
                    ));
                };
                let known = tool_specs()
                    .as_array()
                    .is_some_and(|t| t.iter().any(|t| t["name"] == name));
                if !known {
                    return Some(error(
                        id,
                        code::INVALID_PARAMS,
                        &format!("unknown tool {name:?}"),
                        None,
                    ));
                }
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match self.call_tool(name, &args) {
                    Ok(text) => {
                        json!({ "content": [{ "type": "text", "text": text }], "isError": false })
                    }
                    Err(ToolError(text)) => {
                        json!({ "content": [{ "type": "text", "text": text }], "isError": true })
                    }
                }
            }
            _ => {
                return Some(error(
                    id,
                    code::METHOD_NOT_FOUND,
                    &format!("method not found: {method}"),
                    None,
                ))
            }
        };
        if modern {
            if let Some(obj) = result.as_object_mut() {
                obj.insert("resultType".into(), json!("complete"));
            }
        }
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    /// Handle one line of input: a message, or a batch for older clients.
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(msg) => self.handle(&msg),
            Err(e) => Some(error(
                Value::Null,
                code::PARSE_ERROR,
                &format!("parse error: {e}"),
                None,
            )),
        }
    }
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

/// Serve MCP on stdin/stdout until stdin closes.
pub fn serve(root: &Path) -> Result<()> {
    eprintln!("strata-graph mcp: serving {}", root.display());
    let mut server = Server::new(root.to_path_buf());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_line(&line) {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}
