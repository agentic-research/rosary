//! A tracked bead snapshot necessarily predates the merge commit that closes
//! its own PR. These tests pin fresh-clone reconstruction at that boundary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn fixture_rsry_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rsry"))
}

fn run_fixture_rsry(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(fixture_rsry_binary())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .expect("spawn rsry")
}

fn run_fixture_git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")))
}

fn extract_created_bead_id(output: &Output) -> String {
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

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn init_restores_jsonl_then_reconciles_merged_bead() {
    let producer_home = TempDir::new().unwrap();
    let producer_parent = TempDir::new().unwrap();
    let producer = producer_parent.path().join("project");
    std::fs::create_dir(&producer).unwrap();
    let remote = TempDir::new().unwrap();
    let consumer_home = TempDir::new().unwrap();
    let consumer_parent = TempDir::new().unwrap();
    let consumer = consumer_parent.path().join("repo");

    assert_success(
        "producer git init",
        &run_fixture_git(&producer, &["init", "-q", "-b", "main"]),
    );
    assert_success(
        "remote git init",
        &run_fixture_git(remote.path(), &["init", "--bare", "-q"]),
    );
    assert_success(
        "remote default branch",
        &run_fixture_git(remote.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]),
    );
    for args in [
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Test User"][..],
        &["config", "commit.gpgsign", "false"][..],
    ] {
        assert_success("producer git config", &run_fixture_git(&producer, args));
    }

    let producer_path = producer.to_string_lossy().into_owned();
    assert_success(
        "producer rsry init",
        &run_fixture_rsry(producer_home.path(), &producer, &["init", &producer_path]),
    );
    std::fs::create_dir_all(producer.join("src")).unwrap();
    std::fs::write(producer.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
    let create = run_fixture_rsry(
        producer_home.path(),
        &producer,
        &[
            "bead",
            "create",
            "merge-derived terminal state",
            "--files",
            "src/lib.rs",
        ],
    );
    assert_success("create bead", &create);
    let bead_id = extract_created_bead_id(&create);
    assert_success(
        "export open snapshot",
        &run_fixture_rsry(
            producer_home.path(),
            &producer,
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

    assert_success(
        "stage snapshot",
        &run_fixture_git(&producer, &["add", "-A"]),
    );
    assert_success(
        "commit snapshot",
        &run_fixture_git(
            &producer,
            &["commit", "--no-verify", "-qm", "seed open bead snapshot"],
        ),
    );
    std::fs::write(producer.join("src/lib.rs"), "pub fn value() -> u8 { 2 }\n").unwrap();
    assert_success(
        "stage implementation",
        &run_fixture_git(&producer, &["add", "src/lib.rs"]),
    );
    let merge_subject = format!("[{bead_id}] fix(core): land change (#1)");
    assert_success(
        "commit merge evidence",
        &run_fixture_git(&producer, &["commit", "--no-verify", "-qm", &merge_subject]),
    );
    let remote_path = remote.path().to_string_lossy().into_owned();
    assert_success(
        "add origin",
        &run_fixture_git(&producer, &["remote", "add", "origin", &remote_path]),
    );
    assert_success(
        "push trunk",
        &run_fixture_git(&producer, &["push", "-q", "-u", "origin", "main"]),
    );
    let clone = Command::new("git")
        .args(["clone", "-q", "--branch", "main", &remote_path])
        .arg(&consumer)
        .output()
        .expect("clone consumer");
    assert_success("clone consumer", &clone);

    let consumer_path = consumer.to_string_lossy().into_owned();
    assert_success(
        "consumer rsry init",
        &run_fixture_rsry(consumer_home.path(), &consumer, &["init", &consumer_path]),
    );
    let public_snapshot =
        std::fs::read_to_string(consumer.join(".beads/beads.jsonl")).expect("public JSONL");
    let public_record: serde_json::Value =
        serde_json::from_str(public_snapshot.trim()).expect("public JSONL record");
    assert_eq!(
        public_record["status"], "open",
        "bootstrap must not regenerate or republish the public projection"
    );
    let review = run_fixture_rsry(
        consumer_home.path(),
        &consumer,
        &["bead", "review", &bead_id, "--json"],
    );
    assert_success("review restored bead", &review);
    let panel: serde_json::Value = serde_json::from_slice(&review.stdout).expect("review JSON");
    assert_eq!(
        panel["bead"]["status"], "done",
        "merge evidence must override the necessarily stale open snapshot"
    );
}
