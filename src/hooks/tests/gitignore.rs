//! `.gitignore` shadow detection and repair around `.beads/`.

use super::*;

/// The canonical `.beads/.gitignore` must deny the migration backup —
/// `migrate --commit` renames dolt/ to dolt.bak/ and never deletes it,
/// and it was staged accidentally once.
#[test]
fn beads_gitignore_denies_migration_backup() {
    assert!(crate::init::BEADS_GITIGNORE.contains("dolt.bak/"));
}

/// REAL FIXTURE 1 (lectio, SIMPLE shape) — the actual lines found at
/// .gitignore:10-12 before the fix, hand-applied this session.
/// Verified live via `git check-ignore -q` at the time; this test
/// pins the automated version of that same fix.
#[test]
fn fix_gitignore_shadow_simple_shape_removes_rule_and_self_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::write(
        root.join(".gitignore"),
        "/target\n\
         **/target/\n\
         \n\
         # Local rosary bead store (Dolt DB + daemon token) — local only, never publish.\n\
         # (Also covered by ~/.gitignore_global; repeated here so the repo is self-contained.)\n\
         .beads/\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

    // Precondition: genuinely shadowed before the fix.
    assert!(
        Command::new("git")
            .current_dir(root)
            .args(["check-ignore", "-q", MERGE_ATTR_PATH])
            .status()
            .unwrap()
            .success(),
        "fixture must start shadowed"
    );

    fix_gitignore_shadow(root).unwrap();

    let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(!body.lines().any(|l| l.trim() == ".beads/"), "{body}");
    // Comments and unrelated rules survive untouched.
    assert!(body.contains("/target"));
    assert!(body.contains("Local rosary bead store"));

    // Self-verification means this must ACTUALLY be reachable now,
    // not just "the line is gone" — proves the fix, not the edit.
    assert!(
        !Command::new("git")
            .current_dir(root)
            .args(["check-ignore", "-q", MERGE_ATTR_PATH])
            .status()
            .unwrap()
            .success(),
        "must be reachable after the fix"
    );
}

/// REAL FIXTURE 2 (notme.bot, ALLOWLIST shape) — the actual
/// default-deny structure found this session. Must NOT be
/// auto-edited; must print the exact two-step suggestion and leave
/// the file untouched.
#[test]
fn fix_gitignore_shadow_allowlist_shape_refuses_and_does_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    let original = "# Default deny: ignore everything unless explicitly allowlisted below.\n\
         *\n\
         \n\
         # Keep gitignore itself tracked.\n\
         !.gitignore\n\
         \n\
         # Allowlist project source and metadata.\n\
         !LICENSE\n\
         !README.md\n\
         !src/\n\
         !src/**\n\
         \n\
         # Explicitly ignore local/runtime artifacts.\n\
         .dolt/\n\
         *.db\n\
         .DS_Store\n";
    std::fs::write(root.join(".gitignore"), original).unwrap();
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

    fix_gitignore_shadow(root).unwrap();

    let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert_eq!(body, original, "allowlist .gitignore must be untouched");
}

/// Self-verification is the point, not decoration: a SIMPLE-shape
/// removal that STILL leaves the path shadowed (here, via a second
/// ignore source — `.git/info/exclude` — carrying the same rule)
/// must hard-error rather than report success.
#[test]
fn fix_gitignore_shadow_hard_errors_if_still_shadowed_after_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::write(root.join(".gitignore"), "# comment\n.beads/\n").unwrap();
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();
    // A second, independent ignore source the fix cannot touch.
    std::fs::create_dir_all(root.join(".git/info")).unwrap();
    std::fs::write(root.join(".git/info/exclude"), ".beads/\n").unwrap();

    let err = fix_gitignore_shadow(root).unwrap_err();
    assert!(format!("{err:#}").contains("STILL shadowed"), "{err:#}");
    // The .gitignore edit itself still happened (the fix isn't
    // rolled back) — the hard error is about not CLAIMING success,
    // not about leaving the repo in a worse state.
    let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(!body.lines().any(|l| l.trim() == ".beads/"));
}

#[test]
fn fix_gitignore_shadow_noop_when_not_shadowed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

    fix_gitignore_shadow(root).unwrap();
    let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert_eq!(body, "target/\n");
}

#[test]
fn fix_gitignore_shadow_noop_when_no_beads_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::write(root.join(".gitignore"), ".beads/\n").unwrap();
    // No .beads/ directory created — nothing to fix, must not panic
    // or create one as a side effect.
    fix_gitignore_shadow(root).unwrap();
    assert!(!root.join(".beads").exists());
}

/// `install()` end-to-end: the fix runs as part of the normal
/// install flow, not just when called directly.
#[test]
fn install_fixes_simple_gitignore_shadow() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::write(root.join(".gitignore"), ".beads/\n").unwrap();
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

    install(root).unwrap();

    let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(!body.lines().any(|l| l.trim() == ".beads/"), "{body}");
}
