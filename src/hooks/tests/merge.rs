//! Marker-block merging, the `beads-jsonl` merge driver and `.gitattributes` wiring.

use super::*;

/// The driver config alone is INERT — git only runs it for paths
/// carrying the `merge=` attribute. This asserts the attribute is what
/// actually flips, via `git check-attr` (the thing git consults), not
/// by grepping the file we just wrote.
#[test]
fn routes_tracked_export_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);

    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();

    // Untracked export => opt-in not taken => no file written.
    assert!(!ensure_jsonl_merge_attribute(root).unwrap());
    assert!(!root.join(".gitattributes").exists());

    assert!(git(root, &["add", MERGE_ATTR_PATH]).status.success());
    assert!(ensure_jsonl_merge_attribute(root).unwrap());

    let attr = git(root, &["check-attr", "merge", "--", MERGE_ATTR_PATH]);
    assert!(
        String::from_utf8_lossy(&attr.stdout).contains(MERGE_DRIVER),
        "export must route to the driver, else git line-merges it"
    );

    // Second run is a no-op — no duplicate lines.
    assert!(!ensure_jsonl_merge_attribute(root).unwrap());
    let body = std::fs::read_to_string(root.join(".gitattributes")).unwrap();
    assert_eq!(body.matches(MERGE_DRIVER).count(), 1);
}

/// Pre-existing content is appended to, never clobbered.
#[test]
fn preserves_existing_gitattributes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::write(root.join(".gitattributes"), "*.png binary\n").unwrap();
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(MERGE_ATTR_PATH), "{}\n").unwrap();
    assert!(git(root, &["add", MERGE_ATTR_PATH]).status.success());

    assert!(ensure_jsonl_merge_attribute(root).unwrap());
    let body = std::fs::read_to_string(root.join(".gitattributes")).unwrap();
    assert!(body.contains("*.png binary"), "clobbered user content");
    assert!(body.contains(MERGE_DRIVER));
}

#[test]
fn merge_hook_replaces_existing_marker_block() {
    let existing = format!(
        "#!/bin/sh\n# user header\necho hi\n\n{}\nold block contents\n{}\necho after\n",
        MARKER_START, MARKER_END
    );
    let merged = merge_hook(&existing, "new block contents\n");
    assert!(merged.contains("user header"));
    assert!(merged.contains("echo hi"));
    assert!(merged.contains("echo after"));
    assert!(merged.contains("new block contents"));
    assert!(
        !merged.contains("old block contents"),
        "old marked content should be replaced"
    );
}

#[test]
fn merge_hook_appends_when_no_existing_markers() {
    let existing = "#!/bin/sh\necho user logic\n";
    let merged = merge_hook(existing, "rsry block\n");
    assert!(merged.contains("echo user logic"));
    assert!(merged.contains(MARKER_START));
    assert!(merged.contains("rsry block"));
    assert!(merged.contains(MARKER_END));
    // User content must precede the marker block when appending.
    assert!(
        merged.find("echo user logic").unwrap() < merged.find(MARKER_START).unwrap(),
        "user content should come before appended rsry block",
    );
}

#[test]
fn merge_hook_idempotent_when_block_unchanged() {
    // Two calls with the same block produce the same final content.
    let starting = format!(
        "#!/bin/sh\necho user\n\n{}\nsame block\n{}\n",
        MARKER_START, MARKER_END
    );
    let first = merge_hook(&starting, "same block\n");
    let second = merge_hook(&first, "same block\n");
    assert_eq!(first, second, "merge should be idempotent");
    assert_eq!(
        second.matches(MARKER_START).count(),
        1,
        "no duplicated marker block"
    );
}

#[test]
fn merge_hook_recovers_when_end_marker_missing() {
    // Defensive: if the file has START but no END (manual edit damage),
    // we replace from START to EOF rather than leaving cruft.
    let existing = format!("#!/bin/sh\n{}\nstale\n(no end marker)\n", MARKER_START);
    let merged = merge_hook(&existing, "fresh\n");
    assert!(!merged.contains("stale"));
    assert!(!merged.contains("(no end marker)"));
    assert!(merged.contains("fresh"));
    assert!(merged.contains(MARKER_END));
}

#[test]
fn merge_pre_commit_hook_relocates_before_exec_and_is_idempotent() {
    let existing =
        "#!/bin/sh\nif command -v pre-commit >/dev/null; then\n  exec pre-commit \"$@\"\nfi\n";
    let first = merge_pre_commit_hook(existing, "rsry block\n");
    let second = merge_pre_commit_hook(&first, "rsry block\n");

    assert_eq!(first, second);
    assert_eq!(first.matches(MARKER_START).count(), 1);
    assert!(first.find(MARKER_END).unwrap() < first.find("exec pre-commit").unwrap());
}

#[test]
fn merge_pre_commit_hook_repairs_shebang_without_newline() {
    let merged = merge_pre_commit_hook("#!/bin/sh", "rsry block\n");

    assert!(merged.starts_with("#!/bin/sh\n# >>> rsry-managed"));
    assert_eq!(merged.matches(MARKER_START).count(), 1);
}

/// rosary-f9516f: `hooks install` also configures the `beads-jsonl`
/// merge driver, because gitattributes(5) requires the DEFINITION to
/// live in git config — the committed `.gitattributes` can only
/// reference it by name. Idempotent: a second install converges.
#[test]
fn install_configures_merge_driver_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    install(dir.path()).unwrap();

    let driver = git_config_get(dir.path(), &format!("merge.{MERGE_DRIVER}.driver")).unwrap();
    assert!(driver.contains("bead merge-jsonl"), "{driver}");
    for ph in ["%O", "%A", "%B"] {
        assert!(driver.contains(ph), "driver must pass {ph}: {driver}");
    }
    assert!(git_config_get(dir.path(), &format!("merge.{MERGE_DRIVER}.name")).is_some());

    install(dir.path()).unwrap();
    let again = git_config_get(dir.path(), &format!("merge.{MERGE_DRIVER}.driver")).unwrap();
    assert_eq!(driver, again, "re-install must converge");
    // `git config --get` errors on a multi-valued key; a successful
    // read after two installs proves we overwrote rather than appended.
}

/// The driver resolves rsry when Git invokes it, rather than pinning
/// whichever ephemeral binary happened to run `hooks install`.
#[test]
fn merge_driver_command_is_portable() {
    let ephemeral = "/private/tmp/target/debug/rsry";
    let cmd = merge_driver_command();
    assert!(
        !cmd.contains(ephemeral),
        "driver must not pin installer path: {cmd}"
    );
    assert!(cmd.contains("command -v rsry"), "{cmd}");
    assert!(cmd.contains("$HOME/.local/bin/rsry"), "{cmd}");
    assert!(cmd.contains("RSRY_BIN"), "{cmd}");
    for ph in ["%O", "%A", "%B"] {
        assert!(cmd.contains(ph), "driver must pass {ph}: {cmd}");
    }
}

#[test]
fn marker_constants_have_expected_shape() {
    // Both markers must start with `# >>>` / `# <<<` so they're
    // greppable AND obviously comments in shell. Anyone who edits
    // the markers later should see this test fail before drift
    // leaks into installed hooks.
    assert!(
        MARKER_START.starts_with("# >>> rsry-managed"),
        "MARKER_START shape changed: {MARKER_START}"
    );
    assert!(
        MARKER_END.starts_with("# <<< rsry-managed"),
        "MARKER_END shape changed: {MARKER_END}"
    );
}

#[test]
fn readme_documents_actual_marker_lines() {
    // Drift-detector: if the marker constants change, the docs/git-hooks
    // README MUST reference the new strings. The README explains where
    // users should look in hook files to find the rsry-managed block.
    let readme = include_str!("../../../docs/git-hooks/README.md");
    assert!(
        readme.contains(MARKER_START),
        "README must contain MARKER_START literal so users can grep for it"
    );
    assert!(
        readme.contains(MARKER_END),
        "README must contain MARKER_END literal so users can grep for it"
    );
}
