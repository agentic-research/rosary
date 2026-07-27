//! Coordination-tier records, stored in git refs instead of the working tree.
//!
//! ADR-0022 decides that **bead location derives from role**, and that the
//! symptoms measured on 2026-07-27 were not a wrong canonical location but two
//! *missing* ones. This module is the coordination home: the tier for
//! agent-dispatch notes, run events, and feature-local scratch that has no
//! business landing in a code commit.
//!
//! ## Why refs are right here, having been wrong for canonical
//!
//! ADR-0022 Q3 measured that `+refs/heads/*` is the default refspec, so a plain
//! `git clone` and `actions/checkout` fetch nothing else. For canonical beads
//! that is disqualifying — a fresh clone would have no work record. For
//! coordination it is exactly the point: agent chatter should not clutter a
//! clone, a PR diff, or GitHub's UI, and it should not need a merge driver.
//!
//! ## Concurrency
//!
//! Every update is a **compare-and-swap** on the ref
//! (`git update-ref <ref> <new> <old>`), which is ADR-0020's "write-all-blobs-
//! then-CAS-the-ref is the single linearization point" applied literally. Two
//! agents appending to the same namespace cannot silently clobber one another:
//! the loser's CAS fails and it retries against the new tip. Contrast the
//! working-tree file, where the same race is a merge conflict.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Ref namespace for coordination records. Deliberately NOT `refs/heads/*`, so
/// these never appear as branches and are never fetched by default.
pub const NAMESPACE: &str = "refs/agents";

/// Number of CAS attempts before giving up. Contention here is two agents
/// appending to the *same* dispatch, which is rare; a small bound is honest
/// about the failure rather than spinning.
const MAX_CAS_ATTEMPTS: usize = 5;

fn ref_for(name: &str) -> String {
    format!("{NAMESPACE}/{name}")
}

fn git(repo_root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))
}

/// Resolve a ref to its object id, or `None` when it does not exist.
fn resolve(repo_root: &Path, r: &str) -> Result<Option<String>> {
    let out = git(repo_root, &["rev-parse", "--verify", "--quiet", r])?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// Write `content` as a loose blob and return its oid.
fn write_blob(repo_root: &Path, content: &str) -> Result<String> {
    use std::io::Write as _;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning git hash-object")?;
    child
        .stdin
        .as_mut()
        .context("git hash-object stdin unavailable")?
        .write_all(content.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "git hash-object failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read a namespace's records as raw JSONL. Returns `None` if the namespace
/// has never been written — distinct from "written and empty", which is `Some("")`.
pub fn read(repo_root: &Path, name: &str) -> Result<Option<String>> {
    let r = ref_for(name);
    let Some(oid) = resolve(repo_root, &r)? else {
        return Ok(None);
    };
    let out = git(repo_root, &["cat-file", "-p", &oid])?;
    if !out.status.success() {
        bail!(
            "reading {r}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Append one record line to a namespace, compare-and-swap.
///
/// `record` must be a single line — a newline would silently split it into two
/// records on read, so it is rejected rather than mangled.
pub fn append(repo_root: &Path, name: &str, record: &str) -> Result<()> {
    if record.contains('\n') {
        bail!("coordination record must be a single line (got an embedded newline)");
    }
    if record.trim().is_empty() {
        bail!("coordination record must not be blank");
    }
    let r = ref_for(name);

    for attempt in 1..=MAX_CAS_ATTEMPTS {
        let old = resolve(repo_root, &r)?;
        let existing = match &old {
            Some(_) => read(repo_root, name)?.unwrap_or_default(),
            None => String::new(),
        };
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(record);
        next.push('\n');

        let new_oid = write_blob(repo_root, &next)?;

        // CAS: name the expected old value so a concurrent writer cannot be
        // silently overwritten. Empty string means "must not exist yet".
        let old_arg = old.clone().unwrap_or_default();
        let out = git(repo_root, &["update-ref", &r, &new_oid, &old_arg])?;
        if out.status.success() {
            return Ok(());
        }
        if attempt == MAX_CAS_ATTEMPTS {
            bail!(
                "could not append to {r} after {MAX_CAS_ATTEMPTS} attempts \
                 (concurrent writer kept winning): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    unreachable!("loop returns or bails")
}

/// List coordination namespaces that currently exist.
pub fn list(repo_root: &Path) -> Result<Vec<String>> {
    let out = git(
        repo_root,
        &["for-each-ref", "--format=%(refname)", NAMESPACE],
    )?;
    if !out.status.success() {
        bail!(
            "listing {NAMESPACE}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let prefix = format!("{NAMESPACE}/");
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix(&prefix).map(str::to_string))
        .collect())
}

/// Delete a namespace. Coordination state is expected to be GC-able once its
/// dispatch is folded into the canonical record (ADR-0020 P4's open question 4).
pub fn delete(repo_root: &Path, name: &str) -> Result<bool> {
    let r = ref_for(name);
    if resolve(repo_root, &r)?.is_none() {
        return Ok(false);
    }
    let out = git(repo_root, &["update-ref", "-d", &r])?;
    if !out.status.success() {
        bail!(
            "deleting {r}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
