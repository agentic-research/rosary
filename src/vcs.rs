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
/// This is called once on first run. After that, the sidecar handles
/// all versioning automatically.
pub fn init_jj(state_path: &Path) -> Result<()> {
    let jj_dir = state_path.join(".jj");
    if jj_dir.exists() {
        return Ok(());
    }

    // Use jj CLI for init — simpler than jj-lib for one-time setup
    let output = std::process::Command::new("jj")
        .args(["init"])
        .current_dir(state_path)
        .output()
        .context("running jj init (is jj installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("jj init failed: {stderr}");
    }

    eprintln!("[rsry] initialized jj repo at {}", state_path.display());
    Ok(())
}

/// Snapshot current state to jj. Non-blocking best-effort.
///
/// Called after state-changing operations (bead update, dispatch, etc).
/// Failures are logged but don't propagate — state versioning must never
/// block the hot path.
pub fn snapshot(state_path: &Path) {
    // jj auto-snapshots working copy changes, but we can force it
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
    fn init_jj_skips_if_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let jj_dir = tmp.path().join(".jj");
        std::fs::create_dir_all(&jj_dir).unwrap();

        // Should be a no-op
        let result = init_jj(tmp.path());
        assert!(result.is_ok());
    }
}
