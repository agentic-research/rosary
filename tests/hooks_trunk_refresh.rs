//! Trunk refresh (rosary-e5c0a0, ADR-0024 amendment A): after a merge on the
//! trunk, the post-merge hook re-renders every published bead from the store,
//! commits the projection and pushes it — so the trunk carries the current
//! record of every published bead. Real binary, real `rsry init` hooks, a
//! bare origin, and a peer clone that lands the merge.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

#[path = "common/mod.rs"]
mod trunk_common;
use trunk_common::created_id;

const TRUNK_TEST_JSONL: &str = ".beads/beads.jsonl";

struct TrunkRepo {
    home: TempDir,
    _parent: TempDir,
    root: PathBuf,
    peer: PathBuf,
    remote: TempDir,
}

impl TrunkRepo {
    fn new() -> Self {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        let peer = parent.path().join("peer");
        std::fs::create_dir(&root).unwrap();
        let r = Self {
            home: TempDir::new().unwrap(),
            _parent: parent,
            root,
            peer,
            remote: TempDir::new().unwrap(),
        };
        r.must("git init", &r.git(&r.root, &["init", "-q", "-b", "main"]));
        r.configure(&r.root);
        std::fs::write(r.root.join("README.md"), "# trunk\n").unwrap();
        r.must("add README", &r.git(&r.root, &["add", "README.md"]));
        r.must(
            "seed",
            &r.git(
                &r.root,
                &[
                    "commit",
                    "-q",
                    "--no-verify",
                    "-m",
                    "[rosary-000000] chore: seed",
                ],
            ),
        );
        let root = r.root.to_string_lossy().into_owned();
        r.must("rsry init", &r.rsry(&["init", &root]));
        let remote = r.remote.path().to_string_lossy().into_owned();
        r.must(
            "bare remote",
            &r.git(&r.root, &["init", "-q", "--bare", &remote]),
        );
        r.must(
            "remote add",
            &r.git(&r.root, &["remote", "add", "origin", &remote]),
        );
        r
    }

    fn configure(&self, dir: &Path) {
        for args in [
            &["config", "user.email", "t@example.com"][..],
            &["config", "user.name", "t"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            self.must("git config", &self.git(dir, args));
        }
    }

    fn env(&self, cmd: &mut Command, dir: &Path) {
        cmd.current_dir(dir)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("RSRY_BIN", env!("CARGO_BIN_EXE_rsry"));
    }

    fn rsry(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsry"));
        self.env(&mut cmd, &self.root);
        cmd.args(args).output().expect("spawn rsry")
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Output {
        let mut cmd = Command::new("git");
        self.env(&mut cmd, dir);
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

    /// Publish the whole store by hand, commit on main, push — the trunk's
    /// first projection.
    fn publish_all_and_push(&self, subject: &str) {
        self.must(
            "export",
            &self.rsry(&[
                "bead",
                "export",
                "--jsonl",
                "--status",
                "all",
                "-o",
                TRUNK_TEST_JSONL,
            ]),
        );
        self.must("add", &self.git(&self.root, &["add", TRUNK_TEST_JSONL]));
        self.must(
            "commit",
            &self.git(&self.root, &["commit", "-q", "-m", subject]),
        );
        self.must(
            "push",
            &self.git(&self.root, &["push", "-q", "-u", "origin", "main"]),
        );
    }

    /// A peer clone lands one commit on origin/main so the owner's next pull
    /// is a real merge (post-merge fires only on a merge).
    fn peer_lands_a_commit(&self, subject: &str) {
        let remote = self.remote.path().to_string_lossy().into_owned();
        let peer = self.peer.to_string_lossy().into_owned();
        self.must(
            "peer clone",
            &self.git(self._parent.path(), &["clone", "-q", &remote, &peer]),
        );
        self.configure(&self.peer);
        std::fs::write(self.peer.join("peer.rs"), "fn peer() {}\n").unwrap();
        self.must("peer add", &self.git(&self.peer, &["add", "peer.rs"]));
        self.must(
            "peer commit",
            &self.git(&self.peer, &["commit", "-q", "--no-verify", "-m", subject]),
        );
        self.must(
            "peer push",
            &self.git(&self.peer, &["push", "-q", "origin", "main"]),
        );
    }

    fn trunk_blob(&self, rev: &str) -> String {
        let out = self.git(&self.root, &["show", &format!("{rev}:{TRUNK_TEST_JSONL}")]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn head_subject(&self, rev: &str) -> String {
        let out = self.git(&self.root, &["log", "-1", "--format=%s", rev]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn count_commits(&self, range: &str) -> usize {
        let out = self.git(&self.root, &["rev-list", "--count", range]);
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    /// The projection is clean: `rsry init` leaves untracked files (AGENTS.md,
    /// metadata) that are not this contract's business.
    fn clean(&self) -> bool {
        let out = self.git(
            &self.root,
            &["status", "--porcelain", "--", TRUNK_TEST_JSONL],
        );
        String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }
}

fn trunk_record_for<'a>(blob: &'a str, id: &str) -> Option<&'a str> {
    blob.lines().find(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|r| r["id"].as_str().map(|s| s == id))
            .unwrap_or(false)
    })
}

/// The post-merge hook on the trunk: a store write that never reached the
/// trunk (here a comment) is rendered, committed and pushed by the pull that
/// lands a peer's merge; a second run changes nothing.
#[test]
fn the_post_merge_hook_refreshes_commits_and_pushes_the_trunk_projection() {
    let r = TrunkRepo::new();
    let out = r.rsry(&[
        "bead",
        "create",
        "trunk work",
        "--files",
        "a.rs",
        "--acceptance",
        "cargo test trunk",
    ]);
    r.must("bead create", &out);
    let id = created_id(&out);
    r.publish_all_and_push(&format!("[{id}] chore(beads): publish"));
    let before = r.trunk_blob("origin/main");
    assert!(
        !trunk_record_for(&before, &id)
            .unwrap()
            .contains("landed via merge")
    );

    // A store write the trunk never saw.
    r.must(
        "comment add",
        &r.rsry(&["bead", "comment", "add", &id, "landed via merge"]),
    );
    r.peer_lands_a_commit(&format!("[{id}] feat(core): peer work (#1)"));

    // The owner pulls the merge; the post-merge hook does the rest.
    r.must(
        "pull",
        &r.git(&r.root, &["pull", "-q", "--ff-only", "origin", "main"]),
    );

    assert!(r.clean(), "the tree must be clean after the refresh");
    let subject = r.head_subject("HEAD");
    assert!(
        subject.starts_with(&format!("[{id}] chore(beads): trunk refresh")),
        "HEAD must be the refresh commit, got: {subject}"
    );
    let pushed = r.trunk_blob("origin/main");
    assert!(
        trunk_record_for(&pushed, &id)
            .unwrap()
            .contains("landed via merge"),
        "origin/main must carry the store's current record:\n{pushed}"
    );
    assert_eq!(r.count_commits("HEAD..origin/main"), 0, "pushed");

    // Idempotent: nothing to refresh, nothing committed.
    let head = r.head_subject("HEAD");
    let again = r.rsry(&["bead", "trunk-refresh", "--push"]);
    r.must("second refresh", &again);
    assert!(String::from_utf8_lossy(&again.stderr).contains("already current"));
    assert_eq!(r.head_subject("HEAD"), head, "no second refresh commit");
}

/// A trunk that refuses the push (PR-only): the refresh is parked on
/// `rsry/trunk-refresh`, the trunk is reset to the remote, the tree is clean.
#[test]
fn a_refused_push_parks_the_refresh_and_leaves_the_trunk_clean() {
    let r = TrunkRepo::new();
    let out = r.rsry(&[
        "bead",
        "create",
        "trunk work",
        "--files",
        "a.rs",
        "--acceptance",
        "cargo test trunk",
    ]);
    r.must("bead create", &out);
    let id = created_id(&out);
    r.publish_all_and_push(&format!("[{id}] chore(beads): publish"));
    // Emulate the ruleset: the remote refuses every push from now on.
    let hook = r.remote.path().join("hooks").join("pre-receive");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'GH013: main is protected' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    r.must(
        "comment add",
        &r.rsry(&["bead", "comment", "add", &id, "cannot push this"]),
    );

    let out = r.rsry(&["bead", "trunk-refresh", "--push"]);
    r.must("trunk-refresh must not fail the hook", &out);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("parked"), "{err}");
    assert!(
        err.contains("rosary-e5be46"),
        "must name the decision:\n{err}"
    );
    assert!(r.clean(), "tree must be clean after parking");
    assert_eq!(
        r.count_commits("origin/main..HEAD"),
        0,
        "trunk reset to the remote"
    );
    let parked = r.head_subject("rsry/trunk-refresh");
    assert!(
        parked.starts_with(&format!("[{id}] chore(beads): trunk refresh")),
        "{parked}"
    );
    assert!(
        trunk_record_for(&r.trunk_blob("rsry/trunk-refresh"), &id)
            .unwrap()
            .contains("cannot push this")
    );
}

/// Off the trunk the primitive does nothing; a dirty projection is refused
/// rather than swept.
#[test]
fn skips_off_trunk_and_refuses_a_dirty_projection() {
    let r = TrunkRepo::new();
    let out = r.rsry(&[
        "bead",
        "create",
        "trunk work",
        "--files",
        "a.rs",
        "--acceptance",
        "cargo test trunk",
    ]);
    r.must("bead create", &out);
    let id = created_id(&out);
    r.publish_all_and_push(&format!("[{id}] chore(beads): publish"));

    r.must(
        "checkout feature",
        &r.git(&r.root, &["checkout", "-q", "-b", "feature"]),
    );
    let out = r.rsry(&["bead", "trunk-refresh"]);
    r.must("off-trunk", &out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("skipped"));
    r.must(
        "checkout main",
        &r.git(&r.root, &["checkout", "-q", "main"]),
    );

    std::fs::write(r.root.join(TRUNK_TEST_JSONL), "").unwrap();
    let out = r.rsry(&["bead", "trunk-refresh"]);
    assert!(!out.status.success(), "a dirty projection must be refused");
    assert!(String::from_utf8_lossy(&out.stderr).contains("uncommitted changes"));
}
