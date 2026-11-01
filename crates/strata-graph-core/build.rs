//! Fingerprint the parser and resolver sources, so an index written by a
//! different engine build is re-parsed or re-resolved automatically instead
//! of depending on a hand-bumped revision number.

use std::fs;
use std::path::{Path, PathBuf};

fn files_under(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .expect("readable source dir")
            .map(|e| e.expect("readable dir entry").path())
            .collect();
        entries.sort();
        for e in entries {
            files_under(&e, out);
        }
    } else {
        out.push(path.to_path_buf());
    }
}

/// BLAKE3 of the files under `paths`, by relative path and content, with
/// line endings normalised so checkouts on every OS give the same value.
fn fingerprint(paths: &[&str]) -> String {
    let mut files = Vec::new();
    for p in paths {
        println!("cargo:rerun-if-changed={p}");
        files_under(Path::new(p), &mut files);
    }
    let mut h = blake3::Hasher::new();
    for f in files {
        let text = fs::read_to_string(&f).expect("UTF-8 source file");
        h.update(f.to_string_lossy().replace('\\', "/").as_bytes());
        h.update(&[0]);
        h.update(text.replace("\r\n", "\n").as_bytes());
        h.update(&[0]);
    }
    h.finalize().to_hex()[..16].to_string()
}

fn main() {
    // What parsing produces: cached per-file facts depend on these.
    let parser = fingerprint(&["src/lang", "queries", "src/model.rs"]);
    // What resolution produces from those facts.
    let resolver = fingerprint(&["src/resolve", "src/config.rs", "src/model.rs"]);
    println!("cargo:rustc-env=STRATA_PARSER_HASH={parser}");
    println!("cargo:rustc-env=STRATA_RESOLVER_HASH={resolver}");
}
