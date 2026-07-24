//! Closing a bead must immediately refresh its already-published JSONL record
//! without publishing other records from the richer local store.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn rsry_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rsry"))
}

fn run_close_rsry(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(rsry_binary())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .expect("spawn rsry")
}

fn run_close_git(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")))
}

fn assert_close_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn created_id(output: &Output) -> String {
    let mut clean = String::from_utf8_lossy(&output.stdout).into_owned();
    while let Some(start) = clean.find('\x1b') {
        let end = clean[start..]
            .find('m')
            .map_or(start + 1, |i| start + i + 1);
        clean.replace_range(start..end, "");
    }
    clean
        .split_whitespace()
        .find(|word| {
            word.rsplit_once('-').is_some_and(|(_, suffix)| {
                suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_hexdigit())
            })
        })
        .unwrap_or_else(|| panic!("no bead id in output: {clean}"))
        .to_string()
}

#[test]
fn close_refreshes_published_record_without_publishing_local_only_bead() {
    let home = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();

    assert_close_success(
        "git init",
        &run_close_git(repo.path(), &["init", "-q", "-b", "main"]),
    );
    for args in [
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Test User"][..],
        &["config", "commit.gpgsign", "false"][..],
    ] {
        assert_close_success("git config", &run_close_git(repo.path(), args));
    }

    let repo_path = repo.path().to_string_lossy().into_owned();
    assert_close_success(
        "rsry init",
        &run_close_rsry(home.path(), repo.path(), &["init", &repo_path]),
    );
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::create_dir_all(repo.path().join("tests")).unwrap();
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .unwrap();
    std::fs::write(
        repo.path().join("tests/smoke.rs"),
        "#[test] fn smoke() {}\n",
    )
    .unwrap();

    let published = run_close_rsry(
        home.path(),
        repo.path(),
        &[
            "bead",
            "create",
            "published bead",
            "--files",
            "src/lib.rs",
            "--test-files",
            "tests/smoke.rs",
        ],
    );
    assert_close_success("create published bead", &published);
    let published_id = created_id(&published);

    let local_only = run_close_rsry(
        home.path(),
        repo.path(),
        &[
            "bead",
            "create",
            "local-only bead",
            "--files",
            "src/private.rs",
            "--test-files",
            "tests/private.rs",
        ],
    );
    assert_close_success("create local-only bead", &local_only);
    let local_only_id = created_id(&local_only);

    assert_close_success(
        "export store",
        &run_close_rsry(
            home.path(),
            repo.path(),
            &[
                "bead",
                "export",
                "--jsonl",
                "--status",
                "all",
                "-o",
                ".beads/beads.jsonl",
            ],
        ),
    );
    let jsonl_path = repo.path().join(".beads/beads.jsonl");
    let published_only = std::fs::read_to_string(&jsonl_path)
        .unwrap()
        .lines()
        .filter(|line| line.contains(&published_id))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&jsonl_path, published_only).unwrap();
    assert_close_success(
        "track public projection",
        &run_close_git(repo.path(), &["add", "-A"]),
    );
    assert_close_success(
        "commit public projection",
        &run_close_git(
            repo.path(),
            &["commit", "--no-verify", "-qm", "publish one bead"],
        ),
    );

    assert_close_success(
        "close published bead",
        &run_close_rsry(home.path(), repo.path(), &["bead", "close", &published_id]),
    );

    let records: Vec<serde_json::Value> = std::fs::read_to_string(&jsonl_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records.len(),
        1,
        "close must not publish local-only records"
    );
    assert_eq!(records[0]["id"], published_id);
    assert_eq!(
        records[0]["status"], "closed",
        "close must immediately refresh the published record to the store's terminal status"
    );
    assert!(
        !std::fs::read_to_string(&jsonl_path)
            .unwrap()
            .contains(&local_only_id),
        "local-only bead must remain outside the public projection"
    );

    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 2 }\n",
    )
    .unwrap();
    assert_close_success(
        "stage hook trigger",
        &run_close_git(repo.path(), &["add", "src/lib.rs"]),
    );
    let hook_commit = format!("[{published_id}] test(sync): exercise bounded pre-commit");
    assert_close_success(
        "commit through pre-commit hook",
        &run_close_git(repo.path(), &["commit", "-qm", &hook_commit]),
    );
    let after_hook = std::fs::read_to_string(&jsonl_path).unwrap();
    assert!(
        !after_hook.contains(&local_only_id),
        "pre-commit must use the same publication boundary as close"
    );
    assert_eq!(
        after_hook.lines().count(),
        1,
        "pre-commit must not broaden the public projection"
    );
}
