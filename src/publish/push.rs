//! pre-push gate over the PUSHED ref's blob (rosary-e5c037, ADR-0024 P3).
//!
//! ## What the previous gate measured, and why it was the wrong thing
//!
//! rosary-9c0e6c ran a whole-store export and `cmp`'d it against the
//! WORKING-TREE `.beads/beads.jsonl`. That is a statement about the checkout,
//! not about the commits being pushed. On the branch that wrote, the
//! every-write refresh had just rewritten the file, so the check was a
//! tautology and a stale pushed commit passed (journey observation 3). On any
//! other branch the whole-store export differed from that branch's file by
//! construction, so pushes were refused for beads written elsewhere
//! (observation 5), and the prescribed remedy was a broadening sync commit
//! that swept unrelated bead state into the branch.
//!
//! ## What this gate measures
//!
//! For each ref git hands the hook (`<local ref> <local sha> <remote ref>
//! <remote sha>` on stdin), the commits the push adds are
//! `<remote sha>..<local sha>`. The beads those commits NAME — `[<id>]`
//! brackets in their subjects, the same convention `close-merged --local`
//! reads — must each have a record in the pushed tip's `.beads/beads.jsonl`
//! blob that is byte-identical to what the store renders for them right now.
//! The renderer is the one `upsert_tracked_bead` writes with
//! ([`crate::jsonl_sync::render_bead_line`]), so the comparison is exact, not
//! heuristic.
//!
//! A bead nobody named is not this push's business, whatever the store holds:
//! a code-only range is never refused, and a store full of beads the branch
//! never touched cannot block it. The store is not even opened unless some
//! pushed subject names a bead.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::store::BeadStore;

const PUSHED_JSONL_PATH: &str = ".beads/beads.jsonl";

/// One line of git's pre-push stdin: the ref being pushed and where the remote
/// currently is. Deletes (`local_sha` all zeros) are dropped at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PushedRef {
    pub local_sha: String,
    pub remote_sha: String,
}

fn is_null_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|b| b == b'0')
}

/// Parse `<local ref> <local sha> <remote ref> <remote sha>` lines.
pub(crate) fn parse_prepush_stdin(input: &str) -> Result<Vec<PushedRef>> {
    let mut refs = Vec::new();
    for line in input.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [_local_ref, local_sha, _remote_ref, remote_sha] = fields[..] else {
            bail!(
                "pre-push stdin: expected `<local ref> <local sha> <remote ref> <remote sha>`, got {line:?}"
            );
        };
        if is_null_sha(local_sha) {
            continue; // a ref delete pushes no commits
        }
        refs.push(PushedRef {
            local_sha: local_sha.to_string(),
            remote_sha: remote_sha.to_string(),
        });
    }
    Ok(refs)
}

fn run_git_in(repo_root: &Path, args: &[String]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))
}

/// Subjects of the commits `pushed` adds to the remote.
///
/// A ref the remote does not have yet (`remote_sha` all zeros) has no
/// `<remote>..<local>` range. Taking all of `<local sha>` there would name
/// every bead in the repo's history and re-create observation 5 on every
/// new-branch push, so the range is instead everything not already on a
/// remote-tracking ref — for a branch cut from `origin/main`, exactly the
/// branch's own commits. With no remotes at all it degrades to full history.
fn pushed_subjects(
    repo_root: &Path,
    pushed: &PushedRef,
    remote: Option<&str>,
) -> Result<Vec<String>> {
    let mut args: Vec<String> = vec!["log".into(), "--format=%s".into()];
    if is_null_sha(&pushed.remote_sha) {
        // A new remote ref: everything the DESTINATION does not already have.
        // Scoped to that remote's tracking refs when the hook tells us which
        // one (`--remote "$1"`); a second remote's refs, or a stale tracking
        // ref, must not hide commits from the check. Without the name, fall
        // back to every remote-tracking ref.
        args.push(pushed.local_sha.clone());
        args.push("--not".into());
        args.push(match remote {
            Some(name) if !name.is_empty() => format!("--remotes={name}"),
            _ => "--remotes".into(),
        });
    } else {
        args.push(format!("{}..{}", pushed.remote_sha, pushed.local_sha));
    }
    let out = run_git_in(repo_root, &args)?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Bead ids the pushed commits name in their subjects, deduplicated and sorted.
pub(crate) fn named_bead_ids(subjects: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = subjects
        .iter()
        .flat_map(|s| crate::vcs::extract_bracket_ids(s))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Records in the tip's tracked projection, id → the exact line. A tip
/// without the file has no records, so every named bead is "missing" there.
/// Whether the pushed tip tracks the export at all. Opt-in is a property of
/// the ARTIFACT being pushed, not of the checkout the push runs from: a tip
/// without `.beads/beads.jsonl` never asked for the gate, whatever branch the
/// operator happens to be on.
fn tip_tracks_export(repo_root: &Path, sha: &str) -> Result<bool> {
    let out = run_git_in(
        repo_root,
        &[
            "cat-file".into(),
            "-e".into(),
            format!("{sha}:{PUSHED_JSONL_PATH}"),
        ],
    )?;
    Ok(out.status.success())
}

fn blob_records(repo_root: &Path, sha: &str) -> Result<BlobIndex> {
    let out = run_git_in(
        repo_root,
        &["show".into(), format!("{sha}:{PUSHED_JSONL_PATH}")],
    )?;
    if !out.status.success() {
        return Ok(BlobIndex::default());
    }
    let text = String::from_utf8(out.stdout).context("pushed beads.jsonl is not UTF-8")?;
    Ok(parse_blob(&text))
}

/// The pushed projection, indexed by bead id. A duplicated id is recorded
/// rather than collapsed: a file carrying two lines for one bead is not an
/// exact projection whichever line happens to come last.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BlobIndex {
    pub records: BTreeMap<String, String>,
    pub duplicates: BTreeSet<String>,
}

/// Index a projection's lines by id. A line that does not parse cannot be a
/// current rendering of anything, so it simply does not index.
pub(crate) fn parse_blob(text: &str) -> BlobIndex {
    let mut index = BlobIndex::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Some(id) = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|record| record.get("id")?.as_str().map(str::to_owned))
        else {
            continue;
        };
        if index.records.insert(id.clone(), line.to_owned()).is_some() {
            index.duplicates.insert(id);
        }
    }
    index
}

/// The named beads whose pushed record is absent or differs from the store.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Disagreement {
    pub missing: Vec<String>,
    pub stale: Vec<String>,
}

impl Disagreement {
    fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.stale.is_empty()
    }
}

pub(crate) async fn compare(
    store: &dyn BeadStore,
    repo_name: &str,
    ids: &[String],
    blob: &BlobIndex,
) -> Result<Disagreement> {
    let mut disagreement = Disagreement::default();
    for id in ids {
        // A subject can name a bead this store has never held (another repo's,
        // or a placeholder id); there is no rendering to disagree with.
        let Some(bead) = store.get_bead(id, repo_name).await? else {
            continue;
        };
        let rendered = crate::jsonl_sync::render_bead_line(store, &bead).await?;
        match blob.records.get(id) {
            None => disagreement.missing.push(id.clone()),
            Some(_) if blob.duplicates.contains(id) => disagreement.stale.push(id.clone()),
            Some(line) if *line != rendered => disagreement.stale.push(id.clone()),
            Some(_) => {}
        }
    }
    Ok(disagreement)
}

/// What a push asks the gate to check: each pushed ref with the ids its new
/// commits name. Refs naming no bead are dropped here, so a push of code-only
/// commits never reaches the store.
fn pushed_work(
    repo_root: &Path,
    input: &str,
    remote: Option<&str>,
) -> Result<Vec<(PushedRef, Vec<String>)>> {
    let mut work = Vec::new();
    for pushed in parse_prepush_stdin(input)? {
        if !tip_tracks_export(repo_root, &pushed.local_sha)? {
            continue; // this tip never opted in
        }
        let ids = named_bead_ids(&pushed_subjects(repo_root, &pushed, remote)?);
        if !ids.is_empty() {
            work.push((pushed, ids));
        }
    }
    Ok(work)
}

/// The store the projection is rendered from, and the repo label stamped on
/// each record (the same derivation as `publish::Projection`). `None` when
/// there is no projection to gate: a Dolt repo pushes its own store.
async fn open_projection_store(repo_root: &Path) -> Result<Option<(Box<dyn BeadStore>, String)>> {
    let beads_dir = crate::resolve_beads_dir(repo_root);
    use crate::bead_backend::{BeadBackend, detect_backend};
    match detect_backend(&beads_dir) {
        // Dolt repos push their own store (`dolt push`); no projection gate.
        BeadBackend::Dolt => return Ok(None),
        BeadBackend::Sqlite => {}
        // Fail closed: an ambiguous source of truth cannot vouch for a record.
        BeadBackend::Ambiguous => bail!(
            "bead verify-pushed: {} holds both dolt/ and beads.db — resolve the \
             ambiguous store before pushing bead records (rsry hooks audit)",
            beads_dir.display()
        ),
        // embeddeddolt/ alone is not readable by rsry; nothing to vouch with.
        BeadBackend::UnreadableEmbeddedOnly => bail!(
            "bead verify-pushed: {} holds only embeddeddolt/, which rsry cannot read — \
             migrate the store (rsry bead migrate) before pushing bead records",
            beads_dir.display()
        ),
        BeadBackend::Uninitialized => bail!(
            "bead verify-pushed: the pushed commits name beads, but there is no bead store \
             under {} to check their pushed records against",
            beads_dir.display()
        ),
    }
    let repo_name = beads_dir
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .with_context(|| format!("resolving the repo name from {}", beads_dir.display()))?;
    let store = crate::bead_sqlite::connect_bead_store(&beads_dir).await?;
    Ok(Some((store, repo_name)))
}

/// The operator-facing refusal: per pushed tip, what is missing and what is
/// stale, then the one command that fixes all of it.
pub(crate) fn refusal_message(failures: &[(String, Disagreement)]) -> String {
    let mut lines = Vec::new();
    let mut all_ids = Vec::new();
    for (sha, disagreement) in failures {
        let short = &sha[..sha.len().min(12)];
        let mut parts = Vec::new();
        if !disagreement.missing.is_empty() {
            parts.push(format!("missing {}", disagreement.missing.join(", ")));
        }
        if !disagreement.stale.is_empty() {
            parts.push(format!("stale {}", disagreement.stale.join(", ")));
        }
        lines.push(format!("  {short}: {}", parts.join("; ")));
        all_ids.extend(disagreement.missing.iter().cloned());
        all_ids.extend(disagreement.stale.iter().cloned());
    }
    all_ids.sort();
    all_ids.dedup();
    format!(
        "the pushed {PUSHED_JSONL_PATH} disagrees with the bead store for beads the pushed \
         commits name:\n{}\n  fix: rsry bead publish {} && git commit --amend --no-edit\n  \
         (or commit the publish as a follow-up), then push again",
        lines.join("\n"),
        all_ids.join(" ")
    )
}

/// `rsry bead verify-pushed`: the pre-push hook's primitive. Reads git's
/// pre-push stdin; `Err` (exit 1) names the beads whose pushed record is
/// missing or stale and the fix.
pub async fn verify_pushed(
    repo_root: &Path,
    remote: Option<&str>,
    mut prepush_stdin: impl Read,
) -> Result<()> {
    let mut input = String::new();
    prepush_stdin
        .read_to_string(&mut input)
        .context("reading pre-push stdin")?;
    let work = pushed_work(repo_root, &input, remote)?;
    if work.is_empty() {
        return Ok(());
    }
    let Some((store, repo_name)) = open_projection_store(repo_root).await? else {
        return Ok(());
    };

    let mut failures = Vec::new();
    for (pushed, ids) in work {
        let blob = blob_records(repo_root, &pushed.local_sha)?;
        let disagreement = compare(store.as_ref(), &repo_name, &ids, &blob).await?;
        if !disagreement.is_empty() {
            failures.push((pushed.local_sha, disagreement));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    bail!("{}", refusal_message(&failures))
}

#[cfg(test)]
mod tests;
