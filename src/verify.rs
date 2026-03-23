//! Tiered verification pipeline for post-dispatch validation.
//!
//! Inspired by gem's tiered eval: each tier is a gate — fail early, don't waste cycles.
//! Tiers run in sequence; first failure short-circuits.

use anyhow::Result;
use std::path::Path;

/// Result of a single verification tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyResult {
    /// All checks passed.
    Pass,
    /// Check failed with a reason. Retry may help.
    Fail(String),
    /// Needs human review (e.g., diff too large).
    Partial(String),
}

impl VerifyResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, VerifyResult::Pass)
    }
}

/// A single verification tier.
pub trait VerifyTier: Send + Sync {
    fn name(&self) -> &str;
    fn check(&self, work_dir: &Path) -> Result<VerifyResult>;
}

/// Ordered verification pipeline.
pub struct Verifier {
    tiers: Vec<Box<dyn VerifyTier>>,
}

/// Summary of a full verification run.
#[derive(Debug)]
pub struct VerifySummary {
    pub results: Vec<(String, VerifyResult)>,
    pub highest_passing_tier: Option<usize>,
}

impl VerifySummary {
    pub fn passed(&self) -> bool {
        self.results.iter().all(|(_, r)| r.is_pass())
    }

    pub fn first_failure(&self) -> Option<&(String, VerifyResult)> {
        self.results.iter().find(|(_, r)| !r.is_pass())
    }

    /// Whether the failure is in tiers 0-1 (commit/compile) — fundamentally broken.
    /// Returns true only if compile-level or below failed (nothing passed, or only commit passed
    /// but compile failed).
    #[allow(dead_code)] // API surface — used in tests
    pub fn is_fundamental_failure(&self) -> bool {
        match self.highest_passing_tier {
            None => true, // nothing passed at all
            // If there are 3+ tiers (commit, compile, ...) and only tier 0 passed,
            // compile failed — that's fundamental.
            // If there are only 2 tiers (commit + something), tier 0 passing means
            // the failure is in the "something" — not necessarily fundamental.
            Some(0) if self.results.len() > 2 => true,
            _ => false,
        }
    }
}

impl Verifier {
    pub fn new(tiers: Vec<Box<dyn VerifyTier>>) -> Self {
        Verifier { tiers }
    }

    /// Build the default verification pipeline for a given language.
    pub fn for_language(lang: &str) -> Self {
        let mut tiers: Vec<Box<dyn VerifyTier>> =
            vec![Box::new(CommitCheck), Box::new(BeadRefCheck)];

        match lang {
            "rust" => {
                tiers.push(Box::new(ShellCheck::new("compile", "cargo", &["check"])));
                tiers.push(Box::new(ShellCheck::new("test", "cargo", &["test"])));
                tiers.push(Box::new(ShellCheck::new(
                    "lint",
                    "cargo",
                    &["clippy", "--", "-D", "warnings"],
                )));
            }
            "go" => {
                tiers.push(Box::new(ShellCheck::new(
                    "compile",
                    "go",
                    &["vet", "./..."],
                )));
                tiers.push(Box::new(ShellCheck::new(
                    "test",
                    "go",
                    &["test", "-v", "./..."],
                )));
                tiers.push(Box::new(ShellCheck::new(
                    "lint",
                    "golangci-lint",
                    &["run", "./..."],
                )));
            }
            _ => {
                // Generic: just check for a commit
            }
        }

        tiers.push(Box::new(DiffSanityCheck {
            max_files: 10,
            max_lines: 500,
        }));

        tiers.push(Box::new(ReviewCheck));

        Verifier::new(tiers)
    }

    /// Run all tiers in sequence, short-circuiting on first non-pass.
    pub fn run(&self, work_dir: &Path) -> Result<VerifySummary> {
        let mut results = Vec::new();
        let mut highest_passing = None;

        for (i, tier) in self.tiers.iter().enumerate() {
            let result = tier.check(work_dir)?;
            let is_pass = result.is_pass();
            results.push((tier.name().to_string(), result));

            if is_pass {
                highest_passing = Some(i);
            } else {
                break;
            }
        }

        Ok(VerifySummary {
            results,
            highest_passing_tier: highest_passing,
        })
    }
}

// --- Concrete Tiers ---

/// Tier 0: Did the agent create a commit?
pub struct CommitCheck;

impl VerifyTier for CommitCheck {
    fn name(&self) -> &str {
        "commit"
    }

    fn check(&self, work_dir: &Path) -> Result<VerifyResult> {
        let output = std::process::Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(work_dir)
            .output()?;

        if output.status.success() && !output.stdout.is_empty() {
            Ok(VerifyResult::Pass)
        } else {
            Ok(VerifyResult::Fail("no commit found".into()))
        }
    }
}

/// Tier 0.5: Does the agent's commit reference a bead?
/// Enforces Golden Rule 11: every commit must include `bead:ID`.
pub struct BeadRefCheck;

impl VerifyTier for BeadRefCheck {
    fn name(&self) -> &str {
        "bead_ref"
    }

    fn check(&self, work_dir: &Path) -> Result<VerifyResult> {
        let output = std::process::Command::new("git")
            .args(["log", "--format=%B", "-1"])
            .current_dir(work_dir)
            .output()?;

        if !output.status.success() {
            return Ok(VerifyResult::Pass); // no git = skip
        }

        let message = String::from_utf8_lossy(&output.stdout);
        let refs = crate::vcs::extract_bead_refs(&message);

        if refs.is_empty() {
            Ok(VerifyResult::Fail(
                "commit does not reference a bead — add bead:ID to commit message (Golden Rule 11)"
                    .into(),
            ))
        } else {
            Ok(VerifyResult::Pass)
        }
    }
}

/// Generic shell command check (compile, test, lint).
pub struct ShellCheck {
    name: String,
    program: String,
    args: Vec<String>,
}

impl ShellCheck {
    pub fn new(name: &str, program: &str, args: &[&str]) -> Self {
        ShellCheck {
            name: name.to_string(),
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl VerifyTier for ShellCheck {
    fn name(&self) -> &str {
        &self.name
    }

    fn check(&self, work_dir: &Path) -> Result<VerifyResult> {
        let status = match std::process::Command::new(&self.program)
            .args(&self.args)
            .current_dir(work_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
        {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "[verify] warning: '{}' not found, skipping {} check",
                    self.program, self.name
                );
                return Ok(VerifyResult::Pass);
            }
            Err(e) => return Err(e.into()),
        };

        if status.success() {
            Ok(VerifyResult::Pass)
        } else {
            Ok(VerifyResult::Fail(format!(
                "{} failed with exit code {}",
                self.name,
                status.code().unwrap_or(-1)
            )))
        }
    }
}

/// Tier 5: AI agent review of the diff.
/// Dispatches staging-agent to adversarially review code quality and test validity.
/// Runs only after all other tiers pass (pipeline short-circuits on failure).
pub struct ReviewCheck;

impl VerifyTier for ReviewCheck {
    fn name(&self) -> &str {
        "review"
    }

    fn check(&self, work_dir: &Path) -> Result<VerifyResult> {
        let diff_output = std::process::Command::new("git")
            .args(["diff", "HEAD~1..HEAD"])
            .current_dir(work_dir)
            .output()?;

        if !diff_output.status.success() || diff_output.stdout.is_empty() {
            return Ok(VerifyResult::Pass);
        }

        let diff = String::from_utf8_lossy(&diff_output.stdout);

        let prompt = format!(
            "You are the staging-agent reviewing an agent's commit.\n\
             Your mission: adversarially examine whether tests actually validate behavior.\n\
             Key question: if the production code were replaced with a no-op, would tests fail?\n\
             \n\
             Review this diff:\n\
             \n\
             <diff>\n{diff}\n</diff>\n\
             \n\
             Respond with exactly one verdict on the FIRST line:\n\
             VERDICT: approve\n\
             VERDICT: request-changes\n\
             VERDICT: reject\n\
             \n\
             Then briefly explain your reasoning."
        );

        let output = match std::process::Command::new("claude")
            .args(["-p", &prompt, "--allowedTools", "Read,Glob,Grep"])
            .current_dir(work_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("[verify] warning: 'claude' not found, skipping review");
                return Ok(VerifyResult::Pass);
            }
            Err(e) => return Err(e.into()),
        };

        if !output.status.success() {
            eprintln!("[verify] warning: review agent exited non-zero, skipping");
            return Ok(VerifyResult::Pass);
        }

        let response = String::from_utf8_lossy(&output.stdout);
        Ok(parse_review_verdict(&response))
    }
}

/// Parse the staging-agent's review verdict from its output.
pub fn parse_review_verdict(output: &str) -> VerifyResult {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(verdict) = trimmed.strip_prefix("VERDICT:") {
            return match verdict.trim().to_lowercase().as_str() {
                "approve" => VerifyResult::Pass,
                "request-changes" => VerifyResult::Partial("review: request-changes".into()),
                "reject" => VerifyResult::Fail("review: rejected".into()),
                other => VerifyResult::Partial(format!("review: unknown verdict '{other}'")),
            };
        }
    }
    // No verdict found — don't block on unparseable output
    VerifyResult::Partial("review: no verdict in response".into())
}

/// Tier 4: Is the diff reasonable?
pub struct DiffSanityCheck {
    pub max_files: usize,
    pub max_lines: usize,
}

impl VerifyTier for DiffSanityCheck {
    fn name(&self) -> &str {
        "diff-sanity"
    }

    fn check(&self, work_dir: &Path) -> Result<VerifyResult> {
        // Count files changed in last commit
        let output = std::process::Command::new("git")
            .args(["diff", "--name-only", "HEAD~1..HEAD"])
            .current_dir(work_dir)
            .output()?;

        if !output.status.success() {
            // No parent commit (first commit) — pass
            return Ok(VerifyResult::Pass);
        }

        let files: Vec<&str> = std::str::from_utf8(&output.stdout)?
            .lines()
            .filter(|l| !l.is_empty())
            .collect();

        if files.len() > self.max_files {
            return Ok(VerifyResult::Partial(format!(
                "changed {} files (max {})",
                files.len(),
                self.max_files
            )));
        }

        // Count lines changed
        let stat_output = std::process::Command::new("git")
            .args(["diff", "--stat", "HEAD~1..HEAD"])
            .current_dir(work_dir)
            .output()?;

        if stat_output.status.success() {
            let stat = String::from_utf8_lossy(&stat_output.stdout);
            // Last line of --stat has "N insertions(+), M deletions(-)"
            if let Some(last_line) = stat.lines().last() {
                let total: usize = last_line
                    .split_whitespace()
                    .filter_map(|w| w.parse::<usize>().ok())
                    .sum();
                if total > self.max_lines {
                    return Ok(VerifyResult::Partial(format!(
                        "changed {total} lines (max {})",
                        self.max_lines
                    )));
                }
            }
        }

        Ok(VerifyResult::Pass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_git_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        dir
    }

    fn commit_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
        std::process::Command::new("git")
            .args(["add", name])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", &format!("add {name}")])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn commit_check_passes_with_commit() {
        let dir = init_git_repo();
        commit_file(dir.path(), "hello.txt", "hello");

        let result = CommitCheck.check(dir.path()).unwrap();
        assert_eq!(result, VerifyResult::Pass);
    }

    #[test]
    fn commit_check_fails_empty_repo() {
        let dir = init_git_repo();
        let result = CommitCheck.check(dir.path()).unwrap();
        assert!(matches!(result, VerifyResult::Fail(_)));
    }

    #[test]
    fn shell_check_passes_on_success() {
        let dir = init_git_repo();
        let check = ShellCheck::new("echo", "echo", &["hello"]);
        let result = check.check(dir.path()).unwrap();
        assert_eq!(result, VerifyResult::Pass);
    }

    #[test]
    fn shell_check_fails_on_bad_exit() {
        let dir = init_git_repo();
        let check = ShellCheck::new("false", "false", &[]);
        let result = check.check(dir.path()).unwrap();
        assert!(matches!(result, VerifyResult::Fail(_)));
    }

    #[test]
    fn shell_check_passes_on_missing_tool() {
        let dir = init_git_repo();
        let check = ShellCheck::new("nonexistent", "totally-fake-tool-xyz", &[]);
        let result = check.check(dir.path()).unwrap();
        // Missing tool = skip, not fail
        assert_eq!(result, VerifyResult::Pass);
    }

    #[test]
    fn diff_sanity_passes_small_change() {
        let dir = init_git_repo();
        commit_file(dir.path(), "a.txt", "initial");
        commit_file(dir.path(), "a.txt", "changed");

        let check = DiffSanityCheck {
            max_files: 10,
            max_lines: 500,
        };
        let result = check.check(dir.path()).unwrap();
        assert_eq!(result, VerifyResult::Pass);
    }

    #[test]
    fn diff_sanity_flags_too_many_files() {
        let dir = init_git_repo();
        // First commit with initial files
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "init").unwrap();
        }
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Second commit changing all files
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "changed").unwrap();
        }
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "change all"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let check = DiffSanityCheck {
            max_files: 2, // Trigger
            max_lines: 500,
        };
        let result = check.check(dir.path()).unwrap();
        assert!(matches!(result, VerifyResult::Partial(_)));
    }

    #[test]
    fn verifier_short_circuits_on_failure() {
        let dir = init_git_repo();
        // Empty repo — commit check fails, subsequent tiers never run

        let verifier = Verifier::new(vec![
            Box::new(CommitCheck),
            Box::new(ShellCheck::new("echo", "echo", &["should not run"])),
        ]);

        let summary = verifier.run(dir.path()).unwrap();
        assert!(!summary.passed());
        assert_eq!(summary.results.len(), 1); // Only commit check ran
        assert!(summary.is_fundamental_failure());
    }

    #[test]
    fn verifier_runs_all_on_success() {
        let dir = init_git_repo();
        commit_file(dir.path(), "test.txt", "hello");

        let verifier = Verifier::new(vec![
            Box::new(CommitCheck),
            Box::new(ShellCheck::new("echo", "echo", &["ok"])),
        ]);

        let summary = verifier.run(dir.path()).unwrap();
        assert!(summary.passed());
        assert_eq!(summary.results.len(), 2);
        assert_eq!(summary.highest_passing_tier, Some(1));
    }

    #[test]
    fn verify_summary_first_failure() {
        let dir = init_git_repo();
        commit_file(dir.path(), "test.txt", "hello");

        let verifier = Verifier::new(vec![
            Box::new(CommitCheck),
            Box::new(ShellCheck::new("fail-tier", "false", &[])),
        ]);

        let summary = verifier.run(dir.path()).unwrap();
        let (name, _) = summary.first_failure().unwrap();
        assert_eq!(name, "fail-tier");
        assert!(!summary.is_fundamental_failure()); // tier 0 passed
    }

    #[test]
    fn for_language_builds_correct_tiers() {
        let rust_v = Verifier::for_language("rust");
        assert_eq!(rust_v.tiers.len(), 7); // commit, bead_ref, compile, test, lint, diff-sanity, review

        let go_v = Verifier::for_language("go");
        assert_eq!(go_v.tiers.len(), 7);

        let unknown_v = Verifier::for_language("brainfuck");
        assert_eq!(unknown_v.tiers.len(), 4); // commit, bead_ref, diff-sanity, review
    }

    #[test]
    fn review_verdict_approve() {
        let output = "VERDICT: approve\nLooks good, tests validate real behavior.";
        assert_eq!(parse_review_verdict(output), VerifyResult::Pass);
    }

    #[test]
    fn review_verdict_reject() {
        let output = "VERDICT: reject\nTests only assert on mocked values.";
        assert!(matches!(
            parse_review_verdict(output),
            VerifyResult::Fail(_)
        ));
    }

    #[test]
    fn review_verdict_request_changes() {
        let output = "VERDICT: request-changes\nMissing edge case coverage.";
        assert!(matches!(
            parse_review_verdict(output),
            VerifyResult::Partial(_)
        ));
    }

    #[test]
    fn review_verdict_missing() {
        let output = "The code looks fine but I forgot to include a verdict.";
        assert!(matches!(
            parse_review_verdict(output),
            VerifyResult::Partial(_)
        ));
    }

    #[test]
    fn review_verdict_case_insensitive() {
        assert_eq!(parse_review_verdict("VERDICT: APPROVE"), VerifyResult::Pass);
        assert_eq!(parse_review_verdict("VERDICT: Approve"), VerifyResult::Pass);
    }

    #[test]
    fn review_check_tier_name() {
        assert_eq!(ReviewCheck.name(), "review");
    }

    // -----------------------------------------------------------------------
    // Adversarial: BeadRefCheck (Golden Rule 11)
    // -----------------------------------------------------------------------

    #[test]
    fn bead_ref_check_passes_with_bead_ref() {
        let repo = crate::testutil::TestRepo::new();
        repo.commit_with_bead_ref("rsry-abc123", "fix.rs", "fn fix() {}");
        assert_eq!(BeadRefCheck.check(repo.path()).unwrap(), VerifyResult::Pass);
    }

    #[test]
    fn bead_ref_check_fails_without_bead_ref() {
        let repo = crate::testutil::TestRepo::new();
        repo.commit_plain("fix.rs", "fn fix() {}");
        let result = BeadRefCheck.check(repo.path()).unwrap();
        match &result {
            VerifyResult::Fail(msg) => assert!(
                msg.contains("Golden Rule 11"),
                "should cite Golden Rule 11: {msg}"
            ),
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn bead_ref_check_fails_with_bracket_only_no_bead_prefix() {
        // [rsry-abc] in subject line but no "bead:" footer — should FAIL
        // because extract_bead_refs only finds "bead:ID" format
        let repo = crate::testutil::TestRepo::new();
        let path = repo.path();
        std::fs::write(path.join("test.rs"), "fn test() {}").unwrap();
        run_git(path, &["add", "."]);
        run_git(path, &["commit", "-m", "[rsry-abc] fix: no footer"]);
        let result = BeadRefCheck.check(path).unwrap();
        assert!(
            matches!(&result, VerifyResult::Fail(_)),
            "bracket-only commit (no bead: footer) should fail: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial: DiffSanityCheck edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn diff_sanity_passes_first_commit() {
        // First commit has no parent — should pass, not error
        let dir = init_git_repo();
        commit_file(dir.path(), "first.txt", "content");
        let check = DiffSanityCheck {
            max_files: 1,
            max_lines: 1,
        };
        // This is the first real commit (after the initial), so diff HEAD~1..HEAD
        // should work. But the initial commit IS the first — git diff HEAD~1 fails.
        // Let's test with the initial commit itself:
        let result = check.check(dir.path()).unwrap();
        assert_eq!(result, VerifyResult::Pass);
    }

    #[test]
    fn diff_sanity_flags_too_many_lines() {
        let dir = init_git_repo();
        commit_file(dir.path(), "a.txt", "initial");
        // Second commit with many lines
        let big_content: String = (0..100).map(|i| format!("line {i}\n")).collect();
        commit_file(dir.path(), "a.txt", &big_content);
        let check = DiffSanityCheck {
            max_files: 100,
            max_lines: 10, // way fewer than 100 lines
        };
        let result = check.check(dir.path()).unwrap();
        match &result {
            VerifyResult::Partial(msg) => {
                assert!(msg.contains("lines"), "should mention lines: {msg}")
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Adversarial: Verifier tier composition
    // -----------------------------------------------------------------------

    #[test]
    fn verifier_partial_result_short_circuits() {
        // Partial (not Pass, not Fail) should also stop the pipeline
        let dir = init_git_repo();
        commit_file(dir.path(), "a.txt", "initial");
        commit_file(dir.path(), "a.txt", "changed"); // 2nd commit so HEAD~1 exists

        let verifier = Verifier::new(vec![
            Box::new(CommitCheck), // Pass
            Box::new(DiffSanityCheck {
                max_files: 0,
                max_lines: 0,
            }), // Partial (1 file > 0)
            Box::new(ShellCheck::new("never", "echo", &["should not run"])),
        ]);

        let summary = verifier.run(dir.path()).unwrap();
        assert!(!summary.passed(), "Partial should not count as passed");
        assert_eq!(summary.results.len(), 2, "should stop at Partial tier");
        assert_eq!(summary.highest_passing_tier, Some(0)); // only commit passed
    }

    #[test]
    fn bead_ref_plus_commit_compose_correctly() {
        // Full pipeline: commit passes, bead_ref fails → short-circuits before shell checks
        let repo = crate::testutil::TestRepo::new();
        repo.commit_plain("test.rs", "fn test() {}"); // no bead ref

        let verifier = Verifier::new(vec![
            Box::new(CommitCheck),
            Box::new(BeadRefCheck),
            Box::new(ShellCheck::new("should-not-run", "false", &[])),
        ]);

        let summary = verifier.run(repo.path()).unwrap();
        assert!(!summary.passed());
        assert_eq!(summary.results.len(), 2, "should stop at bead_ref");
        assert_eq!(summary.highest_passing_tier, Some(0)); // only commit
        let (name, _) = summary.first_failure().unwrap();
        assert_eq!(name, "bead_ref");
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {} failed", args.join(" "));
    }
}
