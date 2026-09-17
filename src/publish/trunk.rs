//! Trunk refresh (ADR-0024 amendment A, P4 — rosary-e5c0a0): on the trunk,
//! after a merge, re-render every published bead from the store and commit
//! the projection, so the trunk carries the current record of every published
//! bead. Feature commits carry only the beads they name (`publish::commit`);
//! everything else — merge-evidence comments, close-merged status flips, a
//! comment an agent left on an unrelated bead — reaches the trunk here, on the
//! machine that has the store.
//!
//! The push transport is the trunk-writer decision (rosary-e5be46): when the
//! remote refuses the push (a PR-only trunk), the refresh commit is parked on
//! `rsry/trunk-refresh`, the trunk is reset to what the remote has, and the
//! hint names the decision — the working tree stays clean either way.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

const TRUNK_JSONL_PATH: &str = ".beads/beads.jsonl";
/// Where a refresh commit waits when the trunk refuses a direct push.
pub const PARKING_BRANCH: &str = "rsry/trunk-refresh";

#[derive(Debug, PartialEq, Eq)]
pub enum TrunkRefresh {
    /// Not on the trunk, not the canonical checkout, or the trunk does not
    /// track the projection — nothing for this hook to do here.
    Skipped(&'static str),
    /// Every published record already matched the store.
    NoChange,
    /// The refresh was committed on the trunk (and pushed, if asked and allowed).
    Committed { ids: Vec<String>, pushed: bool },
    /// Committed, the push was refused, and the commit now waits on
    /// [`PARKING_BRANCH`] with the trunk reset to the remote's state.
    Parked { ids: Vec<String> },
}

fn trunk_git(repo_root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))
}

fn trunk_git_ok(repo_root: &Path, args: &[&str]) -> Result<String> {
    let out = trunk_git(repo_root, args)?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The local trunk branch: `main`, else `master`.
fn trunk_branch(repo_root: &Path) -> Option<&'static str> {
    ["main", "master"].into_iter().find(|b| {
        trunk_git(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .map(|o| o.status.success())
        .unwrap_or(false)
    })
}

/// Ids whose rendered line differs between two projections (either side may
/// lack the id — a record that appeared or vanished counts as changed).
pub(crate) fn changed_ids(before: &str, after: &str) -> Vec<String> {
    let a = crate::publish::push::parse_blob(before);
    let b = crate::publish::push::parse_blob(after);
    let mut ids: Vec<String> = a
        .records
        .keys()
        .chain(b.records.keys())
        .filter(|id| a.records.get(*id) != b.records.get(*id))
        .cloned()
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// `[<first id>] chore(beads): trunk refresh — N record(s)`: the commit-msg
/// contract needs a bead in the subject, and the first changed bead is the
/// honest one to name; the rest are listed in the body.
pub(crate) fn refresh_message(ids: &[String]) -> String {
    let first = ids.first().map(String::as_str).unwrap_or("rosary-000000");
    let mut msg = format!(
        "[{first}] chore(beads): trunk refresh — {} record(s)\n\nRe-rendered from the store after a merge (ADR-0024 amendment A, rosary-e5c0a0):\n",
        ids.len()
    );
    for id in ids {
        msg.push_str("  ");
        msg.push_str(id);
        msg.push('\n');
    }
    msg
}

pub async fn refresh_trunk(repo_root: &Path, push: bool) -> Result<TrunkRefresh> {
    let git_dir = trunk_git_ok(repo_root, &["rev-parse", "--git-dir"])?;
    let common = trunk_git_ok(repo_root, &["rev-parse", "--git-common-dir"])?;
    if git_dir != common {
        return Ok(TrunkRefresh::Skipped(
            "linked worktree — the canonical checkout refreshes the trunk",
        ));
    }
    let Some(trunk) = trunk_branch(repo_root) else {
        return Ok(TrunkRefresh::Skipped("no main/master branch"));
    };
    let current = trunk_git_ok(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if current != trunk {
        return Ok(TrunkRefresh::Skipped("not on the trunk"));
    }
    if !trunk_git(
        repo_root,
        &["cat-file", "-e", &format!("HEAD:{TRUNK_JSONL_PATH}")],
    )?
    .status
    .success()
    {
        return Ok(TrunkRefresh::Skipped(
            "the trunk does not track .beads/beads.jsonl",
        ));
    }
    let dirty = trunk_git_ok(
        repo_root,
        &["status", "--porcelain", "--", TRUNK_JSONL_PATH],
    )?;
    if !dirty.is_empty() {
        bail!(
            "{TRUNK_JSONL_PATH} has uncommitted changes in the working tree; commit or discard them before a trunk refresh (under ADR-0024 amendment A nothing but a commit should touch it)"
        );
    }

    let beads_dir = crate::resolve_beads_dir(repo_root);
    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .with_context(|| format!("no repo name for {}", repo_root.display()))?;
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir).await?;
    let path = repo_root.join(TRUNK_JSONL_PATH);
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    crate::jsonl_sync::refresh_tracked_beads_jsonl(store.as_ref(), &repo_name, repo_root).await?;
    let after = std::fs::read_to_string(&path).unwrap_or_default();
    let ids = changed_ids(&before, &after);
    if ids.is_empty() {
        return Ok(TrunkRefresh::NoChange);
    }

    trunk_git_ok(repo_root, &["add", TRUNK_JSONL_PATH])?;
    let message = refresh_message(&ids);
    trunk_git_ok(repo_root, &["commit", "-q", "-m", &message])?;
    if !push {
        return Ok(TrunkRefresh::Committed { ids, pushed: false });
    }
    let pushed = trunk_git(repo_root, &["push", "-q", "origin", trunk])?;
    if pushed.status.success() {
        return Ok(TrunkRefresh::Committed { ids, pushed: true });
    }
    // Refused (a PR-only trunk, or no bypass for this identity): park the
    // commit where a PR can be opened from, and put the trunk back exactly
    // where the remote has it — the tree ends clean, nothing is lost.
    trunk_git_ok(repo_root, &["branch", "-f", PARKING_BRANCH, "HEAD"])?;
    trunk_git_ok(repo_root, &["reset", "-q", "--hard", "HEAD~1"])?;
    eprintln!(
        "[rsry trunk-refresh] push to origin/{trunk} refused:\n  {}\n  the refresh commit is parked on `{PARKING_BRANCH}` ({} record(s)); open a PR from it, or decide the trunk writer (rosary-e5be46) so this can push directly.",
        String::from_utf8_lossy(&pushed.stderr)
            .lines()
            .find(|l| l.contains("rejected") || l.contains("error") || l.contains("GH0"))
            .unwrap_or("(no detail)")
            .trim(),
        ids.len()
    );
    Ok(TrunkRefresh::Parked { ids })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_ids_sees_edits_additions_and_removals() {
        let before = "{\"id\":\"r-a\",\"v\":1}\n{\"id\":\"r-b\",\"v\":1}\n{\"id\":\"r-gone\"}\n";
        let after = "{\"id\":\"r-a\",\"v\":2}\n{\"id\":\"r-b\",\"v\":1}\n{\"id\":\"r-new\"}\n";
        assert_eq!(changed_ids(before, after), vec!["r-a", "r-gone", "r-new"]);
        assert!(changed_ids(after, after).is_empty());
    }

    #[test]
    fn refresh_message_names_the_first_bead_in_the_subject_and_all_in_the_body() {
        let msg = refresh_message(&["rosary-aaaaaa".into(), "rosary-bbbbbb".into()]);
        let subject = msg.lines().next().unwrap();
        assert!(
            subject.starts_with("[rosary-aaaaaa] chore(beads): trunk refresh"),
            "{subject}"
        );
        assert!(msg.contains("  rosary-bbbbbb\n"));
    }
}
