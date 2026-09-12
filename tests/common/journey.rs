//! Shared harness for the `.beads/beads.jsonl` dirty-plague journeys
//! (rosary-3d455a): one fixture, one transcript, one violation collector, and
//! the observations every write surface shares. Each `beads_dirty_journey*.rs`
//! binary supplies the store write (observations 1 and 3 — CLI, MCP stdio,
//! or the post-merge reconciler) and reuses observations 2, 4 and 5 from here.
//!
//! Lives under `tests/common/` for the same reason `common/mod.rs` does: the
//! mache `duplicate_definitions` gate keys on token names crate-wide, so a
//! per-binary copy of `Journey` would register as a structural duplicate
//! (rosary-e5fcd2). Include it under a binary-unique module alias.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

// `created_id` has exactly one definition, in `common/mod.rs`; re-export it
// rather than carry a second copy under this module.
#[path = "mod.rs"]
mod journey_base;
pub use journey_base::created_id;

pub const JSONL: &str = ".beads/beads.jsonl";

pub struct Journey {
    pub home: TempDir,
    _parent: TempDir,
    pub root: PathBuf,
    /// A second, hook-less clone of the remote: the peer whose pushes the
    /// owner pulls (only the reconciler journey uses it).
    pub peer: PathBuf,
    pub remote: TempDir,
    pub transcript: Vec<String>,
    pub violations: Vec<String>,
}

impl Journey {
    pub fn new() -> Self {
        let parent = TempDir::new().unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let home = TempDir::new().unwrap();
        // One identity for the owner, the peer, and every hook-spawned git.
        std::fs::write(
            home.path().join(".gitconfig"),
            "[user]\n\temail = t@example.com\n\tname = t\n[commit]\n\tgpgsign = false\n\
             [pull]\n\trebase = false\n",
        )
        .unwrap();
        Self {
            home,
            peer: parent.path().join("peer"),
            _parent: parent,
            root,
            remote: TempDir::new().unwrap(),
            transcript: Vec::new(),
            violations: Vec::new(),
        }
    }

    /// Env every process in the journey runs under: HOME pinned to the fixture
    /// (config, registry, gitconfig) and `RSRY_BIN` pinned to THIS build so the
    /// hooks exercise it against THIS store, never the installed rsry.
    pub fn env(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("RSRY_BIN", env!("CARGO_BIN_EXE_rsry"));
    }

    pub fn rsry(&mut self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsry"));
        cmd.args(args).current_dir(&self.root);
        self.env(&mut cmd);
        let out = cmd.output().expect("spawn rsry");
        self.record("rsry", args, &out);
        out
    }

    pub fn git(&mut self, args: &[&str]) -> Output {
        let root = self.root.clone();
        self.git_in("git", &root, args)
    }

    pub fn peer_git(&mut self, args: &[&str]) -> Output {
        let peer = self.peer.clone();
        self.git_in("peer git", &peer, args)
    }

    pub fn git_in(&mut self, label: &str, dir: &Path, args: &[&str]) -> Output {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(dir);
        self.env(&mut cmd);
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")));
        self.record(label, args, &out);
        out
    }

    pub fn record(&mut self, prog: &str, args: &[&str], out: &Output) {
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

    pub fn must(&mut self, label: &str, out: &Output) {
        assert!(
            out.status.success(),
            "{label} failed (fixture, not the journey):\n{}\n\ntranscript:\n{}",
            String::from_utf8_lossy(&out.stderr),
            self.transcript.join("\n")
        );
    }

    pub fn observe(&mut self, n: u8, what: &str, ok: bool, detail: String) {
        let mark = if ok { "ok " } else { "RED" };
        self.transcript
            .push(format!("[{n}] {mark} {what}: {detail}"));
        if !ok {
            self.violations.push(format!("({n}) {what}: {detail}"));
        }
    }

    pub fn jsonl_status(&mut self) -> String {
        let out = self.git(&["status", "--porcelain", "--", JSONL]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    /// The bead's status line as `rsry bead list --status all` prints it.
    pub fn bead_line(&mut self, id: &str) -> String {
        let out = self.rsry(&["bead", "list", "--status", "all"]);
        journey_stdout(&out)
            .lines()
            .find(|l| l.contains(id))
            .unwrap_or("")
            .to_string()
    }

    /// Fixture: `main` with the tracked export committed and pushed, hooks
    /// installed by `rsry init` and proven active, the working tree clean.
    /// `seed_beads` are created BEFORE the export is tracked, so the journey's
    /// first store write is the surface under test, not the fixture. Bare
    /// creates: the default close condition is the PR-merge signal, which is
    /// what lets `close-merged` auto-close them.
    pub fn seed(&mut self, seed_beads: &[&str]) -> Vec<String> {
        let o = self.git(&["init", "-q", "-b", "main"]);
        self.must("git init", &o);
        self.write("README.md", "# journey\n");
        let o = self.git(&["add", "README.md"]);
        self.must("add README", &o);
        let o = self.git(&[
            "commit",
            "-q",
            "--no-verify",
            "-m",
            "[rosary-000000] chore: seed",
        ]);
        self.must("seed commit", &o);

        let root = self.root.to_string_lossy().into_owned();
        let o = self.rsry(&["init", &root]);
        self.must("rsry init", &o);
        let ids: Vec<String> = seed_beads
            .iter()
            .map(|title| {
                let o = self.rsry(&["bead", "create", title, "--files", "a.rs"]);
                self.must("seed bead create", &o);
                created_id(&o)
            })
            .collect();
        let o = self.rsry(&["bead", "export", "--jsonl", "--status", "all", "-o", JSONL]);
        self.must("initial export", &o);
        let o = self.git(&[
            "add",
            JSONL,
            ".beads/metadata.json",
            ".beads/.gitignore",
            "AGENTS.md",
        ]);
        self.must("stage export", &o);
        let o = self.git(&[
            "commit",
            "-q",
            "--no-verify",
            "-m",
            "[rosary-000000] chore(beads): track export",
        ]);
        self.must("track export", &o);
        assert_hooks_active(self);

        let remote = self.remote.path().to_string_lossy().into_owned();
        let o = self.git(&["init", "-q", "--bare", &remote]);
        self.must("bare remote", &o);
        let o = self.git(&["remote", "add", "origin", &remote]);
        self.must("remote add", &o);
        let o = self.git(&["push", "-q", "origin", "main"]);
        self.must("push main (fixture)", &o);
        assert_eq!(self.jsonl_status(), "", "fixture must start clean");
        ids
    }

    /// Fixture: a feature branch with one code-only commit, pushed (through the
    /// real pre-push hook, on a clean tree) so a peer can build on it.
    pub fn start_feature(&mut self) {
        let o = self.git(&["checkout", "-q", "-b", "feature"]);
        self.must("checkout feature", &o);
        self.write("a.rs", "fn a() {}\n");
        let o = self.git(&["add", "a.rs"]);
        self.must("add a.rs", &o);
        let o = self.git(&[
            "commit",
            "-q",
            "-m",
            "[rosary-000000] feat(core): code only",
        ]);
        self.must("code-only commit", &o);
        let o = self.git(&["push", "-q", "-u", "origin", "feature"]);
        self.must("push feature (fixture)", &o);
        assert_eq!(self.jsonl_status(), "", "fixture must still be clean");
    }

    /// (1) One store write from the surface under test leaves the tracked
    /// export clean.
    pub fn observe_write_clean(&mut self) {
        let status = self.jsonl_status();
        self.observe(
            1,
            "one bead write leaves the tracked export clean",
            status.is_empty(),
            format!("git status -> {status:?}"),
        );
    }

    /// (2) An explicit-path commit through the real pre-commit hook carries
    /// no bead record but its own — `other`'s churn must not be swept in.
    pub fn observe_explicit_commit(&mut self, id: &str, other: &str) {
        self.write("b.rs", "fn b() {}\n");
        let o = self.git(&["add", "b.rs"]);
        self.must("add b.rs", &o);
        let subject = format!("[{id}] feat(core): more code");
        let o = self.git(&["commit", "-q", "-m", &subject]);
        self.must("explicit-path commit", &o);
        let files = self.git(&["show", "--name-only", "--format=", "HEAD"]);
        let files = journey_stdout(&files);
        assert!(
            files.contains("b.rs"),
            "the staged file must be in the commit:\n{files}"
        );
        let diff = self.git(&["show", "--format=", "HEAD", "--", JSONL]);
        let swept: Vec<String> = added_ids(&journey_stdout(&diff))
            .into_iter()
            .filter(|x| x != id)
            .collect();
        self.observe(2, "explicit-path commit carries no bead record but its own", swept.is_empty(),
            format!("commit touched {}; records swept in besides {id}: {swept:?} (unrelated bead {other} must not appear)",
                files.split_whitespace().collect::<Vec<_>>().join(", ")));
    }

    /// (3) After a store write, push through the real pre-push hook: either the
    /// push is refused, or the pushed blob carries the write (`needle`).
    pub fn observe_push_feature(&mut self, needle: &str) {
        let push = self.git(&["push", "-q", "origin", "feature"]);
        let pushed_blob = self.git(&["show", &format!("origin/feature:{JSONL}")]);
        let blob_has_write = journey_stdout(&pushed_blob).contains(needle);
        let ok = !push.status.success() || blob_has_write;
        self.observe(
            3,
            "push is refused, or the pushed blob agrees with the store",
            ok,
            format!(
                "push exit {}, pushed blob has {needle:?}: {blob_has_write}",
                push.status.code().unwrap_or(-1)
            ),
        );
    }

    /// (4) switch back to main with no stash; (5) nothing was written on main,
    /// so pushing main must not be refused.
    pub fn observe_checkout_main_and_push(&mut self) {
        let co = self.git(&["checkout", "-q", "main"]);
        self.observe(
            4,
            "git checkout main succeeds without a stash",
            co.status.success(),
            String::from_utf8_lossy(&co.stderr).trim().to_string(),
        );
        if !co.status.success() {
            // Recover the way the owner does, so the remaining observation runs.
            let o = self.git(&["checkout", "--", JSONL]);
            self.must("discard projection", &o);
            let o = self.git(&["checkout", "-q", "main"]);
            self.must("checkout main after discard", &o);
        }
        let push_main = self.git(&["push", "-q", "origin", "main"]);
        self.observe(
            5,
            "push main is not refused by a bead written on feature",
            push_main.status.success(),
            String::from_utf8_lossy(&push_main.stderr)
                .lines()
                .find(|l| l.contains("ERROR"))
                .unwrap_or("")
                .to_string(),
        );
    }

    /// The journey's verdict: every violation at once, with the transcript.
    pub fn finish(self, surface: &str) {
        assert!(
            self.violations.is_empty(),
            "beads.jsonl dirty-plague journey via {surface} (rosary-3d455a) — {} violation(s):\n  {}\n\ntranscript:\n{}",
            self.violations.len(),
            self.violations.join("\n  "),
            self.transcript.join("\n")
        );
    }
}

pub fn journey_stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Bead ids named in the `+` lines of a diff of the tracked export.
pub fn added_ids(diff: &str) -> Vec<String> {
    diff.lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .filter_map(|l| {
            let rec: serde_json::Value = serde_json::from_str(&l[1..]).ok()?;
            rec["id"].as_str().map(str::to_owned)
        })
        .collect()
}

/// Guard against a vacuous pass: every observation is trivially green when
/// the hooks are not actually wired, so prove they are before observing.
pub fn assert_hooks_active(j: &mut Journey) {
    let hooks_path = j.git(&["config", "--get", "core.hooksPath"]);
    let hooks_path = journey_stdout(&hooks_path).trim().to_string();
    assert_ne!(hooks_path, "/dev/null", "fixture must not disable hooks");
    let dir = j.git(&["rev-parse", "--git-path", "hooks"]);
    let dir = j.root.join(journey_stdout(&dir).trim());
    for hook in ["pre-commit", "pre-push", "commit-msg", "post-merge"] {
        let body = std::fs::read_to_string(dir.join(hook)).unwrap_or_else(|e| {
            panic!(
                "{hook} not installed by rsry init at {}: {e}",
                dir.display()
            )
        });
        assert!(body.contains("rsry"), "{hook} is not the rsry-managed hook");
    }
}
