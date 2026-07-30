//! Thin wrapper around the `jj` CLI for automatic state versioning.
//!
//! Rosary's state directory (`~/.rsry/`) is a jj repo. Every state change
//! (bead status update, triage score, dispatch record) auto-snapshots: the hot
//! path writes to SQLite, the cold path snapshots to jj asynchronously.
//!
//! **This** module still uses the `jj` CLI throughout — init, snapshot, and
//! push all shell out. rosary as a whole no longer avoids jj-lib, though: since
//! rosary-efd300 `src/workspace/sweep.rs` links `leyline-vcs` (→ jj-lib → gix)
//! to create agent workspaces in-process, so the deps are already paid for and
//! the old rationale here — "rosary carries none of leyline-vcs's heavy
//! transitive deps" (rosary-30374f) — is no longer true. What keeps these calls
//! as subprocesses is inertia plus the fact that jj-lib's snapshot/push surface
//! is a bigger port than the workspace one; not a dependency argument. Migrating
//! them is unclaimed work, not a decision.
//!
//! Agents never interact with this directly — it's pure plumbing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[allow(dead_code)] // API surface — wired when main.rs calls ensure_state_dir on startup
/// Rosary state directory, default `~/.rsry/`.
pub fn state_dir() -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    let dir = home.join(".rsry");
    Ok(dir)
}

#[allow(dead_code)]
/// Ensure the state directory exists and is initialized.
pub fn ensure_state_dir() -> Result<PathBuf> {
    let dir = state_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating state dir: {}", dir.display()))?;
    }
    Ok(dir)
}

#[allow(dead_code)]
/// Initialize a git-backed jj repo in the state directory if one doesn't exist.
///
/// `jj git init` (not the deprecated bare `jj init`) so the repo has a git
/// backend consistent with [`push`]'s `jj git push`. Idempotent — a no-op when
/// `.jj` already exists.
pub fn init_jj(state_path: &Path) -> Result<()> {
    if state_path.join(".jj").exists() {
        return Ok(());
    }
    let status = std::process::Command::new("jj")
        .args(["git", "init"])
        .current_dir(state_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("running jj git init at {}", state_path.display()))?;
    if !status.success() {
        anyhow::bail!("jj git init failed at {}", state_path.display());
    }
    Ok(())
}

#[allow(dead_code)]
/// Snapshot current state to jj. Non-blocking best-effort.
///
/// Called after state-changing operations (bead update, dispatch, etc).
/// Failures are logged but don't propagate — state versioning must never
/// block the hot path.
///
/// Still uses jj CLI (`jj status --quiet`) because JjIntegration::commit_snapshot()
/// requires &dyn Graph which rosary doesn't implement — rosary stores plain files
/// in ~/.rsry/, not a leyline graph. The CLI triggers jj's working-copy snapshot.
pub fn snapshot(state_path: &Path) {
    match std::process::Command::new("jj")
        .args(["status", "--quiet"])
        .current_dir(state_path)
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("[rsry-vcs] snapshot warning: {stderr}");
        }
        Err(e) => {
            eprintln!("[rsry-vcs] snapshot failed: {e}");
        }
    }
}

#[allow(dead_code)]
/// Push state to a remote. Best-effort.
///
/// Called periodically or on graceful shutdown.
pub fn push(state_path: &Path, remote: &str) -> Result<()> {
    let output = std::process::Command::new("jj")
        .args(["git", "push", "--remote", remote])
        .current_dir(state_path)
        .output()
        .context("running jj git push")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("jj push failed: {stderr}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// jj log scanning — extract recent commits for bead transition detection
// ---------------------------------------------------------------------------

/// A commit from jj log, parsed into structured fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsCommit {
    /// jj change ID (short form, e.g., "kxryzmss")
    pub change_id: String,
    /// Commit description (may be multiline)
    pub description: String,
}

/// Scan recent jj commits in a repo. Returns parsed commits.
///
/// Uses `jj log` with a structured template for reliable parsing.
/// The `revset` parameter controls which commits to scan (e.g., `"@"`, `"@-..@"`).
/// Default limit prevents unbounded output.
pub fn scan_jj_log(repo_path: &Path, revset: &str, limit: usize) -> Result<Vec<VcsCommit>> {
    // Template: change_id<NUL>description<NUL><NUL>
    // NUL bytes are safe delimiters — they never appear in commit messages.
    let template = r#"change_id.short() ++ "\0" ++ description ++ "\0\0""#;

    let output = std::process::Command::new("jj")
        .args([
            "log",
            "--no-graph",
            "--no-pager",
            "-r",
            revset,
            "--limit",
            &limit.to_string(),
            "--template",
            template,
        ])
        .current_dir(repo_path)
        .output()
        .with_context(|| format!("running jj log in {}", repo_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // jj not initialized is not an error — repo might use git only
        if stderr.contains("There is no jj repo") || stderr.contains("no jj repo") {
            return Ok(Vec::new());
        }
        anyhow::bail!("jj log failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let commits = parse_jj_log_output(&stdout);
    Ok(commits)
}

/// Parse the NUL-delimited jj log output into VcsCommit structs.
fn parse_jj_log_output(output: &str) -> Vec<VcsCommit> {
    output
        .split("\0\0")
        .filter(|entry| !entry.trim().is_empty())
        .filter_map(|entry| {
            let parts: Vec<&str> = entry.splitn(2, '\0').collect();
            if parts.len() == 2 {
                let change_id = parts[0].trim().to_string();
                let description = parts[1].trim().to_string();
                if !change_id.is_empty() {
                    return Some(VcsCommit {
                        change_id,
                        description,
                    });
                }
            }
            None
        })
        .collect()
}

/// Scan a repo's jj log and extract bead references from recent commits.
///
/// Returns a list of (change_id, WorkRef) pairs — the reconciler uses these
/// to trigger bead state transitions.
pub fn scan_vcs_bead_refs(repo_path: &Path) -> Result<Vec<(String, WorkRef)>> {
    // Scan recent non-immutable commits (working copy + recent work)
    let commits = scan_jj_log(repo_path, "mine()", 50)?;

    let mut results = Vec::new();
    for commit in &commits {
        let refs = extract_bead_refs(&commit.description);
        for bead_ref in refs {
            results.push((commit.change_id.clone(), bead_ref));
        }
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Bead ID extraction from commit messages
// ---------------------------------------------------------------------------

/// A bead reference found in a commit message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRef {
    /// The bead ID (e.g., "rsry-abc123", "loom-7sd", "mache-tgl")
    pub id: String,
    /// Whether this reference closes the bead (e.g., "closes bead:...", "fixes bead:...")
    pub closes: bool,
}

fn is_bead_id(id: &str) -> bool {
    let Some((prefix, suffix)) = id.rsplit_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && !suffix.is_empty()
        && prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && suffix.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Extract bead references from a commit message or jj description.
///
/// Recognized patterns:
/// - `bead:rsry-abc123` — simple reference (dispatched)
/// - `closes bead:rsry-abc123` — closing reference (done)
/// - `fixes bead:rsry-abc123` — closing reference (done)
/// - `bead:loom-7sd` — any repo prefix works
///
/// Bead IDs follow the pattern: `{prefix}-{suffix}` where prefix is lowercase
/// alpha and suffix is lowercase alphanumeric (hex or base36).
pub fn extract_bead_refs(message: &str) -> Vec<WorkRef> {
    let mut refs = Vec::new();
    let lower = message.to_lowercase();

    // Find bracket-format refs: [prefix-suffix] at start of line
    // This is the format agents produce from the dispatch prompt instructions.
    for line in lower.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[')
            && let Some(end) = rest.find(']')
        {
            let id = &rest[..end];
            if is_bead_id(id) {
                refs.push(WorkRef {
                    id: id.to_string(),
                    closes: false,
                });
            }
        }
    }

    // Find all occurrences of "bead:" followed by an ID
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find("bead:") {
        let prev_search_from = search_from;
        let abs_pos = search_from + pos;
        let after = &lower[abs_pos + 5..]; // skip "bead:"

        // Parse the bead ID: {prefix}-{suffix}
        // prefix: one or more lowercase alpha chars
        // suffix: one or more lowercase alphanumeric chars
        let id: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();

        if is_bead_id(&id) {
            // Check for closing prefix: "closes", "fixes", "close", "fix"
            let before = &lower[..abs_pos].trim_end();
            let closes = before.ends_with("closes")
                || before.ends_with("fixes")
                || before.ends_with("close")
                || before.ends_with("fix");

            refs.push(WorkRef {
                id: id.clone(),
                closes,
            });
        }

        // Termination depends entirely on this strictly exceeding `abs_pos`
        // every iteration, or `find("bead:")` matches the same occurrence
        // forever (rosary-b2ae79's mutants rung: a `+`->`-` mutation here
        // hung real wall-clock time instead of failing fast, since nothing
        // enforced the invariant). `.max(abs_pos + 6)` makes it true by
        // construction — past "bead:" (5 bytes) plus at least 1 more byte —
        // regardless of `id.len()`, so a future refactor of this arithmetic
        // can't silently reintroduce an infinite loop.
        search_from = (abs_pos + 5 + id.len()).max(abs_pos + 6);

        // Belt-and-suspenders on the property that actually prevents the
        // hang: a REAL assert (not debug_assert — this must hold in release
        // builds too, since a hang in production is worse than a panic).
        // Catches a broken `abs_pos` computation (e.g. a mutated `+` at
        // line ~264) even if it slips past the `.max()` guard above.
        assert!(
            search_from > prev_search_from,
            "extract_bead_refs: search position failed to advance ({prev_search_from} -> {search_from}); would loop forever"
        );
    }

    // Dedup by ID, keeping closes=true if any ref closes
    refs.sort_by(|a, b| a.id.cmp(&b.id));
    refs.dedup_by(|a, b| {
        if a.id == b.id {
            b.closes = b.closes || a.closes;
            true
        } else {
            false
        }
    });

    refs
}

/// Extract just the bead IDs (ignoring close semantics).
/// Convenience wrapper for simple lookups.
#[allow(dead_code)]
pub fn extract_bead_ids(message: &str) -> Vec<String> {
    extract_bead_refs(message)
        .into_iter()
        .map(|r| r.id)
        .collect()
}

// ---------------------------------------------------------------------------
// Merged-PR closure detection (rsry-native, local — no gh, no webhook, no tunnel)
// ---------------------------------------------------------------------------

/// A merged-PR closure detected from a squash-merge commit subject on the
/// trunk. The convention `[<bead-id>] <subject> (#<pr>)` is the *local* signal
/// that a PR merged — read straight from `git log`, no `gh` / webhook / tunnel.
/// Closing on it is how rosary satisfies a bead's default "PR merges" close
/// condition without an inbound webhook (the same outcome as
/// `serve::github_webhook`, driven by a local pull instead of a POST).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedClosure {
    pub bead_id: String,
    pub pr_number: u64,
}

/// Parse every closure from a squash commit's **subject line only**. The PR
/// number comes from the subject's trailing `(#N)` marker — the squash signal;
/// bead ids are the `[bead-id]` brackets on that same line. Returns empty for
/// a commit with no `(#N)` marker (a work-in-progress commit, not a merge).
///
/// The BODY is deliberately not consulted (rosary-e0e19f). A squash body
/// contains every commit message the PR absorbed, and Golden Rule 11 prefixes
/// each with the bead it was *worked under* — so a body bracket is provenance,
/// not a completion claim. Matching body brackets auto-closed beads that were
/// merely mentioned (four in cloister, and this defect's own bug report,
/// twice), and made it impossible to honestly tag a commit that documents a
/// bead without closing it. The cost is real but smaller: a PR whose title
/// omits the bracket no longer auto-closes its beads and needs a manual close.
pub fn parse_merged_closures(message: &str) -> Vec<MergedClosure> {
    let subject = message.lines().next().unwrap_or("");
    let Some(pr_number) = trailing_pr_number(subject) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    extract_bracket_ids(subject)
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .map(|bead_id| MergedClosure { bead_id, pr_number })
        .collect()
}

/// Subject-only convenience over [`parse_merged_closures`] (first match, if any).
#[allow(dead_code)] // public convenience + exercised by unit tests
pub fn parse_merged_closure(subject: &str) -> Option<MergedClosure> {
    parse_merged_closures(subject).into_iter().next()
}

/// Find every `[<prefix>-<suffix>]` bead-id bracket anywhere in `text` (the
/// caller decides how much of the message qualifies — see
/// [`parse_merged_closures`] on why that is the subject line only).
fn extract_bracket_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = text;
    // `split_once` structurally rules out the off-by-one class this loop
    // used to admit (rosary-b2ae79): raw `open + 1` / `close + 1` index
    // arithmetic left an equivalent mutant (`+` -> `*`) that no test could
    // distinguish, because the only under-consumed byte was always `]`
    // itself, which `find('[')` ignores wherever it lands. There is no
    // arithmetic left here for that mutation class to target.
    while let Some((_, after_open)) = rest.split_once('[') {
        let Some((content, after_close)) = after_open.split_once(']') else {
            break;
        };
        if let Some(id) = valid_bead_id(content) {
            ids.push(id);
        }
        rest = after_close;
    }
    ids
}

/// Validate the `<prefix>-<suffix>` bead-id shape and return the id
/// (case-preserving) if valid. Repo prefixes are themselves multi-segment
/// (`ley-line-open`), so the split is on the LAST dash: everything before it is
/// the prefix (lowercase alpha, `.`, or `-`), everything after is the short-hash
/// suffix (alphanumeric). Splitting on the FIRST dash mis-parsed
/// `ley-line-open-e5addb` as prefix="ley"/suffix="line-open-e5addb" and rejected
/// it, so those release squash commits never became closures (rosary-cb9321).
fn valid_bead_id(id: &str) -> Option<String> {
    let dash = id.rfind('-')?;
    let (prefix, suffix) = (&id[..dash], &id[dash + 1..]);
    let ok = !prefix.is_empty()
        && !suffix.is_empty()
        && prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '.' || c == '-')
        && suffix.chars().all(|c| c.is_ascii_alphanumeric());
    ok.then(|| id.to_string())
}

/// Extract the PR number from a trailing `(#<digits>)` marker — the shape
/// GitHub squash-merge commits carry. Scans for the LAST `(#` so a bead id or
/// body mention doesn't shadow the real trailing PR number.
fn trailing_pr_number(subject: &str) -> Option<u64> {
    let after = &subject[subject.rfind("(#")? + 2..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !after[digits.len()..].starts_with(')') {
        return None;
    }
    digits.parse().ok()
}

/// Resolve the ref whose first-parent history is "the trunk" for merged-PR
/// scanning (rosary-fb8310). Preference order: the remote's own default-branch
/// pointer (`refs/remotes/origin/HEAD`), then the conventional trunk names on
/// origin, then `HEAD` — the pre-fix behavior, kept as the fallback so
/// remoteless repos (tests, scratch fixtures) scan exactly as before.
///
/// Why not just HEAD: the checkout at the registered repo path may sit on a
/// feature branch (or merely behind), in which case merged trunk commits are
/// only reachable from the remote-tracking ref a `git fetch` updates — a
/// fetch-only refresh (vigil, cron, IDE) makes closures visible without
/// anyone mutating the working tree.
fn trunk_scan_ref(repo_path: &Path) -> &'static str {
    for candidate in [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ] {
        let verified = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", candidate])
            .current_dir(repo_path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if verified {
            return candidate;
        }
    }
    "HEAD"
}

/// Scan the trunk's recent first-parent commits for merged-PR closures via
/// `git log` (full message per commit, NUL-separated). VCS-agnostic (works on
/// pure-git and jj-colocated repos). The scanned ref is the remote trunk when
/// one exists (see [`trunk_scan_ref`]) — so a bare `git fetch` is enough to
/// surface closures; the checkout's own branch/dirtiness is irrelevant.
/// Idempotent: returns every closure in the window; the caller closes only
/// beads that are still open, so re-running is harmless.
pub fn scan_merged_closures(repo_path: &Path, limit: usize) -> Vec<MergedClosure> {
    let output = std::process::Command::new("git")
        .args([
            "log",
            trunk_scan_ref(repo_path),
            "--first-parent",
            "-n",
            &limit.to_string(),
            "--format=%B%x00",
        ])
        .current_dir(repo_path)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|m| !m.trim().is_empty())
        .flat_map(parse_merged_closures)
        .collect()
}

/// One squash-merge commit on the trunk, with the identity (`sha`) and time
/// (`committed_at`) git actually witnessed, plus every bead closure its message
/// carries. Distinct from [`MergedClosure`] (which is id+PR only) because the
/// observation lattice needs the commit's *identity* as a dedup key and its
/// *timestamp* as `observed_at` — the two things a closure alone can't provide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeCommit {
    /// Full commit sha — stable, unique, and the natural `source_event_id`.
    pub sha: String,
    /// Commit timestamp (committer date), the moment git witnessed the merge.
    pub committed_at: chrono::DateTime<chrono::Utc>,
    /// Every `[bead-id]`/`(#N)` closure parsed from the commit message.
    pub closures: Vec<MergedClosure>,
}

/// Like [`scan_merged_closures`], but keeps each commit's sha + timestamp so the
/// result can be recorded as observations (`rsry lattice backfill`). Same trunk
/// ref, same first-parent walk, same message parser — only the projection
/// differs. Commits with no `(#N)` marker (non-merges) are dropped.
pub fn scan_merge_commits(repo_path: &Path, limit: usize) -> Vec<MergeCommit> {
    let output = std::process::Command::new("git")
        .args([
            "log",
            trunk_scan_ref(repo_path),
            "--first-parent",
            "-n",
            &limit.to_string(),
            // sha \x01 unix-committer-date \x01 full message, NUL-separated.
            "--format=%H%x01%ct%x01%B%x00",
        ])
        .current_dir(repo_path)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|rec| !rec.trim().is_empty())
        .filter_map(parse_merge_commit_record)
        .collect()
}

/// Parse one `%H\x01%ct\x01%B` record into a [`MergeCommit`]. `None` when the
/// record is malformed or the commit carries no closures.
fn parse_merge_commit_record(record: &str) -> Option<MergeCommit> {
    let record = record.trim_start_matches('\n');
    let mut parts = record.splitn(3, '\x01');
    let sha = parts.next()?.trim().to_string();
    let ts: i64 = parts.next()?.trim().parse().ok()?;
    let message = parts.next()?;
    if sha.is_empty() {
        return None;
    }
    let closures = parse_merged_closures(message);
    if closures.is_empty() {
        return None;
    }
    Some(MergeCommit {
        sha,
        committed_at: chrono::DateTime::from_timestamp(ts, 0)?,
        closures,
    })
}

/// Build the canonical PR URL (`https://github.com/<owner>/<repo>/pull/<n>`)
/// from the repo's `origin` remote, so the local close path can record the same
/// structured `pr_url` event the gh/webhook path emits — letting a parent and
/// its children's PRs surface as a chain without any network call. Returns
/// `None` when origin isn't a recognizable GitHub remote (the caller falls back
/// to a bare `#<n>` reference).
pub fn origin_pr_url(repo_path: &Path, pr_number: u64) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let slug = github_slug(&url)?;
    Some(format!("https://github.com/{slug}/pull/{pr_number}"))
}

/// Normalize a git remote URL to its `<owner>/<repo>` slug. Handles the SSH
/// (`git@github.com:owner/repo.git`) and HTTPS (`https://github.com/owner/repo`)
/// forms, with or without a trailing `.git`.
fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let slug = rest.strip_suffix(".git").unwrap_or(rest).trim_matches('/');
    // Must be exactly owner/repo — reject anything with extra path segments.
    let mut parts = slug.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_bead_refs_finds_bracket_and_bead_colon_forms() {
        let refs = extract_bead_refs("[rosary-abc123] fix: thing\n\nbead:rosary-def456");
        let ids: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"rosary-abc123"));
        assert!(ids.contains(&"rosary-def456"));
    }

    #[test]
    fn extract_bead_refs_detects_closing_prefixes() {
        let refs = extract_bead_refs("closes bead:rosary-abc123");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].closes);

        let refs = extract_bead_refs("bead:rosary-abc123 (not closing)");
        assert_eq!(refs.len(), 1);
        assert!(!refs[0].closes);
    }

    /// Loop-iteration coverage (rosary-b2ae79 mutants rung): multiple
    /// `bead:` occurrences force the `search_from` advancement logic to run
    /// more than once. A `+`->`-` mutation on that arithmetic hung for 55s+
    /// instead of failing — this is the shape that exercises it.
    #[test]
    fn extract_bead_refs_advances_past_multiple_occurrences() {
        let msg = "bead:rosary-aaa111 then bead:rosary-bbb222 then bead:rosary-ccc333";
        let refs = extract_bead_refs(msg);
        let ids: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "{ids:?}");
        assert!(ids.contains(&"rosary-aaa111"));
        assert!(ids.contains(&"rosary-bbb222"));
        assert!(ids.contains(&"rosary-ccc333"));
    }

    /// A `bead:` mention with no valid id after it (fails `is_bead_id`)
    /// must not stall the scan — the next real occurrence must still be
    /// found. This is exactly the case where `id.len()` can be small/zero,
    /// the scenario the `.max(abs_pos + 6)` floor exists for.
    #[test]
    fn extract_bead_refs_skips_invalid_id_and_keeps_scanning() {
        let msg = "bead: (nothing valid here) then bead:rosary-real123";
        let refs = extract_bead_refs(msg);
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0].id, "rosary-real123");
    }

    /// Directly proves the termination invariant survives adversarial
    /// repetition: many adjacent zero-content "bead:" mentions, the shape
    /// most likely to expose a stalled `search_from`. Bounded by the test
    /// harness's own timeout, but the real proof is the function returning
    /// at all rather than needing a wall-clock timeout to notice a hang.
    #[test]
    fn extract_bead_refs_terminates_on_repeated_empty_mentions() {
        let msg = "bead:".repeat(50);
        let refs = extract_bead_refs(&msg);
        assert!(refs.is_empty());
    }

    #[test]
    fn github_slug_handles_ssh_and_https_forms() {
        assert_eq!(
            github_slug("git@github.com:agentic-research/rosary.git").as_deref(),
            Some("agentic-research/rosary")
        );
        assert_eq!(
            github_slug("https://github.com/agentic-research/rosary").as_deref(),
            Some("agentic-research/rosary")
        );
        assert_eq!(
            github_slug("ssh://git@github.com/org/repo.git").as_deref(),
            Some("org/repo")
        );
        // Non-GitHub or malformed remotes yield nothing.
        assert!(github_slug("git@gitlab.com:org/repo.git").is_none());
        assert!(github_slug("https://github.com/org/repo/extra").is_none());
    }

    #[test]
    fn parse_merged_closure_matches_squash_merge_subject() {
        let c = parse_merged_closure("[rosary-4fe0b2] Retire bash gates (#318)").unwrap();
        assert_eq!(c.bead_id, "rosary-4fe0b2");
        assert_eq!(c.pr_number, 318);
    }

    #[test]
    fn parse_merged_closure_requires_pr_marker() {
        // Golden Rule 11 prefix WITHOUT a (#N) merge marker is a work-in-progress
        // commit, not a merge — must not be treated as a closure.
        assert!(parse_merged_closure("[rosary-4fe0b2] work in progress").is_none());
    }

    #[test]
    fn parse_merged_closure_requires_bracket() {
        assert!(parse_merged_closure("fix something (#318)").is_none());
    }

    #[test]
    fn parse_merged_closure_rejects_malformed_id() {
        assert!(parse_merged_closure("[nodash] title (#1)").is_none());
        assert!(parse_merged_closure("[UPPER-CASE] title (#1)").is_none());
        assert!(parse_merged_closure("[rosary-4fe0b2] title (#notanumber)").is_none());
    }

    #[test]
    fn parse_merged_closure_takes_last_pr_marker() {
        // A body/earlier mention must not shadow the real trailing PR number.
        let c = parse_merged_closure("[rosary-abc123] see (#100) then (#318)").unwrap();
        assert_eq!(c.pr_number, 318);
    }

    /// rosary-b2ae79 mutants rung: `rest = &after[close + 1..]` had zero
    /// direct coverage — every existing test uses at most one bracket, so a
    /// `+`->`-`/`*` mutation on the advancement arithmetic still produced
    /// SOME output (not a hang) that happened to match on single-bracket
    /// input. Multiple short brackets in sequence is the shape that exposes
    /// a shifted/wrong advancement: each subsequent id gets parsed from the
    /// wrong offset once one is off.
    #[test]
    fn extract_bracket_ids_advances_correctly_across_multiple_brackets() {
        let ids =
            extract_bracket_ids("[rosary-aaa111] middle [rosary-bbb222] end [rosary-ccc333] tail");
        assert_eq!(ids, vec!["rosary-aaa111", "rosary-bbb222", "rosary-ccc333"]);
    }

    /// An INVALID bracket whose content ends in `[` (rejected by
    /// `is_bead_id`, correctly not pushed) still must not corrupt the scan
    /// for the real bracket that follows. This was the input that exposed
    /// the pre-`split_once` off-by-one bug (rosary-b2ae79): under-advancing
    /// by even one character re-exposed the trailing `[` as a spurious
    /// match, panicking on subtract-with-overflow. Kept as a regression
    /// pin even though `split_once` makes the class structurally
    /// impossible now.
    #[test]
    fn extract_bracket_ids_survives_invalid_bracket_ending_in_open_bracket() {
        let ids = extract_bracket_ids("[rosary-abc[] [rosary-def456]");
        assert_eq!(ids, vec!["rosary-def456"]);
    }

    /// rosary-b2ae79 mutants rung: `digits.is_empty() || !after[..].starts_with(')')`
    /// mutated to `&&` still returned the right answer whenever digits were
    /// empty (the empty string fails `.parse()` regardless, via a different
    /// path) — the distinguishing case is digits PRESENT but not properly
    /// closed, which only the `||` form correctly rejects.
    #[test]
    fn trailing_pr_number_rejects_digits_without_closing_paren() {
        assert_eq!(trailing_pr_number("fix thing (#318 not closed"), None);
    }

    #[test]
    fn parse_merged_closures_ignores_body_mentions() {
        // rosary-e0e19f mutation test: a squash body contains every commit
        // message the PR absorbed, so a bead id anywhere in the BODY is
        // provenance, not a verdict — matching it auto-closed beads that were
        // merely mentioned (four in cloister, plus this defect's own bug
        // report, twice). Only the subject names what the PR claims to
        // deliver; a body-only mention must NOT close.
        let msg = "docs: accuracy sweep after v0.4.0 (#322)\n\n\
                   * [rosary-da720c] docs: first pass\n\
                   * [rosary-da720c] docs: second pass\n\
                   * [rosary-9d181f] docs: third pass\n";
        assert!(parse_merged_closures(msg).is_empty());
    }

    #[test]
    fn parse_merged_closures_subject_brackets_close_body_brackets_do_not() {
        // A multi-bead SUBJECT still resolves (deduped, subject PR number);
        // a body mention of a third bead does not join the closures.
        let msg = "[rosary-da720c] [rosary-da720c] [rosary-9d181f] combined (#322)\n\n\
                   * [rosary-ffffff] absorbed commit touching another bead\n";
        let closures = parse_merged_closures(msg);
        let ids: Vec<&str> = closures.iter().map(|c| c.bead_id.as_str()).collect();
        assert_eq!(closures.len(), 2, "da720c deduped, ffffff excluded");
        assert!(ids.contains(&"rosary-da720c"));
        assert!(ids.contains(&"rosary-9d181f"));
        assert!(closures.iter().all(|c| c.pr_number == 322));
    }

    #[test]
    fn parse_merge_commit_record_carries_sha_and_time() {
        // The `%H\x01%ct\x01%B` record the lattice backfill consumes.
        let rec = "abc123def456\x011700000000\x01[rosary-a66b3a] feat(x): thing (#7)\n";
        let c = parse_merge_commit_record(rec).unwrap();
        assert_eq!(c.sha, "abc123def456");
        assert_eq!(c.committed_at.timestamp(), 1_700_000_000);
        assert_eq!(c.closures.len(), 1);
        assert_eq!(c.closures[0].bead_id, "rosary-a66b3a");
        assert_eq!(c.closures[0].pr_number, 7);
    }

    #[test]
    fn parse_merge_commit_record_drops_non_merges_and_garbage() {
        // No (#N) → not a merge.
        assert!(parse_merge_commit_record("sha\x011700000000\x01wip: nothing").is_none());
        // Malformed records never panic.
        assert!(parse_merge_commit_record("no-separators-at-all").is_none());
        assert!(parse_merge_commit_record("sha\x01notanumber\x01x (#1)").is_none());
        assert!(parse_merge_commit_record("\x011700000000\x01[a-b1] x (#1)").is_none());
    }

    #[test]
    fn parse_merged_closures_empty_without_pr_marker() {
        // No (#N) on the subject → not a merge, even if the body has brackets.
        assert!(parse_merged_closures("wip [rosary-abc123] no pr marker").is_empty());
    }

    #[test]
    fn parse_merged_closure_accepts_multi_segment_repo_prefix() {
        // rosary-cb9321: repo prefixes carry dashes (`ley-line-open`). The id is
        // <repo-prefix>-<short-hash> where the prefix itself may be multi-segment.
        // Splitting on the FIRST dash mis-parses this as prefix="ley",
        // suffix="line-open-e5addb" and rejects it — so the release squash commit
        // `[ley-line-open-e5addb] … (#229)` never becomes a closure and the bead
        // stays open (observed live as checked=0).
        let c =
            parse_merged_closure("[ley-line-open-e5addb] chore(release): 0.7.1 (#229)").unwrap();
        assert_eq!(c.bead_id, "ley-line-open-e5addb");
        assert_eq!(c.pr_number, 229);
    }

    #[test]
    fn state_dir_under_home() {
        let dir = state_dir().unwrap();
        assert!(dir.to_string_lossy().ends_with(".rsry"));
        assert!(!dir.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn ensure_state_dir_creates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".rsry");
        assert!(!dir.exists());

        // Manually test the creation logic
        std::fs::create_dir_all(&dir).unwrap();
        assert!(dir.exists());
    }

    /// `jj` on PATH? (init_jj now shells out; skip when it isn't installed —
    /// same pattern as the dolt tests.)
    fn jj_available() -> bool {
        std::process::Command::new("jj")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn init_jj_creates_repo() {
        if !jj_available() {
            eprintln!("skipping: jj not installed");
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!tmp.path().join(".jj").exists());

        init_jj(tmp.path()).unwrap();
        assert!(tmp.path().join(".jj").exists());
    }

    #[test]
    fn init_jj_idempotent() {
        if !jj_available() {
            eprintln!("skipping: jj not installed");
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();

        // First call inits, second call is a no-op — both succeed.
        init_jj(tmp.path()).unwrap();
        init_jj(tmp.path()).unwrap();
        assert!(tmp.path().join(".jj").exists());
    }

    // --- jj log parsing tests ---

    #[test]
    fn parse_jj_log_single_commit() {
        let output = "kxryzmss\0fix the widget bug\n\nbead:rsry-abc123\0\0";
        let commits = parse_jj_log_output(output);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].change_id, "kxryzmss");
        assert!(commits[0].description.contains("bead:rsry-abc123"));
    }

    #[test]
    fn parse_jj_log_multiple_commits() {
        let output = "aaa\0first commit\0\0bbb\0second commit\n\ncloses bead:rsry-xyz\0\0";
        let commits = parse_jj_log_output(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].change_id, "aaa");
        assert_eq!(commits[1].change_id, "bbb");
    }

    #[test]
    fn parse_jj_log_empty() {
        let commits = parse_jj_log_output("");
        assert!(commits.is_empty());
    }

    #[test]
    fn parse_jj_log_trailing_whitespace() {
        let output = "abc\0some desc\0\0\n";
        let commits = parse_jj_log_output(output);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].change_id, "abc");
    }

    // --- Bead ID extraction tests ---

    #[test]
    fn extract_single_bead_ref() {
        let refs = extract_bead_refs("working on bead:rsry-abc123");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "rsry-abc123");
        assert!(!refs[0].closes);
    }

    #[test]
    fn extract_multiple_bead_refs() {
        let refs = extract_bead_refs("bead:rsry-abc and also bead:loom-7sd");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "loom-7sd");
        assert_eq!(refs[1].id, "rsry-abc");
    }

    #[test]
    fn extract_no_bead_refs() {
        let refs = extract_bead_refs("just a regular commit message");
        assert!(refs.is_empty());
    }

    #[test]
    fn extract_closing_ref_closes() {
        let refs = extract_bead_refs("closes bead:rsry-59f7f9");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "rsry-59f7f9");
        assert!(refs[0].closes);
    }

    #[test]
    fn extract_closing_ref_fixes() {
        let refs = extract_bead_refs("fixes bead:mache-tgl");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].closes);
    }

    #[test]
    fn extract_closing_ref_fix() {
        let refs = extract_bead_refs("fix bead:rsry-abc123");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].closes);
    }

    #[test]
    fn extract_closing_ref_close() {
        let refs = extract_bead_refs("close bead:rsry-abc123");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].closes);
    }

    #[test]
    fn extract_case_insensitive() {
        let refs = extract_bead_refs("BEAD:rsry-abc123");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "rsry-abc123");
    }

    #[test]
    fn extract_mixed_close_and_ref() {
        let refs = extract_bead_refs("closes bead:rsry-aaa, also mentions bead:rsry-bbb");
        assert_eq!(refs.len(), 2);
        let aaa = refs.iter().find(|r| r.id == "rsry-aaa").unwrap();
        let bbb = refs.iter().find(|r| r.id == "rsry-bbb").unwrap();
        assert!(aaa.closes);
        assert!(!bbb.closes);
    }

    #[test]
    fn extract_deduplicates() {
        let refs = extract_bead_refs("bead:rsry-abc and again bead:rsry-abc");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn extract_dedup_keeps_closes() {
        // If one ref closes and another just mentions, closes wins
        let refs = extract_bead_refs("bead:rsry-abc ... closes bead:rsry-abc");
        assert_eq!(refs.len(), 1);
        assert!(refs[0].closes);
    }

    #[test]
    fn extract_ignores_malformed() {
        // No dash → not a bead ID
        assert!(extract_bead_refs("bead:nope").is_empty());
        // Empty prefix
        assert!(extract_bead_refs("bead:-abc").is_empty());
        // Empty suffix
        assert!(extract_bead_refs("bead:rsry-").is_empty());
    }

    #[test]
    fn extract_normalizes_case() {
        // Uppercase input gets lowercased — IDs are always lowercase
        let refs = extract_bead_refs("bead:RSRY-ABC");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "rsry-abc");
    }

    #[test]
    fn extract_in_multiline_message() {
        let msg = "feat: CLI ergonomics\n\nAddresses bead:rsry-59f7f9 (CLI ergonomics),\ncloses bead:rsry-59e7f8 (sync deltas).";
        let refs = extract_bead_refs(msg);
        assert_eq!(refs.len(), 2);
        let f9 = refs.iter().find(|r| r.id == "rsry-59f7f9").unwrap();
        let e8 = refs.iter().find(|r| r.id == "rsry-59e7f8").unwrap();
        assert!(!f9.closes);
        assert!(e8.closes);
    }

    #[test]
    fn extract_hex_suffix() {
        let refs = extract_bead_refs("bead:rsry-8c31a5");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "rsry-8c31a5");
    }

    // --- Bracket-format bead ref tests ---

    #[test]
    fn extract_bracket_format_bead_ref() {
        // Agent prompt tells agents: git commit -m "[rosary-5aae44] type(scope): desc"
        let refs = extract_bead_refs("[rosary-5aae44] fix(xref): add module doc");
        assert_eq!(refs.len(), 1, "bracket format should be detected");
        assert_eq!(refs[0].id, "rosary-5aae44");
    }

    #[test]
    fn extract_bracket_format_with_dots() {
        // Temp dir names produce IDs like ".tmpXXXXXX-a1b2c3"
        let refs = extract_bead_refs("[.tmpabcdef-a1b2c3] fix: something");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn extract_bracket_format_with_hyphenated_repo_prefix() {
        let refs =
            extract_bead_refs("[canonical-hours-4f71c9] feat(observer): publish portable core");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "canonical-hours-4f71c9");
    }

    #[test]
    fn extract_bracket_and_footer_deduplicates() {
        let msg = "[rsry-abc123] fix: thing\n\nbead:rsry-abc123";
        let refs = extract_bead_refs(msg);
        assert_eq!(refs.len(), 1, "bracket + footer should dedup to 1");
        assert_eq!(refs[0].id, "rsry-abc123");
    }

    #[test]
    fn extract_bracket_not_bead_id() {
        // [v1.2.3] or [BREAKING] should NOT match
        let refs = extract_bead_refs("[v1.2.3] release notes");
        assert!(refs.is_empty(), "version tags should not match");

        let refs = extract_bead_refs("[BREAKING] change api");
        assert!(refs.is_empty(), "uppercase brackets should not match");
    }

    #[test]
    fn extract_bead_ids_convenience() {
        let ids = extract_bead_ids("bead:rsry-abc closes bead:loom-xyz");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"rsry-abc".to_string()));
        assert!(ids.contains(&"loom-xyz".to_string()));
    }
}
