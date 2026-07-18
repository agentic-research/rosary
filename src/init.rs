//! `rsry init` — the rsry-native onboarding primitive (the bd-init equivalent,
//! ADR-0014). Bootstraps a repo's `.beads/` store, writes/refreshes the managed
//! `AGENTS.md` section ("this repo's work is tracked as beads via rsry"), and
//! leaves hook install + global registration to the command handler so each
//! concern stays independently testable.
//!
//! Idempotent: safe to re-run. The AGENTS.md section lives between markers so
//! human-authored prose around it is preserved, and a stale **bd**-generated
//! `<!-- BEGIN BEADS INTEGRATION -->` block is replaced wholesale — killing the
//! bd→rsry split-brain propagation (the cloister incident) at its source.

use std::path::Path;

use anyhow::{Context, Result};

/// Canonical AGENTS.md content `rsry init` writes. Single source of truth,
/// embedded at compile time (the same file rolled across the ecosystem).
const AGENTS_TEMPLATE: &str = include_str!("../docs/onboarding/AGENTS.md");

/// Markers around the rsry-managed AGENTS.md section. Content outside them is
/// preserved verbatim across re-runs.
const MARKER_START: &str =
    "<!-- >>> rsry beads (managed by `rsry init` — edit outside these markers) >>>";
const MARKER_END: &str = "<!-- <<< rsry beads <<< -->";

/// bd's integration block, replaced wholesale when found. These are the exact
/// markers `bd setup <tool>` emits.
const BD_BLOCK_START: &str = "<!-- BEGIN BEADS INTEGRATION -->";
const BD_BLOCK_END: &str = "<!-- END BEADS INTEGRATION -->";

/// What `init::run` did to the store, for the command handler's summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOutcome {
    CreatedSqlite,
    CreatedDolt,
    AlreadyPresent,
}

/// What happened to AGENTS.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsOutcome {
    Created,
    SectionUpdated,
    ReplacedBdBlock,
    AppendedSection,
    Unchanged,
}

/// Result of the repo-local half of `rsry init` (store + metadata + AGENTS.md).
/// Hook install and global registration are the handler's job.
#[derive(Debug, Clone)]
pub struct InitOutcome {
    pub store: StoreOutcome,
    pub agents: AgentsOutcome,
}

/// Bootstrap the repo-local store + metadata + AGENTS.md section.
pub async fn run(repo_root: &Path, use_dolt: bool) -> Result<InitOutcome> {
    let beads_dir = repo_root.join(".beads");
    let store = init_store(repo_root, &beads_dir, use_dolt).await?;
    // Never git-track the store binary: a committed binary DB has no git 3-way
    // merge, so ordinary resolutions silently eat live bead state
    // (rosary-05fbe0). Idempotent — also drops the guard into an existing repo
    // that predates it.
    write_beads_gitignore(&beads_dir)?;
    let agents = write_agents(&repo_root.join("AGENTS.md"))?;
    Ok(InitOutcome { store, agents })
}

/// Create the bead store if absent. SQLite is the default — a single local
/// `beads.db` that is **git-ignored** (see [`write_beads_gitignore`]): a binary
/// DB has no git 3-way merge, so committing it lets ordinary resolutions eat
/// live state (rosary-05fbe0). SQLite is single-user/local; `--dolt` opts into
/// the Dolt server path for repos that must share bead state across clones
/// (Dolt syncs over its own remote, not git). Idempotent.
async fn init_store(repo_root: &Path, beads_dir: &Path, use_dolt: bool) -> Result<StoreOutcome> {
    if beads_dir.join("dolt").exists() {
        return Ok(StoreOutcome::AlreadyPresent);
    }
    if use_dolt {
        crate::dolt::init_beads_db(repo_root)
            .await
            .context("initializing Dolt bead store")?;
        return Ok(StoreOutcome::CreatedDolt);
    }
    // SQLite path.
    let db_path = beads_dir.join("beads.db");
    if db_path.exists() {
        return Ok(StoreOutcome::AlreadyPresent);
    }
    // connect() creates `.beads/` + `beads.db` + schema; drop immediately.
    let _store = crate::bead_sqlite::SqliteBeadStore::connect(&db_path)
        .context("creating SQLite bead store")?;
    // bd-compatible metadata marker so the store is self-describing.
    let meta = beads_dir.join("metadata.json");
    if !meta.exists() {
        std::fs::write(&meta, "{\"backend\": \"sqlite\"}\n")
            .with_context(|| format!("writing {}", meta.display()))?;
    }
    Ok(StoreOutcome::CreatedSqlite)
}

/// Canonical `.beads/.gitignore`. The bead-store binary is LOCAL and must never
/// be git-tracked: a committed binary DB has no git 3-way merge, so ordinary
/// resolutions (`checkout --theirs/--ours`, `reset --hard`, `stash pop`)
/// silently overwrite live bead state (rosary-05fbe0). `metadata.json` /
/// `config.yaml` stay tracked so a clone still knows the backend.
const BEADS_GITIGNORE: &str = "\
# Managed by `rsry init`. The bead-store binary is LOCAL — never git-tracked.
# A committed binary DB has no git 3-way merge; ordinary resolutions
# (checkout --theirs/--ours, reset --hard, stash pop) silently eat live bead
# state (rosary-05fbe0). SQLite is single-user/local; share across clones with
# `--dolt` (Dolt syncs over its own remote). metadata.json/config.yaml stay
# tracked so a clone knows the backend.

# SQLite store + WAL/journal sidecars
beads.db
beads.db-shm
beads.db-wal
beads.db-journal
beads.db.migrated
*.db
*.db-shm
*.db-wal
*.db-journal

# Dolt store + server runtime (synced via dolt remote, not git)
dolt/
dolt-server.pid
dolt-server.log
dolt-server.port
dolt-server.lock
dolt-access.lock

# Host-local runtime + credential
.beads-credential-key
.local_version
last-touched
backup/
";

/// Write `.beads/.gitignore` if absent so the store binary is never tracked
/// (rosary-05fbe0). Non-clobbering: an existing gitignore (e.g. a legacy bd
/// one) is left untouched. Returns whether a file was written.
fn write_beads_gitignore(beads_dir: &Path) -> Result<bool> {
    let path = beads_dir.join(".gitignore");
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(&path, BEADS_GITIGNORE)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// Write or refresh the managed AGENTS.md section.
fn write_agents(path: &Path) -> Result<AgentsOutcome> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let (next, outcome) = merge_agents(existing.as_deref());
    if existing.as_deref() != Some(next.as_str()) {
        std::fs::write(path, &next).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(outcome)
}

/// The rsry-managed block, always wrapped in markers so re-runs update in place.
fn managed_block() -> String {
    let mut b = String::with_capacity(AGENTS_TEMPLATE.len() + 128);
    b.push_str(MARKER_START);
    b.push('\n');
    b.push_str(AGENTS_TEMPLATE.trim_end());
    b.push('\n');
    b.push_str(MARKER_END);
    b.push('\n');
    b
}

/// Compute the next AGENTS.md contents from what's there now.
///
/// Precedence: (1) our marker section exists → replace it; (2) a bd
/// `BEGIN/END BEADS INTEGRATION` block exists → replace that span (kills bd's
/// propagation); (3) a non-empty file → append our section; (4) no file → the
/// managed block is the whole file.
fn merge_agents(existing: Option<&str>) -> (String, AgentsOutcome) {
    let block = managed_block();
    let Some(existing) = existing else {
        return (block, AgentsOutcome::Created);
    };

    // (1) Our markers already present — replace between them.
    if let Some(start) = existing.find(MARKER_START) {
        let after = start + MARKER_START.len();
        let end = existing[after..]
            .find(MARKER_END)
            .map(|i| after + i + MARKER_END.len())
            .unwrap_or(existing.len());
        // Consume a single trailing newline after the end marker so we don't
        // accumulate blank lines on every re-run.
        let tail_start = end + usize::from(existing[end..].starts_with('\n'));
        let mut out = String::with_capacity(existing.len() + block.len());
        out.push_str(&existing[..start]);
        out.push_str(&block);
        out.push_str(&existing[tail_start..]);
        let outcome = if out == existing {
            AgentsOutcome::Unchanged
        } else {
            AgentsOutcome::SectionUpdated
        };
        return (out, outcome);
    }

    // (2) bd's block present — replace the whole span, markers included.
    if let Some(start) = existing.find(BD_BLOCK_START) {
        let end = existing[start..]
            .find(BD_BLOCK_END)
            .map(|i| start + i + BD_BLOCK_END.len())
            .unwrap_or(existing.len());
        let tail_start = end + usize::from(existing[end..].starts_with('\n'));
        let mut out = String::with_capacity(existing.len() + block.len());
        out.push_str(&existing[..start]);
        out.push_str(block.trim_end());
        out.push('\n');
        out.push_str(&existing[tail_start..]);
        return (out, AgentsOutcome::ReplacedBdBlock);
    }

    // (3) Append to human-authored content, blank line for a clear boundary.
    let mut out = existing.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&block);
    (out, AgentsOutcome::AppendedSection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_file_is_the_managed_block() {
        let (out, outcome) = merge_agents(None);
        assert_eq!(outcome, AgentsOutcome::Created);
        assert!(out.contains(MARKER_START) && out.contains(MARKER_END));
        assert!(out.contains("tracked as beads") || out.contains("beads"));
    }

    #[test]
    fn rerun_is_idempotent() {
        let (first, _) = merge_agents(None);
        let (second, outcome) = merge_agents(Some(&first));
        assert_eq!(second, first, "re-running init must not drift the file");
        assert_eq!(outcome, AgentsOutcome::Unchanged);
    }

    #[test]
    fn preserves_human_prose_around_section() {
        let (first, _) = merge_agents(None);
        // Human adds a heading above and a note below the managed block.
        let edited = format!("# My Repo\n\nHand-written intro.\n\n{first}\n## Notes\nkeep me\n");
        let (out, outcome) = merge_agents(Some(&edited));
        assert_eq!(outcome, AgentsOutcome::Unchanged);
        assert!(out.contains("Hand-written intro."));
        assert!(out.contains("## Notes\nkeep me"));
    }

    #[test]
    fn replaces_bd_block_wholesale() {
        let bd = "# Agent Instructions\n\nIntro from a human.\n\n\
            <!-- BEGIN BEADS INTEGRATION -->\nUse `bd ready` to find work.\n\
            <!-- END BEADS INTEGRATION -->\n\n## Footer\nkeep\n";
        let (out, outcome) = merge_agents(Some(bd));
        assert_eq!(outcome, AgentsOutcome::ReplacedBdBlock);
        // bd's imperative is gone; our markers are in.
        assert!(!out.contains("bd ready"));
        assert!(!out.contains(BD_BLOCK_START));
        assert!(out.contains(MARKER_START));
        // Human content on both sides survives.
        assert!(out.contains("Intro from a human."));
        assert!(out.contains("## Footer\nkeep"));
    }

    #[tokio::test]
    async fn run_creates_sqlite_store_and_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let first = run(root, false).await.unwrap();
        assert_eq!(first.store, StoreOutcome::CreatedSqlite);
        assert_eq!(first.agents, AgentsOutcome::Created);
        assert!(root.join(".beads/beads.db").exists());
        assert!(root.join(".beads/metadata.json").exists());
        assert!(root.join("AGENTS.md").exists());

        // The store binary is git-ignored, never tracked (rosary-05fbe0), while
        // metadata stays tracked so a clone knows the backend.
        let gi = std::fs::read_to_string(root.join(".beads/.gitignore")).unwrap();
        assert!(gi.contains("beads.db"), "must ignore the store binary");
        assert!(gi.contains("*.db"), "must ignore db sidecars");
        assert!(gi.contains("dolt/"), "must ignore the Dolt store");
        assert!(
            !gi.lines().any(|l| l.trim() == "metadata.json"),
            "metadata must stay tracked (no ignore pattern for it)"
        );

        // Re-run is idempotent: store already present, AGENTS.md unchanged.
        let second = run(root, false).await.unwrap();
        assert_eq!(second.store, StoreOutcome::AlreadyPresent);
        assert_eq!(second.agents, AgentsOutcome::Unchanged);
    }

    #[test]
    fn gitignore_written_once_and_never_clobbered() {
        let tmp = tempfile::tempdir().unwrap();
        let beads = tmp.path().join(".beads");
        std::fs::create_dir_all(&beads).unwrap();

        // First call writes the guard.
        assert!(write_beads_gitignore(&beads).unwrap());
        assert!(beads.join(".gitignore").exists());

        // A pre-existing gitignore (e.g. a legacy bd one) is preserved, not
        // overwritten — the call is non-clobbering and returns false.
        std::fs::write(beads.join(".gitignore"), "custom-user-content\n").unwrap();
        assert!(!write_beads_gitignore(&beads).unwrap());
        assert_eq!(
            std::fs::read_to_string(beads.join(".gitignore")).unwrap(),
            "custom-user-content\n"
        );
    }

    #[test]
    fn appends_when_no_markers_present() {
        let human = "# Agent Instructions\n\nDo the thing.\n";
        let (out, outcome) = merge_agents(Some(human));
        assert_eq!(outcome, AgentsOutcome::AppendedSection);
        assert!(out.starts_with("# Agent Instructions\n\nDo the thing.\n"));
        assert!(out.contains(MARKER_START));
    }
}
