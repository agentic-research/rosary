//! Git hook management for bead sync (post-push / post-merge).
//!
//! Templates are embedded into the binary at compile time via `include_str!`
//! so installation works in any environment — release images, packaged
//! binaries, contributor checkouts — without depending on the source tree
//! being adjacent to the executable.
//!
//! Installation is merge-aware: rather than clobbering existing hooks, the
//! rsry block is spliced between the literal marker lines defined as
//! [`MARKER_START`] and [`MARKER_END`] below. User content outside those
//! markers is preserved across re-installs and the operation is
//! idempotent. `MARKER_END` is intentionally short so the closing line is
//! easy to grep for; `MARKER_START` carries the do-not-edit hint so
//! anyone opening the hook file sees the convention without a separate
//! README round-trip.
//!
//! The hooks directory is resolved via `git rev-parse --git-path hooks`
//! so worktrees, submodules (`.git` is a file pointer to the real
//! gitdir, not a directory), and `core.hooksPath` overrides all route to
//! the right place.
use crate::gitignore::{
    GitignoreShadowShape, allowlist_fix_suggestion, classify_gitignore_shadow_shape,
    remove_gitignore_line,
};

use crate::precommit_yaml::{is_precommit_framework_owned, merge_precommit_yaml};

use anyhow::{Context, Result};

use sha2::{Digest, Sha256};

use std::path::{Path, PathBuf};

use std::process::Command;

mod audit;
mod install;
pub use install::install;
pub(crate) use install::*;

pub use audit::audit;

use audit::{DoltRemoteStatus, GitignoreCheck, check_dolt_remote, classify_gitignore_check};

#[cfg(test)]
mod tests;

/// Begin marker line for the rsry-managed shell block inside a hook file.
/// Anything between this line and [`MARKER_END`] is regenerated on each
/// install. Including the do-not-edit hint on the start marker keeps the
/// convention visible inside the file itself.
pub(crate) const MARKER_START: &str =
    "# >>> rsry-managed (do not edit between these markers; `rsry hooks install` regenerates) >>>";

/// End marker line — closes the rsry-managed section.
pub(crate) const MARKER_END: &str = "# <<< rsry-managed <<<";

/// Hooks rsry manages and their canonical shell-body content.
///
/// Content lives in `docs/git-hooks/*` so it's reviewable alongside the
/// code; `include_str!` bakes it into the binary so installation works
/// in released images without `find_template_dir` style filesystem
/// guessing.
pub(crate) const HOOKS: &[(&str, &str)] = &[
    ("post-push", include_str!("../../docs/git-hooks/post-push")),
    (
        "post-merge",
        include_str!("../../docs/git-hooks/post-merge"),
    ),
    // Canonical-checkout / opt-in-by-tracking shell only: the projection
    // write moved to commit-msg, scoped to the beads the subject names
    // (rosary-e5bfd3, ADR-0024 amendment A).
    (
        "pre-commit",
        include_str!("../../docs/git-hooks/pre-commit"),
    ),
    // The commit contract (Rule 11 + Conventional Commits), then the
    // commit-scoped publication of the beads the subject names into the
    // tracked `.beads/beads.jsonl` (rosary-e5bfd3). Embedded so a fresh `rsry
    // hooks install` configures it without any manual symlink to ~/.rsry/hooks.
    (
        "commit-msg",
        include_str!("../../docs/git-hooks/commit-msg"),
    ),
    // Hard gate over the ARTIFACT being pushed (rosary-e5c037): for the
    // beads the pushed commits name, the pushed tip's `.beads/beads.jsonl`
    // must carry the store's exact rendering (`bead verify-pushed`). The
    // rosary-9c0e6c gate this replaces compared the working tree instead —
    // see the template's header comment for why that measured the wrong
    // thing.
    ("pre-push", include_str!("../../docs/git-hooks/pre-push")),
    // Folds the record commit-msg staged into the commit that named it: git
    // writes the tree from an index it read BEFORE commit-msg, so the stage
    // alone lands in the next commit, not this one (rosary-e5bfd3).
    (
        "post-commit",
        include_str!("../../docs/git-hooks/post-commit"),
    ),
];

/// Resolve the actual hooks directory for `repo_root`.
///
/// Uses `git rev-parse --git-path hooks` so worktrees and submodules
/// (where `.git` is a file pointing at the real gitdir) work correctly —
/// the previous `repo_root.join(".git").join("hooks")` shortcut was wrong
/// for both cases.
pub(crate) fn resolve_hooks_dir(repo_root: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .with_context(|| format!("invoking `git` in {}", repo_root.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "{} is not a git repo: {}",
            repo_root.display(),
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let candidate = Path::new(&rel);
    Ok(if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        repo_root.join(candidate)
    })
}

/// Render install-time metadata into a hook template.
///
/// Generated hooks deliberately do not contain the path of the installing
/// binary: `cargo test`, worktrees, and staged release directories are all
/// ephemeral. Templates resolve rsry at execution time. The version is
/// still embedded so a hook can warn when its runtime binary differs from
/// the binary whose templates installed it.
pub(crate) fn render_block(block: &str) -> String {
    block.replace("__RSRY_VERSION__", env!("CARGO_PKG_VERSION"))
}

/// Provenance line embedded inside each managed hook block.
///
/// The version identifies the binary that installed the hook. The digest
/// identifies the exact compiled-in template, so a hook can become stale
/// without a crate version bump and still be detected.
fn hook_stamp(name: &str, block: &str) -> String {
    let digest = hex::encode(Sha256::digest(block.as_bytes()));
    format!(
        "# rsry-hook {name} v{} sha256:{digest}",
        env!("CARGO_PKG_VERSION")
    )
}

/// Render a template as the complete rsry-managed block written to disk.
fn render_managed_block(name: &str, block: &str) -> String {
    format!("{}\n{}", hook_stamp(name, block), render_block(block))
}

/// Build a fresh hook file from scratch (no existing file at the path).
/// Wraps the rsry block in `#!/bin/sh` + a brief header + markers.
pub(crate) fn fresh_hook(block: &str) -> String {
    let mut out = String::new();
    out.push_str("#!/bin/sh\n");
    out.push_str("# Installed by `rsry hooks install`. Edit outside the rsry-managed\n");
    out.push_str("# section below to add your own logic; re-running install will\n");
    out.push_str("# regenerate only the marked block and preserve everything else.\n\n");
    out.push_str(MARKER_START);
    out.push('\n');
    out.push_str(block);
    if !block.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(MARKER_END);
    out.push('\n');
    out
}

/// Splice `block` into an existing hook file's contents.
///
/// - If the file already has an rsry marker section, replace just that
///   section. Content outside the markers (including the shebang and any
///   user-written shell logic) is preserved verbatim.
/// - If the file has no marker section, append one at the end so the
///   user's pre-existing hook continues to run AND the rsry block runs
///   after it.
pub(crate) fn merge_hook(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(MARKER_START) {
        let after_start = start + MARKER_START.len();
        // Find the matching end marker; if missing (corrupted file),
        // replace through end-of-file rather than leaving stale content.
        let end_inclusive = existing[after_start..]
            .find(MARKER_END)
            .map(|i| after_start + i + MARKER_END.len())
            .unwrap_or(existing.len());
        let mut out = String::with_capacity(existing.len() + block.len());
        out.push_str(&existing[..start]);
        out.push_str(MARKER_START);
        out.push('\n');
        out.push_str(block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(MARKER_END);
        out.push_str(&existing[end_inclusive..]);
        out
    } else {
        // Append. Leave a blank line between user content and our block
        // so the boundary is visually clear when someone `cat`s the file.
        let mut out = existing.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(MARKER_START);
        out.push('\n');
        out.push_str(block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(MARKER_END);
        out.push('\n');
        out
    }
}

/// Pre-commit framework hooks commonly terminate their dispatch branches
/// with `exec`. Appending our block makes it unreachable, so install it
/// immediately after the shebang. Existing managed blocks are relocated
/// on reinstall while all user/framework content is preserved.
fn merge_pre_commit_hook(existing: &str, block: &str) -> String {
    let without_managed = strip_managed_block(existing).unwrap_or_else(|| existing.to_string());
    let insertion = if without_managed.starts_with("#!") {
        without_managed
            .find('\n')
            .map(|offset| offset + 1)
            .unwrap_or(without_managed.len())
    } else {
        0
    };
    let mut out = String::with_capacity(without_managed.len() + block.len() + 4);
    out.push_str(&without_managed[..insertion]);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(MARKER_START);
    out.push('\n');
    out.push_str(block);
    if !block.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(MARKER_END);
    out.push_str("\n\n");
    out.push_str(without_managed[insertion..].trim_start_matches('\n'));
    out
}

fn managed_block_is_after_exec(content: &str) -> bool {
    let Some(marker) = content.find(MARKER_START) else {
        return false;
    };
    content[..marker]
        .lines()
        .any(|line| line.split_whitespace().next() == Some("exec"))
}

/// Remove only the rsry-managed section from a hook, preserving user
/// content before and after it. Used to neutralize dormant standard hooks
/// when `core.hooksPath` points somewhere else.
fn strip_managed_block(existing: &str) -> Option<String> {
    let start = existing.find(MARKER_START)?;
    let after_start = start + MARKER_START.len();
    let end = existing[after_start..]
        .find(MARKER_END)
        .map(|offset| after_start + offset + MARKER_END.len())
        .unwrap_or(existing.len());
    let mut stripped = String::with_capacity(existing.len());
    stripped.push_str(&existing[..start]);
    stripped.push_str(&existing[end..]);
    Some(stripped)
}

/// Name of the git merge driver for `.beads/beads.jsonl` — the token the
/// root `.gitattributes` references as `merge=beads-jsonl`.
pub(crate) const MERGE_DRIVER: &str = "beads-jsonl";

/// The `.gitattributes` line that routes the tracked export to the driver.
pub(crate) const MERGE_ATTR_PATH: &str = ".beads/beads.jsonl";

/// Show which rsry hooks are installed and whether Dolt remotes are configured.
pub fn status(repo_root: &Path) -> Result<()> {
    let hooks_dir = resolve_hooks_dir(repo_root)?;
    println!("repo: {}", repo_root.display());
    println!("hooks dir: {}", hooks_dir.display());
    println!();
    println!("git hooks:");
    for (name, block) in HOOKS {
        let path = hooks_dir.join(name);
        if !path.exists() {
            println!("  ✗ not installed  {name}");
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            println!("  ! unreadable  {name}");
            continue;
        };
        if !content.contains(MARKER_START) {
            println!("  △ exists, no rsry markers  {name} (run `rsry hooks install` to merge in)");
            continue;
        }

        let expected = hook_stamp(name, block);
        if name == &"pre-commit" && managed_block_is_after_exec(&content) {
            println!(
                "  ! UNREACHABLE  {name} (managed block follows `exec`) — run `rsry hooks install`"
            );
        } else if content.lines().any(|line| line == expected) {
            println!(
                "  ✓ current  {name} (v{}, sha256:{})",
                env!("CARGO_PKG_VERSION"),
                &expected[expected.len() - 64..expected.len() - 52]
            );
        } else {
            let installed = content
                .lines()
                .find(|line| line.starts_with("# rsry-hook "))
                .unwrap_or("unversioned managed block");
            println!(
                "  ! STALE  {name} ({installed}; expected v{} sha256:{}) — run `rsry hooks install`",
                env!("CARGO_PKG_VERSION"),
                &expected[expected.len() - 64..expected.len() - 52]
            );
        }
    }

    // Merge driver (rosary-f9516f) — config-resident, so it's per-clone
    // state that `.gitattributes` alone can't carry.
    println!();
    println!("merge driver ({MERGE_DRIVER}):");
    match git_config_get(repo_root, &format!("merge.{MERGE_DRIVER}.driver")) {
        Some(cmd) => println!("  ✓ configured: {cmd}"),
        None => println!("  ✗ not configured (run `rsry hooks install`)"),
    }
    let attrs = repo_root.join(".gitattributes");
    let referenced = std::fs::read_to_string(&attrs)
        .map(|c| c.contains(&format!("merge={MERGE_DRIVER}")))
        .unwrap_or(false);
    if referenced {
        println!("  ✓ referenced by .gitattributes");
    } else {
        println!("  △ no `merge={MERGE_DRIVER}` line in .gitattributes (driver is inert)");
    }

    let repo_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let dolt_dir = repo_root.join(".beads").join("dolt").join(repo_name);
    println!();
    println!("dolt remote:");
    if !dolt_dir.exists() {
        println!("  ? no .beads/dolt/{repo_name} found");
        return Ok(());
    }
    match check_dolt_remote(&dolt_dir) {
        DoltRemoteStatus::Configured(stdout) => print!("{stdout}"),
        DoltRemoteStatus::NotConfigured => println!("  ✗ no remote configured"),
        DoltRemoteStatus::Errored { exit, stderr } => {
            println!(
                "  ! `dolt remote -v` failed (exit {exit}): {}",
                stderr.trim()
            );
        }
        DoltRemoteStatus::NotInvokable(e) => println!("  ? dolt not available: {e}"),
    }
    Ok(())
}

/// Mechanically audit whether this repo's bead-sync config is actually
/// correct — not just installed, but REACHABLE and CONSISTENT. Three
/// checks `status()` doesn't cover, each found live during the
/// 2026-07-29 fleet sweep (rosary-b5c8a1):
///
/// 1. **gitignore shadowing**: a top-level `.gitignore` rule (`.beads/`,
///    `*`, etc.) can silently block `.beads/beads.jsonl` from ever being
///    tracked, no matter how many times `hooks install` runs. Found live
///    in `lectio` (`.beads/` at line 12) and `notme.bot` (`*` at line 2).
/// 2. **backend ambiguity**: `.beads/embeddeddolt/` (bd-era) coexisting
///    with `.beads/beads.db` or `.beads/dolt/` means two stores both
///    claim authority — rosary-909bec's exact defect.
/// 3. **store/export drift**: the local `beads.db` row count vs the
///    tracked `beads.jsonl` line count. A large gap means real bead data
///    has never been exported and has zero durable copy. Found live: 366
///    beads across 9 repos, sitting only on one machine's disk in a
///    gitignored file.
///
/// Execute one embedded hook's managed-block logic directly, by
/// rendering the SAME `docs/git-hooks/<name>` template `install`
/// splices into a raw hook file and running it via `sh -c` in
/// `repo_root` (rosary-00f2b5). Propagates the script's exit status —
/// any non-zero code is returned as an error, matching how git itself
/// would treat a failing hook.
///
/// This is the stable target `hooks install` writes into
/// `.pre-commit-config.yaml`'s `entry:` for a pre-commit-framework-owned
/// repo: the YAML names this COMMAND, never a version-frozen shell
/// snippet, so an `rsry` upgrade updates the check without ever
/// touching the YAML again.
pub fn run(repo_root: &Path, name: &str) -> Result<()> {
    let (_, block) = HOOKS
        .iter()
        .find(|(n, _)| *n == name)
        .with_context(|| format!("unknown hook: {name} (known: {:?})", hook_names()))?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(render_block(block))
        .current_dir(repo_root)
        .status()
        .with_context(|| format!("running hook `{name}`"))?;
    if !status.success() {
        anyhow::bail!(
            "hook `{name}` exited {}",
            status
                .code()
                .map_or("with a signal".to_string(), |c| c.to_string())
        );
    }
    Ok(())
}

fn hook_names() -> Vec<&'static str> {
    HOOKS.iter().map(|(n, _)| *n).collect()
}
