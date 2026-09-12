//! commit-msg publishes exactly the beads the commit subject names
//! (rosary-e5bfd3, journey observation 2) — real git, the real hooks `rsry
//! init` installs, `RSRY_BIN` pinned to this build.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

#[path = "common/mod.rs"]
mod scoped_publish_common;
use scoped_publish_common::created_id;

const TRACKED_JSONL: &str = ".beads/beads.jsonl";

struct Fixture {
    home: TempDir,
    _parent: TempDir,
    root: PathBuf,
}

impl Fixture {
    /// The repo dir is named `project` so bead ids carry a lowercase prefix
    /// (`project-<hex>`), the shape the subject parser accepts.
    fn new() -> Self {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let f = Self {
            home: TempDir::new().unwrap(),
            _parent: parent,
            root,
        };
        f.git_ok(&["init", "-q", "-b", "main"]);
        f.git_ok(&["config", "user.email", "t@example.com"]);
        f.git_ok(&["config", "user.name", "t"]);
        f.git_ok(&["config", "commit.gpgsign", "false"]);
        f.write("README.md", "# scoped\n");
        f.git_ok(&["add", "README.md"]);
        f.git_ok(&["commit", "-q", "--no-verify", "-m", "chore: seed"]);
        let root = f.root.to_string_lossy().into_owned();
        f.rsry_ok(&["init", &root]);
        f.rsry_ok(&["bead", "export", "--jsonl", "--status", "all", "-o", JSONL]);
        f.git_ok(&[
            "add",
            JSONL,
            ".beads/metadata.json",
            ".beads/.gitignore",
            "AGENTS.md",
        ]);
        f.git_ok(&[
            "commit",
            "-q",
            "--no-verify",
            "-m",
            "chore(beads): track export",
        ]);
        let hooks = f.git_ok(&["rev-parse", "--git-path", "hooks"]);
        let hooks = f.root.join(stdout(&hooks).trim());
        for (name, marker) in [("commit-msg", "bead publish"), ("post-commit", "--amend")] {
            let body = std::fs::read_to_string(hooks.join(name))
                .unwrap_or_else(|e| panic!("rsry init installs {name}: {e}"));
            assert!(body.contains(marker), "not the rsry {name}:\n{body}");
        }
        f
    }

    fn cmd(&self, prog: &str, args: &[&str]) -> Output {
        Command::new(prog)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("RSRY_BIN", env!("CARGO_BIN_EXE_rsry"))
            .env("NO_COLOR", "1")
            .output()
            .unwrap_or_else(|e| panic!("{prog} {}: {e}", args.join(" ")))
    }
    fn git(&self, args: &[&str]) -> Output {
        self.cmd("git", args)
    }
    fn git_ok(&self, args: &[&str]) -> Output {
        let out = self.git(args);
        assert!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            stderr(&out)
        );
        out
    }
    fn rsry(&self, args: &[&str]) -> Output {
        self.cmd(env!("CARGO_BIN_EXE_rsry"), args)
    }
    fn rsry_ok(&self, args: &[&str]) -> Output {
        let out = self.rsry(args);
        assert!(
            out.status.success(),
            "rsry {}: {}",
            args.join(" "),
            stderr(&out)
        );
        out
    }
    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }
    fn create_bead(&self, title: &str, file: &str) -> String {
        let out = self.rsry_ok(&[
            "bead",
            "create",
            title,
            "--files",
            file,
            "--acceptance",
            "cargo test x",
        ]);
        created_id(&out)
    }
    /// Until rosary-e5bf6b (stop write-through) lands, a store write still
    /// dirties the working-tree file; discard that so the only writer under
    /// test is commit-msg. A no-op once write-through is gone.
    fn discard_projection(&self) {
        self.git_ok(&["checkout", "--", JSONL]);
        let st = self.git_ok(&["status", "--porcelain", "--", JSONL]);
        assert_eq!(stdout(&st).trim(), "", "projection must start clean");
    }
    fn head(&self) -> String {
        stdout(&self.git_ok(&["rev-parse", "HEAD"]))
            .trim()
            .to_string()
    }
    fn head_files(&self) -> Vec<String> {
        stdout(&self.git_ok(&["show", "--name-only", "--format=", "HEAD"]))
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }
    /// Bead ids whose record is added or changed by HEAD's diff of the export.
    fn head_touched_ids(&self) -> Vec<String> {
        let diff = self.git_ok(&["show", "--format=", "HEAD", "--", JSONL]);
        stdout(&diff)
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .filter_map(|l| {
                let rec: serde_json::Value = serde_json::from_str(&l[1..]).ok()?;
                rec["id"].as_str().map(str::to_owned)
            })
            .collect()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn commit_carries_only_the_bead_its_subject_names() {
    let f = Fixture::new();
    let x = f.create_bead("named", "b.rs");
    let y = f.create_bead("unrelated", "z.rs");
    f.discard_projection();

    f.write("b.rs", "fn b() {}\n");
    f.git_ok(&["add", "b.rs"]);
    let subject = format!("[{x}] feat(core): more code");
    f.git_ok(&["commit", "-q", "-m", &subject]);

    let files = f.head_files();
    assert!(
        files.contains(&"b.rs".to_string()),
        "staged file missing: {files:?}"
    );
    assert!(
        files.contains(&JSONL.to_string()),
        "projection not staged: {files:?}"
    );
    assert_eq!(
        f.head_touched_ids(),
        vec![x.clone()],
        "unrelated {y} must not be swept in"
    );
    let blob = stdout(&f.git_ok(&["show", &format!("HEAD:{JSONL}")]));
    assert!(blob.contains(&x) && !blob.contains(&y));
    let st = f.git_ok(&["status", "--porcelain", "--", JSONL]);
    assert_eq!(stdout(&st).trim(), "", "commit-msg leaves the tree clean");
}

#[test]
fn commit_naming_no_own_bead_leaves_the_projection_untouched() {
    let f = Fixture::new();
    let y = f.create_bead("written but unnamed", "z.rs");
    f.discard_projection();
    f.rsry_ok(&["bead", "comment", "add", &y, "a write after tracking"]);
    let before = stdout(&f.git_ok(&["show", &format!("HEAD:{JSONL}")]));

    // The primitive itself: no id in the subject → nothing published.
    let msg = Path::new(&f.root).join("MSG");
    std::fs::write(&msg, "chore: no id\n\n[project-ffffff] body mention only\n").unwrap();
    let out = f.rsry_ok(&[
        "bead",
        "publish",
        "--from-commit-msg",
        msg.to_str().unwrap(),
    ]);
    assert_eq!(
        stdout(&out).trim(),
        "",
        "nothing to publish, nothing reported"
    );

    // Through the real hook: an id for ANOTHER repo is ignored, commit lands
    // with no projection change.
    f.write("c.rs", "fn c() {}\n");
    f.git_ok(&["add", "c.rs"]);
    f.git_ok(&["commit", "-q", "-m", "[other-000000] chore: not ours"]);
    assert_eq!(f.head_files(), vec!["c.rs".to_string()]);
    assert_eq!(
        stdout(&f.git_ok(&["show", &format!("HEAD:{JSONL}")])),
        before
    );

    // And the literal case: a subject with no id at all is refused by the
    // commit contract before publication is reached — HEAD and the tracked
    // projection are exactly as before.
    let head = f.head();
    f.write("d.rs", "fn d() {}\n");
    f.git_ok(&["add", "d.rs"]);
    let out = f.git(&["commit", "-q", "-m", "chore: no id"]);
    assert!(!out.status.success());
    assert_eq!(f.head(), head);
    assert_eq!(
        stdout(&f.git_ok(&["show", &format!("HEAD:{JSONL}")])),
        before
    );
}

#[test]
fn commit_naming_an_unknown_own_bead_is_refused() {
    let f = Fixture::new();
    let x = f.create_bead("real", "b.rs");
    f.discard_projection();
    let head = f.head();

    f.write("b.rs", "fn b() {}\n");
    f.git_ok(&["add", "b.rs"]);
    let out = f.git(&["commit", "-q", "-m", "[project-zzzzzz] feat(core): typo"]);
    assert!(
        !out.status.success(),
        "a typo must not silently publish nothing"
    );
    assert!(
        stderr(&out).contains("unknown bead project-zzzzzz in commit subject"),
        "{}",
        stderr(&out)
    );
    assert_eq!(f.head(), head, "refused commit must not land");
    let st = f.git_ok(&["status", "--porcelain", "--", JSONL]);
    assert_eq!(
        stdout(&st).trim(),
        "",
        "refusal leaves the projection untouched ({x} unpublished)"
    );
}
