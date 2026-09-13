//! Commit-scoped publication: the commit-msg hook publishes exactly the beads the
//! commit subject names (thread trusted-kernel/projection-timing, P2).
//!
//! The commit is the moment a bead reaches git, and the commit MESSAGE says
//! which bead. Pre-commit runs before the message exists; commit-msg receives
//! it, and git writes the tree AFTER commit-msg — so a `git add` from that hook
//! lands in the commit. Only the SUBJECT is consulted, by the same rule as
//! [`crate::vcs::parse_merged_closures`]: a body mention is provenance, not
//! publication.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::bead_backend::{BeadBackend, detect_backend};

/// Publish the named beads' current records into the tracked projection.
///
/// Ids come from `ids` and from the subject line of `commit_msg`. An id whose
/// prefix is this repo's must exist in the store, else this fails loud — a typo
/// must not silently publish nothing. Ids for other repos are ignored. With no
/// own-repo id at all, nothing is read and nothing is written.
///
/// Prints one line per bead whose record CHANGED in the file, so the hook can
/// `git add` only when there is something to stage.
pub async fn publish_from_commit(
    repo_root: &Path,
    commit_msg: Option<&Path>,
    ids: &[String],
) -> Result<()> {
    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .with_context(|| format!("no repo name for {}", repo_root.display()))?;
    let mut wanted: Vec<String> = ids.to_vec();
    if let Some(path) = commit_msg {
        let message = std::fs::read_to_string(path)
            .with_context(|| format!("reading commit message {}", path.display()))?;
        wanted.extend(subject_bead_ids(&message));
    }
    let own: Vec<String> = dedup(wanted)
        .into_iter()
        .filter(|id| is_own_repo_id(id, &repo_name))
        .collect();
    if own.is_empty() {
        return Ok(());
    }

    let beads_dir = crate::resolve_beads_dir(repo_root);
    if matches!(
        detect_backend(&beads_dir),
        BeadBackend::Uninitialized | BeadBackend::Ambiguous
    ) {
        bail!(
            "commit names {} but {} has no usable bead store (run `rsry init`)",
            own.join(", "),
            beads_dir.display()
        );
    }
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir).await?;

    // Validate every id before writing any: a refused commit must leave the
    // projection exactly as it found it.
    for id in &own {
        if store.get_bead(id, &repo_name).await?.is_none() {
            bail!("unknown bead {id} in commit subject");
        }
    }
    for id in &own {
        let changed =
            crate::jsonl_sync::upsert_tracked_bead(store.as_ref(), id, &repo_name, repo_root, true)
                .await?;
        if changed {
            println!("published {id}");
        }
    }
    Ok(())
}

/// Every `[<prefix>-<suffix>]` bracket on the SUBJECT line, in order.
pub fn subject_bead_ids(message: &str) -> Vec<String> {
    crate::vcs::extract_bracket_ids(message.lines().next().unwrap_or(""))
}

/// Generated ids carry `sanitize_prefix(repo basename)` (`Repo_2` → `repo_2`,
/// `My Repo!` → `my-repo`), so the raw basename alone would disown the very
/// ids this repo mints.
fn is_own_repo_id(id: &str, repo_name: &str) -> bool {
    id.rsplit_once('-').is_some_and(|(prefix, _)| {
        prefix == repo_name || prefix == crate::sanitize_prefix(repo_name)
    })
}

fn dedup(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_ids_only_body_mentions_are_provenance() {
        let msg = "[rosary-aaaaaa] [other-bbbbbb] feat: x\n\n[rosary-cccccc] absorbed\n";
        assert_eq!(subject_bead_ids(msg), ["rosary-aaaaaa", "other-bbbbbb"]);
        assert!(subject_bead_ids("chore: no id\n\n[rosary-dddddd] body only").is_empty());
    }

    #[test]
    fn own_repo_is_the_prefix_before_the_last_dash() {
        assert!(is_own_repo_id("rosary-aaaaaa", "rosary"));
        assert!(!is_own_repo_id("other-aaaaaa", "rosary"));
        assert!(is_own_repo_id("canonical-hours-aaaaaa", "canonical-hours"));
        assert!(!is_own_repo_id("rosaryaaaaaa", "rosary"));
        // the generated grammar: sanitize_prefix normalises the basename
        assert!(is_own_repo_id("repo_2-aaaaaa", "Repo_2"));
        assert!(is_own_repo_id("my-repo-aaaaaa", "My Repo!"));
        assert!(!is_own_repo_id("repo-2-aaaaaa", "Repo_2"));
    }

    #[tokio::test]
    async fn no_own_id_reads_nothing_and_writes_nothing() {
        // No `.beads/` at all: reaching the store would fail loud, so a clean
        // Ok proves the early return.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let msg = root.join("MSG");
        std::fs::write(&msg, "[other-aaaaaa] chore: not ours\n").unwrap();
        publish_from_commit(&root, Some(&msg), &[]).await.unwrap();
        assert!(!root.join(".beads").exists());
    }

    #[tokio::test]
    async fn own_id_without_a_store_fails_loud() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let err = publish_from_commit(&root, None, &["project-aaaaaa".into()])
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("no usable bead store"),
            "{err:#}"
        );
    }
}
