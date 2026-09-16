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
pub async fn audit(repo_root: &Path) -> Result<()> {
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
        trunk_projection_check(repo_root, &beads_dir, &mut problems).await;
    }

    hook_stamp_check(repo_root, &mut problems);

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

/// The trunk ref whose projection is the durable copy: the remote trunk when
/// one is fetched, else a local `main`/`master`. `None` when the repo has no
/// trunk at all (a fresh `git init`), which is not a drift condition.
fn trunk_projection_ref(repo_root: &Path) -> Option<&'static str> {
    [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
        "refs/heads/main",
        "refs/heads/master",
    ]
    .into_iter()
    .find(|candidate| {
        Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", candidate])
            .current_dir(repo_root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// ADR-0024 amendment A (rosary-e5fc0e): the invariant the audit checks is
/// "the trunk carries the current record of every published bead". A feature
/// branch's file is EXPECTED to lag the store, so the old row-count heuristic
/// measured the wrong artifact; this compares the TRUNK blob, record by
/// record, against the store's exact rendering — the same comparison the
/// pre-push gate makes for the beads a push names.
async fn trunk_projection_check(repo_root: &Path, beads_dir: &Path, problems: &mut Vec<String>) {
    let Some(trunk) = trunk_projection_ref(repo_root) else {
        println!("  ? no trunk ref to compare the projection against (fresh repo)");
        return;
    };
    let blob = Command::new("git")
        .args(["show", &format!("{trunk}:.beads/beads.jsonl")])
        .current_dir(repo_root)
        .output();
    let text = match blob {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            println!("  ? {trunk} carries no .beads/beads.jsonl — projection not tracked there");
            return;
        }
    };
    let index = crate::publish::push::parse_blob(&text);
    if index.records.is_empty() {
        println!("  ✓ trunk projection at {trunk} is empty — nothing to drift");
        return;
    }
    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let store = match crate::bead_sqlite::connect_bead_store(beads_dir).await {
        Ok(s) => s,
        Err(e) => {
            println!("  ? could not open the bead store to check the trunk projection: {e:#}");
            return;
        }
    };
    let mut stale = Vec::new();
    let mut missing = Vec::new();
    for (id, line) in &index.records {
        if index.duplicates.contains(id) {
            stale.push(id.clone());
            continue;
        }
        match store.get_bead(id, &repo_name).await {
            Ok(Some(bead)) => {
                match crate::jsonl_sync::render_bead_line(store.as_ref(), &bead).await {
                    Ok(rendered) if rendered == *line => {}
                    Ok(_) => stale.push(id.clone()),
                    Err(e) => {
                        println!("  ? could not render {id}: {e:#}");
                    }
                }
            }
            Ok(None) => missing.push(id.clone()),
            Err(e) => println!("  ? could not read {id} from the store: {e:#}"),
        }
    }
    if !stale.is_empty() {
        println!(
            "  ✗ TRUNK PROJECTION STALE: {} record(s) at {trunk} differ from the store — \
             run `rsry bead trunk-refresh` (rosary-e5c0a0): {}",
            stale.len(),
            stale.join(", ")
        );
        problems.push(format!(
            "{} trunk record(s) stale at {trunk}: {}",
            stale.len(),
            stale.join(", ")
        ));
    }
    if !missing.is_empty() {
        println!(
            "  ✗ STORE BEHIND TRUNK: {} bead(s) published at {trunk} are absent from the \
             store — run `rsry bead import --jsonl .beads/beads.jsonl`: {}",
            missing.len(),
            missing.join(", ")
        );
        problems.push(format!(
            "{} published bead(s) absent from the store: {}",
            missing.len(),
            missing.join(", ")
        ));
    }
    if stale.is_empty() && missing.is_empty() {
        println!(
            "  ✓ trunk projection ({} record(s) at {trunk}) agrees with the store",
            index.records.len()
        );
    }
}

/// An installed hook whose stamp is not this binary's stamp encodes an older
/// contract. Under amendment A that is not cosmetic: a stale pre-push keeps
/// refusing pushes for the OLD reason and a stale pre-commit keeps sweeping
/// unrelated records in (seen live on rosary, 2026-09-15). A managed block
/// with a stale stamp fails the audit; a hook file without a stamp is not
/// rsry-managed and is left alone.
fn hook_stamp_check(repo_root: &Path, problems: &mut Vec<String>) {
    let Ok(hooks_dir) = resolve_hooks_dir(repo_root) else {
        return;
    };
    let mut fresh = 0;
    for (name, block) in HOOKS {
        let Ok(content) = std::fs::read_to_string(hooks_dir.join(name)) else {
            continue;
        };
        let prefix = format!("# rsry-hook {name} v");
        let Some(found) = content.lines().find(|l| l.starts_with(&prefix)) else {
            continue;
        };
        let expected = hook_stamp(name, block);
        if found == expected {
            fresh += 1;
        } else {
            let found_version = found[prefix.len()..]
                .split_whitespace()
                .next()
                .unwrap_or("?");
            println!(
                "  ✗ HOOK STALE: {name} is v{found_version} but this rsry renders v{} — \
                 run `rsry hooks install`",
                env!("CARGO_PKG_VERSION")
            );
            problems.push(format!(
                "hook {name} is stale (v{found_version} vs v{})",
                env!("CARGO_PKG_VERSION")
            ));
        }
    }
    if fresh > 0 {
        println!("  ✓ {fresh} installed hook(s) match this rsry's templates");
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
