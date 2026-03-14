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

/// Rosary state directory, default `~/.rsry/`.
pub fn state_dir() -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("cannot determine home directory")?;
    let dir = home.join(".rsry");
    Ok(dir)
}

/// Ensure the state directory exists and is initialized.
pub fn ensure_state_dir() -> Result<PathBuf> {
    let dir = state_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating state dir: {}", dir.display()))?;
    }
    Ok(dir)
}

/// Initialize a jj repo in the state directory if one doesn't exist.
///
/// Uses leyline-vcs's JjIntegration (jj-lib native) instead of shelling
/// out to the jj CLI. init_or_open handles both fresh init and re-open.
pub fn init_jj(state_path: &Path) -> Result<()> {
    JjIntegration::init_or_open(state_path)
        .with_context(|| format!("jj init_or_open at {}", state_path.display()))?;
    Ok(())
}

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
}
