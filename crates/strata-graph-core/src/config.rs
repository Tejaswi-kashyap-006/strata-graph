//! Import settings read from the repo: the root tsconfig/jsconfig (`paths`,
//! `baseUrl`, `extends`) and the repo's own JS/TS packages (package.json).

use std::path::Path;

use crate::resolve::{self, ProjectConfig};

/// Strip `//` and `/* */` comments, then trailing commas, from JSONC (tsconfig) text.
/// Two passes, so a comment between a trailing comma and `}` is handled.
fn strip_jsonc(src: &str) -> String {
    // Pass 1: comments, leaving string contents (e.g. "https://...") intact.
    let b = src.as_bytes();
    let mut no_comments = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            no_comments.push(c);
            if c == b'\\' && i + 1 < b.len() {
                no_comments.push(b[i + 1]);
                i += 2;
                continue;
            }
            in_str = c != b'"';
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            in_str = c == b'"';
            no_comments.push(c);
            i += 1;
        }
    }
    // Pass 2: commas whose next non-space character closes an object or array.
    let mut out = Vec::with_capacity(no_comments.len());
    let mut in_str = false;
    let mut i = 0;
    while i < no_comments.len() {
        let c = no_comments[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < no_comments.len() {
                out.push(no_comments[i + 1]);
                i += 2;
                continue;
            }
            in_str = c != b'"';
        } else if c == b',' {
            let next = no_comments[i + 1..]
                .iter()
                .find(|x| !x.is_ascii_whitespace());
            if !matches!(next, Some(b'}') | Some(b']')) {
                out.push(c);
            }
        } else {
            in_str = c == b'"';
            out.push(c);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn join(dir: &str, rest: &str) -> String {
    let rest = rest.trim_start_matches("./");
    let joined = if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    };
    // Normalise "a/b/../c" and a bare "." to root-relative form.
    let mut out: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// Read `rel` (a tsconfig/jsconfig) and its relative `extends` chain into `cfg`.
/// Parents are applied first so the child's settings win.
fn load_tsconfig(root: &Path, rel: &str, cfg: &mut ProjectConfig, depth: u32) {
    if depth > 4 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else {
        return;
    };
    let dir = dir_of(rel);
    if let Some(parent) = json.get("extends").and_then(|e| e.as_str()) {
        if parent.starts_with('.') {
            let mut p = join(dir, parent);
            if !p.ends_with(".json") {
                p.push_str(".json");
            }
            load_tsconfig(root, &p, cfg, depth + 1);
        }
    }
    let Some(opts) = json.get("compilerOptions") else {
        return;
    };
    let base = opts
        .get("baseUrl")
        .and_then(|b| b.as_str())
        .map(|b| join(dir, b));
    if let Some(b) = &base {
        cfg.base_url = Some(b.clone());
    }
    if let Some(paths) = opts.get("paths").and_then(|p| p.as_object()) {
        let target_dir = base.clone().unwrap_or_else(|| dir.to_string());
        cfg.paths = paths
            .iter()
            .map(|(pattern, targets)| {
                let ts = targets
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str())
                    .map(|t| join(&target_dir, t))
                    .collect();
                (pattern.clone(), ts)
            })
            .collect();
    }
}

/// Every string inside a package.json `exports` value, in file order.
fn string_leaves(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| string_leaves(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| string_leaves(x, out)),
        _ => {}
    }
}

/// package.json files inside the repo that declare a name. Ignore files are
/// not consulted here: some packages gitignore their own package.json
/// (zod's `packages/zod/.gitignore` does). Copies in build output are harmless,
/// since resolution only accepts targets that exist as indexed source files.
fn workspace_packages(root: &Path) -> Vec<resolve::WorkspacePackage> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .filter_entry(|e| {
            let n = e.file_name();
            n != ".strata" && n != "node_modules" && n != ".git"
        })
        .build()
        .flatten()
    {
        if entry.file_name() != "package.json" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(name) = json.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let dir = dir_of(&rel).to_string();
        let mut exports = Vec::new();
        match json.get("exports") {
            Some(serde_json::Value::Object(o)) if o.keys().any(|k| k.starts_with('.')) => {
                for (key, value) in o {
                    let mut leaves = Vec::new();
                    string_leaves(value, &mut leaves);
                    exports.push((key.clone(), leaves.iter().map(|t| join(&dir, t)).collect()));
                }
            }
            Some(v) => {
                let mut leaves = Vec::new();
                string_leaves(v, &mut leaves);
                exports.push((
                    ".".to_string(),
                    leaves.iter().map(|t| join(&dir, t)).collect(),
                ));
            }
            None => {}
        }
        let entries = ["source", "module", "main", "types"]
            .iter()
            .filter_map(|k| json.get(*k).and_then(|v| v.as_str()))
            .map(|t| join(&dir, t))
            .collect();
        out.push(resolve::WorkspacePackage {
            name: name.to_string(),
            dir,
            exports,
            entries,
        });
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// Import settings: the root `tsconfig.json` (or `jsconfig.json`) and the
/// repo's own JS/TS packages.
pub fn project_config(root: &Path) -> ProjectConfig {
    let mut cfg = ProjectConfig::default();
    for name in ["tsconfig.json", "jsconfig.json"] {
        if root.join(name).is_file() {
            load_tsconfig(root, name, &mut cfg, 0);
            break;
        }
    }
    cfg.packages = workspace_packages(root);
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_with_comments_and_trailing_commas() {
        let src = r#"{
  // line comment
  "a": "https://x.dev/*not-a-comment*/", /* block */
  "b": [1, 2,], /* comment after a trailing comma */
}"#;
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(src)).unwrap();
        assert_eq!(v["a"], "https://x.dev/*not-a-comment*/");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }

    #[test]
    fn reads_fixture_tsconfig_paths() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ts_shop");
        let cfg = project_config(&root);
        assert_eq!(
            cfg.paths,
            vec![("@/*".to_string(), vec!["src/*".to_string()])]
        );
    }
}
