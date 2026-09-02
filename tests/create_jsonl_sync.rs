#[path = "common/mod.rs"]
mod create_common;

use create_common::{git_ok as git, rsry_run as run};
use std::path::Path;
use std::process::{Command, Output};

fn create(cwd: &Path, home: &Path, title: &str) -> Output {
    run(
        cwd,
        home,
        &[
            "bead",
            "create",
            title,
            "--files",
            "src/lib.rs",
            "--test-files",
            "tests/smoke.rs",
        ],
    )
}

#[test]
fn create_adds_only_the_new_bead_to_an_opted_in_projection() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(repo.path(), &["config", "user.email", "test@example.com"]);
    git(repo.path(), &["config", "user.name", "Test User"]);

    let repo_path = repo.path().to_string_lossy().into_owned();
    let init = run(repo.path(), home.path(), &["init", &repo_path]);
    assert!(init.status.success(), "{init:?}");

    let projection = repo.path().join(".beads/beads.jsonl");
    std::fs::write(&projection, "").unwrap();
    git(repo.path(), &["add", ".beads/beads.jsonl"]);
    git(
        repo.path(),
        &["commit", "--no-verify", "-qm", "opt in to bead projection"],
    );

    let output = create(repo.path(), home.path(), "newly published bead");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<serde_json::Value> = std::fs::read_to_string(&projection)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["title"], "newly published bead");
}

#[test]
fn create_does_not_start_a_projection_without_git_opt_in() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);

    let repo_path = repo.path().to_string_lossy().into_owned();
    let init = run(repo.path(), home.path(), &["init", &repo_path]);
    assert!(init.status.success(), "{init:?}");
    let projection = repo.path().join(".beads/beads.jsonl");
    let before = std::fs::read_to_string(&projection).unwrap_or_default();
    let tracked = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", ".beads/beads.jsonl"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        !tracked.status.success(),
        "init must not opt in for the user"
    );

    let output = create(repo.path(), home.path(), "local only bead");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&projection).unwrap_or_default(),
        before,
        "create must not update an untracked projection"
    );
}
