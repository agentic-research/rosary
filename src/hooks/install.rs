//! Hook installation: hooks dir resolution for the standard/`core.hooksPath`
//! cases, the merge-aware block splice, the `beads-jsonl` merge driver and
//! the gitignore-shadow repair. Split out of `hooks/mod.rs` by the
//! projection-timing scaffold (rosary-e5befe).

use super::*;

/// Resolve the conventional hooks directory independently of an active
/// `core.hooksPath` override.
pub(crate) fn standard_hooks_dir(repo_root: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-common-dir"])
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
    let common = Path::new(&rel);
    Ok(if common.is_absolute() {
        common.join("hooks")
    } else {
        repo_root.join(common).join("hooks")
    })
}

/// If hooks execute from a custom `core.hooksPath`, remove stale rsry
/// managed blocks from the dormant conventional `.git/hooks` copies.
/// User-authored content outside the markers is preserved.
pub(crate) fn neutralize_inactive_standard_hooks(
    repo_root: &Path,
    active_hooks_dir: &Path,
) -> Result<()> {
    let standard = standard_hooks_dir(repo_root)?;
    let active = active_hooks_dir
        .canonicalize()
        .unwrap_or_else(|_| active_hooks_dir.to_path_buf());
    let standard_cmp = standard.canonicalize().unwrap_or_else(|_| standard.clone());
    if active == standard_cmp {
        return Ok(());
    }

    for (name, _) in HOOKS {
        let path = standard.join(name);
        if path
            .symlink_metadata()
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(existing) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(stripped) = strip_managed_block(&existing) else {
            continue;
        };
        std::fs::write(&path, stripped)
            .with_context(|| format!("neutralizing dormant hook at {}", path.display()))?;
        eprintln!(
            "[hooks] neutralized dormant rsry block in {} (active hooks dir: {})",
            path.display(),
            active_hooks_dir.display()
        );
    }
    Ok(())
}

/// Install rsry hooks into `repo_root`.
///
/// Merge-aware: existing user hooks are preserved. The rsry block is
/// (re)inserted between markers in each managed hook file. Idempotent —
/// running install twice produces the same file content the second time.
/// Unset a stale bd-era `core.hooksPath` (a `.beads/hooks` fossil left by
/// `bd init`) so hooks resolve to `.git/hooks`. No-op when hooksPath is
/// unset or points elsewhere (e.g. rosary's own `.rsry-hooks`).
pub(crate) fn migrate_bd_hooks_path(repo_root: &Path) {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "core.hooksPath"])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let hp = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if hp.contains(".beads/hooks") {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", "--unset", "core.hooksPath"])
            .status();
        eprintln!("[hooks] migrated off bd-era core.hooksPath ({hp}) → using .git/hooks");
    }
}

/// Build the `merge.beads-jsonl.driver` command line (rosary-f9516f).
///
/// `%O`/`%A`/`%B` are git's ancestor/ours/theirs temp paths; the driver
/// overwrites `%A` with the result. Resolution happens when Git invokes the
/// driver: an explicit `RSRY_BIN`, then PATH, then the conventional
/// per-user install. No checkout/build-specific absolute path is stored in
/// repository config.
pub(crate) fn merge_driver_command() -> String {
    "sh -c 'r=\"${RSRY_BIN:-}\"; \
     if [ -z \"$r\" ]; then r=$(command -v rsry 2>/dev/null || true); fi; \
     if [ -z \"$r\" ] && [ -x \"$HOME/.local/bin/rsry\" ]; then r=\"$HOME/.local/bin/rsry\"; fi; \
     if [ -z \"$r\" ]; then echo \"rsry merge driver: rsry not found\" >&2; exit 1; fi; \
     exec \"$r\" bead merge-jsonl \"$@\"' - \"%O\" \"%A\" \"%B\""
        .to_string()
}

/// Read a single git config value from `repo_root`, `None` if unset.
pub(crate) fn git_config_get(repo_root: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Ensure `.gitattributes` routes `.beads/beads.jsonl` at the merge driver.
///
/// OPT-IN BY TRACKING, matching the pre-commit hook: only repos that have
/// `git add`ed the export get the line. Installing hooks must never start
/// creating a `.gitattributes` in a repo that never opted into JSONL sync.
///
/// Non-clobbering: appends one line, never rewrites existing content. Uses
/// `git check-attr` rather than grepping, so an existing rule that already
/// routes the path — by any pattern, in any `.gitattributes` — counts.
pub(crate) fn ensure_jsonl_merge_attribute(repo_root: &Path) -> Result<bool> {
    let tracked = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["ls-files", "--error-unmatch", MERGE_ATTR_PATH])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !tracked {
        return Ok(false);
    }

    // Already routed (possibly via a broader pattern)? Nothing to do.
    let routed = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["check-attr", "merge", "--", MERGE_ATTR_PATH])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(MERGE_DRIVER))
        .unwrap_or(false);
    if routed {
        return Ok(false);
    }

    let path = repo_root.join(".gitattributes");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut next = existing.clone();
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&format!(
        "\n# Bead export merges by RECORD, not by line (rosary-f9516f). The driver\n\
         # itself is defined in git config by `rsry hooks install` — this line only\n\
         # names it, and without it git would line-merge and shred bead records.\n\
         {MERGE_ATTR_PATH} merge={MERGE_DRIVER}\n"
    ));
    std::fs::write(&path, next).with_context(|| format!("writing {}", path.display()))?;
    println!("  ✓ routed {MERGE_ATTR_PATH} → {MERGE_DRIVER} in .gitattributes");
    Ok(true)
}

/// Auto-fix the SIMPLE gitignore-shadow shape found by `hooks audit`;
/// refuse-and-suggest for the ALLOWLIST shape. No-op if `.beads/`
/// doesn't exist or `.beads/beads.jsonl` isn't actually shadowed
/// (rosary-e97360). Called from `install()`.
///
/// Self-verifying: after a real SIMPLE-shape write, re-checks
/// `git check-ignore -q` and hard-errors if the path is STILL shadowed
/// (e.g. a second ignore source like `.git/info/exclude` or a global
/// gitignore is also matching) — never trusts the edit blindly.
pub(crate) fn fix_gitignore_shadow(repo_root: &Path) -> Result<()> {
    let beads_dir = repo_root.join(".beads");
    if !beads_dir.exists() {
        return Ok(());
    }

    let quiet = Command::new("git")
        .current_dir(repo_root)
        .args(["check-ignore", "-q", MERGE_ATTR_PATH])
        .output();
    let GitignoreCheck::Shadowed(_) = classify_gitignore_check(quiet, None) else {
        return Ok(());
    };

    let gitignore_path = repo_root.join(".gitignore");
    let Ok(content) = std::fs::read_to_string(&gitignore_path) else {
        println!(
            "  ? {MERGE_ATTR_PATH} is gitignore-shadowed, but no readable top-level \
             .gitignore was found to fix"
        );
        return Ok(());
    };

    match classify_gitignore_shadow_shape(&content) {
        GitignoreShadowShape::Simple { pattern } => {
            let fixed = remove_gitignore_line(&content, pattern);
            std::fs::write(&gitignore_path, &fixed)
                .with_context(|| format!("writing {}", gitignore_path.display()))?;

            let recheck = Command::new("git")
                .current_dir(repo_root)
                .args(["check-ignore", "-q", MERGE_ATTR_PATH])
                .output();
            match classify_gitignore_check(recheck, None) {
                GitignoreCheck::Shadowed(_) => anyhow::bail!(
                    "removed `{pattern}` from .gitignore but {MERGE_ATTR_PATH} is STILL \
                     shadowed after the fix (another ignore source, e.g. \
                     .git/info/exclude or a global gitignore, is also matching) — \
                     refusing to claim success"
                ),
                GitignoreCheck::Unknown(e) => anyhow::bail!(
                    "removed `{pattern}` from .gitignore but could not re-verify with \
                     `git check-ignore`: {e}"
                ),
                GitignoreCheck::Reachable => {
                    println!(
                        "  ✓ removed shadowing rule `{pattern}` from {}",
                        gitignore_path.display()
                    );
                }
            }
        }
        GitignoreShadowShape::Allowlist => {
            println!(
                "  ! {MERGE_ATTR_PATH} is gitignore-shadowed by a default-deny allowlist \
                 (.gitignore has a bare `*` plus `!` exceptions) — refusing to guess \
                 which negation to add. Append to .gitignore:"
            );
            print!("{}", allowlist_fix_suggestion());
        }
        GitignoreShadowShape::Unrecognized => {
            println!(
                "  ? {MERGE_ATTR_PATH} is gitignore-shadowed by an unrecognized \
                 .gitignore shape — not auto-fixed. Run `rsry hooks audit` for detail, \
                 then fix by hand."
            );
        }
    }
    Ok(())
}

/// Install the `beads-jsonl` merge driver into the repo's git config
/// (rosary-f9516f).
///
/// gitattributes(5): the driver DEFINITION must live in git config — a
/// `.gitattributes` entry only *references* it by name. That is also why
/// this can't be committed: config is per-clone, so every clone has to run
/// `rsry hooks install` (the `.gitattributes` comment says so).
///
/// `merge.<name>.recursive` is deliberately left unset: unset means "use
/// this driver for the internal merges between multiple common ancestors
/// too", which is exactly right for a driver that is itself a total,
/// never-conflicting function of three inputs.
///
/// Idempotent: `git config <key> <value>` overwrites in place, so a
/// re-install converges on the same two keys.
pub(crate) fn install_merge_driver(repo_root: &Path) -> Result<()> {
    let entries = [
        (
            format!("merge.{MERGE_DRIVER}.name"),
            "rosary bead JSONL export — union by bead id, last-writer-wins on updated_at"
                .to_string(),
        ),
        (
            format!("merge.{MERGE_DRIVER}.driver"),
            merge_driver_command(),
        ),
    ];
    for (key, value) in entries {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", &key, &value])
            .status()
            .with_context(|| format!("invoking `git config {key}`"))?;
        if !status.success() {
            anyhow::bail!("`git config {key}` failed (exit {status})");
        }
    }
    println!("[hooks] configured merge driver merge.{MERGE_DRIVER} (.beads/beads.jsonl)");
    Ok(())
}

pub fn install(repo_root: &Path) -> Result<()> {
    // Migrate off the stale bd-era hooks path before resolving. `bd init`
    // installed hooks into `.beads/hooks` and pointed `core.hooksPath`
    // there; ADR-0014 decoupled rosary from bd, so that dir is a fossil.
    // Unset it (local .git/config) so hooks resolve to the standard
    // `.git/hooks` instead of a tracked bd directory. (rosary's own
    // `.rsry-hooks` is left alone — only `.beads/hooks` is migrated.)
    migrate_bd_hooks_path(repo_root);

    let hooks_dir = resolve_hooks_dir(repo_root)?;
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("creating {}", hooks_dir.display()))?;
    neutralize_inactive_standard_hooks(repo_root, &hooks_dir)?;

    // Detected ONCE, before any writes: the Python pre-commit framework
    // owns and regenerates .git/hooks/pre-commit on every `pre-commit
    // install`/`autoupdate`, silently dropping rsry's spliced block with
    // nothing to warn that it happened (rosary-00f2b5, found live in
    // mache). `.pre-commit-config.yaml` is the durable place a
    // pre-commit-framework repo expects a check to live instead.
    let precommit_config_path = repo_root.join(".pre-commit-config.yaml");
    let precommit_framework_owned = is_precommit_framework_owned(
        precommit_config_path.is_file(),
        std::fs::read_to_string(hooks_dir.join("pre-commit"))
            .ok()
            .as_deref(),
    );

    for (name, block) in HOOKS {
        if *name == "pre-commit" && precommit_framework_owned {
            // Instead of appending to a file the framework will
            // regenerate out from under us, redirect entirely to
            // .pre-commit-config.yaml below.
            println!(
                "[hooks] {name} is pre-commit-framework-owned — writing to \
                 .pre-commit-config.yaml instead of the raw hook file (rosary-00f2b5)"
            );
            continue;
        }
        let dst = hooks_dir.join(name);
        // Replace a stale symlink (e.g. a hand-made commit-msg →
        // ~/.rsry/hooks/commit-msg) with a self-contained managed file —
        // never follow it and clobber the link target.
        if dst
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&dst);
        }
        let block = render_managed_block(name, block);
        let content = if dst.exists() {
            let existing = std::fs::read_to_string(&dst)
                .with_context(|| format!("reading existing hook at {}", dst.display()))?;
            if *name == "pre-commit" {
                merge_pre_commit_hook(&existing, &block)
            } else {
                merge_hook(&existing, &block)
            }
        } else {
            fresh_hook(&block)
        };
        std::fs::write(&dst, &content)
            .with_context(|| format!("writing hook at {}", dst.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&dst)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&dst, perms)?;
        }
        println!("[hooks] installed {} → {}", name, dst.display());
    }

    if precommit_framework_owned {
        let existing = std::fs::read_to_string(&precommit_config_path).unwrap_or_default();
        let updated = merge_precommit_yaml(&existing);
        std::fs::write(&precommit_config_path, &updated)
            .with_context(|| format!("writing {}", precommit_config_path.display()))?;
        println!(
            "[hooks] ✓ added rsry's local hook entry to {} (entry: `rsry hooks run \
             pre-commit`)",
            precommit_config_path.display()
        );
    }

    // The tracked `.beads/beads.jsonl` export is merged by record, not by
    // line (rosary-f9516f). The driver definition lives in git config, so
    // it must be (re)installed per clone — `.gitattributes` only names it.
    install_merge_driver(repo_root)?;

    // ...and the `.gitattributes` line that ROUTES the export to it. The
    // driver definition alone is INERT: gitattributes(5) only runs a driver
    // for paths carrying the matching `merge=` attribute, so a repo with the
    // config but no attribute silently falls back to git's LINE merge — the
    // exact record-shredding `merge_jsonl` exists to prevent.
    //
    // `hooks status` already reported this ("driver is inert"), but install
    // never wrote what it diagnosed, so every repo that didn't hand-commit a
    // `.gitattributes` was unprotected. Measured: 5 of 7 tracked repos.
    ensure_jsonl_merge_attribute(repo_root)?;

    // `hooks audit` DETECTS gitignore shadowing (rosary-b5c8a1); this
    // ACTUALLY FIXES the common case, so a human/agent never again
    // hand-edits another repo's .gitignore under time pressure
    // (rosary-e97360 — found live in 9 of 22 repos in one sweep).
    fix_gitignore_shadow(repo_root)?;

    // Warn if Dolt remote is not configured — hooks will silently no-op otherwise.
    let repo_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let dolt_dir = repo_root.join(".beads").join("dolt").join(repo_name);
    if dolt_dir.exists() {
        match check_dolt_remote(&dolt_dir) {
            DoltRemoteStatus::Configured(_) => {}
            DoltRemoteStatus::NotConfigured => {
                eprintln!(
                    "[hooks] WARNING: no dolt remote configured in {}",
                    dolt_dir.display()
                );
                eprintln!(
                    "[hooks] Run: cd {} && dolt remote add origin <url>",
                    dolt_dir.display()
                );
            }
            DoltRemoteStatus::Errored { exit, stderr } => {
                eprintln!(
                    "[hooks] WARNING: `dolt remote -v` failed in {} (exit {exit}): {}",
                    dolt_dir.display(),
                    stderr.trim()
                );
            }
            DoltRemoteStatus::NotInvokable(e) => {
                eprintln!("[hooks] WARNING: couldn't invoke dolt: {e}");
            }
        }
    }

    Ok(())
}
