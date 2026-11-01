//! Module naming and import-specifier resolution for each language.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::NodeId;

/// Dotted module name for a repo-relative path, following `__init__.py` packages.
pub(super) fn module_name(rel: &str, package_dirs: &HashSet<String>) -> (String, bool) {
    let rel = rel.strip_suffix(".py").unwrap_or(rel);
    let mut parts: Vec<&str> = rel.split('/').collect();
    let stem = parts.pop().unwrap_or("");
    let is_init = stem == "__init__";
    // Top down, a directory is a package if it has an __init__.py, or if its
    // parent is a package (a PEP 420 namespace subpackage such as flask/sansio).
    let mut is_pkg = vec![false; parts.len()];
    for i in 0..parts.len() {
        let dir = parts[..=i].join("/");
        is_pkg[i] = package_dirs.contains(&dir) || (i > 0 && is_pkg[i - 1]);
    }
    // Keep the trailing run of package directories.
    let keep = is_pkg.iter().rev().take_while(|p| **p).count();
    let mut out: Vec<&str> = parts[parts.len() - keep..].to_vec();
    if !is_init {
        out.push(stem);
    }
    let name = if out.is_empty() {
        stem.to_string()
    } else {
        out.join(".")
    };
    (name, is_init)
}

/// Absolute module for a possibly relative `from` target.
pub(super) fn absolute_from(from: &str, module: &str, is_init: bool) -> Option<String> {
    let dots = from.chars().take_while(|c| *c == '.').count();
    if dots == 0 {
        return Some(from.to_string());
    }
    let mut pkg: Vec<&str> = module.split('.').collect();
    if !is_init {
        pkg.pop();
    }
    for _ in 1..dots {
        pkg.pop()?;
    }
    let rest = &from[dots..];
    let mut s = pkg.join(".");
    if !rest.is_empty() {
        if !s.is_empty() {
            s.push('.');
        }
        s.push_str(rest);
    }
    Some(s)
}

/// Project-level settings that change how imports resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// TypeScript `baseUrl`, relative to the repo root ("" is the root).
    pub base_url: Option<String>,
    /// TypeScript `paths`: pattern -> targets, targets relative to the repo root.
    pub paths: Vec<(String, Vec<String>)>,
    /// JS/TS packages defined inside the repo (monorepo workspaces), so that
    /// `import "pkg/sub"` resolves to the package's own source files.
    pub packages: Vec<WorkspacePackage>,
}

/// A package.json found in the repo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePackage {
    /// The package's `name`.
    pub name: String,
    /// Package directory, relative to the repo root.
    pub dir: String,
    /// `exports` subpath (".", "./sub", "./*") -> every target path it lists,
    /// relative to the repo root, in file order.
    pub exports: Vec<(String, Vec<String>)>,
    /// `source`, `module`, `main` and `types` fields, relative to the repo root.
    pub entries: Vec<String>,
}

/// Module name for a TypeScript/JavaScript file: its path without extension.
pub(super) fn ts_module_name(rel: &str) -> String {
    for ext in [".d.ts", ".d.mts", ".d.cts"] {
        if let Some(stem) = rel.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    match rel.rsplit_once('.') {
        Some((stem, _)) if !stem.ends_with('/') => stem.to_string(),
        _ => rel.to_string(),
    }
}

/// Collapse `.` and `..` segments; None if the path climbs above the root.
pub(super) fn normalize_rel(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

pub(super) fn join_rel(dir: &str, rest: &str) -> String {
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

/// Resolve an import specifier in `from_rel` to a repo module name: relative
/// paths, `index` files, `.js` specifiers that point at `.ts` sources, and
/// tsconfig `paths` / `baseUrl`. None means a package outside the repo.
pub(super) fn ts_resolve(
    modules: &HashMap<String, NodeId>,
    config: &ProjectConfig,
    from_rel: &str,
    spec: &str,
) -> Option<String> {
    let dir = from_rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut bases = Vec::new();
    if spec.starts_with('.') {
        bases.push(join_rel(dir, spec));
    } else {
        for (pattern, targets) in &config.paths {
            if let Some(prefix) = pattern.strip_suffix('*') {
                if let Some(rest) = spec.strip_prefix(prefix) {
                    bases.extend(targets.iter().map(|t| t.replacen('*', rest, 1)));
                }
            } else if pattern == spec {
                bases.extend(targets.iter().cloned());
            }
        }
        for pkg in &config.packages {
            let sub = if spec == pkg.name {
                ".".to_string()
            } else if let Some(rest) = spec.strip_prefix(&format!("{}/", pkg.name)) {
                format!("./{rest}")
            } else {
                continue;
            };
            for (key, targets) in &pkg.exports {
                if *key == sub {
                    bases.extend(targets.iter().cloned());
                } else if let Some((pre, post)) = key.split_once('*') {
                    if let Some(mid) = sub.strip_prefix(pre).and_then(|r| r.strip_suffix(post)) {
                        bases.extend(targets.iter().map(|t| t.replacen('*', mid, 1)));
                    }
                }
            }
            if sub == "." {
                bases.extend(pkg.entries.iter().cloned());
                bases.push(join_rel(&pkg.dir, "src/index"));
                bases.push(join_rel(&pkg.dir, "index"));
            } else {
                let rest = &sub[2..];
                bases.push(join_rel(&pkg.dir, &format!("src/{rest}")));
                bases.push(join_rel(&pkg.dir, rest));
            }
        }
        if let Some(b) = &config.base_url {
            bases.push(join_rel(b, spec));
        }
    }
    for b in bases {
        let Some(norm) = normalize_rel(&b) else {
            continue;
        };
        let mut stem = norm.as_str();
        for ext in [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"] {
            if let Some(s) = stem.strip_suffix(ext) {
                stem = s;
                break;
            }
        }
        for cand in [stem.to_string(), join_rel(stem, "index")] {
            if modules.contains_key(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_follow_packages() {
        let pk: HashSet<String> = ["src/app", "src/app/core"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            module_name("src/app/core/db.py", &pk),
            ("app.core.db".into(), false)
        );
        assert_eq!(
            module_name("src/app/__init__.py", &pk),
            ("app".into(), true)
        );
        assert_eq!(module_name("scripts/run.py", &pk), ("run".into(), false));
        // Namespace subpackage under a regular package.
        assert_eq!(
            module_name("src/app/sansio/core.py", &pk),
            ("app.sansio.core".into(), false)
        );
    }

    #[test]
    fn relative_imports() {
        assert_eq!(
            absolute_from(".models", "app.api.views", false).as_deref(),
            Some("app.api.models")
        );
        assert_eq!(
            absolute_from("..core", "app.api.views", false).as_deref(),
            Some("app.core")
        );
        assert_eq!(
            absolute_from(".", "app.api", true).as_deref(),
            Some("app.api")
        );
        assert_eq!(absolute_from("...x", "a.b", false), None);
    }
}
