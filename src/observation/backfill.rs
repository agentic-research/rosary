//! Git-history backfill for the observation lattice (R4b, rosary-a66b3a).
//!
//! # Why this exists
//!
//! ADR-0010's lattice was fully built and unit-tested but **dark in
//! production**: `rsry lattice audit` on the real rosary store reported
//! `beads=1117 comparable=0` — no bead had ever accumulated a foldable
//! observation, so the R4b plan ("compare derived status vs `persist_status`,
//! prove equivalence, delete `persist_status`") had no corpus to compare
//! against. The blocker was the *write path*, not the evidence.
//!
//! Git history is the one high-fidelity write path that already exists. Every
//! squash merge on the trunk (`[bead-id] <type>(<scope>): <subject> (#N)`) is a
//! durable, timestamped, content-addressed witness that a bead's work landed.
//! This module replays that history into the lattice.
//!
//! # What it does and does NOT claim
//!
//! Git witnesses the terminal **merge**, not the intermediate lifecycle. So a
//! backfilled bead gets exactly one observation — `PipelineVerdict::Done`,
//! sourced `git`, at the commit's timestamp — and nothing else. This does not
//! reconstruct a bead's *history*; it reconstructs its *outcome*. The
//! `open → dispatched → verifying` transitions are invisible to git and are the
//! job of the dual-write path (R4b phase 2), not of this backfill.
//!
//! # Invariants
//!
//! - **Behavior-neutral.** Writes `observation` events only. Bead state,
//!   `persist_status`, dispatch and triage are untouched.
//! - **Idempotent.** The commit sha is the `source_event_id`; a bead that
//!   already carries a `git`-sourced observation for that sha is skipped, so a
//!   second run records zero. No side-table — the dedup key IS the lattice's
//!   (ADR-0010 invariant 8) key, read back out of the event log.
//! - **No fabrication.** Only commits git actually contains are recorded, at
//!   the timestamp git recorded them.
//! - **Dangling refs are reported, not invented.** A `[bead-id]` that doesn't
//!   resolve in the store is counted and named — independent evidence for
//!   rosary-225c94 (commit-msg validates bead-id *format*, not *existence*).

use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use super::shadow::parse_events_for;
use super::{Observation, PipelineVerdictValue, Source};
use crate::store::{BeadStore, WorkRef};
use crate::vcs;

/// The `Source` every backfilled observation carries. Distinct from `rosary`
/// (the reconciler's own emissions) so the corpus stays attributable: a fold
/// can always tell "git says this merged" from "the pipeline says this passed".
pub const GIT_SOURCE: &str = "git";

/// Outcome of one backfill run.
#[derive(Debug, Default, Clone)]
pub struct BackfillReport {
    /// Squash-merge commits found in the scanned window.
    pub merge_commits: usize,
    /// `(commit, bead-id)` pairs those commits reference.
    pub closures: usize,
    /// Observations newly written (0 on a repeat run — that's the idempotency
    /// proof).
    pub recorded: usize,
    /// Closures already carrying an observation for this sha.
    pub already_present: usize,
    /// Bead ids referenced by a merge commit that don't exist in the store.
    /// Only *bead-shaped* refs (`<prefix>-<6 hex>`) count — real evidence for
    /// rosary-225c94.
    pub dangling: Vec<String>,
    /// Bracket refs the message parser picked up that aren't bead-shaped at all
    /// (`[--dry-run]`, `[bead-id]`, `[repo-id]` in a squash body's prose).
    /// Counted so the dangling number stays honest, never treated as evidence.
    pub non_bead_brackets: usize,
    /// True when nothing was written.
    pub dry_run: bool,
}

/// Replay `repo_path`'s trunk squash-merge history into `store` as
/// `PipelineVerdict::Done` observations.
///
/// `limit` bounds the first-parent commit window. `repo_name` is the lattice's
/// `WorkRef.repo` — it must match what [`super::audit::audit_store`] uses, or
/// the fold won't associate the observations with their beads.
pub async fn backfill_repo(
    store: &dyn BeadStore,
    repo_path: &Path,
    repo_name: &str,
    limit: usize,
    dry_run: bool,
) -> Result<BackfillReport> {
    let mut report = BackfillReport {
        dry_run,
        ..Default::default()
    };

    let commits = vcs::scan_merge_commits(repo_path, limit);
    report.merge_commits = commits.len();
    if commits.is_empty() {
        return Ok(report);
    }

    // Resolve bead ids against the store once. The trunk's brackets are short
    // refs in principle, so match the close-merged rule: exact id, else unique
    // suffix.
    let known: Vec<String> = store
        .list_all_beads(repo_name)
        .await?
        .into_iter()
        .map(|b| b.id)
        .collect();

    let mut dangling: HashSet<String> = HashSet::new();
    let mut noise: HashSet<String> = HashSet::new();
    // Per-bead cache of the shas it already carries — one read per bead even
    // when several commits reference it.
    let mut seen_shas: std::collections::HashMap<String, HashSet<String>> =
        std::collections::HashMap::new();

    for commit in &commits {
        for closure in &commit.closures {
            report.closures += 1;
            let Some(bead_id) = resolve_bead_id(&known, &closure.bead_id) else {
                if is_bead_shaped(&closure.bead_id) {
                    dangling.insert(closure.bead_id.clone());
                } else {
                    noise.insert(closure.bead_id.clone());
                }
                continue;
            };

            if !seen_shas.contains_key(&bead_id) {
                let existing = existing_git_event_ids(store, &bead_id, repo_name).await?;
                seen_shas.insert(bead_id.clone(), existing);
            }
            let shas = seen_shas.get_mut(&bead_id).expect("just inserted");
            if shas.contains(&commit.sha) {
                report.already_present += 1;
                continue;
            }

            if !dry_run {
                write_merge_observation(store, repo_name, &bead_id, commit, closure.pr_number)
                    .await?;
            }
            // Record the sha either way so a commit referencing the same bead
            // twice counts once, dry-run included.
            shas.insert(commit.sha.clone());
            report.recorded += 1;
        }
    }

    report.non_bead_brackets = noise.len();
    report.dangling = {
        let mut v: Vec<String> = dangling.into_iter().collect();
        v.sort();
        v
    };
    Ok(report)
}

/// Write one merge's `PipelineVerdict::Done` observation onto `bead_id`, in the
/// `{"observation": …, "detail": …}` envelope `shadow::parse_events_for` reads
/// back. The commit sha is the `source_event_id` — the dedup key that makes a
/// re-run a no-op.
async fn write_merge_observation(
    store: &dyn BeadStore,
    repo_name: &str,
    bead_id: &str,
    commit: &vcs::MergeCommit,
    pr_number: u64,
) -> Result<()> {
    let obs = Observation::pipeline_verdict(
        WorkRef {
            repo: repo_name.to_string(),
            scope: String::new(),
            bead_id: bead_id.to_string(),
        },
        Source::new(GIT_SOURCE),
        commit.sha.clone(),
        PipelineVerdictValue::Done,
        commit.committed_at,
    );
    let short = &commit.sha[..commit.sha.len().min(12)];
    let detail = serde_json::to_string(&serde_json::json!({
        "observation": obs,
        "detail": format!("squash merge {short} (PR #{pr_number}) — backfilled from git history"),
    }))?;
    store.log_event(bead_id, "observation", &detail).await;
    Ok(())
}

/// Does `reference` have the shape `generate_bead_id` produces —
/// `<prefix>-<exactly 6 hex>`? The bracket parser (`extract_bracket_ids`,
/// subject-only since rosary-e0e19f) is deliberately loose, so a subject
/// bracket like `[--dry-run]` arrives here looking like a closure. Those are
/// prose, not missing beads; keeping them out of `dangling` is what makes the
/// dangling count usable as rosary-225c94 evidence.
fn is_bead_shaped(reference: &str) -> bool {
    match reference.rsplit_once('-') {
        Some((prefix, suffix)) => {
            !prefix.is_empty() && suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_hexdigit())
        }
        None => false,
    }
}

/// The set of commit shas this bead already carries a `git`-sourced observation
/// for. Read straight back out of the event log — the lattice's own dedup key
/// (`source`, `source_event_id`), no side-table.
async fn existing_git_event_ids(
    store: &dyn BeadStore,
    bead_id: &str,
    repo_name: &str,
) -> Result<HashSet<String>> {
    let events = store.list_event_details(bead_id, "observation").await?;
    let work = WorkRef {
        repo: repo_name.to_string(),
        scope: String::new(),
        bead_id: bead_id.to_string(),
    };
    Ok(parse_events_for(&events, &work)
        .into_iter()
        .filter(|o| o.source.as_str() == GIT_SOURCE)
        .map(|o| o.source_event_id)
        .collect())
}

/// Resolve a bracket ref to a full bead id. Exact match wins; otherwise a
/// *unique* suffix match (the same rule `close-merged --local` applies). An
/// ambiguous suffix resolves to nothing rather than guessing.
fn resolve_bead_id(known: &[String], reference: &str) -> Option<String> {
    if known.iter().any(|k| k == reference) {
        return Some(reference.to_string());
    }
    let mut matches = known.iter().filter(|k| k.ends_with(reference));
    let first = matches.next()?;
    if matches.next().is_some() {
        return None; // ambiguous — refuse to guess
    }
    Some(first.clone())
}

impl BackfillReport {
    /// Human-readable summary for the CLI.
    pub fn render(&self, repo_name: &str) -> String {
        let verb = if self.dry_run {
            "would record"
        } else {
            "recorded"
        };
        let mut out = format!(
            "lattice backfill [{repo_name}]{}: merges={} closures={} {verb}={} \
             already_present={} dangling={} non_bead_brackets={}\n",
            if self.dry_run { " (dry run)" } else { "" },
            self.merge_commits,
            self.closures,
            self.recorded,
            self.already_present,
            self.dangling.len(),
            self.non_bead_brackets,
        );
        for id in &self.dangling {
            out.push_str(&format!(
                "  DANGLING {id} — referenced by a merge commit, absent from the store\n"
            ));
        }
        out.push_str(
            "  note: git witnesses the terminal MERGE only — each backfilled bead gets one \
             Done observation, not a reconstructed lifecycle.\n",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_sqlite::connect_bead_store;
    use crate::store::NewBead;

    /// A throwaway git repo with a trunk carrying the given squash subjects.
    fn git_repo_with_merges(subjects: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}: {ok:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        for (i, s) in subjects.iter().enumerate() {
            std::fs::write(p.join(format!("f{i}")), "x").unwrap();
            run(&["add", &format!("f{i}")]);
            run(&["commit", "-q", "-m", s]);
        }
        tmp
    }

    async fn store_with(ids: &[&str]) -> (tempfile::TempDir, Box<dyn BeadStore>) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = connect_bead_store(&tmp.path().join(".beads"))
            .await
            .unwrap();
        for id in ids {
            store
                .create_bead_full(NewBead {
                    id: (*id).into(),
                    title: (*id).into(),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        (tmp, store)
    }

    #[tokio::test]
    async fn backfill_records_one_done_observation_per_merge() {
        let repo = git_repo_with_merges(&[
            "[myrepo-aaa111] feat(x): thing (#1)",
            "wip: not a merge",
            "[myrepo-bbb222] fix(y): other (#2)",
        ]);
        let (_t, store) = store_with(&["myrepo-aaa111", "myrepo-bbb222"]).await;

        let r = backfill_repo(&*store, repo.path(), "myrepo", 50, false)
            .await
            .unwrap();
        assert_eq!(r.merge_commits, 2, "the wip commit has no (#N)");
        assert_eq!(r.recorded, 2);
        assert!(r.dangling.is_empty());

        // The audit can now see them, and they fold to `done`.
        let report = crate::observation::audit::audit_store(&*store, "myrepo")
            .await
            .unwrap();
        assert_eq!(report.comparable, 2, "corpus is no longer empty");
        // Both beads are still `open` in the mutable cell → both diverge.
        assert_eq!(report.divergences.len(), 2);
        assert!(
            report
                .divergences
                .iter()
                .all(|d| d.expected.as_deref() == Some("done"))
        );
    }

    /// The idempotency proof: a second run records nothing new.
    #[tokio::test]
    async fn backfill_is_idempotent_on_the_commit_sha() {
        let repo = git_repo_with_merges(&["[myrepo-aaa111] feat(x): thing (#1)"]);
        let (_t, store) = store_with(&["myrepo-aaa111"]).await;

        let first = backfill_repo(&*store, repo.path(), "myrepo", 50, false)
            .await
            .unwrap();
        assert_eq!(first.recorded, 1);

        let second = backfill_repo(&*store, repo.path(), "myrepo", 50, false)
            .await
            .unwrap();
        assert_eq!(second.recorded, 0, "re-run adds nothing");
        assert_eq!(second.already_present, 1);

        let events = store
            .list_event_details("myrepo-aaa111", "observation")
            .await
            .unwrap();
        assert_eq!(events.len(), 1, "no duplicate event row");
    }

    #[tokio::test]
    async fn dry_run_writes_nothing() {
        let repo = git_repo_with_merges(&["[myrepo-aaa111] feat(x): thing (#1)"]);
        let (_t, store) = store_with(&["myrepo-aaa111"]).await;

        let r = backfill_repo(&*store, repo.path(), "myrepo", 50, true)
            .await
            .unwrap();
        assert_eq!(r.recorded, 1, "reports what it would do");
        assert!(r.dry_run);
        let events = store
            .list_event_details("myrepo-aaa111", "observation")
            .await
            .unwrap();
        assert!(events.is_empty(), "dry run persisted nothing");
    }

    /// Dangling `[bead-id]`s are reported, never invented (rosary-225c94) —
    /// and only SUBJECT brackets are consulted at all (rosary-e0e19f): a body
    /// bracket is provenance, not a closure, so even a bead-shaped body ref is
    /// neither recorded nor dangling. Prose-shaped subject brackets still land
    /// in `non_bead_brackets` so the dangling number stays honest.
    #[tokio::test]
    async fn unknown_bead_ids_are_reported_as_dangling() {
        let repo = git_repo_with_merges(&[
            "[myrepo-aaa111] feat(x): thing (#1)",
            "[myrepo-9f0571] feat(x): vanished (#2)\n\ndocs mention [--dry-run] and [myrepo-beef01]\n",
            "[--dry-run] docs: prose-shaped subject bracket (#3)",
        ]);
        let (_t, store) = store_with(&["myrepo-aaa111"]).await;

        let r = backfill_repo(&*store, repo.path(), "myrepo", 50, false)
            .await
            .unwrap();
        assert_eq!(r.recorded, 1);
        assert_eq!(
            r.dangling,
            vec!["myrepo-9f0571".to_string()],
            "subject refs only — the body's [myrepo-beef01] must not dangle"
        );
        assert_eq!(
            r.non_bead_brackets, 1,
            "subject [--dry-run] is prose; body brackets never enter"
        );
        assert!(r.render("myrepo").contains("DANGLING myrepo-9f0571"));
    }

    #[test]
    fn bead_shape_separates_real_refs_from_prose_brackets() {
        assert!(is_bead_shaped("rosary-a66b3a"));
        assert!(is_bead_shaped("ley-line-open-e5addb"));
        assert!(!is_bead_shaped("--dry-run"));
        assert!(!is_bead_shaped("bead-id"));
        assert!(!is_bead_shaped("lattice-shadow"));
        assert!(!is_bead_shaped("nodash"));
        assert!(!is_bead_shaped("-a66b3a"), "empty prefix");
    }

    /// Backfill must not touch bead state — `persist_status` stays authoritative.
    #[tokio::test]
    async fn backfill_does_not_mutate_bead_state() {
        let repo = git_repo_with_merges(&["[myrepo-aaa111] feat(x): thing (#1)"]);
        let (_t, store) = store_with(&["myrepo-aaa111"]).await;

        let before = store.list_all_beads("myrepo").await.unwrap()[0]
            .status
            .clone();
        backfill_repo(&*store, repo.path(), "myrepo", 50, false)
            .await
            .unwrap();
        let after = store.list_all_beads("myrepo").await.unwrap()[0]
            .status
            .clone();
        assert_eq!(before, after, "status is untouched");
    }

    #[test]
    fn resolve_prefers_exact_and_refuses_ambiguous_suffixes() {
        let known = vec![
            "myrepo-abc123".to_string(),
            "other-abc123".to_string(),
            "myrepo-unique".to_string(),
        ];
        assert_eq!(
            resolve_bead_id(&known, "myrepo-abc123").as_deref(),
            Some("myrepo-abc123")
        );
        assert_eq!(resolve_bead_id(&known, "abc123"), None, "ambiguous suffix");
        assert_eq!(
            resolve_bead_id(&known, "unique").as_deref(),
            Some("myrepo-unique")
        );
        assert_eq!(resolve_bead_id(&known, "nope"), None);
    }
}
