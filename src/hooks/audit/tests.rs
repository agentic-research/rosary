//! Audit gate tests (rosary-e5fc0e rewrites the drift contract here).

use super::*;
use crate::hooks::tests::*;

/// Forge a `std::process::Output` with the given exit status and
/// stdout/stderr. Used to drive `classify_dolt_remote` deterministically
/// without an actual `dolt` binary.
fn forge_output(success: bool, stdout: &str, stderr: &str) -> std::process::Output {
    use std::os::unix::process::ExitStatusExt;
    let status = std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 });
    std::process::Output {
        status,
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn classify_dolt_remote_configured() {
    let out = forge_output(true, "origin\thttps://example.com (fetch)\n", "");
    match classify_dolt_remote(Ok(out)) {
        DoltRemoteStatus::Configured(s) => assert!(s.contains("origin")),
        other => panic!(
            "expected Configured, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

#[test]
fn classify_dolt_remote_not_configured() {
    let out = forge_output(true, "", "");
    assert!(matches!(
        classify_dolt_remote(Ok(out)),
        DoltRemoteStatus::NotConfigured
    ));
}

#[test]
fn classify_dolt_remote_errored_surfaces_stderr() {
    // The bug Copilot caught: exit-non-zero with empty stdout was
    // misreported as "no remote configured". Now it must surface
    // the failure with stderr preserved.
    let out = forge_output(false, "", "fatal: not a dolt repository\n");
    match classify_dolt_remote(Ok(out)) {
        DoltRemoteStatus::Errored { exit, stderr } => {
            assert_eq!(exit, 1);
            assert!(stderr.contains("not a dolt repository"));
        }
        _ => panic!("expected Errored variant for exit-1"),
    }
}

#[test]
fn classify_dolt_remote_spawn_failure() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
    match classify_dolt_remote(Err(err)) {
        DoltRemoteStatus::NotInvokable(msg) => assert!(msg.contains("no such file")),
        _ => panic!("expected NotInvokable for spawn failure"),
    }
}

#[test]
fn classify_gitignore_check_shadowed_on_quiet_success() {
    let quiet = forge_output(true, "", "");
    let detail = Some(".gitignore:12:.beads/\t.beads/beads.jsonl".to_string());
    match classify_gitignore_check(Ok(quiet), detail) {
        GitignoreCheck::Shadowed(d) => assert!(d.contains(".gitignore:12")),
        other => panic!("expected Shadowed, got {other:?}"),
    }
}

#[test]
fn classify_gitignore_check_reachable_on_quiet_failure_exit() {
    // `-q` exits 1 when the path is NOT ignored — gitignore(5).
    let quiet = forge_output(false, "", "");
    assert_eq!(
        classify_gitignore_check(Ok(quiet), None),
        GitignoreCheck::Reachable
    );
}

/// REGRESSION (found live against notme.bot's default-deny allowlist,
/// 2026-07-29): `-v` exits 0 whenever ANY rule decided the path,
/// including a `!negation` that explicitly un-ignores it — so a
/// `-v`-exit-code-only classifier reported an explicitly TRACKABLE
/// path as shadowed. `-q`'s exit code is the one with real
/// ignored/not-ignored semantics; this pins that distinction so it
/// can't silently regress back to a `-v`-only decision.
#[test]
fn classify_gitignore_check_reachable_when_quiet_disagrees_with_verbose_detail() {
    // -q correctly reports "not ignored" (exit 1)...
    let quiet = forge_output(false, "", "");
    // ...even though a verbose detail naming a NEGATION rule is
    // available (what -v would have reported as its deciding line).
    let detail = Some(".gitignore:41:!.beads/beads.jsonl\t.beads/beads.jsonl".to_string());
    assert_eq!(
        classify_gitignore_check(Ok(quiet), detail),
        GitignoreCheck::Reachable,
        "a negation-decided path must classify Reachable regardless of verbose detail"
    );
}

#[test]
fn classify_gitignore_check_unknown_on_spawn_failure() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
    match classify_gitignore_check(Err(err), None) {
        GitignoreCheck::Unknown(msg) => assert!(msg.contains("no such file")),
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[tokio::test]
async fn audit_flags_gitignore_shadowed_export() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join(".gitignore"), ".beads/\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".beads")).unwrap();
    std::fs::write(dir.path().join(".beads").join("beads.jsonl"), "").unwrap();

    let err = audit(dir.path()).await.unwrap_err();
    assert!(format!("{err:#}").contains("gitignore-shadowed"), "{err:#}");
}

#[tokio::test]
async fn audit_flags_dolt_and_sqlite_coexisting() {
    // The real bug (rosary-9a5926, cloister): a live Dolt server
    // store plus a stray beads.db. This is the shape
    // connect_bead_store's runtime guard has always refused to
    // guess through — the audit check must agree with it now.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::create_dir_all(dir.path().join(".beads").join("dolt")).unwrap();
    std::fs::write(dir.path().join(".beads").join("beads.db"), b"").unwrap();

    let err = audit(dir.path()).await.unwrap_err();
    assert!(format!("{err:#}").contains("ambiguous backend"), "{err:#}");
}

#[tokio::test]
async fn audit_passes_when_embeddeddolt_coexists_with_a_live_store() {
    // Corrected behavior (was a false positive before
    // bead_backend::detect_backend): an unused bd-era embeddeddolt/
    // sitting next to a real beads.db is NOT ambiguous —
    // connect_bead_store has always read beads.db and ignored
    // embeddeddolt unconditionally in this shape.
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::create_dir_all(dir.path().join(".beads").join("embeddeddolt")).unwrap();
    std::fs::write(dir.path().join(".beads").join("beads.db"), b"").unwrap();

    audit(dir.path()).await.unwrap();
}

#[tokio::test]
async fn audit_passes_clean_repo_with_no_beads_dir() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    audit(dir.path()).await.unwrap();
}

#[tokio::test]
async fn audit_flags_foreign_repo_dependency_shape() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let beads_dir = dir.path().join(".beads");
    std::fs::create_dir_all(&beads_dir).unwrap();
    std::fs::write(
        beads_dir.join("beads.jsonl"),
        r#"{"id":"signet-a34639","repo":"signet","dependencies":["mache-4dbad9"]}
"#,
    )
    .unwrap();

    let err = audit(dir.path()).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("signet-a34639 -> mache-4dbad9"),
        "{err:#}"
    );
}

#[tokio::test]
async fn audit_passes_when_dependencies_are_same_repo_or_absent() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let beads_dir = dir.path().join(".beads");
    std::fs::create_dir_all(&beads_dir).unwrap();
    std::fs::write(
        beads_dir.join("beads.jsonl"),
        "{\"id\":\"t-0\",\"repo\":\"t\",\"dependencies\":[\"t-1a2b3c\"]}\n\
         {\"id\":\"t-1\",\"repo\":\"t\"}\n",
    )
    .unwrap();

    audit(dir.path()).await.unwrap();
}

#[test]
fn foreign_repo_dep_true_for_different_repo_prefix() {
    assert!(foreign_repo_dep("signet", "mache-4dbad9"));
}

#[test]
fn foreign_repo_dep_false_for_same_repo_prefix() {
    assert!(!foreign_repo_dep("mache", "mache-4dbad9"));
}

#[test]
fn foreign_repo_dep_false_without_hex_suffix() {
    // No trailing hex-shaped suffix — not a generated bead id at
    // all, so this isn't a foreign-repo shape, just an unrelated
    // string (or a hand-authored non-hex id).
    assert!(!foreign_repo_dep("signet", "not-a-bead-id"));
}

#[test]
fn foreign_repo_dep_handles_multi_hyphen_repo_names() {
    // Repo names themselves contain hyphens (ley-line-open,
    // canonical-hours) — the check must compare against the WHOLE
    // prefix before the hex suffix, not just the last segment.
    assert!(!foreign_repo_dep("ley-line-open", "ley-line-open-3a21ee"));
    assert!(foreign_repo_dep("ley-line-open", "mache-4dbad9"));
    assert!(foreign_repo_dep("mache", "ley-line-open-3a21ee"));
}

/// A stale stamp on an installed hook is a FAILURE, not a warning: the hook
/// encodes the old contract. Mutation: report it with `?` instead of pushing
/// a problem and this passes.
#[tokio::test]
async fn audit_flags_a_stale_hook_stamp() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    seed_commit(dir.path());
    install(dir.path()).unwrap();
    audit(dir.path()).await.unwrap();

    let hook = resolve_hooks_dir(dir.path()).unwrap().join("pre-push");
    let stale = std::fs::read_to_string(&hook).unwrap().replacen(
        &format!("# rsry-hook pre-push v{}", env!("CARGO_PKG_VERSION")),
        "# rsry-hook pre-push v0.0.1",
        1,
    );
    std::fs::write(&hook, stale).unwrap();

    let err = audit(dir.path()).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("hook pre-push is stale (v0.0.1"),
        "{err:#}"
    );
}
