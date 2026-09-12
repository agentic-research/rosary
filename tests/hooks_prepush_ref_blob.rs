//! pre-push gates the PUSHED ref's `.beads/beads.jsonl` blob, not the working
//! tree (rosary-e5c037), driven against the REAL binary with the REAL hooks
//! `rsry init` installs and a real bare remote.
//!
//! The `hooks_run_prepush_*` unit tests drive the template with a stub and
//! `publish::push` unit-tests the comparison; neither pushes. This is the
//! owner's day: commit under a bead, write to that bead afterwards, push.
//!
//! Journey observations this pins (tests/beads_dirty_journey.rs): (3) a
//! pushed commit whose record lags the store is refused and names the bead;
//! (5) a push that names no bead — or nothing to push at all — is never
//! refused by beads written elsewhere.
//!
//! Mutation: point the hook back at the working-tree file and (a)'s refusal
//! goes green-by-tautology, failing its `!success` assert; make the primitive
//! take ALL of the local sha's history for a new branch and (c) starts naming
//! the seed beads.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

#[path = "common/mod.rs"]
mod prepush_common;
use prepush_common::created_id;

const PREPUSH_JSONL: &str = ".beads/beads.jsonl";

struct PrepushRepo {
    home: TempDir,
    _parent: TempDir,
    root: PathBuf,
    _remote: TempDir,
}

impl PrepushRepo {
    /// main with the tracked export committed and pushed to a bare origin,
    /// hooks installed by `rsry init`.
    fn new() -> Self {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let repo = Self {
            home: TempDir::new().unwrap(),
            _parent: parent,
            root,
            _remote: TempDir::new().unwrap(),
        };
        repo.must("git init", &repo.git(&["init", "-q", "-b", "main"]));
        for args in [
            &["config", "user.email", "t@example.com"][..],
            &["config", "user.name", "t"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            repo.must("git config", &repo.git(args));
        }
        repo.write_file("README.md", "# prepush\n");
        repo.must("add README", &repo.git(&["add", "README.md"]));
        repo.must(
            "seed commit",
            &repo.git(&["commit", "-q", "--no-verify", "-m", "chore: seed"]),
        );

        let root = repo.root.to_string_lossy().into_owned();
        repo.must("rsry init", &repo.rsry(&["init", &root]));
        repo.must(
            "initial export",
            &repo.rsry(&[
                "bead",
                "export",
                "--jsonl",
                "--status",
                "all",
                "-o",
                PREPUSH_JSONL,
            ]),
        );
        repo.must(
            "stage export",
            &repo.git(&[
                "add",
                PREPUSH_JSONL,
                ".beads/metadata.json",
                ".beads/.gitignore",
                "AGENTS.md",
            ]),
        );
        repo.must(
            "track export",
            &repo.git(&[
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "chore(beads): track export",
            ]),
        );
        repo.assert_prepush_installed();

        let remote = repo._remote.path().to_string_lossy().into_owned();
        repo.must("bare remote", &repo.git(&["init", "-q", "--bare", &remote]));
        repo.must(
            "remote add",
            &repo.git(&["remote", "add", "origin", &remote]),
        );
        repo.must("push main", &repo.git(&["push", "-q", "origin", "main"]));
        repo
    }

    /// Guard against a vacuous pass: every "allowed" observation is trivially
    /// green if the hook is not wired, so prove it is the rsry-managed one.
    fn assert_prepush_installed(&self) {
        let dir = self.git(&["rev-parse", "--git-path", "hooks"]);
        let dir = self.root.join(out_text(&dir).trim());
        let body = std::fs::read_to_string(dir.join("pre-push"))
            .unwrap_or_else(|e| panic!("pre-push not installed at {}: {e}", dir.display()));
        assert!(
            body.contains("verify-pushed"),
            "pre-push is not the ref-blob gate:\n{body}"
        );
    }

    fn env(&self, cmd: &mut Command) {
        cmd.current_dir(&self.root)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("RSRY_BIN", env!("CARGO_BIN_EXE_rsry"))
            .env("NO_COLOR", "1");
    }

    fn rsry(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsry"));
        self.env(&mut cmd);
        cmd.args(args).output().expect("spawn rsry")
    }

    fn rsry_with_stdin(&self, args: &[&str], stdin: &str) -> Output {
        use std::io::Write as _;
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsry"));
        self.env(&mut cmd);
        let mut child = cmd
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rsry");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn git(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new("git");
        self.env(&mut cmd);
        cmd.args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")))
    }

    fn must(&self, label: &str, out: &Output) {
        assert!(
            out.status.success(),
            "{label} failed:\nstdout:\n{}\nstderr:\n{}",
            out_text(out),
            err_text(out)
        );
    }

    fn write_file(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn create_bead(&self, title: &str, file: &str) -> String {
        let out = self.rsry(&[
            "bead",
            "create",
            title,
            "--files",
            file,
            "--acceptance",
            "cargo test prepush",
        ]);
        self.must("bead create", &out);
        created_id(&out)
    }

    /// Publish the whole store into the tracked file by hand — the explicit
    /// escape hatch. Since #495/#498 a store write publishes nothing, and the
    /// commit-time publish is P2 (rosary-e5bfd3); this fixture only needs X's
    /// record IN the commit so the gate's verdict is about staleness.
    fn publish_all(&self) {
        let out = self.rsry(&[
            "bead",
            "export",
            "--jsonl",
            "--status",
            "all",
            "-o",
            PREPUSH_JSONL,
        ]);
        self.must("bead export", &out);
        self.must("git add jsonl", &self.git(&["add", PREPUSH_JSONL]));
    }

    /// Commit `file` under `subject` through the real commit-msg/pre-commit hooks.
    fn commit_code(&self, file: &str, subject: &str) {
        self.write_file(file, "fn f() {}\n");
        self.must("git add", &self.git(&["add", file]));
        self.must("git commit", &self.git(&["commit", "-q", "-m", subject]));
    }

    fn head(&self) -> String {
        out_text(&self.git(&["rev-parse", "HEAD"]))
            .trim()
            .to_string()
    }

    fn blob(&self, rev: &str) -> String {
        out_text(&self.git(&["show", &format!("{rev}:{PREPUSH_JSONL}")]))
    }
}

fn out_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn err_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn record_for<'a>(blob: &'a str, id: &str) -> Option<&'a str> {
    blob.lines().find(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|r| r["id"].as_str().map(|s| s == id))
            .unwrap_or(false)
    })
}

/// (a) Commit `[X] …`, then write to X, push → refused naming X. Publish X in
/// a follow-up commit → push allowed, and the pushed blob carries the write.
#[test]
fn a_pushed_record_that_lags_the_store_is_refused_until_published() {
    let r = PrepushRepo::new();
    r.must(
        "checkout feature",
        &r.git(&["checkout", "-q", "-b", "feature"]),
    );
    let id = r.create_bead("feature work", "a.rs");
    r.publish_all();
    r.commit_code("a.rs", &format!("[{id}] feat(core): work"));
    let committed = r.blob("HEAD");
    assert!(
        record_for(&committed, &id).is_some(),
        "fixture: the commit must carry X's record so the refusal is about STALENESS, not absence"
    );

    // The store moves on after the commit — the shape of observation 3.
    r.must(
        "comment add",
        &r.rsry(&["bead", "comment", "add", &id, "opened PR #1"]),
    );

    let push = r.git(&["push", "-q", "-u", "origin", "feature"]);
    let err = err_text(&push);
    assert!(
        !push.status.success(),
        "a pushed record that lags the store must be refused:\n{err}"
    );
    assert!(err.contains(&id), "the refusal must name the bead:\n{err}");
    assert!(
        err.contains("rsry bead publish"),
        "the refusal must prescribe the fix:\n{err}"
    );
    assert!(
        !err.contains("origin/feature"),
        "the refused push must not have created the remote ref"
    );

    // Publish X. `rsry bead publish X` is the verb (rosary-e5bfd3); until it
    // lands, the whole-store export is the explicit hand that puts X's current
    // record into the tracked file — the same artifact, then committed.
    r.publish_all();
    r.must(
        "publish commit",
        &r.git(&["commit", "-q", "-m", &format!("[{id}] chore: publish")]),
    );
    let push = r.git(&["push", "-q", "-u", "origin", "feature"]);
    r.must("push after publish", &push);

    let pushed = r.blob("origin/feature");
    let record = record_for(&pushed, &id).expect("pushed blob carries X");
    assert!(
        record.contains("opened PR #1"),
        "the pushed record must be the store's current one:\n{record}"
    );
}

/// (b) On main with nothing to push, a store holding beads main's file lacks
/// is not this push's business — neither for `git push` nor for the primitive
/// fed an empty ref list or an empty range.
#[test]
fn b_nothing_to_push_is_allowed_whatever_the_store_holds() {
    let r = PrepushRepo::new();
    let id = r.create_bead("main's file lacks me", "z.rs");
    assert!(
        record_for(&r.blob("HEAD"), &id).is_none(),
        "fixture: main's committed file must lack the new bead"
    );

    let push = r.git(&["push", "-q", "origin", "main"]);
    r.must("push main with nothing to push", &push);

    let empty = r.rsry_with_stdin(&["bead", "verify-pushed"], "");
    r.must("verify-pushed with no refs", &empty);
    let head = r.head();
    let same = r.rsry_with_stdin(
        &["bead", "verify-pushed"],
        &format!("refs/heads/main {head} refs/heads/main {head}\n"),
    );
    r.must("verify-pushed with an empty range", &same);
}

/// (c) A code-only commit pushed while the store is dirty: the range's
/// subjects name no bead, so the push is allowed whatever the store holds —
/// the exact push observation 5 saw refused.
#[test]
fn c_a_code_only_push_is_allowed_while_the_store_is_dirty() {
    let r = PrepushRepo::new();
    let id = r.create_bead("written elsewhere", "w.rs");

    // commit-msg requires a bead bracket on every subject; the gate under
    // test is pre-push, so the code-only commit bypasses commit-msg.
    r.write_file("c.rs", "fn c() {}\n");
    r.must("add c.rs", &r.git(&["add", "c.rs"]));
    r.must(
        "code-only commit",
        &r.git(&["commit", "-q", "--no-verify", "-m", "chore: code only"]),
    );
    // Dirty the store further after the commit, for a bead the pushed blob
    // does not even carry.
    r.must(
        "comment add",
        &r.rsry(&["bead", "comment", "add", &id, "store moved on"]),
    );
    assert!(
        record_for(&r.blob("HEAD"), &id).is_none(),
        "fixture: the pushed blob must lack the dirty bead"
    );

    let push = r.git(&["push", "-q", "origin", "main"]);
    r.must("code-only push while the store is dirty", &push);
    assert_eq!(r.blob("origin/main"), r.blob("HEAD"));
}
