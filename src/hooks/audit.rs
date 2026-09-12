//! `rsry hooks audit` — the mechanical gate (rosary-b5c8a1): gitignore shadowing,
//! backend ambiguity, store/export drift, foreign-repo dependency shapes.
//!
//! Split out of the hooks module by the projection-timing scaffold
//! (rosary-e5befe) so the audit contract can change (rosary-e5fc0e) without
//! touching hook installation.

use super::*;
/// Unlike `status()` (purely informational), this is a GATE: returns
/// `Err` naming every failing check if any check fails, so `rsry hooks
/// audit` exits non-zero and is safe to script/CI against.
pub fn audit(repo_root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let beads_dir = repo_root.join(".beads");
    let jsonl_rel = ".beads/beads.jsonl";

    // --- 1. gitignore shadowing ----------------------------------------
    if beads_dir.exists() {
        let quiet = Command::new("git")
            .current_dir(repo_root)
            .args(["check-ignore", "-q", jsonl_rel])
            .output();
        // `-v`'s exit code means "some rule (possibly a negation) decided
        // the path" — NOT "is ignored" (verified live against notme.bot's
        // default-deny allowlist: `-v` exits 0 on the deciding `!pattern`
        // line even though the path is NOT ignored). Only `-q`'s exit code
        // has gitignore(5)'s real ignored/not-ignored semantics; `-v` is
        // fetched purely for the human-readable detail, only when needed.
        let verbose_detail = if matches!(&quiet, Ok(o) if o.status.success()) {
            Command::new("git")
                .current_dir(repo_root)
                .args(["check-ignore", "-v", jsonl_rel])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };
        match classify_gitignore_check(quiet, verbose_detail) {
            GitignoreCheck::Shadowed(detail) => {
                println!("  ✗ GITIGNORE-SHADOWED: {jsonl_rel} is blocked — {detail}");
                problems.push(format!("{jsonl_rel} is gitignore-shadowed: {detail}"));
            }
            GitignoreCheck::Reachable => {
                println!("  ✓ {jsonl_rel} reachable through .gitignore");
            }
            GitignoreCheck::Unknown(e) => {
                println!("  ? could not run git check-ignore: {e}");
            }
        }
    }

    // --- 2. backend ambiguity -------------------------------------------
    let sqlite_db = crate::bead_backend::sqlite_path(&beads_dir);
    let backend = crate::bead_backend::detect_backend(&beads_dir);
    let has_sqlite = matches!(backend, crate::bead_backend::BeadBackend::Sqlite);
    if backend.is_ambiguous() {
        println!(
            "  ✗ BACKEND-AMBIGUOUS: {} and {} both exist — two stores claim authority",
            crate::bead_backend::dolt_dir(&beads_dir).display(),
            sqlite_db.display()
        );
        problems.push(format!(
            "{} and {} both exist — ambiguous backend",
            crate::bead_backend::dolt_dir(&beads_dir).display(),
            sqlite_db.display()
        ));
    } else if beads_dir.exists() {
        println!("  ✓ no ambiguous backend coexistence");
    }

    // --- 3. store/export drift -------------------------------------------
    if has_sqlite {
        match count_sqlite_issues(&sqlite_db) {
            Ok(db_count) => {
                let jsonl_lines = std::fs::read_to_string(beads_dir.join("beads.jsonl"))
                    .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count())
                    .unwrap_or(0);
                if store_export_drifted(db_count, jsonl_lines) {
                    println!(
                        "  ✗ STORE/EXPORT DRIFT: beads.db has {db_count} bead(s), beads.jsonl has {jsonl_lines} line(s) — no durable copy"
                    );
                    problems.push(format!(
                        "{db_count} bead(s) in beads.db, only {jsonl_lines} in beads.jsonl"
                    ));
                } else {
                    println!(
                        "  ✓ store/export roughly agree (beads.db={db_count}, beads.jsonl={jsonl_lines} line(s))"
                    );
                }
            }
            Err(e) => println!("  ? could not read beads.db to check drift: {e}"),
        }
    }

    // --- 4. cross-repo dependency shape (rosary-d93ab7) -----------------
    if let Ok(content) = std::fs::read_to_string(beads_dir.join("beads.jsonl")) {
        let mut foreign = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let (Some(repo), Some(id)) = (
                v.get("repo").and_then(|r| r.as_str()),
                v.get("id").and_then(|r| r.as_str()),
            ) else {
                continue;
            };
            for dep in v
                .get("dependencies")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(dep_id) = dep.as_str()
                    && foreign_repo_dep(repo, dep_id)
                {
                    foreign.push(format!("{id} -> {dep_id}"));
                }
            }
        }
        if foreign.is_empty() {
            if beads_dir.exists() {
                println!("  ✓ no foreign-repo-shaped dependencies in beads.jsonl");
            }
        } else {
            println!(
                "  ✗ CROSS-REPO DEP SHAPE: {} committed dependenc{} look foreign-repo \
                 (rosary-d93ab7) — cross-repo edges belong in the global LinkageStore, \
                 never a same-repo dependencies array: {}",
                foreign.len(),
                if foreign.len() == 1 { "y" } else { "ies" },
                foreign.join(", ")
            );
            problems.push(format!(
                "{} foreign-repo-shaped committed dependencies: {}",
                foreign.len(),
                foreign.join(", ")
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} problem(s) found: {}",
            problems.len(),
            problems.join("; ")
        )
    }
}

/// True if `dep_id` looks like a bead id (`<repo>-<hex>`) whose repo
/// prefix differs from `bead_repo` — a same-repo `dependencies` entry
/// pointing at what is structurally a DIFFERENT repo's bead id.
/// Cross-repo edges belong exclusively in the global LinkageStore
/// (`~/.rsry/backend.db`'s `dependencies` table), never a per-repo
/// `dependencies` array; this shape can only arise from a caller
/// bypassing that routing (rosary-d93ab7 found 24 such entries across
/// 10 repos, all predating rosary-98ee93's auto-detection fix in
/// `rsry_bead_link`).
///
/// The hex-suffix check (>=6 lowercase hex chars after the last `-`)
/// mirrors the generated-id shape (`generate_bead_id`) so a same-repo id
/// that merely contains a hyphen (e.g. a literal "repo-name" prefix
/// that happens to differ, on a non-generated legacy id) doesn't
/// false-positive.
pub(crate) fn foreign_repo_dep(bead_repo: &str, dep_id: &str) -> bool {
    match dep_id.rsplit_once('-') {
        Some((prefix, suffix))
            if suffix.len() >= 6 && suffix.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            prefix != bead_repo
        }
        _ => false,
    }
}

/// Result of classifying a `git check-ignore -v` probe on
/// `.beads/beads.jsonl`. Mirrors [`DoltRemoteStatus`]'s shape: a pure
/// classifier over `io::Result<Output>` so it's unit- and
/// property-testable without spawning git.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GitignoreCheck {
    /// `check-ignore` matched — the export is unreachable regardless of
    /// how many times hooks are (re)installed. Carries the matching
    /// rule (`<file>:<line>:<pattern>\t<path>`).
    Shadowed(String),
    /// `check-ignore` found no match — the path is trackable.
    Reachable,
    /// The `git` binary itself couldn't be spawned.
    Unknown(String),
}

/// Classify a `git check-ignore -q <path>` invocation — `-q`'s exit code
/// is the one with real gitignore(5) ignored/not-ignored semantics (0 =
/// ignored, 1 = not ignored, anything higher is an error). `-v`'s exit
/// code is NOT equivalent: it reports 0 whenever any rule, including a
/// `!negation`, decided the path — so a `-v`-only classifier misreports
/// an explicitly un-ignored path as shadowed (found live against
/// notme.bot's default-deny allowlist, 2026-07-29). `verbose_detail` is
/// the caller's separately-fetched `-v` output, attached to `Shadowed`
/// purely for the human-readable rule; never used to make the decision.
pub(crate) fn classify_gitignore_check(
    result: std::io::Result<std::process::Output>,
    verbose_detail: Option<String>,
) -> GitignoreCheck {
    match result {
        Err(e) => GitignoreCheck::Unknown(e.to_string()),
        Ok(out) if out.status.success() => {
            GitignoreCheck::Shadowed(verbose_detail.unwrap_or_default())
        }
        Ok(_) => GitignoreCheck::Reachable,
    }
}

/// Has the local store outrun its tracked export badly enough that real
/// bead data has no durable copy? Lines needn't match exactly (status
/// filters, in-flight writes change the count run to run) — this states
/// the boundary as a threshold shape, not a hardcoded magic number, so
/// the property tests characterize it independent of the exact ratio.
///
/// Laws: an empty store never drifts (nothing to lose); a nonempty store
/// with zero exported lines always drifts (the exact incident this
/// check exists for — 366 beads, 9 repos, 2026-07-29); drift is
/// monotonic in `jsonl_lines` — exporting more can only cure a flagged
/// state, never cause one; an export meeting or exceeding the store
/// count never drifts.
pub(crate) fn store_export_drifted(db_count: i64, jsonl_lines: usize) -> bool {
    db_count > 0 && (jsonl_lines == 0 || jsonl_lines * 2 < db_count as usize)
}

/// Row count of the `issues` table in a bead SQLite store, opened
/// read-only so an audit run can never itself mutate or lock the store.
fn count_sqlite_issues(path: &Path) -> Result<i64> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opening {}", path.display()))?;
    conn.query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .context("counting issues")
}

/// Result of probing `dolt remote -v` in a Dolt-backed bead directory.
///
/// Distinguishes "command ran cleanly with no remote" from "command
/// failed" — the previous code lumped both into "no remote configured"
/// and hid real errors (e.g. corrupted repo, unsupported Dolt version).
pub(crate) enum DoltRemoteStatus {
    /// `dolt remote -v` exited 0 with non-empty stdout.
    Configured(String),
    /// `dolt remote -v` exited 0 with empty stdout — truly no remote.
    NotConfigured,
    /// `dolt remote -v` exited non-zero; preserve stderr for the user.
    Errored { exit: i32, stderr: String },
    /// The `dolt` binary couldn't be spawned (missing, no exec perm).
    NotInvokable(String),
}

/// Classify a `dolt remote -v` invocation result. Pure function over the
/// command output so it can be unit-tested without an actual `dolt`
/// binary.
pub(crate) fn classify_dolt_remote(
    result: std::io::Result<std::process::Output>,
) -> DoltRemoteStatus {
    match result {
        Err(e) => DoltRemoteStatus::NotInvokable(e.to_string()),
        Ok(out) if !out.status.success() => DoltRemoteStatus::Errored {
            exit: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Ok(out) if out.stdout.is_empty() => DoltRemoteStatus::NotConfigured,
        Ok(out) => DoltRemoteStatus::Configured(String::from_utf8_lossy(&out.stdout).into_owned()),
    }
}

/// Run `dolt remote -v` in the given directory and classify the result.
pub(super) fn check_dolt_remote(dolt_dir: &Path) -> DoltRemoteStatus {
    let result = Command::new("dolt")
        .args(["remote", "-v"])
        .current_dir(dolt_dir)
        .output();
    classify_dolt_remote(result)
}

#[cfg(test)]
mod tests;
