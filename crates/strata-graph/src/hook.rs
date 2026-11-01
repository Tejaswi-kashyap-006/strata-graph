//! Agent hooks: append graph facts to the tool results agents already read.
//!
//! `strata-graph hook` reads one hook event as JSON on stdin and prints a
//! `hookSpecificOutput.additionalContext` reply, the format shared by Claude
//! Code and Codex. It handles `SessionStart` and `PostToolUse` for Claude
//! Code's Read, Grep and Bash tools and for shell commands that read or
//! search code (`rg`, `grep`, `sed -n`, `cat`, `head`, ...), which is how
//! Codex reads files. It never changes source files and never fails the
//! agent's tool call: on any problem it prints nothing and exits 0.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use strata_graph_core::annotate::{name_facts, pattern_names, range_facts, render, Fact};
use strata_graph_core::{index_path, open_index, stale_files, update_index, Graph, Tier};

/// Largest hook input read from stdin.
const MAX_INPUT: u64 = 8 * 1024 * 1024;
/// Default size of one annotation, in characters (about 500 tokens).
pub const DEFAULT_MAX_CHARS: usize = 2000;
/// Most files and names annotated for one tool call.
const MAX_TARGETS: usize = 3;
/// Session dedup files older than this are removed at session start.
const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
/// Marker that identifies our entries in agent settings files.
const MARKER: &str = "strata-graph hook";

// ---------------------------------------------------------------- shell ---

/// A word or control operator from a shell command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// `|`
    Pipe,
    /// `;`, `&&`, `||`, `&` or a newline: ends a pipeline.
    End,
}

/// Split a command line into words and operators, honouring quotes and
/// backslashes. Returns `None` for constructs this reader does not follow
/// (command substitution, unterminated quotes), so nothing is guessed.
fn tokenize(cmd: &str) -> Option<Vec<Tok>> {
    let mut toks = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    let flush = |toks: &mut Vec<Tok>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            toks.push(Tok::Word(std::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => word.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            ch @ ('"' | '\\' | '$' | '`') => word.push(ch),
                            ch => {
                                word.push('\\');
                                word.push(ch);
                            }
                        },
                        '`' => return None,
                        '$' if chars.peek() == Some(&'(') => return None,
                        ch => word.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some('\n') | None => {}
                    Some(ch) => word.push(ch),
                }
            }
            '`' => return None,
            '$' if chars.peek() == Some(&'(') => return None,
            '|' => {
                flush(&mut toks, &mut word, &mut in_word);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    toks.push(Tok::End);
                } else {
                    toks.push(Tok::Pipe);
                }
            }
            ';' | '\n' | '&' => {
                flush(&mut toks, &mut word, &mut in_word);
                if c == '&' && chars.peek() == Some(&'&') {
                    chars.next();
                }
                toks.push(Tok::End);
            }
            c if c.is_whitespace() => flush(&mut toks, &mut word, &mut in_word),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut toks, &mut word, &mut in_word);
    Some(toks)
}

/// What one tool call looked at: line ranges of files, or names searched for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Targets {
    /// (path as written, 1-based start, end; `u32::MAX` means end of file).
    files: Vec<(PathBuf, u32, u32)>,
    names: Vec<String>,
}

/// Short options of grep-like tools that take a value as the next word.
const GREP_VALUE_SHORT: &[&str] = &[
    "-e", "-f", "-g", "-t", "-T", "-m", "-M", "-j", "-A", "-B", "-C", "-E", "-r",
];
/// Long options of grep-like tools that take a value as the next word.
const GREP_VALUE_LONG: &[&str] = &[
    "--regexp",
    "--file",
    "--glob",
    "--iglob",
    "--type",
    "--type-not",
    "--max-count",
    "--max-depth",
    "--context",
    "--after-context",
    "--before-context",
    "--replace",
    "--sort",
    "--sortr",
    "--encoding",
    "--max-filesize",
    "--include",
    "--exclude",
    "--exclude-dir",
    "--threads",
    "--type-add",
    "--max-columns",
    "--colors",
];

/// The search pattern of an `rg` / `grep` / `git grep` / `ag` / `ack` call.
fn grep_pattern(args: &[String], is_rg: bool) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            return args.get(i + 1).cloned();
        }
        if a == "-e" || a == "--regexp" {
            return args.get(i + 1).cloned();
        }
        if let Some(p) = a.strip_prefix("--regexp=") {
            return Some(p.to_string());
        }
        if a.starts_with("--") {
            if !a.contains('=') && GREP_VALUE_LONG.contains(&a.as_str()) {
                i += 1;
            }
        } else if a.starts_with('-') && a.len() > 1 {
            // In grep, -r is recursive and -E is extended regex; in rg both take a value.
            let takes =
                GREP_VALUE_SHORT.contains(&a.as_str()) && (is_rg || (a != "-r" && a != "-E"));
            if takes {
                i += 1;
            }
        } else {
            return Some(a.clone());
        }
        i += 1;
    }
    None
}

/// Line ranges selected by a `sed -n` script such as `10,40p` or `5p;9,$p`.
fn sed_ranges(script: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for part in script.split(';') {
        let Some(addr) = part.trim().strip_suffix('p') else {
            continue;
        };
        let num = |s: &str| -> Option<u32> {
            if s == "$" {
                Some(u32::MAX)
            } else {
                s.parse().ok()
            }
        };
        let range = match addr.split_once(',') {
            Some((a, b)) => num(a).zip(num(b)),
            None => num(addr).map(|n| (n, n)),
        };
        if let Some((a, b)) = range {
            if a >= 1 && b >= a {
                out.push((a, b));
            }
        }
    }
    out
}

fn positional(args: &[String]) -> Vec<&String> {
    args.iter().filter(|a| !a.starts_with('-')).collect()
}

/// `-n 40`, `-n40`, `-40`, `--lines=40` (and `+K` for tail) as a count.
fn line_count(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut count = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-n" || a == "--lines" {
            count = args.get(i + 1).cloned();
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--lines=").or_else(|| a.strip_prefix("-n")) {
            count = Some(v.to_string());
        } else if a.len() > 1 && a.starts_with('-') && a[1..].chars().all(|c| c.is_ascii_digit()) {
            count = Some(a[1..].to_string());
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }
    (count, rest)
}

/// Apply a filter that reads a whole stream (head/tail/sed -n) to a range.
fn narrow(range: (u32, u32), part: (u32, u32)) -> Option<(u32, u32)> {
    // `part` is relative to the first line of `range`.
    let start = range.0.saturating_add(part.0 - 1);
    let end = if part.1 == u32::MAX {
        range.1
    } else {
        range.0.saturating_add(part.1 - 1).min(range.1)
    };
    (start <= end).then_some((start, end))
}

/// Record what one pipeline (`a | b | c`) reads or searches, then reset it.
fn finish(stages: &mut Vec<Vec<String>>, cwd: &mut PathBuf, t: &mut Targets) {
    // The file(s) the first stage reads, narrowed by later filters.
    let mut current: Vec<(PathBuf, u32, u32)> = Vec::new();
    for (si, words) in stages.iter().enumerate() {
        let Some((prog, args)) = words.split_first() else {
            continue;
        };
        let prog = prog.rsplit('/').next().unwrap_or(prog);
        let (prog, args) = if prog == "git" && args.first().is_some_and(|a| a == "grep") {
            ("git-grep", &args[1..])
        } else {
            (prog, args)
        };
        match prog {
            "cd" if si == 0 && stages.len() == 1 => {
                if let Some(dir) = args.first() {
                    *cwd = cwd.join(dir);
                }
            }
            "rg" | "grep" | "egrep" | "fgrep" | "git-grep" | "ag" | "ack" => {
                if let Some(p) = grep_pattern(args, prog == "rg") {
                    for n in pattern_names(&p) {
                        if !t.names.contains(&n) {
                            t.names.push(n);
                        }
                    }
                }
            }
            "cat" | "nl" | "bat" | "less" | "more" => {
                for f in positional(args) {
                    current.push((cwd.join(f), 1, u32::MAX));
                }
            }
            "head" | "tail" => {
                let (count, rest) = line_count(args);
                let files = positional(&rest);
                let n: u32 = count
                    .as_deref()
                    .map(|c| c.trim_start_matches('+'))
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(10);
                let from_start = count.as_deref().is_some_and(|c| c.starts_with('+'));
                let part = |len: u32| -> (u32, u32) {
                    match (prog, from_start) {
                        ("head", _) => (1, n.max(1)),
                        (_, true) => (n.max(1), u32::MAX),
                        _ => (len.saturating_sub(n).saturating_add(1).max(1), u32::MAX),
                    }
                };
                if files.is_empty() {
                    current = current
                        .into_iter()
                        .filter_map(|(p, a, b)| {
                            let len = if b == u32::MAX {
                                file_lines(&p)
                            } else {
                                b - a + 1
                            };
                            narrow((a, b), part(len)).map(|(x, y)| (p, x, y))
                        })
                        .collect();
                } else {
                    for f in files {
                        let path = cwd.join(f);
                        let (a, b) = part(file_lines(&path));
                        current.push((path, a, b));
                    }
                }
            }
            "sed" => {
                let quiet = args
                    .iter()
                    .any(|a| a == "-n" || a == "--quiet" || a == "--silent");
                if !quiet {
                    continue;
                }
                let mut words: Vec<&String> = Vec::new();
                let mut script = None;
                let mut i = 0;
                while i < args.len() {
                    if args[i] == "-e" {
                        script = args.get(i + 1);
                        i += 2;
                        continue;
                    }
                    if !args[i].starts_with('-') {
                        words.push(&args[i]);
                    }
                    i += 1;
                }
                let (script, files) = match script {
                    Some(s) => (s, words),
                    None => match words.split_first() {
                        Some((s, rest)) => (*s, rest.to_vec()),
                        None => continue,
                    },
                };
                let ranges = sed_ranges(script);
                if files.is_empty() {
                    current = current
                        .into_iter()
                        .flat_map(|(p, a, b)| {
                            ranges
                                .iter()
                                .filter_map(|r| narrow((a, b), *r).map(|(x, y)| (p.clone(), x, y)))
                                .collect::<Vec<_>>()
                        })
                        .collect();
                } else {
                    for f in files {
                        for &(a, b) in &ranges {
                            current.push((cwd.join(f), a, b));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    t.files.append(&mut current);
    stages.clear();
    stages.push(Vec::new());
}

/// Files read and names searched by a shell command. `cd` changes the
/// directory later paths resolve against.
fn shell_targets(cmd: &str, cwd: &Path) -> Targets {
    let mut t = Targets::default();
    let Some(toks) = tokenize(cmd) else { return t };
    let mut cwd = cwd.to_path_buf();
    let mut pipeline: Vec<Vec<String>> = vec![Vec::new()];
    for tok in toks {
        match tok {
            Tok::Word(w) => {
                if let Some(stage) = pipeline.last_mut() {
                    stage.push(w);
                }
            }
            Tok::Pipe => pipeline.push(Vec::new()),
            Tok::End => finish(&mut pipeline, &mut cwd, &mut t),
        }
    }
    finish(&mut pipeline, &mut cwd, &mut t);
    t
}

fn file_lines(path: &Path) -> u32 {
    fs::read(path)
        .map(|b| b.iter().filter(|&&c| c == b'\n').count() as u32 + 1)
        .unwrap_or(u32::MAX)
}

// ----------------------------------------------------------- tool input ---

/// Files and names one tool call looked at, from its hook input.
fn tool_targets(input: &Value, cwd: &Path) -> Targets {
    let tool = input["tool_name"].as_str().unwrap_or_default();
    let args = &input["tool_input"];
    match tool {
        "Read" => {
            let Some(path) = args["file_path"].as_str() else {
                return Targets::default();
            };
            let start = args["offset"].as_u64().unwrap_or(1).max(1);
            let end = args["limit"]
                .as_u64()
                .map(|l| start.saturating_add(l.max(1) - 1))
                .unwrap_or(u64::from(u32::MAX));
            let clamp = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
            Targets {
                files: vec![(cwd.join(path), clamp(start), clamp(end))],
                names: Vec::new(),
            }
        }
        "Grep" => Targets {
            files: Vec::new(),
            names: args["pattern"]
                .as_str()
                .map(pattern_names)
                .unwrap_or_default(),
        },
        // Claude Code's Bash, and Codex, whose tool calls carry a command.
        _ => match args["command"].as_str() {
            Some(cmd) => shell_targets(cmd, cwd),
            None => match args["command"].as_array() {
                // Codex may send an argv array; `bash -lc "<script>"` is common.
                Some(argv) => {
                    let words: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
                    match words.as_slice() {
                        [sh, flag, script]
                            if sh.ends_with("sh")
                                && flag.starts_with('-')
                                && flag.contains('c') =>
                        {
                            shell_targets(script, cwd)
                        }
                        _ => shell_targets(&words.join(" "), cwd),
                    }
                }
                None => Targets::default(),
            },
        },
    }
}

// ----------------------------------------------------------- repo + state ---

/// The repository a hook event belongs to: the nearest directory at or above
/// `cwd` with a `.strata` index, else with `.git`, else `cwd` itself.
fn find_root(cwd: &Path) -> PathBuf {
    for marker in [".strata", ".git"] {
        if let Some(dir) = cwd.ancestors().find(|d| d.join(marker).exists()) {
            return dir.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

/// `path` relative to `root` with `/` separators, if it is inside it.
fn repo_rel(root: &Path, path: &Path) -> Option<String> {
    let abs = path.canonicalize().ok()?;
    let rel = abs.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Lines already shown in one agent session, so repeats are not re-sent.
struct Seen {
    path: Option<PathBuf>,
    lines: HashSet<String>,
}

impl Seen {
    fn load(root: &Path, session: Option<&str>) -> Self {
        let path = session
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .filter(|s| {
                s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
            .map(|s| {
                root.join(".strata")
                    .join("sessions")
                    .join(format!("{s}.seen"))
            });
        let lines = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| t.lines().map(str::to_string).collect())
            .unwrap_or_default();
        Seen { path, lines }
    }

    fn fresh(&self, fact: &Fact) -> bool {
        !self.lines.contains(&fact.text)
    }

    fn record(&self, facts: &[&Fact]) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        let text: String = facts.iter().map(|f| format!("{}\n", f.text)).collect();
        let _ = f.write_all(text.as_bytes());
    }

    fn reset(&self) {
        if let Some(p) = &self.path {
            let _ = fs::remove_file(p);
        }
    }
}

fn prune_sessions(root: &Path) {
    let Ok(entries) = fs::read_dir(root.join(".strata").join("sessions")) else {
        return;
    };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > SESSION_TTL);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// The graph, updated first if files changed since the last index.
fn fresh_graph(root: &Path) -> Result<Graph> {
    if stale_files(root)? > 0 {
        Ok(update_index(root)?.0)
    } else {
        open_index(root)
    }
}

// ----------------------------------------------------------------- events ---

fn session_start(root: &Path, input: &Value, seen: &Seen) -> Result<Option<String>> {
    prune_sessions(root);
    // After /clear or compaction the model no longer has earlier facts.
    if matches!(input["source"].as_str(), Some("clear" | "compact")) {
        seen.reset();
    }
    let g = update_index(root)?.0;
    let mut langs: Vec<&str> = g
        .files
        .keys()
        .filter_map(|f| strata_graph_core::lang::lang_for(Path::new(f)))
        .map(|l| match l {
            strata_graph_core::Lang::Python => "Python",
            strata_graph_core::Lang::TypeScript => "TypeScript/JavaScript",
        })
        .collect();
    langs.sort_unstable();
    langs.dedup();
    if g.files.is_empty() {
        return Ok(None);
    }
    let s = &g.stats;
    Ok(Some(format!(
        "[strata-graph] This repo has a code graph: {} files ({}), {} symbols, {} resolved calls. \
         File reads and code searches are followed by graph facts tagged [strata-graph]: who calls \
         a definition, what it calls, base classes and subclasses.\n\
         Tiers: T2 structural; T1 resolved through scopes, imports or types; T0 the only definition \
         with that name. Ambiguous matches and calls into libraries are left out, and \"no resolved \
         callers\" does not prove code is unused (dynamic calls are not tracked).\n\
         For more: `strata-graph callers|callees|impact|context SYMBOL`, or the strata_* MCP tools if connected.",
        g.files.len(),
        langs.join(", "),
        g.nodes.len(),
        s.resolved_t1 + s.resolved_t0,
    )))
}

fn post_tool_use(
    root: &Path,
    cwd: &Path,
    input: &Value,
    seen: &Seen,
    max_chars: usize,
) -> Result<Option<String>> {
    let targets = tool_targets(input, cwd);
    if targets.files.is_empty() && targets.names.is_empty() {
        return Ok(None);
    }
    // Building a large index inside a tool call would stall the agent; the
    // SessionStart hook (or `strata-graph index`) builds it.
    if !index_path(root).exists() {
        return Ok(None);
    }
    let g = fresh_graph(root)?;
    let tier = Tier::NameOnly;
    let mut sections: Vec<(String, Vec<Fact>)> = Vec::new();
    let mut files_done = 0;
    for (path, start, end) in &targets.files {
        if files_done == MAX_TARGETS {
            break;
        }
        let Some(rel) = repo_rel(root, path) else {
            continue;
        };
        if !g.files.contains_key(&rel) {
            continue;
        }
        files_done += 1;
        let facts: Vec<Fact> = range_facts(&g, &rel, *start, *end, tier)
            .into_iter()
            .filter(|f| seen.fresh(f))
            .collect();
        if !facts.is_empty() {
            let lines = if *end == u32::MAX && *start <= 1 {
                String::new()
            } else if *end == u32::MAX {
                format!(":{start}-end")
            } else {
                format!(":{start}-{end}")
            };
            sections.push((format!("[strata-graph] {rel}{lines}"), facts));
        }
    }
    for name in targets.names.iter().take(MAX_TARGETS) {
        let facts: Vec<Fact> = name_facts(&g, name, tier)
            .into_iter()
            .filter(|f| seen.fresh(f))
            .collect();
        if !facts.is_empty() {
            sections.push((format!("[strata-graph] definitions named {name}"), facts));
        }
    }
    if sections.is_empty() {
        return Ok(None);
    }
    // Share the budget between sections; render() trims each to its share.
    let share = max_chars / sections.len();
    let mut out = String::new();
    let mut shown: Vec<&Fact> = Vec::new();
    for (header, facts) in &sections {
        let text = render(header, facts, share);
        shown.extend(facts.iter().filter(|f| text.lines().any(|l| l == f.text)));
        out.push_str(&text);
    }
    seen.record(&shown);
    Ok(Some(out.trim_end().to_string()))
}

/// Handle one hook event. Returns the JSON reply to print, if any.
fn respond(input: &Value, root_override: Option<&Path>, max_chars: usize) -> Result<Option<Value>> {
    let event = input["hook_event_name"].as_str().unwrap_or_default();
    let cwd = match input["cwd"].as_str() {
        Some(c) => PathBuf::from(c),
        None => std::env::current_dir()?,
    };
    let root = match root_override {
        Some(r) => r.to_path_buf(),
        None => find_root(&cwd),
    }
    .canonicalize()
    .context("resolving repository root")?;
    let seen = Seen::load(&root, input["session_id"].as_str());
    let text = match event {
        "SessionStart" => session_start(&root, input, &seen)?,
        "PostToolUse" => post_tool_use(&root, &cwd, input, &seen, max_chars)?,
        _ => None,
    };
    Ok(text.map(
        |t| json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": t } }),
    ))
}

/// Entry point for `strata-graph hook`. Never returns an error to the agent:
/// failures are reported on stderr (visible in the agent's debug output).
pub fn run(root_override: Option<&Path>, max_chars: usize) -> Result<()> {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().take(MAX_INPUT).read_to_string(&mut raw) {
        eprintln!("strata-graph hook: could not read input: {e}");
        return Ok(());
    }
    let reply = serde_json::from_str::<Value>(&raw)
        .context("hook input is not JSON")
        .and_then(|input| respond(&input, root_override, max_chars));
    match reply {
        Ok(Some(v)) => println!("{v}"),
        Ok(None) => {}
        Err(e) => eprintln!("strata-graph hook: {e:#}"),
    }
    Ok(())
}

// ---------------------------------------------------------------- install ---

/// Which agent's settings to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    /// Claude Code: `.claude/settings.json` (or `settings.local.json`).
    Claude,
    /// Codex: `.codex/hooks.json`.
    Codex,
}

fn hook_entry(matcher: Option<&str>, command: &str, timeout: Option<u64>) -> Value {
    let mut handler = json!({ "type": "command", "command": command });
    if let Some(t) = timeout {
        handler["timeout"] = json!(t);
    }
    let mut group = json!({ "hooks": [handler] });
    if let Some(m) = matcher {
        group["matcher"] = json!(m);
    }
    group
}

/// Add our hook groups to `settings`, leaving every other key and hook as
/// it is. Events that already run `strata-graph hook` are left alone.
/// Returns the events that were added.
fn merge_hooks(settings: &mut Value, agent: Agent, command: &str) -> Result<Vec<&'static str>> {
    let Some(obj) = settings.as_object_mut() else {
        bail!("settings file is not a JSON object");
    };
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("\"hooks\" in the settings file is not an object");
    };
    let wanted: [(&'static str, Option<&str>, Option<u64>); 2] = match agent {
        Agent::Claude => [
            ("SessionStart", None, Some(120)),
            ("PostToolUse", Some("Read|Grep|Bash"), Some(30)),
        ],
        Agent::Codex => [
            ("SessionStart", Some("startup|resume|clear"), None),
            ("PostToolUse", Some("Bash"), None),
        ],
    };
    let mut added = Vec::new();
    for (event, matcher, timeout) in wanted {
        let list = hooks.entry(event).or_insert_with(|| json!([]));
        let Some(list) = list.as_array_mut() else {
            bail!("hooks.{event} in the settings file is not a list");
        };
        let present = list.iter().any(|g| {
            g["hooks"].as_array().is_some_and(|hs| {
                hs.iter()
                    .any(|h| h["command"].as_str().is_some_and(|c| c.contains(MARKER)))
            })
        });
        if !present {
            list.push(hook_entry(matcher, command, timeout));
            added.push(event);
        }
    }
    Ok(added)
}

/// Entry point for `strata-graph install-hooks`.
pub fn install(
    root: &Path,
    agent: Agent,
    local: bool,
    command: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let command = command.unwrap_or(MARKER);
    let path = match (agent, local) {
        (Agent::Claude, false) => root.join(".claude").join("settings.json"),
        (Agent::Claude, true) => root.join(".claude").join("settings.local.json"),
        (Agent::Codex, _) => root.join(".codex").join("hooks.json"),
    };
    let mut settings = match fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => json!({}),
        Ok(text) => serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not valid JSON; fix it or remove it first",
                path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let added = merge_hooks(&mut settings, agent, command)?;
    let text = serde_json::to_string_pretty(&settings)? + "\n";
    if dry_run {
        print!("{text}");
        return Ok(());
    }
    if added.is_empty() {
        println!("{}: strata-graph hooks already installed", path.display());
    } else {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        println!("{}: added {} hook(s)", path.display(), added.join(" and "));
    }
    if !index_path(root).exists() {
        println!("tip: run `strata-graph index` once now; later sessions keep it fresh.");
    }
    if agent == Agent::Codex {
        println!("Codex hooks are behind a feature flag: add `codex_hooks = true` under [features] in ~/.codex/config.toml.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Tok {
        Tok::Word(s.to_string())
    }

    #[test]
    fn tokenizer_handles_quotes_and_operators() {
        assert_eq!(
            tokenize(r#"cd src && rg -n "def foo\(" | head -5; echo 'a b'"#).unwrap(),
            vec![
                w("cd"),
                w("src"),
                Tok::End,
                w("rg"),
                w("-n"),
                w(r"def foo\("),
                Tok::Pipe,
                w("head"),
                w("-5"),
                Tok::End,
                w("echo"),
                w("a b")
            ]
        );
        assert!(tokenize("cat $(git ls-files)").is_none());
        assert!(tokenize("echo 'open").is_none());
    }

    #[test]
    fn shell_reads_and_searches_are_recognised() {
        let cwd = Path::new("/repo");
        let t = shell_targets("sed -n '10,40p' shop/api.py", cwd);
        assert_eq!(t.files, vec![(PathBuf::from("/repo/shop/api.py"), 10, 40)]);

        let t = shell_targets("nl -ba shop/api.py | sed -n '5,9p;20,$p'", cwd);
        assert_eq!(
            t.files,
            vec![
                (PathBuf::from("/repo/shop/api.py"), 5, 9),
                (PathBuf::from("/repo/shop/api.py"), 20, u32::MAX)
            ]
        );

        let t = shell_targets("cd shop && head -n 30 api.py", cwd);
        assert_eq!(t.files, vec![(PathBuf::from("/repo/shop/api.py"), 1, 30)]);

        let t = shell_targets("cat a.py b.py | head -50", cwd);
        assert_eq!(t.files.len(), 2);
        assert!(
            t.files.iter().all(|f| f.1 == 1 && f.2 == 50),
            "{:?}",
            t.files
        );

        let t = shell_targets(r#"rg -n -g '*.py' "class\s+TaxCalculator" shop"#, cwd);
        assert_eq!(t.names, ["TaxCalculator"]);
        let t = shell_targets("grep -rn round_money .", cwd);
        assert_eq!(t.names, ["round_money"]);
        let t = shell_targets("git grep -e 'Invoice.total'", cwd);
        assert_eq!(t.names, ["Invoice", "total"]);

        // Commands that read nothing, or that we cannot follow, give nothing.
        assert_eq!(
            shell_targets("cargo test && ls -la", cwd),
            Targets::default()
        );
        assert_eq!(shell_targets("sed 's/a/b/' x.py", cwd), Targets::default());
        assert_eq!(
            shell_targets("cat $(find . -name '*.py')", cwd),
            Targets::default()
        );
    }

    #[test]
    fn claude_tool_inputs_are_recognised() {
        let cwd = Path::new("/repo");
        let read = json!({ "tool_name": "Read", "tool_input": { "file_path": "/repo/a.py", "offset": 20, "limit": 10 } });
        assert_eq!(
            tool_targets(&read, cwd).files,
            vec![(PathBuf::from("/repo/a.py"), 20, 29)]
        );
        let whole = json!({ "tool_name": "Read", "tool_input": { "file_path": "/repo/a.py" } });
        assert_eq!(
            tool_targets(&whole, cwd).files,
            vec![(PathBuf::from("/repo/a.py"), 1, u32::MAX)]
        );
        let grep = json!({ "tool_name": "Grep", "tool_input": { "pattern": "def round_money" } });
        assert_eq!(tool_targets(&grep, cwd).names, ["round_money"]);
        let codex = json!({ "tool_name": "Bash", "tool_input": { "command": ["bash", "-lc", "sed -n '1,5p' a.py"] } });
        assert_eq!(
            tool_targets(&codex, cwd).files,
            vec![(PathBuf::from("/repo/a.py"), 1, 5)]
        );
    }

    #[test]
    fn merging_keeps_existing_settings_and_is_idempotent() {
        let mut s = json!({
            "model": "opus",
            "hooks": { "PostToolUse": [{ "matcher": "Edit", "hooks": [{ "type": "command", "command": "fmt.sh" }] }] }
        });
        let added = merge_hooks(&mut s, Agent::Claude, MARKER).unwrap();
        assert_eq!(added, ["SessionStart", "PostToolUse"]);
        assert_eq!(s["model"], "opus");
        let post = s["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2);
        assert_eq!(post[0]["hooks"][0]["command"], "fmt.sh");
        assert_eq!(post[1]["matcher"], "Read|Grep|Bash");
        assert_eq!(post[1]["hooks"][0]["timeout"], 30);
        assert!(merge_hooks(&mut s, Agent::Claude, MARKER)
            .unwrap()
            .is_empty());

        let mut c = json!({});
        merge_hooks(&mut c, Agent::Codex, MARKER).unwrap();
        assert_eq!(c["hooks"]["PostToolUse"][0]["matcher"], "Bash");
        assert!(c["hooks"]["PostToolUse"][0]["hooks"][0]
            .get("timeout")
            .is_none());
        assert!(merge_hooks(&mut json!([]), Agent::Codex, MARKER).is_err());
    }
}
