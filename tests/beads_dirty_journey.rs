//! The `.beads/beads.jsonl` dirty-plague journey (rosary-3d455a), driven
//! against the REAL binary with the REAL hooks `rsry init` installs.
//!
//! Every existing test observes one piece of this path in isolation:
//! `src/publish/tests.rs` asserts file content and never looks at `git
//! status`; the `hooks_run_*` unit tests drive the hook logic with a stub
//! binary; `tests/init_jsonl_reconciliation.rs` and `tests/close_jsonl_sync.rs`
//! are single-branch, single-writer, and commit with `--no-verify`. None of
//! them creates a second branch, commits through the pre-commit hook after a
//! store write, pushes through the pre-push hook, or asserts a clean tree.
//!
//! This test is the owner's actual day: write a bead on a feature branch,
//! commit code with explicit paths, push, switch back to main, push main. It
//! collects every violation instead of stopping at the first, so one run
//! reports the whole shape of the defect. It is RED by design until the
//! materialization-timing decision in rosary-3d455a (ADR-0024 amendment)
//! lands; it is fix-agnostic — commit-time materialization, main-only
//! regeneration, or a projection outside the working tree all turn it green.
//!
//! Mutation: re-enable write-through in `PublishingBeadStore::publish`, or
//! point pre-push back at the working-tree file, and observations 1/3/4/5 go
//! red again; drop `--published-from` from pre-commit and observation 2 does.
//!
//! It is `#[ignore]`d so `task check` on main stays green while the decision
//! is pending. Run it with `cargo test --test beads_dirty_journey -- --ignored`;
//! that invocation is RED today and is the bead's close condition. The fix
//! removes the `#[ignore]` — a fix that leaves it in place has not landed.

use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

#[path = "common/mod.rs"]
mod journey_common;
use journey_common::created_id;

const JSONL: &str = ".beads/beads.jsonl";

struct Journey {
    home: TempDir,
    _parent: TempDir,
    root: PathBuf,
    remote: TempDir,
    transcript: Vec<String>,
    violations: Vec<String>,
}

impl Journey {
    fn new() -> Self {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        Self {
            home: TempDir::new().unwrap(),
            _parent: parent,
            root,
            remote: TempDir::new().unwrap(),
            transcript: Vec::new(),
            violations: Vec::new(),
        }
    }

    /// The hooks resolve the binary through `RSRY_BIN` first, and read config
    /// under `$HOME`; both are pinned to the fixture so the hooks exercise
    /// THIS build against THIS store, never the developer's installed rsry.
    fn rsry(&mut self, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_rsry"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("RSRY_BIN", env!("CARGO_BIN_EXE_rsry"))
            .output()
            .expect("spawn rsry");
        self.record("rsry", args, &out);
        out
    }

    fn git(&mut self, args: &[&str]) -> Output {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("RSRY_BIN", env!("CARGO_BIN_EXE_rsry"))
            .output()
            .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")));
        self.record("git", args, &out);
        out
    }

    fn record(&mut self, prog: &str, args: &[&str], out: &Output) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        self.transcript.push(format!(
            "$ {prog} {}  -> exit {}{}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            if stderr.is_empty() {
                String::new()
            } else {
                format!("\n    {}", stderr.replace('\n', "\n    "))
            }
        ));
    }

    fn must(&mut self, label: &str, out: &Output) {
        assert!(
            out.status.success(),
            "{label} failed (fixture, not the journey):\n{}\n\ntranscript:\n{}",
            String::from_utf8_lossy(&out.stderr),
            self.transcript.join("\n")
        );
    }

    fn observe(&mut self, n: u8, what: &str, ok: bool, detail: String) {
        let mark = if ok { "ok " } else { "RED" };
        self.transcript
            .push(format!("[{n}] {mark} {what}: {detail}"));
        if !ok {
            self.violations.push(format!("({n}) {what}: {detail}"));
        }
    }

    fn jsonl_status(&mut self) -> String {
        let out = self.git(&["status", "--porcelain", "--", JSONL]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Bead ids named in the `+` lines of a diff of the tracked export.
fn added_ids(diff: &str) -> Vec<String> {
    diff.lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .filter_map(|l| {
            let rec: serde_json::Value = serde_json::from_str(&l[1..]).ok()?;
            rec["id"].as_str().map(str::to_owned)
        })
        .collect()
}

/// Guard against a vacuous pass: every observation below is trivially green
/// when the hooks are not actually wired, so prove they are before observing.
fn assert_hooks_active(j: &mut Journey) {
    let hooks_path = j.git(&["config", "--get", "core.hooksPath"]);
    let hooks_path = stdout(&hooks_path).trim().to_string();
    assert_ne!(hooks_path, "/dev/null", "fixture must not disable hooks");
    let dir = j.git(&["rev-parse", "--git-path", "hooks"]);
    let dir = j.root.join(stdout(&dir).trim());
    for hook in ["pre-commit", "pre-push", "commit-msg"] {
        let body = std::fs::read_to_string(dir.join(hook)).unwrap_or_else(|e| {
            panic!(
                "{hook} not installed by rsry init at {}: {e}",
                dir.display()
            )
        });
        assert!(body.contains("rsry"), "{hook} is not the rsry-managed hook");
    }
}

#[test]
#[ignore = "rosary-3d455a: RED by design until the ADR-0024 materialization amendment lands; run with -- --ignored"]
fn a_bead_written_on_a_feature_branch_does_not_dirty_or_block_the_checkout() {
    let mut j = Journey::new();

    // ---- fixture: main with the tracked export committed and pushed --------
    let o = j.git(&["init", "-q", "-b", "main"]);
    j.must("git init", &o);
    for args in [
        &["config", "user.email", "t@example.com"][..],
        &["config", "user.name", "t"][..],
        &["config", "commit.gpgsign", "false"][..],
    ] {
        let o = j.git(args);
        j.must("git config", &o);
    }
    j.write("README.md", "# journey\n");
    let o = j.git(&["add", "README.md"]);
    j.must("add README", &o);
    let o = j.git(&[
        "commit",
        "-q",
        "--no-verify",
        "-m",
        "[rosary-000000] chore: seed",
    ]);
    j.must("seed commit", &o);

    let root = j.root.to_string_lossy().into_owned();
    let o = j.rsry(&["init", &root]);
    j.must("rsry init", &o);
    let o = j.rsry(&["bead", "export", "--jsonl", "--status", "all", "-o", JSONL]);
    j.must("initial export", &o);
    let o = j.git(&[
        "add",
        JSONL,
        ".beads/metadata.json",
        ".beads/.gitignore",
        "AGENTS.md",
    ]);
    j.must("stage export", &o);
    let o = j.git(&[
        "commit",
        "-q",
        "--no-verify",
        "-m",
        "[rosary-000000] chore(beads): track export",
    ]);
    j.must("track export", &o);
    assert_hooks_active(&mut j);

    let remote = j.remote.path().to_string_lossy().into_owned();
    let o = j.git(&["init", "-q", "--bare", &remote]);
    j.must("bare remote", &o);
    let o = j.git(&["remote", "add", "origin", &remote]);
    j.must("remote add", &o);
    let o = j.git(&["push", "-q", "origin", "main"]);
    j.must("push main (fixture)", &o);
    assert_eq!(j.jsonl_status(), "", "fixture must start clean");

    // ---- the journey ---------------------------------------------------------
    let o = j.git(&["checkout", "-q", "-b", "feature"]);
    j.must("checkout feature", &o);
    j.write("a.rs", "fn a() {}\n");
    let o = j.git(&["add", "a.rs"]);
    j.must("add a.rs", &o);
    let o = j.git(&[
        "commit",
        "-q",
        "-m",
        "[rosary-000000] feat(core): code only",
    ]);
    j.must("code-only commit", &o);

    // (1) one store write from any surface — the CLI here; MCP, the HTTP daemon
    //     and the reconciler all go through the same PublishingBeadStore.
    let o = j.rsry(&[
        "bead",
        "create",
        "feature work",
        "--files",
        "a.rs",
        "--acceptance",
        "cargo test feature_work",
    ]);
    j.must("bead create", &o);
    let id = created_id(&o);
    let status = j.jsonl_status();
    j.observe(
        1,
        "one bead write leaves the tracked export clean",
        status.is_empty(),
        format!("git status -> {status:?}"),
    );

    // A second, unrelated bead: the churn a code commit must NOT carry.
    let o = j.rsry(&[
        "bead",
        "create",
        "unrelated note",
        "--files",
        "z.rs",
        "--acceptance",
        "cargo test unrelated",
    ]);
    j.must("second bead create", &o);
    let other = created_id(&o);

    // (2) explicit-path commit through the real pre-commit hook.
    j.write("b.rs", "fn b() {}\n");
    let o = j.git(&["add", "b.rs"]);
    j.must("add b.rs", &o);
    let subject = format!("[{id}] feat(core): more code");
    let o = j.git(&["commit", "-q", "-m", &subject]);
    j.must("explicit-path commit", &o);
    let files = j.git(&["show", "--name-only", "--format=", "HEAD"]);
    let files = stdout(&files);
    assert!(
        files.contains("b.rs"),
        "the staged file must be in the commit:\n{files}"
    );
    let diff = j.git(&["show", "--format=", "HEAD", "--", JSONL]);
    let swept: Vec<String> = added_ids(&stdout(&diff))
        .into_iter()
        .filter(|x| x != &id)
        .collect();
    j.observe(2, "explicit-path commit carries no bead record but its own", swept.is_empty(),
        format!("commit touched {}; records swept in besides {id}: {swept:?} (unrelated bead {other} must not appear)",
            files.split_whitespace().collect::<Vec<_>>().join(", ")));

    // (3) a bead write AFTER the commit, then push through the real pre-push hook.
    let o = j.rsry(&["bead", "comment", "add", &id, "opened PR #1"]);
    j.must("comment add", &o);
    let push = j.git(&["push", "-q", "-u", "origin", "feature"]);
    let pushed_blob = j.git(&["show", &format!("origin/feature:{JSONL}")]);
    let blob_has_comment = stdout(&pushed_blob).contains("opened PR #1");
    let ok = !push.status.success() || blob_has_comment;
    j.observe(
        3,
        "push is refused, or the pushed blob agrees with the store",
        ok,
        format!(
            "push exit {}, pushed blob has the comment: {blob_has_comment}",
            push.status.code().unwrap_or(-1)
        ),
    );

    // (4) switch back to main with no stash.
    let co = j.git(&["checkout", "-q", "main"]);
    j.observe(
        4,
        "git checkout main succeeds without a stash",
        co.status.success(),
        String::from_utf8_lossy(&co.stderr).trim().to_string(),
    );
    if !co.status.success() {
        // Recover the way the owner does, so the remaining observation runs.
        let o = j.git(&["checkout", "--", JSONL]);
        j.must("discard projection", &o);
        let o = j.git(&["checkout", "-q", "main"]);
        j.must("checkout main after discard", &o);
    }

    // (5) nothing was written on main; pushing main must not be refused.
    let push_main = j.git(&["push", "-q", "origin", "main"]);
    j.observe(
        5,
        "push main is not refused by a bead written on feature",
        push_main.status.success(),
        String::from_utf8_lossy(&push_main.stderr)
            .lines()
            .find(|l| l.contains("ERROR"))
            .unwrap_or("")
            .to_string(),
    );

    assert!(
        j.violations.is_empty(),
        "beads.jsonl dirty-plague journey (rosary-3d455a) — {} violation(s):\n  {}\n\ntranscript:\n{}",
        j.violations.len(),
        j.violations.join("\n  "),
        j.transcript.join("\n")
    );
}
