//! Single writer for the tracked bead projection (ADR-0024 amendment A,
//! `rosary-e5fb88`).
//!
//! ## The defect this closes
//!
//! Measured 2026-09-12: twelve direct writes to `.beads/beads.jsonl` outside
//! the publish module — five in `main.rs`, seven in the MCP handlers. Each one
//! was a second place that *knew when the projection is written*: the
//! re-declaration class `rosary-c1f669` named, live in the substrate that was
//! supposed to have closed it. Under amendment A the answer is fixed — the
//! hooks publish at commit time and on the trunk, nothing else does — so any
//! call site that reaches a projection-writing primitive from anywhere but
//! `src/publish/` is wrong by construction.
//!
//! ## How the check derives, rather than copies
//!
//! `src/column_rail.rs` style: the set of "projection-writing primitives" is
//! not a list maintained here. It is read out of the authority,
//! `src/jsonl_sync.rs` — every `pub` function that reaches `atomic_replace`
//! (the one syscall-level write in that file), directly or through another
//! writer. Add a new writer there and this gate widens on its own; rename one
//! and the gate follows. The only thing declared here is WHERE a writer may be
//! named: `src/publish/**` and the authority itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The one module that owns the write primitives.
const AUTHORITY: &str = "src/jsonl_sync.rs";
/// The one module allowed to call them.
const WRITER_MODULE: &str = "src/publish";
/// The syscall-level write every projection writer bottoms out in.
const SINK: &str = "atomic_replace(";

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `Some((name, is_pub))` when `line` declares a function at the top level of
/// a source file — no indentation, so methods and nested fns do not count.
fn top_level_fn_decl(line: &str) -> Option<(String, bool)> {
    let trimmed = line.trim_start();
    if trimmed.len() != line.len() {
        return None;
    }
    let is_pub = trimmed.starts_with("pub ");
    let rest = trimmed
        .trim_start_matches("pub ")
        .trim_start_matches("async ");
    let sig = rest.strip_prefix("fn ")?;
    let name = sig.split(['(', '<']).next()?;
    Some((name.to_string(), is_pub))
}

/// `(name, is_pub, body)` for every function declared at the top level of a
/// source file. Bodies run to the next top-level `fn` or `#[cfg(test)]`, which
/// is crude but exact for what this file needs: "does this body mention X".
fn top_level_fns(src: &str) -> Vec<(String, bool, String)> {
    let lines: Vec<&str> = src.lines().collect();
    // Every boundary a body can end at; a `#[cfg(test)]` boundary has no name.
    let boundaries: Vec<(usize, Option<(String, bool)>)> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            if *line == "#[cfg(test)]" {
                Some((idx, None))
            } else {
                top_level_fn_decl(line).map(|decl| (idx, Some(decl)))
            }
        })
        .collect();
    boundaries
        .iter()
        .enumerate()
        .filter_map(|(i, (start, decl))| {
            let (name, is_pub) = decl.clone()?;
            let end = boundaries.get(i + 1).map_or(lines.len(), |b| b.0);
            Some((name, is_pub, lines[*start..end].join("\n")))
        })
        .collect()
}

/// Every `pub` fn in the authority that reaches the sink — the transitive
/// closure, so a wrapper around a writer is itself a writer.
fn projection_writers(authority_src: &str) -> BTreeSet<String> {
    let fns: BTreeMap<String, (bool, String)> = top_level_fns(authority_src)
        .into_iter()
        .map(|(name, is_pub, body)| (name, (is_pub, body)))
        .collect();
    let mut writers: BTreeSet<String> = fns
        .iter()
        .filter(|(_, (_, body))| body.contains(SINK))
        .map(|(name, _)| name.clone())
        .collect();
    loop {
        let before = writers.len();
        for (name, (_, body)) in &fns {
            if writers.iter().any(|w| body.contains(&format!("{w}("))) {
                writers.insert(name.clone());
            }
        }
        if writers.len() == before {
            break;
        }
    }
    writers.into_iter().filter(|name| fns[name].0).collect()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Code with `//` comments removed, so a doc comment may still *explain* a
/// writer by name without being a call to it.
fn without_line_comments(line: &str) -> &str {
    line.split_once("//").map_or(line, |(code, _)| code)
}

fn mentions_identifier(code: &str, ident: &str) -> bool {
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
    let mut rest = code;
    while let Some(i) = rest.find(ident) {
        let before_ok = i == 0 || !rest[..i].ends_with(is_ident_char);
        let after = &rest[i + ident.len()..];
        let after_ok = !after.starts_with(is_ident_char);
        if before_ok && after_ok {
            return true;
        }
        rest = after;
    }
    false
}

#[test]
fn the_derivation_distinguishes_writers_from_renderers() {
    let src = std::fs::read_to_string(crate_root().join(AUTHORITY)).unwrap();
    let writers = projection_writers(&src);
    assert!(
        !writers.is_empty(),
        "no projection writer derived from {AUTHORITY} — the gate below would be vacuous"
    );
    assert!(
        !writers.contains("export_published_beads_contract_jsonl"),
        "a pure renderer returns a String and must not count as a writer: {writers:?}"
    );
    assert!(
        writers.contains("refresh_tracked_beads_jsonl"),
        "the bounded whole-file refresh is the archetypal writer: {writers:?}"
    );
}

#[test]
fn no_call_site_outside_the_publish_module_writes_the_projection() {
    let root = crate_root();
    let authority = root.join(AUTHORITY);
    let writer_module = root.join(WRITER_MODULE);
    let writers = projection_writers(&std::fs::read_to_string(&authority).unwrap());

    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources);
    let mut violations = Vec::new();
    for path in sources {
        if path == authority || path.starts_with(&writer_module) {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        for (idx, line) in src.lines().enumerate() {
            let code = without_line_comments(line);
            for writer in &writers {
                if mentions_identifier(code, writer) {
                    violations.push(format!(
                        "{}:{}: {writer}",
                        path.strip_prefix(&root).unwrap().display(),
                        idx + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "the tracked projection has ONE writer, {WRITER_MODULE}/ (ADR-0024 amendment A). \
         These sites name a projection-writing primitive from {AUTHORITY} elsewhere — \
         a second place that knows when the projection is written:\n  {}",
        violations.join("\n  ")
    );
}
