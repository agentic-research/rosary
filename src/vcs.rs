//! Thin wrapper around leyline-vcs for automatic state versioning.
//!
//! Rosary's state directory (`~/.rsry/`) is a jj repo. Every state change
//! (bead status update, triage score, dispatch record) auto-snapshots via
//! leyline-vcs's sidecar pattern: the hot path writes to SQLite, the cold
//! path snapshots to jj asynchronously.
//!
//! Agents never interact with this directly — it's pure plumbing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use leyline_vcs::JjIntegration;

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
/// Initialize a jj repo in the state directory if one doesn't exist.
///
/// Uses leyline-vcs's JjIntegration (jj-lib native) instead of shelling
/// out to the jj CLI. init_or_open handles both fresh init and re-open.
pub fn init_jj(state_path: &Path) -> Result<()> {
    JjIntegration::init_or_open(state_path)
        .with_context(|| format!("jj init_or_open at {}", state_path.display()))?;
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
// Bead ID extraction from commit messages
// ---------------------------------------------------------------------------

/// A bead reference found in a commit message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeadRef {
    /// The bead ID (e.g., "rsry-abc123", "loom-7sd", "mache-tgl")
    pub id: String,
    /// Whether this reference closes the bead (e.g., "closes bead:...", "fixes bead:...")
    pub closes: bool,
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
pub fn extract_bead_refs(message: &str) -> Vec<BeadRef> {
    let mut refs = Vec::new();
    let lower = message.to_lowercase();

    // Find all occurrences of "bead:" followed by an ID
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find("bead:") {
        let abs_pos = search_from + pos;
        let after = &lower[abs_pos + 5..]; // skip "bead:"

        // Parse the bead ID: {prefix}-{suffix}
        // prefix: one or more lowercase alpha chars
        // suffix: one or more lowercase alphanumeric chars
        let id: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();

        // Must contain at least one '-' and have content on both sides
        if let Some(dash_pos) = id.find('-') {
            let prefix = &id[..dash_pos];
            let suffix = &id[dash_pos + 1..];
            if !prefix.is_empty()
                && !suffix.is_empty()
                && prefix.chars().all(|c| c.is_ascii_lowercase())
            {
                // Check for closing prefix: "closes", "fixes", "close", "fix"
                let before = &lower[..abs_pos].trim_end();
                let closes = before.ends_with("closes")
                    || before.ends_with("fixes")
                    || before.ends_with("close")
                    || before.ends_with("fix");

                refs.push(BeadRef {
                    id: id.clone(),
                    closes,
                });
            }
        }

        search_from = abs_pos + 5 + id.len().max(1);
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn init_jj_creates_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!tmp.path().join(".jj").exists());

        init_jj(tmp.path()).unwrap();
        assert!(tmp.path().join(".jj").exists());
    }

    #[test]
    fn init_jj_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();

        // First call inits, second call opens — both succeed
        init_jj(tmp.path()).unwrap();
        init_jj(tmp.path()).unwrap();
        assert!(tmp.path().join(".jj").exists());
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

    #[test]
    fn extract_bead_ids_convenience() {
        let ids = extract_bead_ids("bead:rsry-abc closes bead:loom-xyz");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"rsry-abc".to_string()));
        assert!(ids.contains(&"loom-xyz".to_string()));
    }
}
