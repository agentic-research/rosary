//! `rsry hooks audit` under ADR-0024 amendment A (rosary-e5fc0e): the drift it
//! checks is the TRUNK projection against the store, record by record, and a
//! stale installed hook is a failure. Real binary, real `rsry init` hooks,
//! a bare origin standing in for the trunk.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

#[path = "common/mod.rs"]
mod audit_common;
use audit_common::created_id;

const AUDIT_JSONL: &str = ".beads/beads.jsonl";

struct AuditRepo {
    home: TempDir,
    _parent: TempDir,
    root: PathBuf,
    _remote: TempDir,
}

impl AuditRepo {
    fn new() -> Self {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let r = Self {
            home: TempDir::new().unwrap(),
            _parent: parent,
            root,
            _remote: TempDir::new().unwrap(),
        };
        r.must("git init", &r.git(&["init", "-q", "-b", "main"]));
        for args in [
            &["config", "user.email", "t@example.com"][..],
            &["config", "user.name", "t"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            r.must("git config", &r.git(args));
        }
        std::fs::write(r.root.join("README.md"), "# audit\n").unwrap();
        r.must("add README", &r.git(&["add", "README.md"]));
        r.must(
            "seed",
            &r.git(&[
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "[rosary-000000] chore: seed",
            ]),
        );
        let root = r.root.to_string_lossy().into_owned();
        r.must("rsry init", &r.rsry(&["init", &root]));
        let remote = r._remote.path().to_string_lossy().into_owned();
        r.must("bare remote", &r.git(&["init", "-q", "--bare", &remote]));
        r.must("remote add", &r.git(&["remote", "add", "origin", &remote]));
        r
    }

    fn env(&self, cmd: &mut Command) {
        cmd.current_dir(&self.root)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("RSRY_BIN", env!("CARGO_BIN_EXE_rsry"));
    }

    fn rsry(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsry"));
        self.env(&mut cmd);
        cmd.args(args).output().expect("spawn rsry")
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
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Publish the whole store by hand, commit it through the hooks, push main.
    fn publish_commit_push(&self, subject: &str) {
        self.must(
            "export",
            &self.rsry(&[
                "bead",
                "export",
                "--jsonl",
                "--status",
                "all",
                "-o",
                AUDIT_JSONL,
            ]),
        );
        self.must("add jsonl", &self.git(&["add", AUDIT_JSONL]));
        self.must("commit", &self.git(&["commit", "-q", "-m", subject]));
        self.must(
            "push main",
            &self.git(&["push", "-q", "-u", "origin", "main"]),
        );
    }

    fn audit(&self) -> (bool, String) {
        let out = self.rsry(&["hooks", "audit"]);
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn hooks_dir(&self) -> PathBuf {
        let out = self.git(&["rev-parse", "--git-path", "hooks"]);
        self.root.join(String::from_utf8_lossy(&out.stdout).trim())
    }
}

fn hook_path(r: &AuditRepo, name: &str) -> PathBuf {
    let p = r.hooks_dir().join(name);
    assert!(p.is_file(), "{} not installed", p.display());
    p
}

fn path_str(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// The trunk projection agrees → pass; a store write after the push → the
/// audit names the bead and the remedy; republish → pass again.
#[test]
fn audit_tracks_the_trunk_projection_against_the_store() {
    let r = AuditRepo::new();
    let out = r.rsry(&[
        "bead",
        "create",
        "audited work",
        "--files",
        "a.rs",
        "--acceptance",
        "cargo test audit",
    ]);
    r.must("bead create", &out);
    let id = created_id(&out);
    r.publish_commit_push(&format!("[{id}] chore(beads): publish"));

    let (ok, text) = r.audit();
    assert!(ok, "fresh trunk projection must pass:\n{text}");
    assert!(text.contains("agrees with the store"), "{text}");

    r.must(
        "comment add",
        &r.rsry(&["bead", "comment", "add", &id, "a write after the push"]),
    );
    let (ok, text) = r.audit();
    assert!(!ok, "a trunk record that lags the store must fail:\n{text}");
    assert!(text.contains("TRUNK PROJECTION STALE"), "{text}");
    assert!(text.contains(&id), "must name the bead:\n{text}");
    assert!(
        text.contains("trunk-refresh"),
        "must name the remedy:\n{text}"
    );

    r.publish_commit_push(&format!("[{id}] chore(beads): republish"));
    let (ok, text) = r.audit();
    assert!(ok, "republished trunk must pass again:\n{text}");
}

/// A working-tree file that differs from the trunk is NOT drift: the audit
/// reads the trunk blob, so a feature-branch checkout with an untouched (or
/// even hand-edited) working-tree file does not fail for that.
#[test]
fn audit_reads_the_trunk_blob_not_the_working_tree() {
    let r = AuditRepo::new();
    let out = r.rsry(&[
        "bead",
        "create",
        "audited work",
        "--files",
        "a.rs",
        "--acceptance",
        "cargo test audit",
    ]);
    r.must("bead create", &out);
    let id = created_id(&out);
    r.publish_commit_push(&format!("[{id}] chore(beads): publish"));
    r.must(
        "checkout feature",
        &r.git(&["checkout", "-q", "-b", "feature"]),
    );
    std::fs::write(r.root.join(AUDIT_JSONL), "").unwrap();
    let (ok, text) = r.audit();
    assert!(
        ok,
        "an emptied working-tree file is not trunk drift:\n{text}"
    );
}

/// A stale installed hook fails the audit outright.
#[test]
fn audit_fails_on_a_stale_hook_stamp() {
    let r = AuditRepo::new();
    let (ok, text) = r.audit();
    assert!(ok, "fresh install must pass:\n{text}");
    let hook = hook_path(&r, "pre-push");
    let content = std::fs::read_to_string(&hook).unwrap();
    let stamp = content
        .lines()
        .find(|l| l.starts_with("# rsry-hook pre-push v"))
        .expect("installed pre-push carries a stamp")
        .to_string();
    std::fs::write(
        &hook,
        content.replace(&stamp, "# rsry-hook pre-push v0.0.1 sha256:0000"),
    )
    .unwrap();
    let (ok, text) = r.audit();
    assert!(!ok, "a stale hook must fail the audit:\n{text}");
    assert!(text.contains("HOOK STALE"), "{text}");
    assert!(text.contains(path_str(Path::new("pre-push"))), "{text}");
}
